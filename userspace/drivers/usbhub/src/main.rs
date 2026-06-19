//! Ring-3 USB hub class driver — Phase 78b Track B.
//!
//! This daemon is responsible for enumerating USB hubs detected by the xHCI
//! host-controller driver, applying `SET_FEATURE(PORT_POWER)` to bring each
//! downstream port out of power-off, watching for `PORT_RESET` completions,
//! and reporting newly-attached devices back to the kernel's device-host
//! substrate via the `usb-core` IPC protocol.
//!
//! # Phase status
//!
//! **Phase 78b Track B** implemented the hub class logic in
//! `kernel_core::usb::hub` (descriptor parsing, `PortId` topology tree,
//! route-string computation). **Phase 92 Track A** turns this daemon live: the
//! xHCI server now surfaces `CLASS_HUB` interfaces (`device_info_from_ctx`), and
//! the daemon walks the `NextAttach` cursor for a hub, reads its descriptor over
//! EP0, and drives the per-port `SET_FEATURE(PORT_POWER)`/`PORT_RESET` bring-up
//! (the standard hub power/reset sequence). It exits cleanly when no hub is
//! present so the service manager marks the service stopped without burning its
//! restart budget.
//!
//! # Live enumeration path
//!
//! 1. Receive `AttachNotice { interface_class: CLASS_HUB, … }` via `NextAttach`.
//! 2. Issue `GET_DESCRIPTOR(Hub)` via `UsbRequest::ControlRequest`.
//! 3. Parse the `HubDescriptor` for `bNbrPorts` / `bPwrOn2PwrGood`.
//! 4. For each downstream port, send `SET_FEATURE(PORT_POWER)`, settle, read
//!    `GET_PORT_STATUS`, and on a connected port send `SET_FEATURE(PORT_RESET)`
//!    and ack `C_PORT_RESET` once it enables.
//!
//! Surfacing the downstream device as its own `AttachNotice` (tier-2 enumeration
//! via the route string — `PortTopology` + Slot Context route string, A.4/A.5)
//! is scheduled as **Phase 92a**.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;
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
    syscall_lib::write_str(STDOUT_FILENO, "usbhub: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "usbhub: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

// ---------------------------------------------------------------------------
// Public constants (load-bearing: asserted in the smoke test below)
// ---------------------------------------------------------------------------

/// Boot-log marker written when the hub daemon starts.
///
/// The `xhci-bringup-smoke` / future `usbhub-smoke` gate asserts this line
/// appears in serial output so the test infrastructure can confirm the daemon
/// reached `program_main`.
pub const BOOT_LOG_MARKER: &str = "usbhub: spawned\n";

// ---------------------------------------------------------------------------
// Hub-class interface classifier (exercises kernel_core::usb::hub at the
// usbhub build site, confirming the link is live even while the IPC path
// is dormant in 78b).
// ---------------------------------------------------------------------------

/// Returns `true` if `b_interface_class` identifies a USB Hub interface.
///
/// Delegates to [`kernel_core::usb::hub::is_hub_interface`].  Called here to
/// give the build a real reference into the hub logic crate — the compiler
/// verifies the symbol resolves and the ABI matches even when the live IPC
/// path is not yet wired up.
pub fn classify_hub_interface(b_interface_class: u8) -> bool {
    kernel_core::usb::hub::is_hub_interface(b_interface_class)
}

// ---------------------------------------------------------------------------
// Live hub bring-up (Phase 92 Track A) — IPC plumbing + control-transfer helpers
// ---------------------------------------------------------------------------

#[cfg(not(test))]
use alloc::vec::Vec;
#[cfg(not(test))]
use kernel_core::usb::hub::{
    HubDescriptor, PORT_POWER, PORT_RESET, PortTopology, clear_port_feature, get_hub_descriptor,
    get_port_status, port_status_connected, port_status_enabled, port_status_speed_code,
    set_port_feature,
};
#[cfg(not(test))]
use kernel_core::usb::xhci::trb::SetupPacket;
#[cfg(not(test))]
use usb_core::protocol::{
    AttachNotice, USB_MSG_MAX, USB_REQ_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest,
};

/// `CLEAR_FEATURE` selector for the C_PORT_RESET change bit (USB 2.0 §11.24.2).
#[cfg(not(test))]
const C_PORT_RESET: u16 = 20;
/// Minimum hub-descriptor request length — covers `bNbrPorts` (byte 2) and
/// `bPwrOn2PwrGood` (byte 5). Requesting exactly this caps the device's reply so
/// the control-IN data stage is never short.
#[cfg(not(test))]
const HUB_DESC_REQ_LEN: u16 = 9;

/// Pack a [`SetupPacket`] into the 8 little-endian bytes the `ControlRequest`
/// IPC carries.
#[cfg(not(test))]
fn setup_to_bytes(s: SetupPacket) -> [u8; 8] {
    [
        s.bm_request_type,
        s.b_request,
        s.w_value as u8,
        (s.w_value >> 8) as u8,
        s.w_index as u8,
        (s.w_index >> 8) as u8,
        s.w_length as u8,
        (s.w_length >> 8) as u8,
    ]
}

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

/// Run a control transfer for `setup` on the hub's EP0, returning the data stage
/// (empty for a no-data OUT). `length` is the IN data-stage byte count (0 for an
/// OUT such as `SET_FEATURE`). Returns `None` on a transport failure / STALL.
#[cfg(not(test))]
fn control(usb_ep: u32, slot_id: u8, setup: [u8; 8], length: u16) -> Option<Vec<u8>> {
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

#[cfg(not(test))]
fn write_u8_dec(n: u8) {
    let mut buf = [0u8; 3];
    let mut i = buf.len();
    let mut v = n;
    if v == 0 {
        syscall_lib::write_str(STDOUT_FILENO, "0");
        return;
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10);
        v /= 10;
    }
    // SAFETY: buf[i..] is ASCII digits.
    syscall_lib::write_str(STDOUT_FILENO, unsafe {
        core::str::from_utf8_unchecked(&buf[i..])
    });
}

/// Drive one hub: read its descriptor, power every downstream port, then probe
/// each port's status and reset any port reporting a connected device. This is
/// the standard hub power/reset sequence (USB 2.0 §11.5.1.5). Surfacing the
/// downstream device as its own `AttachNotice` (tier-2 enumeration via the route
/// string, A.4/A.5) is scheduled as Phase 92a.
#[cfg(not(test))]
fn enumerate_hub(usb_ep: u32, notice: &AttachNotice) {
    let slot_id = notice.slot_id;

    // GET_DESCRIPTOR(Hub) over EP0 → bNbrPorts + bPwrOn2PwrGood.
    let setup = setup_to_bytes(get_hub_descriptor(HUB_DESC_REQ_LEN));
    let Some(desc_bytes) = control(usb_ep, slot_id, setup, HUB_DESC_REQ_LEN) else {
        syscall_lib::write_str(STDOUT_FILENO, "usbhub: GET_DESCRIPTOR(Hub) failed\n");
        return;
    };
    let Some(desc) = HubDescriptor::parse(&desc_bytes) else {
        syscall_lib::write_str(STDOUT_FILENO, "usbhub: hub descriptor parse failed\n");
        return;
    };
    let nports = desc.b_nbr_ports;
    syscall_lib::write_str(STDOUT_FILENO, "XHCI_HUB:enumerated ports=");
    write_u8_dec(nports);
    syscall_lib::write_str(STDOUT_FILENO, "\n");

    // SET_FEATURE(PORT_POWER) on every downstream port.
    for port in 1..=nports {
        let setup = setup_to_bytes(set_port_feature(PORT_POWER, port));
        if control(usb_ep, slot_id, setup, 0).is_none() {
            syscall_lib::write_str(STDOUT_FILENO, "usbhub: PORT_POWER failed port=");
            write_u8_dec(port);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
        }
    }
    // Honor bPwrOn2PwrGood (units of 2 ms) before reading port status.
    let settle_ns = (desc.b_pwr_on2_pwr_good as u32)
        .max(1)
        .saturating_mul(2_000_000);
    let _ = syscall_lib::nanosleep_for(0, settle_ns.min(500_000_000));

    // Probe each port; reset any that reports a connected device.
    for port in 1..=nports {
        let setup = setup_to_bytes(get_port_status(port));
        let Some(st) = control(usb_ep, slot_id, setup, 4) else {
            continue;
        };
        if !port_status_connected(&st) {
            continue;
        }
        syscall_lib::write_str(STDOUT_FILENO, "usbhub: port ");
        write_u8_dec(port);
        syscall_lib::write_str(STDOUT_FILENO, " device connected\n");

        // SET_FEATURE(PORT_RESET) and poll until the port enables.
        let setup = setup_to_bytes(set_port_feature(PORT_RESET, port));
        let _ = control(usb_ep, slot_id, setup, 0);
        for _ in 0..50 {
            let _ = syscall_lib::nanosleep_for(0, 4_000_000);
            let setup = setup_to_bytes(get_port_status(port));
            let Some(s) = control(usb_ep, slot_id, setup, 4) else {
                continue;
            };
            if port_status_enabled(&s) {
                // Ack the C_PORT_RESET change bit (RW1C via CLEAR_FEATURE).
                let setup = setup_to_bytes(clear_port_feature(C_PORT_RESET, port));
                let _ = control(usb_ep, slot_id, setup, 0);
                syscall_lib::write_str(STDOUT_FILENO, "usbhub: port ");
                write_u8_dec(port);
                syscall_lib::write_str(STDOUT_FILENO, " reset+enabled\n");

                // Tier-2 enumeration (A.4/A.5): compute the route string for a
                // device on this downstream port (the hub sits directly on the
                // root-hub port `notice.port`, so the device is tier-2) and ask
                // the server to enumerate it through that route.
                let mut topo = PortTopology::new();
                let hub_idx = topo.add_root_port(notice.port);
                match topo.add_child_port(hub_idx, port) {
                    Some(dev_idx) => {
                        let route = topo.route_string(dev_idx);
                        let root = topo.root_hub_port(dev_idx).unwrap_or(notice.port);
                        let speed = port_status_speed_code(&s);
                        match usb_call(
                            usb_ep,
                            &UsbRequest::EnumerateChild {
                                parent_slot_id: slot_id,
                                route_string: route,
                                root_hub_port: root,
                                speed,
                            },
                        ) {
                            Some(UsbReply::Attach {
                                notice: Some(child),
                            }) => {
                                syscall_lib::write_str(
                                    STDOUT_FILENO,
                                    "usbhub: child enumerated slot=",
                                );
                                write_u8_dec(child.slot_id);
                                syscall_lib::write_str(STDOUT_FILENO, " class=");
                                write_u8_dec(child.interface_class);
                                syscall_lib::write_str(STDOUT_FILENO, "\n");
                            }
                            _ => {
                                syscall_lib::write_str(
                                    STDOUT_FILENO,
                                    "usbhub: child enumerate failed port=",
                                );
                                write_u8_dec(port);
                                syscall_lib::write_str(STDOUT_FILENO, "\n");
                            }
                        }
                    }
                    None => {
                        // Nesting beyond MAX_HUB_DEPTH — skip gracefully (A.5).
                        syscall_lib::write_str(
                            STDOUT_FILENO,
                            "usbhub: nesting beyond MAX_HUB_DEPTH — skipping port ",
                        );
                        write_u8_dec(port);
                        syscall_lib::write_str(STDOUT_FILENO, "\n");
                    }
                }
                break;
            }
        }
    }
    syscall_lib::write_str(STDOUT_FILENO, "USB_HUB:ready\n");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Hub daemon main — Phase 92 Track A.
///
/// Logs [`BOOT_LOG_MARKER`], waits on the `usb` service, walks the `NextAttach`
/// cursor for a `CLASS_HUB` interface, and drives each hub through its descriptor
/// read + per-port `PORT_POWER`/`PORT_RESET` bring-up. Exits cleanly when no hub
/// is present (the common machine) so init's `on-failure` policy marks the
/// service stopped rather than looping.
#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    if !syscall_lib::ipc_wait_service(USB_SERVICE_NAME, 10_000) {
        syscall_lib::write_str(STDOUT_FILENO, "usbhub: 'usb' service absent — exiting\n");
        return 0;
    }
    let usb_ep = {
        let h = syscall_lib::ipc_lookup_service(USB_SERVICE_NAME);
        if h == u64::MAX {
            syscall_lib::write_str(STDOUT_FILENO, "usbhub: 'usb' lookup failed — exiting\n");
            return 0;
        }
        h as u32
    };

    // Walk the NextAttach cursor for hub-class interfaces (A.1).
    let mut hubs: Vec<AttachNotice> = Vec::new();
    let mut cursor = 0u8;
    while let Some(UsbReply::Attach {
        notice: Some(notice),
    }) = usb_call(usb_ep, &UsbRequest::NextAttach { cursor })
    {
        cursor = cursor.saturating_add(1);
        if notice.attached && classify_hub_interface(notice.interface_class) {
            syscall_lib::write_str(STDOUT_FILENO, "usbhub: bound hub slot=");
            write_u8_dec(notice.slot_id);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
            hubs.push(notice);
        }
    }

    if hubs.is_empty() {
        syscall_lib::write_str(STDOUT_FILENO, "usbhub: no hub attached — exiting cleanly\n");
        return 0;
    }

    for notice in &hubs {
        enumerate_hub(usb_ep, notice);
    }
    0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{BOOT_LOG_MARKER, classify_hub_interface};

    #[test]
    fn boot_log_marker_correct() {
        assert_eq!(BOOT_LOG_MARKER, "usbhub: spawned\n");
    }

    #[test]
    fn classify_hub_interface_hub_class() {
        // CLASS_HUB = 0x09
        assert!(classify_hub_interface(0x09));
    }

    #[test]
    fn classify_hub_interface_non_hub_class() {
        // CLASS_HID = 0x03, not a hub.
        assert!(!classify_hub_interface(0x03));
        assert!(!classify_hub_interface(0x00));
        assert!(!classify_hub_interface(0xFF));
    }
}
