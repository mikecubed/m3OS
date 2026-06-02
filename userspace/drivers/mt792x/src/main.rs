//! Ring-3 MediaTek mt792x Wi-Fi driver — Phase 81 Track DRV-shell.
//!
//! Enumerates Wi-Fi class PCI functions, matches the mt792x family via
//! `kernel_core::nic_ids::is_mt792x`, claims the device, maps BAR0, resets
//! the WFDMA engine, downloads firmware (if present), allocates MCU +
//! data rings, and enables DMA.
//!
//! ## This track (DRV-shell)
//!
//! Implements the hardware shell only: PCI selection, BAR0/WFDMA bring-up,
//! firmware download seam, MCU command ring, and WFDMA data rings. The
//! net.nic registration, RX/TX rewrite, EAPOL demux, and key-install are
//! Wave 3 (Track DRV-net).

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;
#[cfg(not(test))]
use mt792x_hal::fw::firmware_blob;
#[cfg(not(test))]
use mt792x_hal::init::Mt792x;
#[cfg(not(test))]
use mt792x_hal::{FW_ABSENT_SENTINEL, SERVER_READY_SENTINEL, select_mt792x};
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
    syscall_lib::write_str(STDOUT_FILENO, "mt792x_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "mt792x_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
const BOOT_LOG_MARKER: &str = "mt792x_driver: spawned\n";

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    // Enumerate Wi-Fi class PCI functions (class 0x02, subclass 0x80, prog_if 0x00).
    // Unlike Ethernet drivers we cannot use the convenience `enumerate_ethernet_functions`
    // wrapper — that targets class 0x02/0x00. We replicate the inline pattern:
    // call enumerate_pci_class with the Wi-Fi class triple, then read_vendor_device
    // to build the PciFunctionId list.
    let functions = {
        use driver_runtime::pci_enum::{PciFunctionId, enumerate_pci_class, read_vendor_device};
        use kernel_core::nic_ids::{WIFI_CLASS, WIFI_PROG_IF, WIFI_SUBCLASS};

        let mut out = alloc::vec![];
        let keys = match enumerate_pci_class(WIFI_CLASS, WIFI_SUBCLASS, WIFI_PROG_IF) {
            Ok(keys) => keys,
            Err(_) => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "mt792x_driver: PCI enumerate failed — exiting cleanly\n",
                );
                return 0;
            }
        };
        for key in keys {
            if let Ok((vendor, device)) = read_vendor_device(key) {
                out.push(PciFunctionId {
                    key,
                    vendor,
                    device,
                });
            }
        }
        out
    };

    // Select the first mt792x device from the enumerated list.
    let key = match select_mt792x(&functions) {
        Some(k) => k,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "mt792x_driver: no mt792x device present — exiting cleanly\n",
            );
            return 0;
        }
    };

    // Locate the firmware blob (None until coordinator E.2 stages it).
    let fw = firmware_blob();

    // If no firmware blob is present, emit the degraded sentinel and continue
    // with bring-up (the hardware shell still resets and allocates rings).
    if fw.is_none() {
        syscall_lib::write_str(STDOUT_FILENO, FW_ABSENT_SENTINEL);
    }

    // Bring up the hardware shell: claim, BAR map, WFDMA reset, MCU ring,
    // data rings, DMA enable.
    let _mt792x = match Mt792x::bring_up(key, fw) {
        Ok(dev) => dev,
        Err(mt792x_hal::init::BringUpError::ResetTimeout) => {
            syscall_lib::write_str(STDOUT_FILENO, "mt792x_driver: WFDMA reset timeout\n");
            return 2;
        }
        Err(mt792x_hal::init::BringUpError::Runtime(_)) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "mt792x_driver: bring-up failed (device unavailable) — exiting cleanly\n",
            );
            return 0;
        }
        Err(mt792x_hal::init::BringUpError::FirmwareAbsent) => {
            // Already emitted FW_ABSENT_SENTINEL above; continue.
            return 0;
        }
    };

    syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);

    // Wave 3 (DRV-net): run_io_loop(...) — net.nic registration + RX/TX rewrite
    // + EAPOL demux + key install go here.
    //
    // For this track (DRV-shell) we park the driver by blocking on the IRQ
    // notification so the binary compiles and boots without burning CPU.
    // The real net-IPC loop is wired in the DRV-net track.
    loop {
        let _bits = _mt792x.irq.wait();
        // Discard interrupt bits — the net-IPC loop will process them in DRV-net.
        core::hint::spin_loop();
    }
}
