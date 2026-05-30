//! Ring-3 xHCI USB host-controller driver — Phase 78a (host-controller
//! bring-up).
//!
//! Phase 78a stands the controller up: claim the `qemu-xhci` controller,
//! map BAR0, discover the register regions, perform the BIOS/OS handoff and
//! controller reset, program the DCBAA + scratchpad + command ring + event
//! ring (ERST), wire an MSI-X interrupter, set the controller running, and
//! reach a first `Enable Slot` Command Completion event delivered off the
//! event ring **by interrupt**. Device enumeration, hubs and HID are 78b/78c.
//!
//! # Module layout
//!
//! | Module | Purpose |
//! |---|---|
//! | [`regs`] (kernel-core) | Capability-register decoders (host-tested) |
//! | [`trb`] (kernel-core)  | TRB encode/decode + cycle bit + DCI (host-tested) |
//! | [`port`] (kernel-core) | PORTSC bit logic + speed→MPS (host-tested) |
//!
//! The pure-logic layer lives in `kernel_core::usb::xhci`; this crate is the
//! MMIO / DMA / IRQ glue that the host-test layer cannot cover.
//!
//! # Run-time flow
//!
//! 1. `program_main` claims [`SENTINEL_BDF`] and maps BAR0. A missing device
//!    (QEMU launched without `-device qemu-xhci`) is logged and the process
//!    exits cleanly so the service manager marks it permanently stopped
//!    rather than burning its restart budget.
//! 2. The capability registers are discovered and `[xhci] N ports detected`
//!    is emitted.
//! 3. Bring-up (reset → DCBAA/scratchpad/contexts → rings → MSI-X → run →
//!    Enable Slot) runs; on the first interrupt-delivered Command Completion
//!    the driver emits [`ENABLE_SLOT_OK_SENTINEL`].
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;
#[cfg(not(test))]
use driver_runtime::{DeviceCapKey, DeviceHandle, DriverRuntimeError, Mmio};
#[cfg(not(test))]
use kernel_core::device_host::DeviceHostError;
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "xhci_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "xhci_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Boot-log marker written when the driver scaffold starts.
pub const BOOT_LOG_MARKER: &str = "xhci_driver: spawned\n";

/// Sentinel emitted on the first interrupt-delivered `Enable Slot` Command
/// Completion event. The `xhci-bringup-smoke` gate (Track C.1) asserts this
/// exact line; a `[xhci] N ports detected` line alone is **not** sufficient
/// for PASS. The spelling is load-bearing.
pub const ENABLE_SLOT_OK_SENTINEL: &str = "XHCI_BRINGUP:enable-slot:OK\n";

/// Sentinel PCI BDF QEMU assigns to `-device qemu-xhci,addr=0x6` under m3OS
/// (bus 0, device 6, function 0). Slot +6 is the next free slot after the
/// net (3), nvme (4) and audio (5) family slots — see the AC'97 device
/// comment in `xtask/src/main.rs`.
#[cfg(not(test))]
const SENTINEL_BDF: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x06, 0);

/// BAR0 length the driver asks the kernel to map. The xHCI register space
/// (Capability + Operational + Runtime + Doorbell + the MSI-X table) fits
/// comfortably in 64 KiB on `qemu-xhci`; the kernel maps the actual BAR
/// size and this bound only governs the wrapper's debug bounds-check.
#[cfg(not(test))]
const BAR0_EXPECTED_BYTES: usize = 0x1_0000;

/// xHCI Capability register offsets (from BAR0 base).
#[cfg(not(test))]
mod cap_off {
    /// CAPLENGTH is the low byte of the dword at offset 0.
    pub const CAPLENGTH: usize = 0x00;
    /// HCSPARAMS1 — MaxSlots / MaxIntrs / MaxPorts.
    pub const HCSPARAMS1: usize = 0x04;
}

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    let handle = match DeviceHandle::claim(SENTINEL_BDF) {
        Ok(h) => h,
        // The controller is not available to us. `NotClaimed` (ENODEV — QEMU
        // launched without `-device qemu-xhci`) and `AlreadyClaimed` (EBUSY —
        // the slot is occupied by an unrelated device) both mean "no xHCI
        // here"; exit cleanly so init's `on-failure` policy stops the service
        // rather than restarting against a device that will never appear.
        Err(DriverRuntimeError::Device(
            DeviceHostError::NotClaimed | DeviceHostError::AlreadyClaimed,
        )) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "xhci_driver: no qemu-xhci controller at sentinel BDF — exiting cleanly\n",
            );
            return 0;
        }
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "xhci_driver: device claim failed\n");
            return 3;
        }
    };

    let bar0 = match Mmio::<XhciBar0>::map(&handle, 0, BAR0_EXPECTED_BYTES) {
        Ok(m) => m,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "xhci_driver: BAR0 map failed\n");
            return 4;
        }
    };

    syscall_lib::write_str(STDOUT_FILENO, "[xhci] claimed 0000:00:06.0\n");

    let caplength = bar0.read_reg::<u8>(cap_off::CAPLENGTH);
    let hcsparams1 = bar0.read_reg::<u32>(cap_off::HCSPARAMS1);
    let max_ports = ((hcsparams1 >> 24) & 0xFF) as u8;
    let _ = caplength;

    write_ports_detected(max_ports);

    // Full bring-up (reset → DCBAA/scratchpad/contexts → rings → MSI-X → run
    // → Enable Slot) lands in the A.3–A.7 glue. Until then the scaffold has
    // proven claim + BAR map + register discovery; exit cleanly.
    0
}

/// Typestate marker for the xHCI BAR0 MMIO window.
#[cfg(not(test))]
struct XhciBar0;

/// Print `[xhci] N ports detected` without pulling in `alloc::format!`.
#[cfg(not(test))]
fn write_ports_detected(n: u8) {
    syscall_lib::write_str(STDOUT_FILENO, "[xhci] ");
    write_u8_dec(n);
    syscall_lib::write_str(STDOUT_FILENO, " ports detected\n");
}

/// Write a `u8` as decimal to stdout (max three digits).
#[cfg(not(test))]
fn write_u8_dec(mut n: u8) {
    let mut buf = [0u8; 3];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10);
        n /= 10;
        if n == 0 {
            break;
        }
    }
    // SAFETY: `buf[i..]` only ever contains ASCII digits.
    let s = unsafe { core::str::from_utf8_unchecked(&buf[i..]) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}

#[cfg(test)]
mod tests {
    use super::{BOOT_LOG_MARKER, ENABLE_SLOT_OK_SENTINEL};

    #[test]
    fn boot_log_marker_matches_acceptance() {
        assert_eq!(BOOT_LOG_MARKER, "xhci_driver: spawned\n");
    }

    #[test]
    fn enable_slot_sentinel_matches_acceptance() {
        // The xhci-bringup-smoke gate (Track C.1) greps for this exact line.
        assert_eq!(ENABLE_SLOT_OK_SENTINEL, "XHCI_BRINGUP:enable-slot:OK\n");
    }
}
