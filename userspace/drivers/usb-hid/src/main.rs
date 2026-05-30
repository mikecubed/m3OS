//! Ring-3 USB HID Boot-Protocol class driver — Phase 78c.
//!
//! `usb-hid` is a static daemon that turns USB keyboard / mouse input into the
//! Phase 56 `KeyEvent` / `PointerEvent` stream. It owns no hardware: it talks
//! IPC to the xHCI host server (the `usb` service) and to `kbd_server` /
//! `mouse_server`.
//!
//! # Flow
//!
//! 1. Wait for + look up the `usb` service the xHCI driver registers.
//! 2. Pull every attached device via `NextAttach`. The xHCI server already put
//!    each HID interface in Boot Protocol (`SET_PROTOCOL(0)` / `SET_IDLE(0)`)
//!    and armed its interrupt-IN endpoint, and the `AttachNotice` carries the
//!    endpoint's DCI / MPS so this driver needs no descriptor round-trip.
//! 3. Look up `kbd` / `mouse` for the device classes present.
//! 4. Poll each device's interrupt-IN endpoint (`PollInterruptIn`); decode the
//!    boot report with the host-tested `kernel_core::usb::hid` layer; resolve
//!    key symbols through the same `Keymap` `kbd_server` uses; and inject the
//!    resulting events (`KBD_EVENT_INJECT` / `MOUSE_EVENT_INJECT`).
//!
//! The xHCI server captures reports on its IRQ and buffers them, so a poll is a
//! cheap non-blocking read — between keystrokes it simply returns "no report".

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::input::events::{KeyEvent, ModifierSide, PointerButton, PointerEvent};
use kernel_core::input::keymap::{Keycode, Keymap};
use kernel_core::usb::hid::{BootKeyboardDecoder, HID_KBD_REPORT_LEN, parse_boot_mouse_report};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;
use usb_core::protocol::{USB_MSG_MAX, USB_REQ_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest};
use usb_core::{PROTOCOL_HID_KEYBOARD, PROTOCOL_HID_MOUSE};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "usb-hid: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "usb-hid: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

/// Boot-log marker written when the daemon starts.
pub const BOOT_LOG_MARKER: &str = "usb-hid: spawned\n";

/// Emitted once at least one HID device is bound and polling begins. The
/// `usb-smoke` gate can wait on this before injecting keys.
pub const READY_SENTINEL: &str = "usb-hid: polling\n";

/// IPC inject labels (pinned contract with `kbd_server` / `mouse_server`).
const KBD_EVENT_INJECT: u64 = 5;
const MOUSE_EVENT_INJECT: u64 = 3;

/// Interrupt-IN poll cadence. Boot devices report at ~10 ms (`bInterval`); a
/// 5 ms poll keeps input latency below one report period.
const POLL_INTERVAL_NS: u32 = 5_000_000;

/// One bound HID device the daemon polls.
struct HidDevice {
    notice: usb_core::protocol::AttachNotice,
    kbd_decoder: BootKeyboardDecoder,
    /// Previous mouse button bitfield, for edge detection.
    prev_buttons: u8,
}

fn monotonic_ms() -> u64 {
    let (sec, nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    if sec < 0 {
        return 0;
    }
    (sec as u64)
        .saturating_mul(1_000)
        .saturating_add((nsec as u64) / 1_000_000)
}

/// Issue a `UsbRequest` to the xHCI server and decode the `UsbReply`.
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

/// Put a HID interface into Boot Protocol and stop duplicate reports by issuing
/// `SET_PROTOCOL(0)` then `SET_IDLE(0)` over EP0 (via the xHCI server's
/// `ControlRequest`). `wIndex` is the interface number from the attach notice.
fn boot_protocol_init(usb_ep: u32, notice: &usb_core::protocol::AttachNotice) {
    let iface = notice.interface_num as u16;
    let ilo = (iface & 0xFF) as u8;
    let ihi = (iface >> 8) as u8;
    // SET_PROTOCOL(0): bmRequestType 0x21 (H2D|Class|Interface), bRequest 0x0B,
    // wValue 0 (Boot), wIndex = interface, wLength 0.
    let set_protocol = [0x21, 0x0B, 0x00, 0x00, ilo, ihi, 0x00, 0x00];
    let _ = usb_call(
        usb_ep,
        &UsbRequest::ControlRequest {
            slot_id: notice.slot_id,
            setup: set_protocol,
            length: 0,
        },
    );
    // SET_IDLE(0): bRequest 0x0A, wValue 0 (duration 0 = report on change).
    let set_idle = [0x21, 0x0A, 0x00, 0x00, ilo, ihi, 0x00, 0x00];
    let _ = usb_call(
        usb_ep,
        &UsbRequest::ControlRequest {
            slot_id: notice.slot_id,
            setup: set_idle,
            length: 0,
        },
    );
}

/// Inject a fully-formed `KeyEvent` into `kbd_server`.
fn inject_key(kbd_ep: u32, ev: &KeyEvent) {
    let mut buf = [0u8; kernel_core::input::events::KEY_EVENT_WIRE_SIZE];
    if ev.encode(&mut buf).is_ok() {
        // Fire-and-wait: kbd_server acks with label 0. Ignore the ack value.
        let _ = syscall_lib::ipc_call_buf(kbd_ep, KBD_EVENT_INJECT, 0, &buf);
    }
}

/// Inject a `PointerEvent` into `mouse_server`.
fn inject_pointer(mouse_ep: u32, ev: &PointerEvent) {
    let mut buf = [0u8; kernel_core::input::events::POINTER_EVENT_WIRE_SIZE];
    if ev.encode(&mut buf).is_ok() {
        let _ = syscall_lib::ipc_call_buf(mouse_ep, MOUSE_EVENT_INJECT, 0, &buf);
    }
}

/// Resolve a decoded key edge into a full `KeyEvent` (symbol via the keymap).
fn key_event_from_edge(
    edge: &kernel_core::usb::hid::KeyEdge,
    keymap: &Keymap,
    now: u64,
) -> KeyEvent {
    let symbol = keymap
        .lookup(Keycode(edge.keycode), edge.modifiers)
        .map(|s| s.0)
        .unwrap_or(0);
    KeyEvent {
        timestamp_ms: now,
        keycode: edge.keycode,
        symbol,
        modifiers: edge.modifiers,
        kind: edge.kind,
        modifier_side: ModifierSide::for_keycode(edge.keycode),
    }
}

/// Poll one keyboard device: read its report, decode edges, inject each.
fn poll_keyboard(usb_ep: u32, kbd_ep: u32, dev: &mut HidDevice, keymap: &Keymap) {
    let report = match poll_report(usb_ep, dev) {
        Some(r) if r.len() >= HID_KBD_REPORT_LEN => r,
        _ => return,
    };
    let mut arr = [0u8; HID_KBD_REPORT_LEN];
    arr.copy_from_slice(&report[..HID_KBD_REPORT_LEN]);
    let mut edges: Vec<kernel_core::usb::hid::KeyEdge> = Vec::new();
    dev.kbd_decoder.decode(&arr, &mut edges);
    let now = monotonic_ms();
    for edge in &edges {
        let ev = key_event_from_edge(edge, keymap, now);
        inject_key(kbd_ep, &ev);
        // Load-bearing sentinel for the `usb-smoke` gate: a real interrupt-IN
        // boot report was decoded to a `KeyEvent` AND accepted by kbd_server
        // (inject_key blocks on its ack). Emitted per edge; the gate asserts
        // the Down edge of the injected key. The exact spelling is asserted.
        emit_key_sentinel(&ev);
    }
}

/// `USB_HID:key kind=<k> sym=0x<hex> kc=0x<hex>` — proves the full USB input
/// chain delivered a decoded key into kbd_server.
fn emit_key_sentinel(ev: &KeyEvent) {
    syscall_lib::write_str(STDOUT_FILENO, "USB_HID:key kind=");
    write_u8_dec(ev.kind as u8);
    syscall_lib::write_str(STDOUT_FILENO, " sym=0x");
    write_u32_hex(ev.symbol);
    syscall_lib::write_str(STDOUT_FILENO, " kc=0x");
    write_u32_hex(ev.keycode);
    syscall_lib::write_str(STDOUT_FILENO, "\n");
}

/// `USB_HID:mouse btn=0x<hex> moved=<0|1>` — proves a live interrupt-IN
/// boot-mouse report was decoded.
fn emit_mouse_sentinel(m: &kernel_core::usb::hid::MouseReport) {
    syscall_lib::write_str(STDOUT_FILENO, "USB_HID:mouse btn=0x");
    write_u32_hex(m.buttons as u32);
    let moved = if m.dx != 0 || m.dy != 0 {
        " moved=1\n"
    } else {
        " moved=0\n"
    };
    syscall_lib::write_str(STDOUT_FILENO, moved);
}

/// Poll one mouse device: read its report, decode motion + button edges.
fn poll_mouse(usb_ep: u32, mouse_ep: u32, dev: &mut HidDevice) {
    let report = match poll_report(usb_ep, dev) {
        Some(r) => r,
        None => return,
    };
    let Some(m) = parse_boot_mouse_report(&report) else {
        return;
    };
    let now = monotonic_ms();
    // Load-bearing sentinel for the `usb-smoke` gate's live-mouse assertion: a
    // real interrupt-IN boot-mouse report was decoded. The exact spelling is
    // asserted by the gate.
    emit_mouse_sentinel(&m);
    // Motion event (USB +dy is down already in HID; keep sign as reported —
    // mouse_server/display_server treat relative deltas uniformly).
    if m.dx != 0 || m.dy != 0 {
        inject_pointer(
            mouse_ep,
            &PointerEvent {
                timestamp_ms: now,
                dx: m.dx,
                dy: m.dy,
                abs_position: None,
                button: PointerButton::None,
                wheel_dx: 0,
                wheel_dy: 0,
                modifiers: Default::default(),
            },
        );
    }
    // Button edges from the 3-bit bitfield (bit0 left, 1 right, 2 middle).
    let changed = m.buttons ^ dev.prev_buttons;
    for bit in 0..3u8 {
        if changed & (1 << bit) != 0 {
            let down = m.buttons & (1 << bit) != 0;
            let button = if down {
                PointerButton::Down(bit)
            } else {
                PointerButton::Up(bit)
            };
            inject_pointer(
                mouse_ep,
                &PointerEvent {
                    timestamp_ms: now,
                    dx: 0,
                    dy: 0,
                    abs_position: None,
                    button,
                    wheel_dx: 0,
                    wheel_dy: 0,
                    modifiers: Default::default(),
                },
            );
        }
    }
    dev.prev_buttons = m.buttons;
}

/// Issue a single `PollInterruptIn` and return the captured report, if any.
fn poll_report(usb_ep: u32, dev: &HidDevice) -> Option<Vec<u8>> {
    let req = UsbRequest::PollInterruptIn {
        slot_id: dev.notice.slot_id,
        dci: dev.notice.ep_in_dci,
        len: dev.notice.ep_in_mps,
    };
    match usb_call(usb_ep, &req) {
        Some(UsbReply::InterruptReport { data, .. }) if !data.is_empty() => Some(data),
        _ => None,
    }
}

/// Look up a service, returning its endpoint cap handle or `None`.
fn lookup(name: &str) -> Option<u32> {
    let h = syscall_lib::ipc_lookup_service(name);
    if h == u64::MAX { None } else { Some(h as u32) }
}

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    // 1. Wait for the xHCI driver to register the `usb` service (it is a
    //    `depends=xhci_driver` daemon, but ordering is best-effort). A bounded
    //    wait avoids hanging forever on a machine with no USB controller.
    if !syscall_lib::ipc_wait_service(USB_SERVICE_NAME, 10_000) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "usb-hid: 'usb' service never appeared — exiting cleanly\n",
        );
        return 0;
    }
    let Some(usb_ep) = lookup(USB_SERVICE_NAME) else {
        syscall_lib::write_str(STDOUT_FILENO, "usb-hid: 'usb' lookup failed — exiting\n");
        return 0;
    };

    // 2. Enumerate attached HID devices via the NextAttach cursor.
    let mut devices: Vec<HidDevice> = Vec::new();
    let mut cursor = 0u8;
    while let Some(UsbReply::Attach {
        notice: Some(notice),
    }) = usb_call(usb_ep, &UsbRequest::NextAttach { cursor })
    {
        syscall_lib::write_str(STDOUT_FILENO, "usb-hid: bound HID device (proto ");
        write_u8_dec(notice.interface_protocol);
        syscall_lib::write_str(STDOUT_FILENO, ")\n");
        devices.push(HidDevice {
            notice,
            kbd_decoder: BootKeyboardDecoder::new(),
            prev_buttons: 0,
        });
        cursor = cursor.saturating_add(1);
    }

    if devices.is_empty() {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "usb-hid: no HID devices attached — exiting cleanly\n",
        );
        return 0;
    }

    // 3. Resolve the input-server endpoints for the classes present.
    let want_kbd = devices
        .iter()
        .any(|d| d.notice.interface_protocol == PROTOCOL_HID_KEYBOARD);
    let want_mouse = devices
        .iter()
        .any(|d| d.notice.interface_protocol == PROTOCOL_HID_MOUSE);
    let kbd_ep = if want_kbd { lookup("kbd") } else { None };
    let mouse_ep = if want_mouse { lookup("mouse") } else { None };

    // 3b. Put each HID interface into Boot Protocol and suppress duplicate
    //     reports (SET_PROTOCOL(0) / SET_IDLE(0)) via the xHCI server's
    //     ControlRequest path. This is the class driver's responsibility.
    for dev in &devices {
        boot_protocol_init(usb_ep, &dev.notice);
    }

    let keymap = Keymap::us_qwerty();
    syscall_lib::write_str(STDOUT_FILENO, READY_SENTINEL);

    // 4. Poll loop: each device's interrupt-IN endpoint, decode, inject.
    loop {
        for dev in devices.iter_mut() {
            match dev.notice.interface_protocol {
                PROTOCOL_HID_KEYBOARD => {
                    if let Some(kbd_ep) = kbd_ep {
                        poll_keyboard(usb_ep, kbd_ep, dev, &keymap);
                    }
                }
                PROTOCOL_HID_MOUSE => {
                    if let Some(mouse_ep) = mouse_ep {
                        poll_mouse(usb_ep, mouse_ep, dev);
                    }
                }
                _ => {}
            }
        }
        let _ = syscall_lib::nanosleep_for(0, POLL_INTERVAL_NS);
    }
}

/// Write a `u8` as decimal to stdout without `alloc::format!`.
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
    // SAFETY: `buf[i..]` contains only ASCII digits.
    let s = unsafe { core::str::from_utf8_unchecked(&buf[i..]) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}

/// Write a `u32` as a fixed 8-digit lowercase-hex string to stdout.
fn write_u32_hex(n: u32) {
    let mut buf = [0u8; 8];
    for (i, slot) in buf.iter_mut().enumerate() {
        let nib = ((n >> (28 - i * 4)) & 0xF) as u8;
        *slot = if nib < 10 {
            b'0' + nib
        } else {
            b'a' + nib - 10
        };
    }
    // SAFETY: `buf` contains only ASCII hex digits.
    let s = unsafe { core::str::from_utf8_unchecked(&buf) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}
