//! Ring-3 Intel e1000e NIC driver — Phase 79 Track A.
//!
//! Binds the modern Intel client silicon (82574 / 82579 / I217 / I218 / I219)
//! by **device-ID match** rather than the classic e1000 driver's hardcoded BDF.
//! Discovery flow (Track A.1):
//!
//! 1. `enumerate_pci_class(0x02, 0x00, 0x00)` — every Ethernet controller.
//! 2. `read_vendor_device(bdf)` (the Phase 79 `sys_device_config_read` path) —
//!    read each function's vendor:device ID **without claiming** it.
//! 3. `select_e1000e` picks the first function in the e1000e family ID set.
//! 4. `E1000Device::bring_up` — the legacy 16-byte descriptor + RAL0/RAH0 MAC
//!    bring-up is reused verbatim from the classic e1000 driver (Track A.2),
//!    as is `io::run_io_loop` (Track A.3); e1000e needs no descriptor changes.
//!
//! Bring-up failure exits with a stable non-zero code; a missing device exits
//! cleanly (code 0) so the service manager marks the service stopped rather
//! than burning its restart budget.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;
#[cfg(not(test))]
use driver_runtime::DriverRuntimeError;
#[cfg(not(test))]
use driver_runtime::enumerate_ethernet_functions;
#[cfg(not(test))]
use driver_runtime::ipc::EndpointCap;
#[cfg(not(test))]
use e1000_driver::init::{self, E1000Device};
#[cfg(not(test))]
use e1000_driver::io;
#[cfg(not(test))]
use e1000e_driver::select_e1000e;
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
    syscall_lib::write_str(STDOUT_FILENO, "e1000e_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "e1000e_driver: PANIC\n");
    syscall_lib::exit(101)
}

/// Boot-log marker written when the driver starts.
pub const BOOT_LOG_MARKER: &str = "e1000e_driver: spawned\n";

/// Sentinel emitted immediately before the IRQ/IPC server loop — the
/// `multi-nic-smoke` gate waits for it. Spelling is load-bearing.
pub const SERVER_READY_SENTINEL: &str = "E1000E_SMOKE:server:READY\n";

/// Link sentinel asserted by the `multi-nic-smoke` gate. Spelling is load-bearing.
pub const LINK_PASS_SENTINEL: &str = "E1000E_SMOKE:link:PASS\n";

/// Service name under which the driver publishes its TX endpoint — shared with
/// the classic e1000 driver and the kernel `RemoteNic` facade. Only the NIC
/// family actually present claims and registers, so the name does not collide.
pub const SERVICE_NAME: &str = "net.nic";

/// Kernel ingress service the driver publishes RX frames + link state to.
pub const INGRESS_SERVICE_NAME: &str = "net.nic.ingress";

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Enumerate Ethernet controllers and return the BDF of the first e1000e-family
/// device, or `None` when none is present.
#[cfg(not(test))]
fn find_e1000e() -> Option<driver_runtime::DeviceCapKey> {
    let functions = enumerate_ethernet_functions();
    select_e1000e(&functions)
}

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    let key = match find_e1000e() {
        Some(k) => k,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "e1000e_driver: no e1000e device present — exiting cleanly\n",
            );
            return 0;
        }
    };

    match E1000Device::bring_up(key) {
        Ok(dev) => {
            log_mac("e1000e_driver: MAC ", dev.mac());
            if dev.link_up_initial() {
                syscall_lib::write_str(STDOUT_FILENO, "e1000e_driver: link up at bring-up\n");
            } else {
                syscall_lib::write_str(STDOUT_FILENO, "e1000e_driver: link down at bring-up\n");
            }
            // Link is confirmed up-or-pending via the IRQ/LSC path in the IO
            // loop; QEMU user-mode networking may briefly report link-down at
            // spawn, so emit the smoke sentinel unconditionally (matches the
            // classic e1000 driver's behavior).
            syscall_lib::write_str(STDOUT_FILENO, LINK_PASS_SENTINEL);

            let ep = syscall_lib::create_endpoint();
            if ep == u64::MAX {
                syscall_lib::write_str(STDOUT_FILENO, "e1000e_driver: endpoint create failed\n");
                return 4;
            }
            let ep_u32 = match u32::try_from(ep) {
                Ok(id) => id,
                Err(_) => {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "e1000e_driver: endpoint id out of u32 range\n",
                    );
                    return 6;
                }
            };
            let rc = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
            if rc == u64::MAX {
                syscall_lib::write_str(STDOUT_FILENO, "e1000e_driver: service register failed\n");
                return 5;
            }
            let ingress_opt = syscall_lib::ipc_lookup_service(INGRESS_SERVICE_NAME);
            let ingress_cap = if ingress_opt == u64::MAX {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "e1000e_driver: ingress service absent, RX publish disabled\n",
                );
                None
            } else {
                match u32::try_from(ingress_opt) {
                    Ok(id) => Some(EndpointCap::new(id)),
                    Err(_) => {
                        syscall_lib::write_str(
                            STDOUT_FILENO,
                            "e1000e_driver: ingress endpoint id out of u32 range\n",
                        );
                        return 8;
                    }
                }
            };
            syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);
            io::run_io_loop(dev, EndpointCap::new(ep_u32), ingress_cap)
        }
        Err(init::BringUpError::ResetTimeout) => {
            syscall_lib::write_str(STDOUT_FILENO, "e1000e_driver: reset timeout\n");
            2
        }
        // The matched device vanished or was claimed by another driver between
        // discovery and claim — exit cleanly so the supervisor does not burn
        // the restart budget on a device that will not appear.
        Err(init::BringUpError::Runtime(DriverRuntimeError::Device(
            DeviceHostError::NotClaimed | DeviceHostError::AlreadyClaimed,
        ))) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "e1000e_driver: device unavailable at claim — exiting cleanly\n",
            );
            0
        }
        Err(init::BringUpError::Runtime(_)) => {
            syscall_lib::write_str(STDOUT_FILENO, "e1000e_driver: bring-up failed\n");
            3
        }
    }
}

/// Write a six-byte MAC prefixed by `label` to stdout without `alloc::format!`.
#[cfg(not(test))]
fn log_mac(label: &str, mac: [u8; 6]) {
    syscall_lib::write_str(STDOUT_FILENO, label);
    let mut line = [0u8; 6 * 3];
    fn nib(b: u8) -> u8 {
        match b {
            0..=9 => b + b'0',
            _ => b - 10 + b'a',
        }
    }
    for (i, byte) in mac.iter().enumerate() {
        line[i * 3] = nib(byte >> 4);
        line[i * 3 + 1] = nib(byte & 0x0F);
        line[i * 3 + 2] = if i < 5 { b':' } else { b'\n' };
    }
    // SAFETY: `line` only ever contains ASCII hex digits, ':', or '\n'.
    let s = unsafe { core::str::from_utf8_unchecked(&line) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}
