//! Ring-3 Realtek RTL8111/8168/8169 Gigabit NIC driver — Phase 79 Track C.
//!
//! Discovery scaffold: enumerate Ethernet controllers, match the r8169 family
//! (`kernel_core::nic_ids::is_r8169`) via the pre-claim `sys_device_config_read`
//! path. QEMU has no r8169 model, so runtime bring-up is hardware-only; the
//! OWN-bit/TxPoll ring, Cfg9346 window, and XID chip-versioning are layered on
//! this scaffold (Track C).

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;
#[cfg(not(test))]
use driver_runtime::ipc::EndpointCap;
#[cfg(not(test))]
use r8169_hal::init::Nic;
#[cfg(not(test))]
use r8169_hal::{INGRESS_SERVICE_NAME, SERVER_READY_SENTINEL, SERVICE_NAME, io, select_r8169};
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
    syscall_lib::write_str(STDOUT_FILENO, "r8169_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "r8169_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
const BOOT_LOG_MARKER: &str = "r8169_driver: spawned\n";

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);
    let functions = driver_runtime::enumerate_ethernet_functions();
    let key = match select_r8169(&functions) {
        Some(k) => k,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "r8169_driver: no r8169 device present — exiting cleanly\n",
            );
            return 0;
        }
    };

    // Claim + map BAR + reset + detect XID version + set up rings + enable.
    let nic = match Nic::bring_up(key) {
        Ok(nic) => nic,
        Err(r8169_hal::init::BringUpError::ResetTimeout) => {
            syscall_lib::write_str(STDOUT_FILENO, "r8169_driver: reset timeout\n");
            return 2;
        }
        Err(r8169_hal::init::BringUpError::Runtime(_)) => {
            // Device not present / already claimed at runtime — exit cleanly so
            // the service manager does not burn the restart budget.
            syscall_lib::write_str(
                STDOUT_FILENO,
                "r8169_driver: bring-up failed (device unavailable) — exiting cleanly\n",
            );
            return 0;
        }
    };
    nic.log_version();

    // Firmware is required for 8168G-and-later (and all 8125). Blob staging is
    // coordinator-owned (E.2); if the chip needs firmware we emit a degraded
    // warning rather than panicking — the kernel_core::r8169 firmware path makes
    // this decision host-testable (see resolve_firmware).
    if nic.version.requires_firmware() {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "r8169_driver: WARNING firmware-required chip; blob staging is coordinator-owned — continuing degraded\n",
        );
    }

    // Create + register the command endpoint so the kernel RemoteNic facade can
    // forward TX, then look up the ingress endpoint for RX publishing.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "r8169_driver: endpoint create failed\n");
        return 4;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(id) => id,
        Err(_) => return 6,
    };
    if syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "r8169_driver: service register failed\n");
        return 5;
    }
    let ingress = syscall_lib::ipc_lookup_service(INGRESS_SERVICE_NAME);
    let ingress_cap = if ingress == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "r8169_driver: ingress service absent, RX publish disabled\n",
        );
        None
    } else {
        u32::try_from(ingress).ok().map(EndpointCap::new)
    };

    syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);
    io::run_io_loop(nic, EndpointCap::new(ep_u32), ingress_cap)
}
