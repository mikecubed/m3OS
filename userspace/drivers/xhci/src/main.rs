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

/// Bring-up glue (MMIO / DMA / IRQ) around the host-tested
/// `kernel_core::usb::xhci` pure logic. Compiled only for the OS target —
/// it speaks the syscall ABI and has no host-test surface.
#[cfg(not(test))]
mod controller;

#[cfg(not(test))]
use crate::controller::{BringUpError, Controller, XhciBar0};
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

    // Discover the register regions + capabilities (A.2).
    let mut controller = Controller::new(handle, bar0);
    write_ports_detected(controller.max_ports());
    // Context size (32 vs 64) is selected from HCCPARAMS1.CSZ and threaded
    // into all later context allocation; report it during discovery.
    syscall_lib::write_str(STDOUT_FILENO, "[xhci] context size ");
    write_u8_dec(controller.context_size() as u8);
    syscall_lib::write_str(STDOUT_FILENO, " bytes\n");

    // Ordered bring-up (A.3 checklist): handoff → reset(CNR) → MaxSlotsEn →
    // DCBAA(+scratchpad) → command ring → event ring(ERST) → MSI-X
    // interrupter → run → Enable Slot. Any stage failure exits non-zero so
    // the service manager observes it.
    if let Err(e) = controller.release_bios_ownership() {
        return bringup_failed(e);
    }
    if let Err(e) = controller.reset() {
        return bringup_failed(e);
    }
    controller.program_max_slots();
    if let Err(e) = controller.init_dcbaa() {
        return bringup_failed(e);
    }
    if let Err(e) = controller.init_command_ring() {
        return bringup_failed(e);
    }
    if let Err(e) = controller.init_event_ring() {
        return bringup_failed(e);
    }
    let irq = match controller.init_interrupter() {
        Ok(irq) => irq,
        Err(e) => return bringup_failed(e),
    };
    if let Err(e) = controller.run() {
        return bringup_failed(e);
    }

    // A.7: reset any device already connected at the root hub (e.g. a
    // `usb-kbd` present at machine creation) so its port reaches Enabled and
    // its speed is decoded. Hotplug after this is event-driven in the loop.
    controller.scan_ports();

    // Milestone: enqueue Enable Slot, ring Doorbell 0, then drain the event
    // ring on the MSI-X wake — the `XHCI_BRINGUP:enable-slot:OK` sentinel is
    // emitted only from the interrupt-driven completion path.
    controller.enqueue_enable_slot();
    controller.event_loop(irq)
}

/// Map a bring-up stage failure to a stable non-zero exit code + log line.
#[cfg(not(test))]
fn bringup_failed(err: BringUpError) -> i32 {
    let (msg, code) = match err {
        BringUpError::BiosHandoffTimeout => (
            "xhci_driver: BIOS/OS handoff timeout (still BIOS-owned)\n",
            9,
        ),
        BringUpError::ResetTimeout => ("xhci_driver: controller reset timeout\n", 5),
        BringUpError::RunTimeout => ("xhci_driver: controller run timeout (HCH stuck)\n", 6),
        BringUpError::DmaAlloc => ("xhci_driver: DMA allocation failed\n", 7),
        BringUpError::IrqSubscribe => ("xhci_driver: MSI-X IRQ subscribe failed\n", 8),
    };
    syscall_lib::write_str(STDOUT_FILENO, msg);
    code
}

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
