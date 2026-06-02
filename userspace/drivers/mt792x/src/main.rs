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
use driver_runtime::ipc::EndpointCap;
#[cfg(not(test))]
use mt792x_hal::fw::firmware_blob;
#[cfg(not(test))]
use mt792x_hal::init::Mt792x;
#[cfg(not(test))]
use mt792x_hal::{
    FW_ABSENT_SENTINEL, INGRESS_SERVICE_NAME, SERVER_READY_SENTINEL, SERVICE_NAME, io,
    select_mt792x,
};
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
#[cfg(not(test))]
use wifi_core::fsm::WifiFsm;

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
    let mt792x = match Mt792x::bring_up(key, fw) {
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

    // Register the net.nic TX endpoint (shared service name across NIC families —
    // only the present NIC registers it) and resolve the kernel ingress endpoint
    // used to publish RX frames + link-state.
    let ep = syscall_lib::create_endpoint();
    let ep_u32 = match u32::try_from(ep) {
        Ok(v) => v,
        Err(_) => return 3,
    };
    if syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "mt792x_driver: net.nic register failed\n");
        return 5;
    }
    // Phase 81 (C.3): advertise the wireless-medium marker so the kernel marks
    // this NIC wireless and prefers a link-up wired NIC over it for the default
    // route. Best-effort — failure only loses the wired-over-wireless preference.
    let _ = syscall_lib::ipc_register_service(ep_u32, "net.nic.wireless");
    let ingress = syscall_lib::ipc_lookup_service(INGRESS_SERVICE_NAME);
    let ingress_cap = if ingress == u64::MAX {
        None
    } else {
        u32::try_from(ingress).ok().map(EndpointCap::new)
    };

    // Station MAC (used as the 802.11 source address + link-state MAC).
    let sta_mac = mt792x.mac();

    // Load /etc/wpa.conf → WPA2-PSK supplicant FSM. Best-effort: if the config
    // is absent or malformed the driver still serves as a passive L2 NIC.
    let fsm = load_supplicant(sta_mac);
    if fsm.is_none() {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "mt792x_driver: no usable /etc/wpa.conf — passive L2 mode\n",
        );
    }

    syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);

    // Enter the net.nic data path: serve kernel TX, drain WFDMA RX (demuxing
    // EAPOL to the supplicant FSM), process FSM actions, emit link-state.
    // (Scan/auth/assoc orchestration against a real radio is Track E.4.)
    io::run_io_loop(mt792x, EndpointCap::new(ep_u32), ingress_cap, fsm, sta_mac)
}

/// Read `/etc/wpa.conf`, parse it, and build a WPA2-PSK supplicant FSM.
///
/// Returns `None` on any error (file absent, read error, malformed config, or
/// out-of-range passphrase) so the driver degrades to a passive L2 NIC.
#[cfg(not(test))]
fn load_supplicant(sta_mac: [u8; 6]) -> Option<WifiFsm> {
    // O_RDONLY = 0.
    let fd = syscall_lib::open(b"/etc/wpa.conf\0", 0, 0);
    if fd < 0 {
        return None;
    }
    let fd = fd as i32;
    let mut data = alloc::vec::Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        let n = syscall_lib::read(fd, &mut chunk);
        if n <= 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n as usize]);
        if data.len() > 4096 {
            break; // bound the read
        }
    }
    let _ = syscall_lib::close(fd);

    let text = core::str::from_utf8(&data).ok()?;
    let cfg = wifi_core::config::parse_wpa_conf(text).ok()?;

    // Draw a fresh, unpredictable SNonce from the kernel CSPRNG for the 4-way
    // handshake (never the FSM's deterministic test seed). One SNonce per boot
    // is sufficient for the single static association at 1.0.
    let mut snonce = [0u8; 32];
    if syscall_lib::getrandom(&mut snonce) != snonce.len() as isize {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "mt792x_driver: getrandom failed for SNonce\n",
        );
        return None;
    }
    Some(WifiFsm::new_with_snonce(
        *cfg.pmk(),
        cfg.ssid().to_vec(),
        sta_mac,
        snonce,
    ))
}
