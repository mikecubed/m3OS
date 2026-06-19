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
//! 2. Pull every attached device via `NextAttach`. The `AttachNotice` carries
//!    the interrupt-IN endpoint's DCI / MPS so this driver needs no descriptor
//!    round-trip. This driver then puts each HID interface into Boot Protocol
//!    itself (`SET_PROTOCOL(0)` / `SET_IDLE(0)` via `boot_protocol_init`, over
//!    the xHCI server's `ControlRequest` EP0 path); the interrupt-IN endpoint
//!    arms lazily on the first `PollInterruptIn`.
//! 3. Look up `kbd` / `mouse` for the device classes present.
//! 4. Poll each device's interrupt-IN endpoint (`PollInterruptIn`); decode the
//!    boot report with the host-tested `kernel_core::usb::hid` layer; resolve
//!    key symbols through the same `Keymap` `kbd_server` uses; and inject the
//!    resulting events (`KBD_EVENT_INJECT` / `MOUSE_EVENT_INJECT`).
//!
//! The xHCI server captures reports on its IRQ and buffers them, so a poll is a
//! cheap non-blocking read — between keystrokes it simply returns "no report".
//!
//! # Report Protocol (Phase 92 Track B)
//!
//! Beyond the two Boot-Protocol classes, this driver also binds **Report
//! Protocol** HID pointers — touchpads, tablets, gaming mice — whose report
//! layout is not the fixed boot 8/3-byte shape. At bind it reads + parses the
//! device's HID Report descriptor (`fetch_report_fields`, B.1) into a
//! `ReportField` layout, and decodes each interrupt-IN report against that
//! layout with `kernel_core::usb::hid_report::decode_pointer_report` (B.2) —
//! emitting multi-axis motion, a scroll wheel, and arbitrary buttons into
//! `mouse_server`. Report-Protocol keyboards additionally drive their Caps /
//! Num / Scroll Lock LEDs via `SET_REPORT` (B.4), and the resident walk
//! re-checks `NextAttach` so a hot-unplugged device's state is released (C.4).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::input::events::{
    KeyEvent, KeyEventKind, ModifierSide, ModifierState, PointerButton, PointerEvent,
};
use kernel_core::input::keymap::{KEY_CAPSLOCK, KEY_NUMLOCK, KEY_SCROLLLOCK, Keycode, Keymap};
use kernel_core::usb::descriptor::{CLASS_HID, SUBCLASS_HID_BOOT};
use kernel_core::usb::hid::{
    BootKeyboardDecoder, HID_KBD_REPORT_LEN, hid_consumer_usage_to_keycode, parse_boot_mouse_report,
};
use kernel_core::usb::hid_report::{
    DecodedPointer, ReportField, decode_consumer_usages, decode_pointer_report,
    parse_report_descriptor,
};
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

/// How this driver decodes a bound HID interface.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceRole {
    /// Boot-Protocol keyboard (subclass 1, protocol 1) — 8-byte boot report.
    BootKeyboard,
    /// Boot-Protocol mouse (subclass 1, protocol 2) — 3-byte boot report.
    BootMouse,
    /// Report-Protocol pointer (tablet / touchpad / gaming mouse) — variable
    /// layout decoded against the parsed `ReportField` array (Phase 92 B.2).
    ReportPointer,
    /// Report-Protocol consumer-control interface (media / volume keys, Usage
    /// Page 0x0C) decoded against the parsed layout (Phase 92 B.3).
    ReportConsumer,
    /// A HID interface this driver does not drive (no usable layout).
    Ignore,
}

/// Decide how to decode a freshly-bound interface from its boot protocol and
/// parsed Report-descriptor layout. Boot keyboard/mouse keep the fixed-format
/// boot decode; a Report-Protocol interface is classified by the usages its
/// `ReportField` layout actually carries (pointer axes/buttons vs consumer
/// controls), so a tablet drives `mouse_server` and a media-key strip routes
/// consumer keys — without either being mistaken for the other.
fn classify_role(notice: &usb_core::protocol::AttachNotice, fields: &[ReportField]) -> DeviceRole {
    // Per USB HID §4.2/§4.3 the boot protocol field (1 = keyboard, 2 = mouse) is
    // only meaningful when the interface declares the Boot subclass. A
    // Report-Protocol interface (subclass 0) that happens to advertise protocol
    // 1/2 must NOT be driven as a boot device — that would issue boot-only
    // SET_PROTOCOL/SET_IDLE and pick the fixed-format boot decoder over the
    // parsed `ReportField` layout. Honor the boot protocol only under the Boot
    // subclass; otherwise classify by the layout the descriptor actually carries.
    let is_boot = notice.interface_sub_class == SUBCLASS_HID_BOOT;
    match notice.interface_protocol {
        PROTOCOL_HID_KEYBOARD if is_boot => DeviceRole::BootKeyboard,
        PROTOCOL_HID_MOUSE if is_boot => DeviceRole::BootMouse,
        _ => {
            if notice.interface_class != CLASS_HID || fields.is_empty() {
                return DeviceRole::Ignore;
            }
            if fields_have_pointer(fields) {
                DeviceRole::ReportPointer
            } else if fields.iter().any(|f| f.usage_page == USAGE_PAGE_CONSUMER) {
                DeviceRole::ReportConsumer
            } else {
                DeviceRole::Ignore
            }
        }
    }
}

/// HID Usage Page 0x01 — Generic Desktop (pointer axes live here).
const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
/// HID Usage Page 0x09 — Button.
const USAGE_PAGE_BUTTON: u16 = 0x09;
/// HID Usage Page 0x0C — Consumer (media/volume/brightness controls).
const USAGE_PAGE_CONSUMER: u16 = 0x0C;

/// True if the parsed layout carries pointer input — a Generic-Desktop X/Y
/// axis or any Button-page field — i.e. this interface should drive
/// `mouse_server`.
fn fields_have_pointer(fields: &[ReportField]) -> bool {
    fields.iter().any(|f| {
        f.usage_page == USAGE_PAGE_BUTTON
            || (f.usage_page == USAGE_PAGE_GENERIC_DESKTOP && matches!(f.usage, 0x30 | 0x31))
    })
}

/// One bound HID device the daemon polls.
struct HidDevice {
    notice: usb_core::protocol::AttachNotice,
    /// How this device's reports are decoded (Phase 92 Track B).
    role: DeviceRole,
    /// The `NextAttach` table index this device was bound from — a stable,
    /// unique handle to this enumeration event (the table is append-only),
    /// used by the C.4 reconcile instead of the reusable packed `slot_id`.
    source_cursor: u8,
    kbd_decoder: BootKeyboardDecoder,
    /// Previous mouse button bitfield, for boot-mouse edge detection.
    prev_buttons: u8,
    /// Previous Report-Protocol pointer button bitmap, for edge detection
    /// across the (up to 32) buttons a `decode_pointer_report` surfaces.
    prev_pointer_buttons: u32,
    /// Previous Consumer-control pressed-usage bitmap snapshot, for edge
    /// detection on a Report-Protocol media-key interface (B.3).
    prev_consumer: u64,
    /// Caps/Num/Scroll Lock LED bitfield last pushed to the device via
    /// `SET_REPORT` (Phase 92 B.4). Boot keyboard output report: bit0 Num,
    /// bit1 Caps, bit2 Scroll Lock.
    led_state: u8,
    /// Parsed HID Report descriptor field layout (Phase 92 Track B.1). Empty for
    /// a non-HID interface or if the Report descriptor read failed. The boot
    /// keyboard/mouse path decodes via the Boot Protocol; a `ReportPointer` /
    /// `ReportConsumer` device decodes its interrupt-IN reports against this
    /// layout (B.2/B.3).
    report_fields: Vec<ReportField>,
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
    control_setup_step(
        usb_ep,
        notice.slot_id,
        set_protocol,
        "usb-hid: warn: SET_PROTOCOL(0) failed; continuing to poll\n",
    );
    // SET_IDLE(0): bRequest 0x0A, wValue 0 (duration 0 = report on change).
    let set_idle = [0x21, 0x0A, 0x00, 0x00, ilo, ihi, 0x00, 0x00];
    control_setup_step(
        usb_ep,
        notice.slot_id,
        set_idle,
        "usb-hid: warn: SET_IDLE(0) failed; continuing to poll\n",
    );
}

/// Issue one EP0 boot-protocol control transfer and emit `warn_msg` if the
/// xHCI server reports a failed, error, or missing reply. The daemon still
/// proceeds to poll the interrupt-IN endpoint — a `SET_PROTOCOL` / `SET_IDLE`
/// failure (wrong interface number, stalled EP0, controller error) is surfaced
/// in the log rather than silently swallowed, so the failure mode stays
/// diagnosable. Success is `UsbReply::ControlData { completion_code: 1, .. }`.
fn control_setup_step(usb_ep: u32, slot_id: u8, setup: [u8; 8], warn_msg: &str) {
    let ok = matches!(
        usb_call(
            usb_ep,
            &UsbRequest::ControlRequest {
                slot_id,
                setup,
                length: 0,
            },
        ),
        Some(UsbReply::ControlData {
            completion_code: 1,
            ..
        })
    );
    if !ok {
        syscall_lib::write_str(STDOUT_FILENO, warn_msg);
    }
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
    // Bare-metal diagnostic: prove a non-empty interrupt-IN report actually
    // arrived from the keyboard. Logged only when a key/modifier byte is set, so
    // an idle keyboard stays quiet but a real keypress is visible even if the
    // decode/inject/kbd_server path downstream is broken — this isolates "no
    // report delivered" (controller/arming) from "report delivered but not
    // injected" (decode/IPC).
    if report.iter().take(HID_KBD_REPORT_LEN).any(|&b| b != 0) {
        syscall_lib::write_str(STDOUT_FILENO, "usb-hid: kbd report");
        for &b in report.iter().take(HID_KBD_REPORT_LEN) {
            syscall_lib::write_str(STDOUT_FILENO, " ");
            write_u8_hex(b);
        }
        syscall_lib::write_str(STDOUT_FILENO, "\n");
    }
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
    // Phase 92 B.4 — a lock-key press toggles the matching LED and pushes the new
    // bitfield to the keyboard via SET_REPORT (a write-back to the device over
    // the H.2-hardened EP0 control path, interleaved with the armed interrupt-IN
    // poll above).
    maybe_update_leds(usb_ep, dev, &edges);
}

/// Boot-keyboard LED output-report bit positions (USB HID §B.1 / boot output
/// report): bit0 Num Lock, bit1 Caps Lock, bit2 Scroll Lock.
const LED_NUM_LOCK: u8 = 0x01;
const LED_CAPS_LOCK: u8 = 0x02;
const LED_SCROLL_LOCK: u8 = 0x04;

/// Phase 92 B.4 — fold any newly-pressed lock key into the device's LED state
/// and, when it changed, push the new bitfield to the keyboard. Each lock key is
/// a toggle: its Down edge flips the corresponding LED bit. Boot keyboards
/// expose a 1-byte LED output report, so this drives the physical Caps / Num /
/// Scroll Lock LEDs.
fn maybe_update_leds(usb_ep: u32, dev: &mut HidDevice, edges: &[kernel_core::usb::hid::KeyEdge]) {
    let mut changed = false;
    for edge in edges {
        if edge.kind != KeyEventKind::Down {
            continue;
        }
        let bit = if edge.keycode == KEY_CAPSLOCK.0 {
            LED_CAPS_LOCK
        } else if edge.keycode == KEY_NUMLOCK.0 {
            LED_NUM_LOCK
        } else if edge.keycode == KEY_SCROLLLOCK.0 {
            LED_SCROLL_LOCK
        } else {
            continue;
        };
        dev.led_state ^= bit;
        changed = true;
    }
    if changed {
        set_keyboard_leds(usb_ep, dev);
    }
}

/// Issue `SET_REPORT(Output)` carrying the device's current LED bitfield over
/// the live `ControlWrite` EP0 path (bmRequestType `0x21` = H2D|Class|Interface,
/// bRequest `0x09` = SET_REPORT, wValue `0x0200` = Output report / report id 0,
/// wIndex = interface, wLength = 1). Emits a `USB_HID:led` sentinel on a
/// successful write so the gate can assert the round-trip.
fn set_keyboard_leds(usb_ep: u32, dev: &HidDevice) {
    let iface = dev.notice.interface_num as u16;
    let setup = [
        0x21,
        0x09,
        0x00,
        0x02,
        (iface & 0xFF) as u8,
        (iface >> 8) as u8,
        0x01,
        0x00,
    ];
    let ok = matches!(
        usb_call(
            usb_ep,
            &UsbRequest::ControlWrite {
                slot_id: dev.notice.slot_id,
                setup,
                data: alloc::vec![dev.led_state],
            },
        ),
        Some(UsbReply::ControlData {
            completion_code: 1,
            ..
        })
    );
    if ok {
        syscall_lib::write_str(STDOUT_FILENO, "USB_HID:led state=0x");
        write_u8_hex(dev.led_state);
        syscall_lib::write_str(STDOUT_FILENO, "\n");
    } else {
        syscall_lib::write_str(STDOUT_FILENO, "usb-hid: warn: SET_REPORT(LED) failed\n");
    }
}

/// Poll a Report-Protocol pointer (tablet / touchpad / gaming mouse): read its
/// interrupt-IN report, decode it against the parsed `ReportField` layout
/// (`decode_pointer_report`, B.2), and inject motion + wheel + button edges into
/// `mouse_server`. A tablet reports an absolute position; a gaming mouse reports
/// relative deltas + a scroll wheel + extra buttons.
fn poll_report_pointer(usb_ep: u32, mouse_ep: u32, dev: &mut HidDevice) {
    let report = match poll_report(usb_ep, dev) {
        Some(r) => r,
        None => return,
    };
    let p: DecodedPointer = decode_pointer_report(&dev.report_fields, &report);
    // Nothing moved and no button changed — stay quiet (an idle tablet still
    // reports its position every frame, but `any_input` gates the sentinel).
    if !p.any_input && p.buttons == dev.prev_pointer_buttons {
        return;
    }
    let now = monotonic_ms();
    // Load-bearing sentinel for the I.2 Report-Protocol gate arm: a live
    // Report-Protocol pointer report decoded against the parsed layout.
    emit_report_pointer_sentinel(&p);

    let abs_position = match (p.abs_x, p.abs_y) {
        (Some(x), Some(y)) => Some((x as i32, y as i32)),
        _ => None,
    };
    if abs_position.is_some() || p.rel_x != 0 || p.rel_y != 0 || p.wheel != 0 {
        inject_pointer(
            mouse_ep,
            &PointerEvent {
                timestamp_ms: now,
                dx: p.rel_x,
                dy: p.rel_y,
                abs_position,
                button: PointerButton::None,
                wheel_dx: 0,
                wheel_dy: p.wheel,
                modifiers: Default::default(),
            },
        );
    }

    // Button edges across up to 32 buttons (a gaming mouse exposes ≥4).
    let changed = p.buttons ^ dev.prev_pointer_buttons;
    for bit in 0..32u8 {
        if changed & (1u32 << bit) != 0 {
            let down = p.buttons & (1u32 << bit) != 0;
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
    dev.prev_pointer_buttons = p.buttons;
}

/// `HID_REPORT:pointer btn=0x<hex> abs=<0|1> moved=<0|1>` — proves a live
/// Report-Protocol pointer report was decoded against the parsed field layout
/// (B.2). The exact prefix is asserted by the Report-Protocol gate arm.
fn emit_report_pointer_sentinel(p: &DecodedPointer) {
    syscall_lib::write_str(STDOUT_FILENO, "HID_REPORT:pointer btn=0x");
    write_u32_hex(p.buttons);
    syscall_lib::write_str(STDOUT_FILENO, " abs=");
    write_u8_dec(u8::from(p.abs_x.is_some()));
    let moved = p.abs_x.is_some() || p.rel_x != 0 || p.rel_y != 0 || p.wheel != 0;
    syscall_lib::write_str(STDOUT_FILENO, " moved=");
    write_u8_dec(u8::from(moved));
    syscall_lib::write_str(STDOUT_FILENO, "\n");
}

/// Poll a Report-Protocol consumer-control interface (media / volume keys): read
/// its report, decode the asserted Consumer usages against the parsed layout
/// (`decode_consumer_usages`, B.3), map each via `hid_consumer_usage_to_keycode`,
/// and inject a Down+Up `KeyEvent` into kbd_server — which routes media/volume
/// keys onward (`display_server` → `audio_server`). Press-edge-detected so a held
/// key fires once. (No QEMU device emits consumer reports, so this path is
/// bare-metal/VFIO-validated; the decode is host-tested in `kernel-core`.)
fn poll_report_consumer(usb_ep: u32, kbd_ep: u32, dev: &mut HidDevice, keymap: &Keymap) {
    let report = match poll_report(usb_ep, dev) {
        Some(r) => r,
        None => return,
    };
    let active = decode_consumer_usages(&dev.report_fields, &report);
    let now = monotonic_ms();
    let mut snapshot: u64 = 0;
    for (idx, f) in dev
        .report_fields
        .iter()
        .filter(|f| f.usage_page == USAGE_PAGE_CONSUMER)
        .enumerate()
        .take(64)
    {
        if !active.contains(&f.usage) {
            continue;
        }
        snapshot |= 1u64 << idx;
        // Fire only on the press transition (this field was not set last poll).
        if dev.prev_consumer & (1u64 << idx) == 0
            && let Some(kc) = hid_consumer_usage_to_keycode(f.usage)
        {
            inject_consumer_key(kbd_ep, kc, keymap, now);
        }
    }
    dev.prev_consumer = snapshot;
}

/// Inject a consumer/media keycode as a Down then Up `KeyEvent` into kbd_server
/// and log a `USB_HID:consumer` sentinel.
fn inject_consumer_key(kbd_ep: u32, keycode: u32, keymap: &Keymap, now: u64) {
    let symbol = keymap
        .lookup(Keycode(keycode), ModifierState::empty())
        .map(|s| s.0)
        .unwrap_or(0);
    for kind in [KeyEventKind::Down, KeyEventKind::Up] {
        inject_key(
            kbd_ep,
            &KeyEvent {
                timestamp_ms: now,
                keycode,
                symbol,
                modifiers: ModifierState::empty(),
                kind,
                modifier_side: ModifierSide::for_keycode(keycode),
            },
        );
    }
    syscall_lib::write_str(STDOUT_FILENO, "USB_HID:consumer kc=0x");
    write_u32_hex(keycode);
    syscall_lib::write_str(STDOUT_FILENO, "\n");
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

/// Read a HID interface's **Report descriptor** over EP0 and parse it into a
/// `ReportField` layout (Phase 92 Track B.1). Issues the standard
/// `GET_DESCRIPTOR(Report)` (bmRequestType `0x81`, bRequest `0x06`, wValue
/// `0x2200` = Report descriptor type 0x22 / index 0, wIndex = interface).
///
/// The request length is the HID descriptor's `wDescriptorLength` (read via
/// `GetDescriptors`, H.1) so the Report descriptor is read at exactly its
/// declared size (B.1 readiness — over-reading a hard-coded 256 risks pulling
/// trailing zero padding the parser would have to ignore). Falls back to a
/// generous 256 when the HID descriptor's length is unavailable — the control-IN
/// short-packet still terminates the transfer at the device's real descriptor
/// length, so the parser sees only the real bytes. Returns an empty `Vec` if the
/// read fails or the descriptor parses to no fields.
fn fetch_report_fields(usb_ep: u32, notice: &usb_core::protocol::AttachNotice) -> Vec<ReportField> {
    // Clamp the requested length so a device's declared `wDescriptorLength`
    // (untrusted) can't force an oversized EP0 scratch allocation or a
    // `ControlData` reply that overruns `USB_MSG_MAX`. The xHCI server now also
    // rejects an over-budget `ControlRequest` length (`> USB_MSG_MAX - 4` →
    // EINVAL) as defense-in-depth; clamping here keeps the request at the
    // device's real descriptor size so the read succeeds instead of bouncing
    // off that server-side guard. The inline `ControlData` encode overhead is
    // tag(1) + completion(1) + len-prefix(2) = 4 bytes, so the data body must
    // fit in `USB_MSG_MAX - 4`.
    const MAX_REPORT_DESC_LEN: u16 = (USB_MSG_MAX - 4) as u16;
    let req_len = report_descriptor_len(usb_ep, notice)
        .unwrap_or(256)
        .min(MAX_REPORT_DESC_LEN);
    let iface = notice.interface_num;
    let setup = [
        0x81,
        0x06,
        0x00,
        0x22,
        iface,
        0x00,
        (req_len & 0xFF) as u8,
        (req_len >> 8) as u8,
    ];
    match usb_call(
        usb_ep,
        &UsbRequest::ControlRequest {
            slot_id: notice.slot_id,
            setup,
            length: req_len,
        },
    ) {
        Some(UsbReply::ControlData {
            data,
            completion_code: 1,
        }) if !data.is_empty() => parse_report_descriptor(&data),
        _ => Vec::new(),
    }
}

/// Resolve the Report descriptor's declared length for `notice.interface_num`
/// by reading the device's Configuration descriptor (`GetDescriptors`, H.1) and
/// scanning it for the interface's HID descriptor (Phase 92 B.1 readiness).
/// `None` if the descriptor set is unavailable or carries no HID descriptor for
/// the interface — the caller then falls back to a generous request length.
fn report_descriptor_len(usb_ep: u32, notice: &usb_core::protocol::AttachNotice) -> Option<u16> {
    let config = match usb_call(
        usb_ep,
        &UsbRequest::GetDescriptors {
            slot_id: notice.slot_id,
        },
    ) {
        Some(UsbReply::Descriptors { config, .. }) => config,
        _ => return None,
    };
    hid_report_descriptor_len(&config, notice.interface_num)
}

/// Pure scan of a raw Configuration descriptor blob for the Report-descriptor
/// `wDescriptorLength` declared in the HID descriptor (bDescriptorType 0x21)
/// belonging to interface `iface`. Standard TLV walk: each descriptor is
/// `[bLength, bDescriptorType, …]`; track the current Interface (0x04) and, at
/// the HID descriptor (0x21) under it, read the `wDescriptorLength` of its first
/// Report (0x22) class-descriptor entry (USB HID §6.2.1: the HID descriptor is
/// `bLength, 0x21, bcdHID[2], bCountryCode, bNumDescriptors`, then
/// `bNumDescriptors × [bDescriptorType, wDescriptorLength[2]]`). All indexing is
/// bounds-checked (`?`) so a malformed/truncated blob yields `None`, never a
/// panic.
fn hid_report_descriptor_len(config: &[u8], iface: u8) -> Option<u16> {
    let mut i = 0usize;
    let mut cur_iface: Option<u8> = None;
    while i + 2 <= config.len() {
        let blen = config[i] as usize;
        if blen < 2 || i + blen > config.len() {
            break;
        }
        match config[i + 1] {
            // Interface descriptor — bInterfaceNumber is at offset 2.
            0x04 => cur_iface = config.get(i + 2).copied(),
            // HID descriptor under the target interface.
            0x21 if cur_iface == Some(iface) => {
                let num = *config.get(i + 5)? as usize;
                // Class-descriptor entries live *within* this HID descriptor's
                // own `bLength` (`[i, i+blen)`). Bound the walk to it so a
                // malformed `bNumDescriptors` (or a truncated descriptor) fails
                // closed (→ `None`) instead of reading `wDescriptorLength` out of
                // the next TLV descriptor and returning a bogus length.
                let end = i + blen;
                let mut e = i + 6;
                for _ in 0..num {
                    // Each entry is `[bDescriptorType, wDescriptorLength[2]]`; it
                    // must fit entirely within this descriptor.
                    if e + 3 > end {
                        return None;
                    }
                    let dtype = config[e];
                    let lo = config[e + 1] as u16;
                    let hi = config[e + 2] as u16;
                    if dtype == 0x22 {
                        return Some((hi << 8) | lo);
                    }
                    e += 3;
                }
            }
            _ => {}
        }
        i += blen;
    }
    None
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

/// Bind one attached interface: read + parse its Report descriptor (for a
/// HID-class interface), classify its decode role, and assemble the polled
/// device state. Shared by the boot-time enumeration walk and the C.4 hot-plug
/// reconcile so a device attached after boot is brought up identically.
///
/// `source_cursor` is the `NextAttach` table index this device was bound from.
/// The server's table is append-only, so that index is a stable, unique handle
/// to *this* enumeration event — unlike the packed `slot_id`, which the server
/// reuses for a reclaimed slot (H.3). Keying the C.4 reconcile on it avoids
/// confusing a re-attached device with the stale detached entry it reused.
fn build_device(
    usb_ep: u32,
    notice: usb_core::protocol::AttachNotice,
    source_cursor: u8,
) -> HidDevice {
    // Read + parse the Report descriptor for HID-class interfaces (the surfaced
    // device list can also include a hub/NIC this daemon ignores — issuing a HID
    // GET_DESCRIPTOR at those would stall EP0). Boot keyboard/mouse still decode
    // via Boot Protocol; the parsed layout drives the Report-Protocol decode.
    let report_fields = if notice.interface_class == CLASS_HID {
        let f = fetch_report_fields(usb_ep, &notice);
        if f.is_empty() {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "usb-hid: report descriptor unavailable/empty\n",
            );
        } else {
            syscall_lib::write_str(STDOUT_FILENO, "USB_HID:report-parsed proto=");
            write_u8_dec(notice.interface_protocol);
            syscall_lib::write_str(STDOUT_FILENO, " fields=");
            write_u8_dec(f.len().min(255) as u8);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
        }
        f
    } else {
        Vec::new()
    };
    let role = classify_role(&notice, &report_fields);
    HidDevice {
        notice,
        role,
        source_cursor,
        kbd_decoder: BootKeyboardDecoder::new(),
        prev_buttons: 0,
        prev_pointer_buttons: 0,
        prev_consumer: 0,
        led_state: 0,
        report_fields,
    }
}

/// Phase 92 Track C.4 — reconcile the bound-device set against the server's live
/// attach table. A hot-removed device flips its `AttachNotice` to
/// `attached: false` (C.2); a hot-added one appends a fresh attached entry
/// (C.1/C.3). Re-walk the whole `NextAttach` cursor and: **release** any device
/// we hold whose source entry is now detached (the usb-hid arm of C.4 — drops
/// its per-device state so a removed device stops being polled), and **bind**
/// any newly-attached HID interface we do not yet hold.
///
/// Identity is keyed on the **table index** (`source_cursor`), not the packed
/// `slot_id`: the server reuses a reclaimed slot's handle (H.3), so a detach +
/// re-attach of the same physical port appends a new entry that carries the same
/// `slot_id`. Keying on the (stable, append-only) index lets a single reconcile
/// pass that observes both entries correctly release the old device (its source
/// entry went `attached:false`) *and* bind the new one (a fresh index) without
/// the two aliasing.
fn reconcile_attachments(usb_ep: u32, devices: &mut Vec<HidDevice>) {
    // 1. Snapshot the full attach table (append-only; index == NextAttach cursor).
    let mut table: Vec<usb_core::protocol::AttachNotice> = Vec::new();
    let mut cursor = 0u8;
    while let Some(UsbReply::Attach {
        notice: Some(notice),
    }) = usb_call(usb_ep, &UsbRequest::NextAttach { cursor })
    {
        table.push(notice);
        cursor = match cursor.checked_add(1) {
            Some(c) => c,
            None => break,
        };
    }

    // 2. Release held devices whose source entry is gone or now detached.
    let mut i = 0;
    while i < devices.len() {
        let alive = table
            .get(devices[i].source_cursor as usize)
            .is_some_and(|n| n.attached);
        if alive {
            i += 1;
        } else {
            syscall_lib::write_str(STDOUT_FILENO, "usb-hid: released slot=");
            write_u8_dec(devices[i].notice.slot_id);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
            devices.remove(i);
        }
    }

    // 3. Bind newly-attached HID interfaces we do not already hold (an entry whose
    //    index backs no held device). A still-present device keeps its index held,
    //    so it is skipped; a stale detached entry is `attached:false`, so skipped.
    for (idx, n) in table.iter().enumerate() {
        if !n.attached {
            continue;
        }
        let idx = idx as u8;
        if devices.iter().any(|d| d.source_cursor == idx) {
            continue;
        }
        let dev = build_device(usb_ep, *n, idx);
        if dev.role == DeviceRole::Ignore {
            continue;
        }
        if matches!(dev.role, DeviceRole::BootKeyboard | DeviceRole::BootMouse) {
            boot_protocol_init(usb_ep, &dev.notice);
        }
        syscall_lib::write_str(STDOUT_FILENO, "usb-hid: hot-attached slot=");
        write_u8_dec(n.slot_id);
        syscall_lib::write_str(STDOUT_FILENO, "\n");
        devices.push(dev);
    }
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

    // 2. Enumerate attached HID devices via the NextAttach cursor. `idx` is the
    //    table index this device is bound from — passed to `build_device` as its
    //    stable `source_cursor` for the C.4 reconcile.
    let mut devices: Vec<HidDevice> = Vec::new();
    let mut cursor = 0u8;
    while let Some(UsbReply::Attach {
        notice: Some(notice),
    }) = usb_call(usb_ep, &UsbRequest::NextAttach { cursor })
    {
        let idx = cursor;
        cursor = match cursor.checked_add(1) {
            Some(c) => c,
            None => break,
        };
        // A boot enumeration only surfaces attached devices, but guard the flag
        // so the same walk is correct if it ever sees a stale detached entry.
        if !notice.attached {
            continue;
        }
        syscall_lib::write_str(STDOUT_FILENO, "usb-hid: bound HID device (proto ");
        write_u8_dec(notice.interface_protocol);
        syscall_lib::write_str(STDOUT_FILENO, ")\n");
        devices.push(build_device(usb_ep, notice, idx));
    }

    if devices.is_empty() {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "usb-hid: no HID devices attached — exiting cleanly\n",
        );
        return 0;
    }

    // Diagnostic: report every bound interface so a bare-metal boot log shows
    // exactly what enumerated — in particular whether a real HID keyboard
    // (proto=1) appeared at all, vs. only the NIC or a hub's status interface.
    for dev in &devices {
        let n = &dev.notice;
        syscall_lib::write_str(STDOUT_FILENO, "usb-hid: bound vid=0x");
        write_u16_hex(n.vendor_id);
        syscall_lib::write_str(STDOUT_FILENO, " pid=0x");
        write_u16_hex(n.product_id);
        syscall_lib::write_str(STDOUT_FILENO, " class=");
        write_u8_dec(n.interface_class);
        syscall_lib::write_str(STDOUT_FILENO, " proto=");
        write_u8_dec(n.interface_protocol);
        syscall_lib::write_str(
            STDOUT_FILENO,
            match dev.role {
                DeviceRole::BootKeyboard => " role=KEYBOARD\n",
                DeviceRole::BootMouse => " role=MOUSE\n",
                DeviceRole::ReportPointer => " role=REPORT_POINTER\n",
                DeviceRole::ReportConsumer => " role=REPORT_CONSUMER\n",
                DeviceRole::Ignore => " role=other\n",
            },
        );
    }

    // 3. Resolve the input-server endpoints for the classes present. A
    //    Report-Protocol pointer (tablet/touchpad) drives `mouse_server`; a
    //    consumer-control interface drives `kbd_server` (→ audio_server).
    let want_kbd = devices.iter().any(|d| {
        matches!(
            d.role,
            DeviceRole::BootKeyboard | DeviceRole::ReportConsumer
        )
    });
    let want_mouse = devices
        .iter()
        .any(|d| matches!(d.role, DeviceRole::BootMouse | DeviceRole::ReportPointer));
    // Wait (bounded) for the input servers to register before looking them up:
    // `usb_hid` only `depends=xhci_driver`, so on a cold boot it can win the
    // race against `kbd_server` / `mouse_server`. A plain `lookup` that loses
    // that race returns `None` once and the device's input is dead for the
    // process's lifetime. Mirror the bounded wait already used for the `usb`
    // service so a genuinely-absent server still exits the wait cleanly.
    let kbd_ep = if want_kbd {
        if syscall_lib::ipc_wait_service("kbd", 10_000) {
            lookup("kbd")
        } else {
            syscall_lib::write_str(STDOUT_FILENO, "usb-hid: 'kbd' service never appeared\n");
            None
        }
    } else {
        None
    };
    let mouse_ep = if want_mouse {
        if syscall_lib::ipc_wait_service("mouse", 10_000) {
            lookup("mouse")
        } else {
            syscall_lib::write_str(STDOUT_FILENO, "usb-hid: 'mouse' service never appeared\n");
            None
        }
    } else {
        None
    };

    // 3b. Put each Boot-Protocol keyboard/mouse interface into Boot Protocol and
    //     suppress duplicate reports (SET_PROTOCOL(0) / SET_IDLE(0)) via the xHCI
    //     server's ControlRequest path. Only boot devices get this — a
    //     Report-Protocol device (tablet/consumer) has no boot interface, and a
    //     non-HID interface (e.g. a surfaced NIC) would just stall EP0.
    for dev in &devices {
        if matches!(dev.role, DeviceRole::BootKeyboard | DeviceRole::BootMouse) {
            boot_protocol_init(usb_ep, &dev.notice);
        }
    }

    let keymap = Keymap::us_qwerty();
    syscall_lib::write_str(STDOUT_FILENO, READY_SENTINEL);

    // 4. Poll loop: each device's interrupt-IN endpoint, decode by role, inject.
    //    Every `RECONCILE_EVERY` ticks, reconcile against the live attach table
    //    so a hot-plugged device is bound and a hot-unplugged one is released
    //    (C.4) without restarting the daemon. ~200 ms cadence at the 5 ms poll
    //    period — fast enough to observe an attach/detach pair, cheap enough not
    //    to flood the server with `NextAttach` walks.
    const RECONCILE_EVERY: u32 = 40;
    let mut tick: u32 = 0;
    loop {
        if tick.is_multiple_of(RECONCILE_EVERY) {
            reconcile_attachments(usb_ep, &mut devices);
        }
        tick = tick.wrapping_add(1);
        for dev in devices.iter_mut() {
            match dev.role {
                DeviceRole::BootKeyboard => {
                    if let Some(kbd_ep) = kbd_ep {
                        poll_keyboard(usb_ep, kbd_ep, dev, &keymap);
                    }
                }
                DeviceRole::BootMouse => {
                    if let Some(mouse_ep) = mouse_ep {
                        poll_mouse(usb_ep, mouse_ep, dev);
                    }
                }
                DeviceRole::ReportPointer => {
                    if let Some(mouse_ep) = mouse_ep {
                        poll_report_pointer(usb_ep, mouse_ep, dev);
                    }
                }
                DeviceRole::ReportConsumer => {
                    if let Some(kbd_ep) = kbd_ep {
                        poll_report_consumer(usb_ep, kbd_ep, dev, &keymap);
                    }
                }
                DeviceRole::Ignore => {}
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

/// Write a `u8` as a fixed 2-digit lowercase-hex string to stdout.
fn write_u8_hex(n: u8) {
    let hi = n >> 4;
    let lo = n & 0xF;
    let mut buf = [0u8; 2];
    buf[0] = if hi < 10 { b'0' + hi } else { b'a' + hi - 10 };
    buf[1] = if lo < 10 { b'0' + lo } else { b'a' + lo - 10 };
    // SAFETY: `buf` contains only ASCII hex digits.
    let s = unsafe { core::str::from_utf8_unchecked(&buf) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}

/// Write a `u16` as a fixed 4-digit lowercase-hex string to stdout.
fn write_u16_hex(n: u16) {
    write_u8_hex((n >> 8) as u8);
    write_u8_hex((n & 0xFF) as u8);
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
