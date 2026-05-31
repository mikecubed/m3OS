//! Ring-3 Realtek RTL8125 (2.5GbE) / RTL8126 (5GbE) NIC driver — Phase 79 Track D.
//!
//! Discovery scaffold: enumerate Ethernet controllers, match the r8125 family
//! (`kernel_core::nic_ids::is_r8125`, the corrected `0x8125`/`0x8126`) via the
//! pre-claim `sys_device_config_read` path. QEMU has no r8125 model, so runtime
//! bring-up is hardware-only; the V2 32-bit interrupt block and signed-PHY
//! firmware load are layered on this scaffold (Track D).

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
use r8125_driver::firmware::plan_firmware;
#[cfg(not(test))]
use r8125_driver::{INGRESS_SERVICE_NAME, SERVER_READY_SENTINEL, SERVICE_NAME, select_r8125};
#[cfg(not(test))]
use r8169_hal::init::Nic;
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
    syscall_lib::write_str(STDOUT_FILENO, "r8125_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "r8125_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
const BOOT_LOG_MARKER: &str = "r8125_driver: spawned\n";

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);
    let functions = driver_runtime::enumerate_ethernet_functions();
    let key = match select_r8125(&functions) {
        Some(k) => k,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "r8125_driver: no r8125 device present — exiting cleanly\n",
            );
            return 0;
        }
    };

    // The 8125 is a second-generation r8169 MAC: reuse the r8169 OWN-bit/TxPoll
    // ring + Cfg9346 reset bring-up to claim, map BAR, reset, detect XID, and
    // set up rings. The interrupt block differs (V2) and is armed in the V2 loop.
    let nic = match Nic::bring_up(key) {
        Ok(nic) => nic,
        Err(r8169_hal::init::BringUpError::ResetTimeout) => {
            syscall_lib::write_str(STDOUT_FILENO, "r8125_driver: reset timeout\n");
            return 2;
        }
        Err(r8169_hal::init::BringUpError::Runtime(_)) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "r8125_driver: bring-up failed (device unavailable) — exiting cleanly\n",
            );
            return 0;
        }
    };
    nic.log_version();

    // The interrupt subsystem version-branches to the 32-bit V2 registers for
    // 8125/8126 parts. Log which block is in use (the V2 loop arms it).
    if r8125_driver::interrupt::uses_v2(nic.version) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "r8125_driver: using V2 32-bit interrupt block\n",
        );
    } else {
        // An XID that did not resolve to an 8125 (e.g. unknown silicon at the
        // 0x8125 device ID). Fall through with the classic block armed by the
        // r8169 bring-up rather than panicking.
        syscall_lib::write_str(
            STDOUT_FILENO,
            "r8125_driver: WARNING device-ID is 8125 but XID is not an 8125 part — using classic IRQ block\n",
        );
    }

    // Firmware: all 8125 parts require a signed PHY blob. Staging is
    // coordinator-owned (E.2); absent/corrupt blobs DEGRADE with a warning
    // sentinel rather than panicking. The policy + validation are host-tested
    // (firmware::plan_firmware -> kernel_core::r8169::resolve_firmware).
    let plan = plan_firmware(nic.version, firmware_blob());
    if let Some(sentinel) = plan.sentinel() {
        syscall_lib::write_str(STDOUT_FILENO, sentinel);
    }

    // Create + register the command endpoint, look up the ingress endpoint.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "r8125_driver: endpoint create failed\n");
        return 4;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(id) => id,
        Err(_) => return 6,
    };
    if syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "r8125_driver: service register failed\n");
        return 5;
    }
    let ingress = syscall_lib::ipc_lookup_service(INGRESS_SERVICE_NAME);
    let ingress_cap = if ingress == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "r8125_driver: ingress service absent, RX publish disabled\n",
        );
        None
    } else {
        u32::try_from(ingress).ok().map(EndpointCap::new)
    };

    syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);
    r8125_driver::io::run_io_loop_v2(nic, EndpointCap::new(ep_u32), ingress_cap)
}

/// Locate the staged `rtl_nic` firmware blob for the present 8125 chip.
///
/// Blob staging is the coordinator's E.2 responsibility (sourced from host
/// `linux-firmware` at image-build time; not vendored). Until that wiring lands
/// no blob is reachable from here, so this returns `None` and the firmware path
/// degrades with a warning sentinel rather than panicking — exactly the Track
/// D.1 contract. When E.2 stages a blob, this is the single seam to read it.
#[cfg(not(test))]
fn firmware_blob() -> Option<&'static [u8]> {
    None
}
