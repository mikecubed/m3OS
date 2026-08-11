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
use kernel_core::input::hid_poll::{HUB_POLL_BASE_NS, hub_next_backoff_ns};
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

/// `CLEAR_FEATURE` selectors for the per-port change bits (USB 2.0 §11.24.2,
/// Table 11-17). These are `PORT_xxx + 16`: clearing them is RW1C, so a hub
/// holds a change bit set until the host explicitly acknowledges it. The hub
/// walker must clear the change bits it has observed, otherwise `wPortChange`
/// stays non-zero forever and the steady-state monitor (D.3) never idles —
/// re-enumerating on every poll and pinning a core (the exact failure D.3
/// exists to remove).
#[cfg(not(test))]
const C_PORT_CONNECTION: u16 = 16;
#[cfg(not(test))]
const C_PORT_ENABLE: u16 = 17;
#[cfg(not(test))]
const C_PORT_SUSPEND: u16 = 18;
#[cfg(not(test))]
const C_PORT_OVER_CURRENT: u16 = 19;
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

/// Mirror a short diagnostic line into the kernel dmesg ring (via
/// `sys_debug_print` → `[userspace] …`). A ring-3 driver's stdout (fd 1) is not
/// captured by `dmesg`/`/proc/kmsg`, so on a bare-metal GUI boot — where the
/// only off-box channel is `dmesg` over SSH — these lines are how the hub
/// daemon's tier-2 enumeration of devices behind a dock hub becomes observable.
#[cfg(not(test))]
fn klog(msg: &str) {
    syscall_lib::serial_print(msg);
}

#[cfg(not(test))]
fn monotonic_ns() -> u64 {
    let (sec, nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    if sec < 0 {
        return 0;
    }
    (sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(nsec as u64)
}

#[cfg(not(test))]
fn monotonic_ms() -> u64 {
    monotonic_ns() / 1_000_000
}

/// Per-RPC budget for a synchronous call to the shared, single-threaded xHCI
/// server. Generous (3 s) — comfortably above any single legitimate operation
/// (an `EnumerateChild` runs a full Enable-Slot/Address-Device/descriptor
/// sequence) — but finite, so a server monopolised at boot can never park usbhub
/// forever in `BlockedOnReply` with no waker (the a90aa2ca wedge that usb-hid was
/// hardened against but usbhub was not; on bare metal it left the tier-2
/// keyboard/mouse behind a dock hub unenumerated).
#[cfg(not(test))]
const USB_CALL_TIMEOUT_NS: u64 = 3_000_000_000; // 3 s

/// Total wall-clock budget for the boot-time hub-enumeration retry. The server
/// can be busy bringing up controllers for several seconds when usbhub first
/// asks for the attach table, so a single timed-out `NextAttach` does NOT mean
/// "no hub". Bounded so a machine with genuinely no hub still exits.
#[cfg(not(test))]
const INITIAL_ENUM_BUDGET_MS: u64 = 15_000;

/// Sleep between hub-enumeration retries while the server is busy.
#[cfg(not(test))]
const ENUM_RETRY_SLEEP_NS: u32 = 500_000_000; // 500 ms

/// Outcome of one `usb_call`: a decoded reply, a server **timeout** (busy —
/// worth retrying), or a transport **failure**.
#[cfg(not(test))]
enum CallStatus {
    Reply(UsbReply),
    TimedOut,
    Failed,
}

#[cfg(not(test))]
fn usb_call_status(usb_ep: u32, req: &UsbRequest) -> CallStatus {
    const NEG_ETIMEDOUT: u64 = (-110_i64) as u64;
    let req_bytes = req.encode();
    let deadline_ns = monotonic_ns().saturating_add(USB_CALL_TIMEOUT_NS);
    let rc = syscall_lib::ipc_call_buf_timeout(usb_ep, USB_REQ_LABEL, 0, &req_bytes, deadline_ns);
    if rc == NEG_ETIMEDOUT {
        return CallStatus::TimedOut;
    }
    if rc == u64::MAX {
        return CallStatus::Failed;
    }
    let mut reply_buf = [0u8; USB_MSG_MAX];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX {
        return CallStatus::Failed;
    }
    match UsbReply::decode(&reply_buf[..n as usize]) {
        Some(r) => CallStatus::Reply(r),
        None => CallStatus::Failed,
    }
}

/// Issue a `UsbRequest` to the xHCI server and decode the `UsbReply`. Bounded by
/// [`USB_CALL_TIMEOUT_NS`] so a monopolised server can never wedge usbhub.
#[cfg(not(test))]
fn usb_call(usb_ep: u32, req: &UsbRequest) -> Option<UsbReply> {
    match usb_call_status(usb_ep, req) {
        CallStatus::Reply(r) => Some(r),
        CallStatus::TimedOut | CallStatus::Failed => None,
    }
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

#[cfg(not(test))]
fn write_u32_dec(n: u32) {
    let mut buf = [0u8; 10]; // max u32 decimal is 10 digits
    let mut i = buf.len();
    let mut v = n;
    if v == 0 {
        syscall_lib::write_str(STDOUT_FILENO, "0");
        return;
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    // SAFETY: buf[i..] is ASCII digits.
    syscall_lib::write_str(STDOUT_FILENO, unsafe {
        core::str::from_utf8_unchecked(&buf[i..])
    });
}

/// Check whether any port on a hub has a pending status-change bit.
///
/// Reads GET_PORT_STATUS for each port (1..=nports) and inspects the
/// `wPortChange` word (bytes 2–3 of the 4-byte response).  Any non-zero
/// change word means the hub flagged a transition (connection, enable,
/// reset-complete, etc.) since the last time we cleared those bits.
///
/// Returns `true` as soon as a change is found; short-circuits the scan.
#[cfg(not(test))]
fn hub_ports_have_change(usb_ep: u32, slot_id: u8, nports: u8) -> bool {
    for port in 1..=nports {
        let setup = setup_to_bytes(get_port_status(port));
        if let Some(st) = control(usb_ep, slot_id, setup, 4)
            && st.len() >= 4
        {
            let change_word = u16::from_le_bytes([st[2], st[3]]);
            if change_word != 0 {
                return true;
            }
        }
    }
    false
}

/// How often to emit the idle-occupancy sentinel.
/// At `HUB_POLL_MAX_IDLE_NS` (200 ms) this is approximately every 20 s.
#[cfg(not(test))]
const HUB_IDLE_LOG_EVERY: u32 = 100;

/// Acknowledge (RW1C) every standard per-port change bit for `port`, so the
/// hub's `wPortChange` word returns to zero once we have observed and acted on
/// the current status. Without this the connect-change bit (`C_PORT_CONNECTION`)
/// stays set on a populated port forever, `hub_ports_have_change` reports a
/// change on every steady-state poll, and the D.3 backoff never engages. A
/// genuine later hot-plug / hot-unplug re-sets the relevant bit and re-triggers
/// enumeration. `C_PORT_RESET` is cleared on the reset path, so it is not
/// repeated here.
#[cfg(not(test))]
fn clear_port_change_bits(usb_ep: u32, slot_id: u8, port: u8) {
    for selector in [
        C_PORT_CONNECTION,
        C_PORT_ENABLE,
        C_PORT_SUSPEND,
        C_PORT_OVER_CURRENT,
    ] {
        let setup = setup_to_bytes(clear_port_feature(selector, port));
        let _ = control(usb_ep, slot_id, setup, 0);
    }
}

/// Drive one hub: read its descriptor, power every downstream port, then probe
/// each port's status and reset any port reporting a connected device. This is
/// the standard hub power/reset sequence (USB 2.0 §11.5.1.5). Surfacing the
/// downstream device as its own `AttachNotice` (tier-2 enumeration via the route
/// string, A.4/A.5) is scheduled as Phase 92a.
///
/// Returns `Some(nports)` on success so the caller can store the port count
/// for use in the steady-state monitoring loop (Phase 100 Track D.3).
#[cfg(not(test))]
fn enumerate_hub(usb_ep: u32, notice: &AttachNotice) -> Option<u8> {
    let slot_id = notice.slot_id;

    // GET_DESCRIPTOR(Hub) over EP0 → bNbrPorts + bPwrOn2PwrGood.
    let setup = setup_to_bytes(get_hub_descriptor(HUB_DESC_REQ_LEN));
    let Some(desc_bytes) = control(usb_ep, slot_id, setup, HUB_DESC_REQ_LEN) else {
        syscall_lib::write_str(STDOUT_FILENO, "usbhub: GET_DESCRIPTOR(Hub) failed\n");
        return None;
    };
    let Some(desc) = HubDescriptor::parse(&desc_bytes) else {
        syscall_lib::write_str(STDOUT_FILENO, "usbhub: hub descriptor parse failed\n");
        return None;
    };
    let nports = desc.b_nbr_ports;
    syscall_lib::write_str(STDOUT_FILENO, "XHCI_HUB:enumerated ports=");
    write_u8_dec(nports);
    syscall_lib::write_str(STDOUT_FILENO, "\n");
    klog(&alloc::format!(
        "usbhub: hub slot={} has {} downstream ports\n",
        slot_id,
        nports
    ));

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
        // Acknowledge the change bits we just read (RW1C) so the steady-state
        // monitor's `wPortChange` returns to zero and the D.3 backoff can idle.
        // Done for every probed port (connected or not) so a disconnect-change
        // on an empty port is cleared too.
        clear_port_change_bits(usb_ep, slot_id, port);
        if !port_status_connected(&st) {
            continue;
        }
        syscall_lib::write_str(STDOUT_FILENO, "usbhub: port ");
        write_u8_dec(port);
        syscall_lib::write_str(STDOUT_FILENO, " device connected\n");
        klog(&alloc::format!("usbhub: port {port} device connected\n"));

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
                klog(&alloc::format!("usbhub: port {port} reset+enabled\n"));

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
                                klog(&alloc::format!(
                                    "usbhub: child enumerated port={} slot={} class={} sub={} proto={}\n",
                                    port,
                                    child.slot_id,
                                    child.interface_class,
                                    child.interface_sub_class,
                                    child.interface_protocol
                                ));
                            }
                            other => {
                                syscall_lib::write_str(
                                    STDOUT_FILENO,
                                    "usbhub: child enumerate failed port=",
                                );
                                write_u8_dec(port);
                                syscall_lib::write_str(STDOUT_FILENO, "\n");
                                klog(&alloc::format!(
                                    "usbhub: child enumerate FAILED port={} (reply={})\n",
                                    port,
                                    match other {
                                        Some(UsbReply::Attach { notice: None }) => "empty-attach",
                                        Some(_) => "wrong-reply",
                                        None => "timeout/transport",
                                    }
                                ));
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
    Some(nports)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// One full `NextAttach` walk collecting hub-class interfaces. Returns the hubs
/// found plus whether the walk was cut short by a server **timeout** (busy —
/// retry) rather than reaching the end of the attach table.
#[cfg(not(test))]
fn enumerate_hubs_once(usb_ep: u32) -> (Vec<AttachNotice>, bool) {
    let mut hubs: Vec<AttachNotice> = Vec::new();
    let mut cursor = 0u8;
    loop {
        match usb_call_status(usb_ep, &UsbRequest::NextAttach { cursor }) {
            CallStatus::TimedOut => return (hubs, true),
            CallStatus::Reply(UsbReply::Attach {
                notice: Some(notice),
            }) => {
                cursor = match cursor.checked_add(1) {
                    Some(c) => c,
                    None => return (hubs, false),
                };
                if notice.attached && classify_hub_interface(notice.interface_class) {
                    klog("usbhub: bound hub\n");
                    syscall_lib::write_str(STDOUT_FILENO, "usbhub: bound hub slot=");
                    write_u8_dec(notice.slot_id);
                    syscall_lib::write_str(STDOUT_FILENO, "\n");
                    hubs.push(notice);
                }
            }
            // End of the attach table, a transport failure, or any other reply:
            // the walk is done and the server was responsive (not a busy timeout).
            CallStatus::Reply(_) | CallStatus::Failed => return (hubs, false),
        }
    }
}

/// Hub daemon main — Phase 92 Track A / Phase 100 Track D.3.
///
/// Logs [`BOOT_LOG_MARKER`], waits on the `usb` service, walks the `NextAttach`
/// cursor for a `CLASS_HUB` interface, and drives each hub through its descriptor
/// read + per-port `PORT_POWER`/`PORT_RESET` bring-up. Exits cleanly when no hub
/// is present (the common machine) so init's `on-failure` policy marks the
/// service stopped rather than looping.
///
/// Phase 100 Track D.3: after initial enumeration, enters a steady-state
/// port-monitoring loop with adaptive backoff so the walker no longer pins a
/// core at idle. Full notification-driven port-status changes (using the hub's
/// interrupt-IN endpoint for status-change notifications) are deferred to
/// Phase 103 (USB runtime power management).
///
/// Gated `not(test)`: this is the `entry_point!` target and its body is pure
/// syscall plumbing (`STDOUT_FILENO`, the `usb` IPC service, the control-transfer
/// helpers), none of which exists in a host `std` test build.
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

    // Walk the NextAttach cursor for hub-class interfaces (A.1), retrying while
    // the server is still busy with controller bring-up: a single timed-out
    // NextAttach means "busy, try again", NOT "no hub". A clean empty reply means
    // the server is responsive and there genuinely is no hub → exit. Bounded by
    // INITIAL_ENUM_BUDGET_MS so a hub-less machine still exits.
    klog("usbhub: spawned\n");
    let enum_deadline_ms = monotonic_ms().saturating_add(INITIAL_ENUM_BUDGET_MS);
    let hubs: Vec<AttachNotice> = loop {
        let (found, timed_out) = enumerate_hubs_once(usb_ep);
        if !found.is_empty() || !timed_out || monotonic_ms() >= enum_deadline_ms {
            break found;
        }
        klog("usbhub: 'usb' server busy (controller bring-up?); retrying hub scan\n");
        let _ = syscall_lib::nanosleep_for(0, ENUM_RETRY_SLEEP_NS);
    };

    if hubs.is_empty() {
        klog("usbhub: no hub attached — exiting cleanly\n");
        syscall_lib::write_str(STDOUT_FILENO, "usbhub: no hub attached — exiting cleanly\n");
        return 0;
    }
    klog("usbhub: hub(s) found; enumerating downstream ports\n");

    // Initial enumeration: bring up each hub and collect its port count for
    // the steady-state monitoring loop.
    let mut hub_nports: Vec<u8> = Vec::with_capacity(hubs.len());
    for notice in &hubs {
        // enumerate_hub returns Some(nports) on success; fall back to 0 (no
        // ports to monitor) on descriptor failure so we still enter the idle
        // loop and emit idle sentinels rather than exiting.
        hub_nports.push(enumerate_hub(usb_ep, notice).unwrap_or(0));
    }

    // ----------------------------------------------------------------
    // Phase 100 Track D.3 — steady-state port-monitoring loop.
    //
    // After initial enumeration the daemon no longer exits.  It wakes
    // every `hub_next_backoff_ns(consecutive_idle)` to check whether any
    // port's wPortChange word is non-zero (indicating a hot-plug or
    // hot-unplug since the previous check).  On a change it re-runs
    // `enumerate_hub` to power and reset the newly-connected port and
    // register the downstream device with the xHCI server.
    //
    // Full USB interrupt-endpoint notification (blocking on the hub's
    // status-change pipe instead of polling) is deferred to Phase 103
    // (USB runtime power management).  This bounded-backoff polling
    // reduces idle core-wake frequency from ~20/s (50 ms fixed) to
    // ~5/s (200 ms cap) without any change to the xHCI server or the
    // usb-core IPC protocol.
    // ----------------------------------------------------------------
    let _ = HUB_POLL_BASE_NS; // suppress unused-import lint when logging is disabled
    let mut consecutive_idle: u32 = 0;
    loop {
        let sleep_ns = hub_next_backoff_ns(consecutive_idle);
        let _ = syscall_lib::nanosleep_for(0, sleep_ns);

        // Idle-occupancy sentinel — falsifiable evidence that the walker is not
        // pinning a core (Phase 100 D.3 acceptance criterion). Emitted on the
        // FIRST idle tick (so `usb-hub-smoke` can assert the walker reaches idle
        // within the smoke window — it never would while the C_PORT_CONNECTION
        // re-enumeration bug was live) and then periodically thereafter.
        if consecutive_idle == 1
            || (consecutive_idle > 0 && consecutive_idle.is_multiple_of(HUB_IDLE_LOG_EVERY))
        {
            syscall_lib::write_str(STDOUT_FILENO, "USB_HUB:idle ticks=");
            write_u32_dec(consecutive_idle);
            syscall_lib::write_str(STDOUT_FILENO, " backoff_ns=");
            write_u32_dec(sleep_ns);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
        }

        // Check each hub's ports for pending status-change bits.
        let mut any_change = false;
        for (notice, &nports) in hubs.iter().zip(hub_nports.iter()) {
            if nports > 0 && hub_ports_have_change(usb_ep, notice.slot_id, nports) {
                any_change = true;
                break;
            }
        }

        if any_change {
            consecutive_idle = 0;
            // Mirror into the kernel dmesg ring (via sys_debug_print) as well as
            // fd 1: on a bare-metal GUI boot the only off-box channel is `dmesg`
            // over SSH, and a repeating re-enumerate line there is the fingerprint
            // of a dock-hub change-bit storm (vs. a one-shot legitimate hot-plug).
            syscall_lib::serial_print("usbhub: port status change detected; re-enumerating\n");
            syscall_lib::write_str(
                STDOUT_FILENO,
                "usbhub: port status change detected; re-enumerating\n",
            );
            for (notice, nports) in hubs.iter().zip(hub_nports.iter_mut()) {
                if let Some(n) = enumerate_hub(usb_ep, notice) {
                    *nports = n;
                }
            }
        } else {
            consecutive_idle = consecutive_idle.saturating_add(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{BOOT_LOG_MARKER, classify_hub_interface};
    use kernel_core::input::hid_poll::{
        HUB_POLL_BASE_NS, HUB_POLL_MAX_IDLE_NS, hub_next_backoff_ns,
    };

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

    // Phase 100 Track D.3 — hub-monitoring backoff smoke tests.
    // Verifies the invariants documented in the monitoring loop:
    // fast at idle-start, non-decreasing, capped at max.

    #[test]
    fn hub_backoff_starts_at_base() {
        assert_eq!(hub_next_backoff_ns(0), HUB_POLL_BASE_NS);
        assert_eq!(hub_next_backoff_ns(1), HUB_POLL_BASE_NS);
        assert_eq!(hub_next_backoff_ns(3), HUB_POLL_BASE_NS);
    }

    #[test]
    fn hub_backoff_grows_after_threshold() {
        let base = hub_next_backoff_ns(0);
        let grown = hub_next_backoff_ns(4);
        // Strictly greater, not `>=`: the point of the backoff is that an idle
        // hub polls *less* often, so a curve flattened to a constant is the
        // regression this test exists to catch — and `>=` would pass for it.
        assert!(
            grown > base,
            "backoff must grow once the idle threshold is crossed: {base} -> {grown}"
        );
    }

    #[test]
    fn hub_backoff_capped_at_max() {
        assert_eq!(hub_next_backoff_ns(u32::MAX), HUB_POLL_MAX_IDLE_NS);
        assert_eq!(hub_next_backoff_ns(1_000_000), HUB_POLL_MAX_IDLE_NS);
    }
}
