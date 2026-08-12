//! Phase 92e Track G — ring-3 USB-Ethernet class driver (CDC-ECM / CDC-NCM).
//!
//! This daemon is the *class-compliant* generalisation of the Phase 96 vendor
//! `ure` driver: instead of a Realtek register map it speaks the standard CDC
//! Ethernet datapath, so an arbitrary CDC-ECM or CDC-NCM dongle brings up a
//! `RemoteNic` over the exact same bulk primitives (`PollBulkIn` /
//! `SubmitBulkOut`) the `ure` NIC already proved — no new transport.
//!
//! # Flow
//!
//! 1. Wait on the `usb` service and walk the `NextAttach` cursor.
//! 2. Route each device through the shared device-match registry
//!    ([`kernel_core::usb::cdc::match_usb_net_driver`]): a Realtek `0bda:815x`
//!    device routes to the vendor `ure` driver (whose binary is not on `main` —
//!    it lands with the Phase 96 merge — so this daemon logs the verdict and
//!    leaves the device alone), while a class-compliant CDC interface is bound
//!    here.
//! 3. `GetDescriptors` → parse the CDC Ethernet functional descriptor for the
//!    MAC string index and refine ECM-vs-NCM from the config blob.
//! 4. `SET_INTERFACE(alt=1)` to activate the data-interface bulk pair, read the
//!    MAC from its string descriptor, register a `net.nic` `RemoteNic`.
//! 5. Serve TX requests from the kernel `RemoteNic` (frame → bulk-OUT, raw for
//!    ECM or NTB-aggregated for NCM) and poll bulk-IN for RX, publishing frames
//!    back to the kernel ingress.
//! 6. Release the device on an `attached: false` `NextAttach` notice (C.4).
//!
//! # Validation status
//!
//! QEMU ships **no** CDC-ECM/NCM device model, so the live datapath is
//! bare-metal/VFIO-only (skip-with-reason in CI — `usb-eth-smoke`), mirroring
//! the established `usb-eth-smoke` / `wifi-smoke` pattern. The pure framing /
//! descriptor-parse / device-match logic is host-tested in
//! `kernel_core::usb::cdc`; this crate is the glue that compiles for
//! `x86_64-m3os` and wires that logic to the live USB + `RemoteNic` seams.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use alloc::vec::Vec;
#[cfg(not(test))]
use core::alloc::Layout;
#[cfg(not(test))]
use core::cell::Cell;

#[cfg(not(test))]
use driver_runtime::ipc::EndpointCap;
#[cfg(not(test))]
use driver_runtime::ipc::net::{NetReply, NetRequest, NetServer};
#[cfg(not(test))]
use kernel_core::driver_ipc::net::{NetDriverError, NetLinkEvent};
#[cfg(not(test))]
use kernel_core::usb::cdc::{
    CdcVariant, UsbNetDriver, UsbNetVendor, build_ntb16, find_ethernet_functional_desc,
    get_string_descriptor_setup, match_usb_net_driver, parse_ecm_mac, parse_ntb16,
    refine_cdc_variant,
};
#[cfg(not(test))]
use kernel_core::usb::uac::set_interface_setup;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
#[cfg(not(test))]
use syscall_lib::{STDOUT_FILENO, write_str};
#[cfg(not(test))]
use usb_core::protocol::{
    AttachNotice, USB_MSG_MAX, USB_REQ_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest,
};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "usb-net: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "usb-net: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Boot-log marker written when the daemon starts.
pub const BOOT_LOG_MARKER: &str = "usb-net: spawned\n";

/// Service name the driver registers its `RemoteNic` TX endpoint under (the key
/// the kernel `RemoteNic` facade looks up to forward TX). Shared with the
/// PCI NIC drivers — only one NIC backs a given machine.
#[cfg(not(test))]
const NET_SERVICE_NAME: &str = "net.nic";

/// Kernel ingress endpoint the driver publishes RX frames / link state to.
#[cfg(not(test))]
const NET_INGRESS_SERVICE_NAME: &str = "net.nic.ingress";

/// Bounded number of attach-scan passes before the daemon exits cleanly when no
/// CDC-ECM/NCM device is present (the common case — most machines have none).
/// Mirrors the `usb-audio` / `usb-video` single-device lifecycle.
#[cfg(not(test))]
const MAX_SCAN_PASSES: u32 = 50;

/// Milliseconds between attach-scan passes (and between RX polls in the io loop).
#[cfg(not(test))]
const POLL_INTERVAL_MS: u64 = 200;

/// Length used to arm each bulk-IN RX poll. A CDC-NCM RX unit is a whole NTB
/// (`parse_ntb16`), which legitimately aggregates several Ethernet frames and
/// can exceed a single 1514-byte frame, so this is sized to the largest inline
/// transfer the protocol allows: `USB_MSG_MAX - 4` is the maximum `len` whose
/// `BulkData` reply (data + a 4-byte wire header) still fits `USB_MSG_MAX`, the
/// xHCI server's H.6 accept threshold. A smaller cap would truncate large NTBs
/// and make `parse_ntb16` drop the aggregated frames.
#[cfg(not(test))]
const RX_POLL_LEN: u16 = (USB_MSG_MAX - 4) as u16;

/// Re-walk the `NextAttach` table for a detach check every N io-loop passes
/// (C.4). At `POLL_INTERVAL_MS` that is roughly one detach probe per second.
#[cfg(not(test))]
const RECONCILE_EVERY: u32 = 5;

/// Per-device state for a bound CDC-ECM/NCM interface.
#[cfg(not(test))]
struct CdcDevice {
    /// xHCI slot ID of the device.
    slot_id: u8,
    /// Device Context Index of the bulk-IN endpoint (RX).
    bulk_in_dci: u8,
    /// Device Context Index of the bulk-OUT endpoint (TX).
    bulk_out_dci: u8,
    /// ECM (one frame per transfer) or NCM (NTB-aggregated).
    variant: CdcVariant,
    /// The device MAC, read from the ECM MAC string descriptor.
    mac: [u8; 6],
    /// Rolling NTB sequence counter for NCM TX (`Cell` so TX takes `&self`).
    ntb_seq: Cell<u16>,
    /// The `NextAttach` cursor index this device was bound at — the stable key
    /// the C.4 detach reconcile re-reads to observe `attached: false`.
    source_cursor: u8,
}

// ---------------------------------------------------------------------------
// IPC plumbing to the xHCI `usb` server (mirrors usb-storage's usb_call)
// ---------------------------------------------------------------------------

/// Issue a `UsbRequest` to the xHCI server and decode the `UsbReply`.
#[cfg(not(test))]
fn usb_call(usb_ep: u32, req: &UsbRequest) -> Option<UsbReply> {
    let req_bytes = req.encode();
    let rc = syscall_lib::ipc_call_buf(usb_ep, USB_REQ_LABEL, 0, &req_bytes);
    if rc == u64::MAX {
        return None;
    }
    let mut reply_buf = [0u8; USB_MSG_MAX];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX {
        return None;
    }
    UsbReply::decode(&reply_buf[..n as usize])
}

#[cfg(not(test))]
fn lookup(name: &str) -> Option<u32> {
    let h = syscall_lib::ipc_lookup_service(name);
    if h == u64::MAX { None } else { Some(h as u32) }
}

/// Issue a control-IN transfer and return the response bytes on success.
#[cfg(not(test))]
fn control_in(usb_ep: u32, slot_id: u8, setup: [u8; 8], length: u16) -> Option<Vec<u8>> {
    match usb_call(
        usb_ep,
        &UsbRequest::ControlRequest {
            slot_id,
            setup,
            length,
        },
    ) {
        Some(UsbReply::ControlData {
            data,
            completion_code: 1,
        }) => Some(data),
        _ => None,
    }
}

/// Issue a no-data control transfer (e.g. SET_INTERFACE). Returns `true` on a
/// success completion code.
#[cfg(not(test))]
fn control_no_data(usb_ep: u32, slot_id: u8, setup: [u8; 8]) -> bool {
    matches!(
        usb_call(
            usb_ep,
            &UsbRequest::ControlRequest {
                slot_id,
                setup,
                length: 0
            }
        ),
        Some(UsbReply::ControlData {
            completion_code: 1,
            ..
        })
    )
}

/// Read the full configuration blob via `GetDescriptors` (H.1).
#[cfg(not(test))]
fn get_config(usb_ep: u32, slot_id: u8) -> Option<Vec<u8>> {
    match usb_call(usb_ep, &UsbRequest::GetDescriptors { slot_id }) {
        Some(UsbReply::Descriptors { config, .. }) => Some(config),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Bind / device lifecycle
// ---------------------------------------------------------------------------

/// Read the device MAC from the ECM MAC-address string descriptor named by the
/// Ethernet functional descriptor in `config`.
#[cfg(not(test))]
fn read_ecm_mac(usb_ep: u32, slot_id: u8, config: &[u8]) -> Option<[u8; 6]> {
    let eth = find_ethernet_functional_desc(config)?;
    if eth.mac_string_index == 0 {
        return None;
    }
    let desc = control_in(
        usb_ep,
        slot_id,
        get_string_descriptor_setup(eth.mac_string_index),
        255,
    )?;
    parse_ecm_mac(&desc)
}

/// Attempt to bind a CDC-ECM/NCM device from a `NextAttach` notice. Returns
/// `None` if the notice is not a class-compliant USB-Ethernet device this daemon
/// should drive (a vendor `ure` device is logged and skipped; anything else is
/// ignored).
#[cfg(not(test))]
fn try_bind(usb_ep: u32, notice: &AttachNotice, cursor: u8) -> Option<CdcDevice> {
    // Route through the shared device-match registry (G.3).
    match match_usb_net_driver(
        notice.vendor_id,
        notice.product_id,
        notice.interface_class,
        notice.interface_sub_class,
    )? {
        UsbNetDriver::Vendor(UsbNetVendor::Realtek) => {
            // The vendor-native `ure` driver owns this device. Its binary is not
            // on `main` yet (it lands with the Phase 96 merge), so log the
            // routing verdict and leave the device for it.
            write_str(
                STDOUT_FILENO,
                "usb-net: device routes to vendor `ure` (Phase 96) — skipping\n",
            );
            return None;
        }
        UsbNetDriver::Cdc => {}
    }

    // A CDC data path needs a bulk IN+OUT pair (surfaced on the data interface).
    if notice.bulk_in_dci == 0 || notice.bulk_out_dci == 0 {
        return None;
    }

    // Read the full config blob to refine ECM-vs-NCM and locate the MAC.
    let config = get_config(usb_ep, notice.slot_id)?;
    let variant = refine_cdc_variant(&config);
    let mac = match read_ecm_mac(usb_ep, notice.slot_id, &config) {
        Some(mac) => mac,
        None => {
            // The `iMACAddress` lookup failed. Do NOT fall back to a fixed
            // constant — two devices that both fail the read would then present
            // the same L2 address and break ARP/NDP. Derive a *unique*
            // locally-administered MAC (first octet 0x02: LAA bit set, multicast
            // bit clear) from stable device identifiers (VID/PID/xHCI slot).
            let mac = [
                0x02,
                (notice.vendor_id >> 8) as u8,
                notice.vendor_id as u8,
                (notice.product_id >> 8) as u8,
                notice.product_id as u8,
                notice.slot_id,
            ];
            write_str(
                STDOUT_FILENO,
                "usb-net: iMACAddress read failed — using derived locally-administered MAC\n",
            );
            mac
        }
    };

    // Activate the data-interface bulk pair (alt 0 is the zero-bandwidth idle
    // setting for ECM/NCM; the bulk endpoints live on alt 1). Best-effort: a
    // device that already exposes the endpoints on alt 0 still works.
    let _ = control_no_data(
        usb_ep,
        notice.slot_id,
        set_interface_setup(notice.interface_num, 1),
    );

    write_str(STDOUT_FILENO, "usb-net: bound CDC ");
    write_str(
        STDOUT_FILENO,
        match variant {
            CdcVariant::Ecm => "ECM",
            CdcVariant::Ncm => "NCM",
        },
    );
    write_str(STDOUT_FILENO, " slot=");
    syscall_lib::write_u64(STDOUT_FILENO, notice.slot_id as u64);
    write_str(STDOUT_FILENO, "\n");

    Some(CdcDevice {
        slot_id: notice.slot_id,
        bulk_in_dci: notice.bulk_in_dci,
        bulk_out_dci: notice.bulk_out_dci,
        variant,
        mac,
        ntb_seq: Cell::new(0),
        source_cursor: cursor,
    })
}

/// Walk the `NextAttach` cursor once and bind the first class-compliant
/// USB-Ethernet device found.
#[cfg(not(test))]
fn scan_once(usb_ep: u32) -> Option<CdcDevice> {
    let mut cursor = 0u8;
    loop {
        match usb_call(usb_ep, &UsbRequest::NextAttach { cursor }) {
            Some(UsbReply::Attach {
                notice: Some(notice),
            }) => {
                if notice.attached
                    && let Some(dev) = try_bind(usb_ep, &notice, cursor)
                {
                    return Some(dev);
                }
                if cursor == u8::MAX {
                    return None;
                }
                cursor = cursor.saturating_add(1);
            }
            // Cursor exhausted or transport error — done with this pass.
            _ => return None,
        }
    }
}

/// Re-read the device's `NextAttach` slot; `true` once it reports
/// `attached: false` (or vanishes), i.e. the device was unplugged (C.4).
#[cfg(not(test))]
fn device_detached(usb_ep: u32, cursor: u8) -> bool {
    match usb_call(usb_ep, &UsbRequest::NextAttach { cursor }) {
        Some(UsbReply::Attach {
            notice: Some(notice),
        }) => !notice.attached,
        // Missing entry / transport error — treat as gone.
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// TX / RX datapath
// ---------------------------------------------------------------------------

/// Transmit one Ethernet frame: raw bulk-OUT for ECM, NTB-wrapped for NCM.
#[cfg(not(test))]
fn tx_frame(usb_ep: u32, dev: &CdcDevice, frame: &[u8]) -> bool {
    let payload: Vec<u8> = match dev.variant {
        CdcVariant::Ecm => frame.to_vec(),
        CdcVariant::Ncm => {
            let seq = dev.ntb_seq.get();
            dev.ntb_seq.set(seq.wrapping_add(1));
            match build_ntb16(seq, &[frame]) {
                Some(ntb) => ntb,
                None => return false,
            }
        }
    };
    // A `SubmitBulkOut` request encodes as `data + 5` bytes on the wire
    // (tag + slot_id + dci + a u16 length prefix); standard-MTU frames are far
    // under `USB_MSG_MAX`, but guard the bound explicitly.
    if payload.len() + 5 > USB_MSG_MAX {
        return false;
    }
    matches!(
        usb_call(
            usb_ep,
            &UsbRequest::SubmitBulkOut {
                slot_id: dev.slot_id,
                dci: dev.bulk_out_dci,
                data: payload,
            },
        ),
        Some(UsbReply::TransferComplete {
            completion_code: 1,
            ..
        })
    )
}

/// Poll the bulk-IN endpoint once and return any received Ethernet frames
/// (one for ECM, the de-aggregated NTB datagrams for NCM).
#[cfg(not(test))]
fn rx_poll(usb_ep: u32, dev: &CdcDevice) -> Vec<Vec<u8>> {
    match usb_call(
        usb_ep,
        &UsbRequest::PollBulkIn {
            slot_id: dev.slot_id,
            dci: dev.bulk_in_dci,
            len: RX_POLL_LEN,
        },
    ) {
        Some(UsbReply::BulkData { data, .. }) if !data.is_empty() => match dev.variant {
            CdcVariant::Ecm => alloc::vec![data],
            CdcVariant::Ncm => parse_ntb16(&data).unwrap_or_default(),
        },
        _ => Vec::new(),
    }
}

/// Serve the `RemoteNic` command endpoint (TX) and poll bulk-IN (RX) until the
/// device detaches (C.4). Returns when the device is gone so the caller can
/// publish link-down and exit.
#[cfg(not(test))]
fn run_io_loop(usb_ep: u32, dev: CdcDevice, net: &NetServer) {
    let mut ticks = 0u32;
    loop {
        // 1. Serve one pending TX request, if any (non-blocking).
        let _ = net.try_handle_next(
            |req: NetRequest| {
                let ok = tx_frame(usb_ep, &dev, &req.frame);
                NetReply {
                    status: if ok {
                        NetDriverError::Ok
                    } else {
                        NetDriverError::DeviceAbsent
                    },
                }
            },
            |_bits| {},
        );

        // 2. Drain any received frames to the kernel ingress.
        let frames = rx_poll(usb_ep, &dev);
        if !frames.is_empty() {
            let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
            let _ = net.publish_rx_frames(&refs);
        }

        // 3. Periodic detach reconcile (C.4).
        ticks += 1;
        if ticks >= RECONCILE_EVERY {
            ticks = 0;
            if device_detached(usb_ep, dev.source_cursor) {
                write_str(STDOUT_FILENO, "usb-net: released slot=");
                syscall_lib::write_u64(STDOUT_FILENO, dev.slot_id as u64);
                write_str(STDOUT_FILENO, "\n");
                return;
            }
        }

        let _ = syscall_lib::nanosleep_for(0, (POLL_INTERVAL_MS * 1_000_000) as u32);
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    if !syscall_lib::ipc_wait_service(USB_SERVICE_NAME, 10_000) {
        write_str(STDOUT_FILENO, "usb-net: usb service never appeared\n");
        return 0;
    }
    let usb_ep = match lookup(USB_SERVICE_NAME) {
        Some(ep) => ep,
        None => {
            write_str(STDOUT_FILENO, "usb-net: usb service lookup failed\n");
            return 0;
        }
    };

    // Scan for a class-compliant USB-Ethernet device. Absent one (the common
    // case — QEMU has no CDC-ECM model), exit cleanly without registering a
    // phantom NIC.
    let mut dev = None;
    for _ in 0..MAX_SCAN_PASSES {
        if let Some(d) = scan_once(usb_ep) {
            dev = Some(d);
            break;
        }
        let _ = syscall_lib::nanosleep_for(0, (POLL_INTERVAL_MS * 1_000_000) as u32);
    }
    let dev = match dev {
        Some(d) => d,
        None => {
            write_str(STDOUT_FILENO, "usb-net: no CDC-ECM/NCM device found\n");
            return 0;
        }
    };

    // Register the `RemoteNic` TX endpoint (the kernel net stack binds it).
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        write_str(STDOUT_FILENO, "usb-net: endpoint create failed\n");
        return 1;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(id) => id,
        Err(_) => {
            write_str(STDOUT_FILENO, "usb-net: endpoint id out of range\n");
            return 1;
        }
    };
    if syscall_lib::ipc_register_service(ep_u32, NET_SERVICE_NAME) == u64::MAX {
        write_str(STDOUT_FILENO, "usb-net: net.nic register failed\n");
        return 1;
    }
    let net = match lookup(NET_INGRESS_SERVICE_NAME) {
        Some(ingress) => NetServer::new(EndpointCap::new(ep_u32))
            .with_ingress_endpoint(EndpointCap::new(ingress)),
        None => {
            write_str(
                STDOUT_FILENO,
                "usb-net: ingress absent, RX publish disabled\n",
            );
            NetServer::new(EndpointCap::new(ep_u32))
        }
    };

    // Announce the link up with the device MAC.
    let _ = net.publish_link_state(NetLinkEvent {
        up: true,
        mac: dev.mac,
        speed_mbps: 0,
    });
    write_str(STDOUT_FILENO, "USB_NET:registered\n");

    let mac = dev.mac;
    run_io_loop(usb_ep, dev, &net);

    // Device detached: announce link-down and exit (C.4). Re-attach is a
    // bare-metal follow-up (matches usb-audio/usb-video single-device lifecycle).
    let _ = net.publish_link_state(NetLinkEvent {
        up: false,
        mac,
        speed_mbps: 0,
    });
    0
}
