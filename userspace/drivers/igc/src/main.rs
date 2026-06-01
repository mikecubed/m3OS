//! Ring-3 Intel igc (I225/I226 2.5GbE) NIC driver — Phase 79 Track B.2.
//!
//! Enumerates Ethernet controllers, matches the igc family (`is_igc`,
//! I225/I226 only) via the pre-claim `sys_device_config_read` path, claims the
//! first match, and brings it up on the **advanced** read/write-back descriptor
//! ring with a single-vector EICR/EIMS interrupt path (shared with igb) plus the
//! Clause-45 MMD PHY accessor in `init`. On success it registers `net.nic`,
//! emits `IGC_SMOKE:server:READY`, and enters the IRQ/IPC server loop.
//!
//! QEMU has **no igc model**, so runtime bring-up is hardware-only; the
//! advanced-descriptor + EICR + MMD-PHY logic is correct-by-construction against
//! Linux `drivers/net/ethernet/intel/igc` and is fully host-tested.

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
use driver_runtime::ipc::EndpointCap;
#[cfg(not(test))]
use igc_driver::{
    BOOT_LOG_MARKER, INGRESS_SERVICE_NAME, SERVER_READY_SENTINEL, SERVICE_NAME, init, io,
    select_igc,
};
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
    syscall_lib::write_str(STDOUT_FILENO, "igc_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "igc_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn find_igc() -> Option<driver_runtime::DeviceCapKey> {
    let functions = driver_runtime::enumerate_ethernet_functions();
    select_igc(&functions)
}

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    let key = match find_igc() {
        Some(k) => k,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "igc_driver: no igc device present — exiting cleanly\n",
            );
            return 0;
        }
    };

    match init::IgcDevice::bring_up(key) {
        Ok(dev) => {
            log_mac("igc_driver: MAC ", dev.mac());
            if dev.link_up_initial() {
                syscall_lib::write_str(STDOUT_FILENO, "igc_driver: link up at bring-up\n");
            } else {
                syscall_lib::write_str(STDOUT_FILENO, "igc_driver: link down at bring-up\n");
            }

            let ep = syscall_lib::create_endpoint();
            if ep == u64::MAX {
                syscall_lib::write_str(STDOUT_FILENO, "igc_driver: endpoint create failed\n");
                return 4;
            }
            let ep_u32 = match u32::try_from(ep) {
                Ok(id) => id,
                Err(_) => {
                    syscall_lib::write_str(STDOUT_FILENO, "igc_driver: endpoint id out of range\n");
                    return 6;
                }
            };
            let rc = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
            if rc == u64::MAX {
                syscall_lib::write_str(STDOUT_FILENO, "igc_driver: service register failed\n");
                return 5;
            }
            let ingress_opt = syscall_lib::ipc_lookup_service(INGRESS_SERVICE_NAME);
            let ingress_cap = if ingress_opt == u64::MAX {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "igc_driver: ingress service absent, RX publish disabled\n",
                );
                None
            } else {
                match u32::try_from(ingress_opt) {
                    Ok(id) => Some(EndpointCap::new(id)),
                    Err(_) => {
                        syscall_lib::write_str(
                            STDOUT_FILENO,
                            "igc_driver: ingress endpoint id out of range\n",
                        );
                        return 8;
                    }
                }
            };
            syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);
            io::run_io_loop(dev, EndpointCap::new(ep_u32), ingress_cap)
        }
        Err(init::BringUpError::ResetTimeout) => {
            syscall_lib::write_str(STDOUT_FILENO, "igc_driver: reset timeout\n");
            2
        }
        Err(init::BringUpError::Runtime(DriverRuntimeError::Device(
            DeviceHostError::NotClaimed | DeviceHostError::AlreadyClaimed,
        ))) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "igc_driver: igc device not claimable — exiting cleanly\n",
            );
            0
        }
        Err(init::BringUpError::Runtime(_)) => {
            syscall_lib::write_str(STDOUT_FILENO, "igc_driver: bring-up failed\n");
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
    // SAFETY: `line` only ever holds ASCII hex digits, ':', or '\n'.
    let s = unsafe { core::str::from_utf8_unchecked(&line) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}
