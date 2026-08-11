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
//!
//! `cfg(not(test))` gates protect the OS-only entry point (allocator, panic /
//! alloc-error handlers, `_start`) so
//! `cargo test -p usb_hid --target x86_64-unknown-linux-gnu`
//! compiles the daemon body as a plain host `std` test binary — `std` supplies
//! those lang items, so leaving ours in scope is a duplicate-lang-item error.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]
// Under `cfg(test)` the `entry_point!` below is gated out, so nothing calls
// `program_main` and — transitively — none of the daemon body. That is expected
// for a binary crate whose logic is exercised by unit tests rather than by a
// caller, so silence `dead_code` for the test build only. The production
// (`cfg(not(test))`) build keeps the lint fully live.
#![cfg_attr(test, allow(dead_code))]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
// Only the `cfg(not(test))` alloc-error handler names `Layout`.
#[cfg(not(test))]
use core::alloc::Layout;
use core::sync::atomic::{AtomicU32, Ordering};

use kernel_core::input::events::{
    KeyEvent, KeyEventKind, ModifierSide, ModifierState, PointerButton, PointerEvent,
};
use kernel_core::input::hid_poll::next_hid_backoff_ns;
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
// Only the `cfg(not(test))` `#[global_allocator]` names `BrkAllocator`; under
// `cfg(test)` the host `std` allocator is used instead.
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
use usb_core::protocol::{USB_MSG_MAX, USB_REQ_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest};
use usb_core::{PROTOCOL_HID_KEYBOARD, PROTOCOL_HID_MOUSE};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    // Mirror to the kernel log ring so a bare-metal OOM death is visible in
    // `dmesg` over SSH (driver fd-1 output is not captured there — see `klog`).
    klog("usb-hid: alloc error\n");
    syscall_lib::write_str(STDOUT_FILENO, "usb-hid: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    klog("usb-hid: PANIC\n");
    syscall_lib::write_str(STDOUT_FILENO, "usb-hid: PANIC\n");
    syscall_lib::exit(101)
}

/// Mirror a short diagnostic line into the kernel `dmesg` ring via
/// `sys_debug_print` (it logs `[userspace] <msg>`, which `_kernel_print` writes
/// to the dmesg ring + serial). A ring-3 driver's normal stdout (fd 1) is NOT
/// captured by `dmesg`/`/proc/kmsg`, so on a bare-metal GUI boot — where the
/// only off-box channel is `dmesg` over SSH — these lines are how the driver's
/// lifecycle (bound role, exit reason, a crash) becomes observable.
fn klog(msg: &str) {
    syscall_lib::serial_print(msg);
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Boot-log marker written when the daemon starts.
pub const BOOT_LOG_MARKER: &str = "usb-hid: spawned\n";

/// Emitted once at least one HID device is bound and polling begins. The
/// `usb-smoke` gate can wait on this before injecting keys.
pub const READY_SENTINEL: &str = "usb-hid: polling\n";

/// IPC inject labels (pinned contract with `kbd_server` / `mouse_server`).
const KBD_EVENT_INJECT: u64 = 5;
const MOUSE_EVENT_INJECT: u64 = 3;

/// Hot-plug reconcile interval (ms). Kept time-based so the cadence is
/// independent of the adaptive-backoff sleep duration.
const RECONCILE_INTERVAL_MS: u64 = 200;

/// Emit an idle-occupancy sentinel every this many consecutive empty polls
/// (after we have already reached the max-backoff plateau).  At 100 ms idle
/// sleep this is roughly every 10 s — visible in logs without flooding them.
const IDLE_LOG_EVERY: u32 = 100;

/// C.1 bare-metal sentinel — cumulative count of `PointerEvent`s successfully
/// injected into `mouse_server` via [`inject_pointer`] since startup.
/// Emitted as `USB_HID:pointer-injected count=<n>` on the first inject and
/// every 64th inject thereafter, providing greppable evidence that a non-zero
/// injected-event count accumulated over the dock-hub topology.
static INJECTED_PTR_COUNT: AtomicU32 = AtomicU32::new(0);

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
    // Boot keyboard/mouse: under the Boot subclass the boot protocol field
    // (1 = keyboard, 2 = mouse) is authoritative — keep the fixed-format boot
    // decode.
    let is_boot = notice.interface_sub_class == SUBCLASS_HID_BOOT;
    match notice.interface_protocol {
        PROTOCOL_HID_KEYBOARD if is_boot => return DeviceRole::BootKeyboard,
        PROTOCOL_HID_MOUSE if is_boot => return DeviceRole::BootMouse,
        _ => {}
    }
    // Everything past here must be a HID-class interface with a usable layout.
    if notice.interface_class != CLASS_HID {
        return DeviceRole::Ignore;
    }
    // A non-boot interface that still declares the Keyboard protocol (1) is a
    // keyboard (USB HID §4.3) — many modern keyboards default to Report Protocol
    // (subclass 0). Drive it as a boot keyboard regardless of subclass:
    // `boot_protocol_init` issues SET_PROTOCOL(0), which makes a Report-only
    // keyboard emit the fixed 8-byte boot report `BootKeyboardDecoder` already
    // handles (virtually every keyboard supports boot protocol). This relaxation
    // is deliberately keyboard-only — the mouse path stays Boot-subclass-gated
    // above, because a Report-Protocol pointer (tablet/touchpad/gaming mouse) has
    // a richer layout the `ReportPointer` path decodes and must NOT be collapsed
    // to the 3-byte boot mouse decode (the Phase 92 caution).
    if notice.interface_protocol == PROTOCOL_HID_KEYBOARD {
        return DeviceRole::BootKeyboard;
    }
    if fields.is_empty() {
        return DeviceRole::Ignore;
    }
    // Classify the remaining Report-Protocol interfaces by the usages their
    // parsed layout actually carries. Pointer is tested before keyboard so a
    // combo device that exposes both axes and a keyboard collection on one
    // interface (e.g. a touchpad with hotkeys) keeps driving `mouse_server`
    // rather than being forced into the boot-keyboard decode.
    if fields_have_pointer(fields) {
        DeviceRole::ReportPointer
    } else if fields_have_keyboard(fields) {
        // Subclass 0, protocol 0, but the descriptor carries Keyboard-page
        // usages and no pointer axes — a Report-only keyboard. Boot-protocol it.
        DeviceRole::BootKeyboard
    } else if fields.iter().any(|f| f.usage_page == USAGE_PAGE_CONSUMER) {
        DeviceRole::ReportConsumer
    } else {
        DeviceRole::Ignore
    }
}

/// HID Usage Page 0x01 — Generic Desktop (pointer axes live here).
const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
/// HID Usage Page 0x07 — Keyboard / Keypad.
const USAGE_PAGE_KEYBOARD: u16 = 0x07;
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

/// True if the parsed layout carries Keyboard-page usages — i.e. a keyboard that
/// declared neither the Boot subclass nor the Keyboard protocol but still emits
/// keystrokes. Such an interface is driven as a boot keyboard (see
/// [`classify_role`]): `SET_PROTOCOL(0)` switches it to the fixed boot report.
fn fields_have_keyboard(fields: &[ReportField]) -> bool {
    fields.iter().any(|f| f.usage_page == USAGE_PAGE_KEYBOARD)
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

fn monotonic_ns() -> u64 {
    let (sec, nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    if sec < 0 {
        return 0;
    }
    (sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(nsec as u64)
}

/// Per-RPC budget for a synchronous call to the shared, single-threaded xHCI
/// server. Comfortably above the server's worst-case per-request bound (~400 ms
/// command / ~200 ms control) so a legitimately slow-but-completing call is
/// never aborted, yet finite so a monopolised server (a dock-hub re-enumeration
/// storm from `usbhub` keeps the single-threaded `usb` server out of `recv`)
/// can never park usb-hid forever in `BlockedOnReply` with no waker.
const USB_CALL_TIMEOUT_NS: u64 = 1_000_000_000; // 1 s

/// Total wall-clock budget for the boot-time enumeration retry. The
/// single-threaded xHCI server can be busy bringing up one or more controllers
/// (each bring-up wait is bounded but several seconds in aggregate on a laptop
/// with two xHCI controllers) when usb-hid first asks for the attach table, so a
/// single timed-out `NextAttach` does NOT mean "no devices". Retry until the
/// server answers — but bounded, so a machine with a genuinely absent/wedged
/// controller still exits instead of spinning forever.
const INITIAL_ENUM_BUDGET_MS: u64 = 15_000;

/// Sleep between enumeration retries while the server is busy. Each timed-out
/// `NextAttach` already consumed up to `USB_CALL_TIMEOUT_NS`, so this only adds
/// a little slack to avoid hammering a busy server.
const ENUM_RETRY_SLEEP_NS: u32 = 500_000_000; // 500 ms

/// Issue a `UsbRequest` to the xHCI server and decode the `UsbReply`.
///
/// Uses the deadline-bounded `ipc_call_buf_timeout`: if the (shared,
/// single-threaded) server does not reply within [`USB_CALL_TIMEOUT_NS`], the
/// call returns `NEG_ETIMEDOUT` instead of parking forever, and we surface
/// `None` so the caller's poll loop treats it as "no report" and retries (with
/// adaptive backoff) on the next tick.
fn usb_call(usb_ep: u32, req: &UsbRequest) -> Option<UsbReply> {
    match usb_call_status(usb_ep, req) {
        CallStatus::Reply(r) => Some(r),
        CallStatus::TimedOut | CallStatus::Failed => None,
    }
}

/// Outcome of one `usb_call`, distinguishing a **server timeout** (the
/// single-threaded server was too busy to reply within the budget — e.g. still
/// busy-spinning controller bring-up — and the request is worth retrying) from a
/// transport **failure** and from a decoded **reply**. The boot enumeration uses
/// this to wait out a busy server instead of declaring "no HID devices" on the
/// first timed-out `NextAttach`.
enum CallStatus {
    Reply(UsbReply),
    TimedOut,
    Failed,
}

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
        let reply = syscall_lib::ipc_call_buf(mouse_ep, MOUSE_EVENT_INJECT, 0, &buf);
        // Only count injects that actually reached `mouse_server`. A failed
        // transport returns `u64::MAX` (no endpoint, server down, or reject);
        // counting those would let the C.1 sentinel — and `usb-smoke` — pass
        // even when the decode→inject seam is broken, defeating its purpose.
        if reply == u64::MAX {
            return;
        }
        // C.1 bare-metal sentinel: emit a greppable injected-event count so
        // that, when run on real hardware over the dock-hub topology, logs
        // capture proof of a non-zero injected count.  Emitted on the first
        // successful inject and then every 64th inject to coalesce output.
        let n = INJECTED_PTR_COUNT
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        if n == 1 || n.is_multiple_of(64) {
            syscall_lib::write_str(STDOUT_FILENO, "USB_HID:pointer-injected count=");
            write_u32_dec(n);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
        }
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
/// Returns `true` if a non-empty report was received (used by the adaptive
/// backoff state machine to snap back to the fast poll cadence).
fn poll_keyboard(usb_ep: u32, kbd_ep: u32, dev: &mut HidDevice, keymap: &Keymap) -> bool {
    let report = match poll_report(usb_ep, dev) {
        Some(r) if r.len() >= HID_KBD_REPORT_LEN => r,
        _ => return false,
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
        // Mirror into dmesg: proves an interrupt-IN boot report actually arrived
        // from the keyboard over the (dock-hub) topology — isolating a delivery
        // (controller/arming) failure from a decode/inject one on bare metal.
        klog(&alloc::format!(
            "usb-hid: kbd report {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}\n",
            report[0],
            report[1],
            report[2],
            report[3],
            report[4],
            report[5],
            report[6],
            report[7],
        ));
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
    true
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
///
/// Returns `true` only when the report carried new pointer activity (motion,
/// wheel, or a button-state change). Returns `false` both when no report was
/// available and when an idle report decoded to no movement and no button
/// change — e.g. a tablet that re-reports its static position every frame.
/// The caller uses this to drive the adaptive-backoff state machine, so
/// "empty" here means "no new activity", not "no USB transfer".
fn poll_report_pointer(usb_ep: u32, mouse_ep: u32, dev: &mut HidDevice) -> bool {
    let report = match poll_report(usb_ep, dev) {
        Some(r) => r,
        None => return false,
    };
    let p: DecodedPointer = decode_pointer_report(&dev.report_fields, &report);
    // Nothing moved and no button changed — stay quiet (an idle tablet still
    // reports its position every frame, but `any_input` gates the sentinel).
    if !p.any_input && p.buttons == dev.prev_pointer_buttons {
        return false;
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
    true
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
/// Returns `true` if a non-empty report was received.
fn poll_report_consumer(usb_ep: u32, kbd_ep: u32, dev: &mut HidDevice, keymap: &Keymap) -> bool {
    let report = match poll_report(usb_ep, dev) {
        Some(r) => r,
        None => return false,
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
    true
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
    // Mirror into dmesg: proves the full USB→decode→kbd_server chain delivered a
    // keystroke (visible over SSH on a bare-metal GUI boot).
    klog(&alloc::format!(
        "USB_HID:key kind={} sym=0x{:x} kc=0x{:x}\n",
        ev.kind as u8,
        ev.symbol,
        ev.keycode,
    ));
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
/// Returns `true` if a non-empty report was received.
fn poll_mouse(usb_ep: u32, mouse_ep: u32, dev: &mut HidDevice) -> bool {
    let report = match poll_report(usb_ep, dev) {
        Some(r) => r,
        None => return false,
    };
    let Some(m) = parse_boot_mouse_report(&report) else {
        return false;
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
    true
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

/// One full `NextAttach` walk. Returns every attached HID interface as a built
/// [`HidDevice`], plus whether the walk was cut short by a server **timeout**
/// (`true`) rather than reaching the end of the attach table (`false`). The boot
/// path retries while the result is empty *and* a timeout occurred — i.e. the
/// server was merely busy (controller bring-up) rather than reporting no devices.
fn enumerate_once(usb_ep: u32) -> (Vec<HidDevice>, bool) {
    let mut devices: Vec<HidDevice> = Vec::new();
    let mut cursor = 0u8;
    loop {
        match usb_call_status(usb_ep, &UsbRequest::NextAttach { cursor }) {
            CallStatus::TimedOut => return (devices, true),
            CallStatus::Reply(UsbReply::Attach {
                notice: Some(notice),
            }) => {
                let idx = cursor;
                cursor = match cursor.checked_add(1) {
                    Some(c) => c,
                    None => return (devices, false),
                };
                // A boot enumeration only surfaces attached devices, but guard
                // the flag so the walk is correct if it sees a stale detached one.
                if !notice.attached {
                    continue;
                }
                let dev = build_device(usb_ep, notice, idx);
                // Only claim "bound" for interfaces this daemon actually
                // drives. The attach table also surfaces mass-storage sticks,
                // hubs, and NICs (role `Ignore` — the poll loop skips them);
                // the old unconditional print here logged e.g. `bound HID
                // device (proto 80)` for a BOT mass-storage interface, which
                // sent the Phase 106 dual-smoke investigation down a false
                // trail.
                if dev.role == DeviceRole::Ignore {
                    devices.push(dev);
                    continue;
                }
                syscall_lib::write_str(STDOUT_FILENO, "usb-hid: bound HID device (proto ");
                write_u8_dec(notice.interface_protocol);
                syscall_lib::write_str(STDOUT_FILENO, ")\n");
                devices.push(dev);
            }
            // End of the attach table (`Attach { notice: None }`), a transport
            // failure, or any other reply: the walk is done and the server was
            // responsive enough to answer — not a "busy" timeout.
            CallStatus::Reply(_) | CallStatus::Failed => return (devices, false),
        }
    }
}

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);
    klog("usb-hid: spawned\n");

    // 1. Wait for the xHCI driver to register the `usb` service (it is a
    //    `depends=xhci_driver` daemon, but ordering is best-effort). A bounded
    //    wait avoids hanging forever on a machine with no USB controller.
    if !syscall_lib::ipc_wait_service(USB_SERVICE_NAME, 10_000) {
        klog("usb-hid: 'usb' service never appeared — exiting cleanly\n");
        syscall_lib::write_str(
            STDOUT_FILENO,
            "usb-hid: 'usb' service never appeared — exiting cleanly\n",
        );
        return 0;
    }
    let Some(usb_ep) = lookup(USB_SERVICE_NAME) else {
        klog("usb-hid: 'usb' lookup failed — exiting\n");
        syscall_lib::write_str(STDOUT_FILENO, "usb-hid: 'usb' lookup failed — exiting\n");
        return 0;
    };

    // 2. Enumerate attached HID devices via the NextAttach cursor, retrying while
    //    the server is still too busy to answer. A multi-controller bring-up can
    //    keep the single-threaded xHCI server out of `recv` for several seconds —
    //    longer than one `USB_CALL_TIMEOUT_NS` — so a single timed-out
    //    `NextAttach` means "busy, try again", NOT "no devices". A clean empty
    //    reply means the server is responsive and there genuinely are no HID
    //    devices (the QEMU-without-HID path), so exit promptly. The retry is
    //    bounded by `INITIAL_ENUM_BUDGET_MS` so a wedged/absent controller still
    //    exits instead of looping forever.
    let enum_deadline_ms = monotonic_ms().saturating_add(INITIAL_ENUM_BUDGET_MS);
    let mut devices: Vec<HidDevice> = loop {
        let (found, timed_out) = enumerate_once(usb_ep);
        if !found.is_empty() || !timed_out || monotonic_ms() >= enum_deadline_ms {
            break found;
        }
        klog("usb-hid: 'usb' server busy (controller bring-up?); retrying enumeration\n");
        let _ = syscall_lib::nanosleep_for(0, ENUM_RETRY_SLEEP_NS);
    };

    if devices.is_empty() {
        klog("usb-hid: no HID devices attached — exiting cleanly\n");
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
        let role_str = match dev.role {
            DeviceRole::BootKeyboard => " role=KEYBOARD\n",
            DeviceRole::BootMouse => " role=MOUSE\n",
            DeviceRole::ReportPointer => " role=REPORT_POINTER\n",
            DeviceRole::ReportConsumer => " role=REPORT_CONSUMER\n",
            DeviceRole::Ignore => " role=other\n",
        };
        syscall_lib::write_str(STDOUT_FILENO, role_str);
        // Mirror the bound vid/pid/class/proto/role into the kernel dmesg ring
        // so a bare-metal GUI boot — where the only off-box channel is `dmesg`
        // over SSH and driver fd-1 output is invisible — reveals exactly what
        // enumerated and how each interface was classified (the data point that
        // distinguishes a classification gap from an enumeration/arming one).
        klog(&alloc::format!(
            "usb-hid: bound vid=0x{:04x} pid=0x{:04x} class={} sub={} proto={}{}",
            n.vendor_id,
            n.product_id,
            n.interface_class,
            n.interface_sub_class,
            n.interface_protocol,
            role_str,
        ));
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
    klog(READY_SENTINEL);
    syscall_lib::write_str(STDOUT_FILENO, READY_SENTINEL);

    // 4. Poll loop: each device's interrupt-IN endpoint, decode by role, inject.
    //
    // Phase 100 Track D.2 — adaptive-backoff bring-up step.
    //
    // While reports are arriving the fast cadence (POLL_INTERVAL_NS = 5 ms) is
    // preserved so input latency stays below one report period.  When N
    // consecutive polls across all devices return no data the idle sleep grows
    // (via `next_hid_backoff_ns`) up to a cap of 100 ms, reducing idle core-wake
    // frequency from ~200/s to ~10/s without any change to the xHCI server.
    //
    // Hot-plug reconcile uses a monotonic timestamp so its ~200 ms cadence is
    // independent of the adaptive sleep duration.
    //
    // Full xHCI transfer-event notification (blocking on the controller's
    // IRQ-driven wakeup instead of polling) is deferred to Phase 103 (USB runtime
    // power management).  The adaptive backoff is the Phase 100 bring-up step.
    let mut last_reconcile_ms = monotonic_ms();
    let mut consecutive_empty: u32 = 0;
    loop {
        // Time-based hot-plug reconcile: stays at ~200 ms regardless of backoff.
        let now = monotonic_ms();
        if now.wrapping_sub(last_reconcile_ms) >= RECONCILE_INTERVAL_MS {
            reconcile_attachments(usb_ep, &mut devices);
            last_reconcile_ms = now;
        }

        // Poll all devices; track whether any returned a non-empty report.
        let mut got_report = false;
        for dev in devices.iter_mut() {
            let had = match dev.role {
                DeviceRole::BootKeyboard => kbd_ep
                    .map(|ep| poll_keyboard(usb_ep, ep, dev, &keymap))
                    .unwrap_or(false),
                DeviceRole::BootMouse => mouse_ep
                    .map(|ep| poll_mouse(usb_ep, ep, dev))
                    .unwrap_or(false),
                DeviceRole::ReportPointer => mouse_ep
                    .map(|ep| poll_report_pointer(usb_ep, ep, dev))
                    .unwrap_or(false),
                DeviceRole::ReportConsumer => kbd_ep
                    .map(|ep| poll_report_consumer(usb_ep, ep, dev, &keymap))
                    .unwrap_or(false),
                DeviceRole::Ignore => false,
            };
            if had {
                got_report = true;
            }
        }

        // Update the consecutive-empty counter and choose the sleep duration.
        if got_report {
            consecutive_empty = 0;
        } else {
            consecutive_empty = consecutive_empty.saturating_add(1);

            // Periodic idle-occupancy sentinel — falsifiable evidence that the
            // driver is no longer pinning a core at idle (Phase 100 D.2 acceptance).
            if consecutive_empty > 0 && consecutive_empty.is_multiple_of(IDLE_LOG_EVERY) {
                let sleep_ns = next_hid_backoff_ns(consecutive_empty);
                syscall_lib::write_str(STDOUT_FILENO, "USB_HID:idle ticks=");
                write_u32_dec(consecutive_empty);
                syscall_lib::write_str(STDOUT_FILENO, " backoff_ns=");
                write_u32_dec(sleep_ns);
                syscall_lib::write_str(STDOUT_FILENO, "\n");
            }
        }

        let sleep_ns = next_hid_backoff_ns(consecutive_empty);
        let _ = syscall_lib::nanosleep_for(0, sleep_ns);
    }
}

/// Write a `u32` as decimal to stdout without `alloc::format!`.
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
    // SAFETY: `buf[i..]` contains only ASCII digits.
    let s = unsafe { core::str::from_utf8_unchecked(&buf[i..]) };
    syscall_lib::write_str(STDOUT_FILENO, s);
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

// ---------------------------------------------------------------------------
// Host-side unit tests
// ---------------------------------------------------------------------------
//
// This crate previously could not be compiled for the host at all (its
// allocator / `#[panic_handler]` / `#[alloc_error_handler]` were ungated, so
// `cargo test` hit a duplicate-lang-item error). With the `cfg(not(test))`
// gates above in place the daemon body builds as a plain `std` test binary,
// which lets the pure decision logic — the interface classifier and the HID
// descriptor walk — be covered without QEMU. Everything else in this file is
// syscall/IPC-bound and stays gate-covered (`usb-smoke`, `usb-report-smoke`).

#[cfg(test)]
mod tests {
    use super::*;
    use usb_core::protocol::AttachNotice;

    /// An `AttachNotice` with every field zeroed but the class triple, which is
    /// all `classify_role` reads.
    fn notice(class: u8, sub_class: u8, protocol: u8) -> AttachNotice {
        AttachNotice {
            port: 1,
            slot_id: 1,
            interface_class: class,
            interface_sub_class: sub_class,
            interface_protocol: protocol,
            attached: true,
            ep_in_dci: 3,
            ep_in_mps: 8,
            ep_in_interval: 8,
            interface_num: 0,
            vendor_id: 0,
            product_id: 0,
            bulk_in_dci: 0,
            bulk_in_mps: 0,
            bulk_out_dci: 0,
            bulk_out_mps: 0,
        }
    }

    /// A `ReportField` carrying only the usage page/usage the classifier reads.
    fn field(usage_page: u16, usage: u16) -> ReportField {
        ReportField {
            usage_page,
            usage,
            bit_offset: 0,
            bit_size: 8,
            report_id: 0,
            is_relative: true,
        }
    }

    // -- classify_role ------------------------------------------------------

    #[test]
    fn boot_subclass_keyboard_is_boot_keyboard() {
        let n = notice(CLASS_HID, SUBCLASS_HID_BOOT, PROTOCOL_HID_KEYBOARD);
        assert!(matches!(classify_role(&n, &[]), DeviceRole::BootKeyboard));
    }

    #[test]
    fn boot_subclass_mouse_is_boot_mouse() {
        let n = notice(CLASS_HID, SUBCLASS_HID_BOOT, PROTOCOL_HID_MOUSE);
        assert!(matches!(classify_role(&n, &[]), DeviceRole::BootMouse));
    }

    /// The boot protocol field is authoritative under the Boot subclass even
    /// when a Report layout was parsed — the fixed-format decode wins.
    #[test]
    fn boot_subclass_mouse_beats_parsed_pointer_layout() {
        let n = notice(CLASS_HID, SUBCLASS_HID_BOOT, PROTOCOL_HID_MOUSE);
        let fields = [field(USAGE_PAGE_GENERIC_DESKTOP, 0x30)];
        assert!(matches!(classify_role(&n, &fields), DeviceRole::BootMouse));
    }

    /// A non-Boot-subclass interface that still declares protocol 1 is a
    /// keyboard (USB HID §4.3) and is driven through `SET_PROTOCOL(0)`.
    #[test]
    fn report_subclass_keyboard_protocol_is_boot_keyboard() {
        let n = notice(CLASS_HID, 0, PROTOCOL_HID_KEYBOARD);
        assert!(matches!(classify_role(&n, &[]), DeviceRole::BootKeyboard));
    }

    /// The keyboard relaxation is deliberately NOT extended to protocol 2: a
    /// Report-Protocol pointer must keep its rich layout decode.
    #[test]
    fn report_subclass_mouse_protocol_is_not_collapsed_to_boot_mouse() {
        let n = notice(CLASS_HID, 0, PROTOCOL_HID_MOUSE);
        let fields = [field(USAGE_PAGE_GENERIC_DESKTOP, 0x31)];
        assert!(matches!(
            classify_role(&n, &fields),
            DeviceRole::ReportPointer
        ));
    }

    #[test]
    fn non_hid_class_is_ignored() {
        // Mass storage (0x08) with a HID-looking protocol byte.
        let n = notice(0x08, 0, PROTOCOL_HID_MOUSE);
        assert!(matches!(classify_role(&n, &[]), DeviceRole::Ignore));
    }

    #[test]
    fn hid_class_without_layout_is_ignored() {
        let n = notice(CLASS_HID, 0, 0);
        assert!(matches!(classify_role(&n, &[]), DeviceRole::Ignore));
    }

    #[test]
    fn button_page_alone_classifies_as_report_pointer() {
        let n = notice(CLASS_HID, 0, 0);
        let fields = [field(USAGE_PAGE_BUTTON, 1)];
        assert!(matches!(
            classify_role(&n, &fields),
            DeviceRole::ReportPointer
        ));
    }

    #[test]
    fn consumer_page_alone_classifies_as_report_consumer() {
        let n = notice(CLASS_HID, 0, 0);
        let fields = [field(USAGE_PAGE_CONSUMER, 0xE9)];
        assert!(matches!(
            classify_role(&n, &fields),
            DeviceRole::ReportConsumer
        ));
    }

    #[test]
    fn keyboard_page_alone_classifies_as_boot_keyboard() {
        let n = notice(CLASS_HID, 0, 0);
        let fields = [field(USAGE_PAGE_KEYBOARD, 0x04)];
        assert!(matches!(
            classify_role(&n, &fields),
            DeviceRole::BootKeyboard
        ));
    }

    /// A combo interface exposing both pointer axes and a keyboard collection
    /// must keep driving `mouse_server` (pointer is tested first).
    #[test]
    fn pointer_wins_over_keyboard_on_a_combo_layout() {
        let n = notice(CLASS_HID, 0, 0);
        let fields = [
            field(USAGE_PAGE_KEYBOARD, 0x04),
            field(USAGE_PAGE_GENERIC_DESKTOP, 0x30),
        ];
        assert!(matches!(
            classify_role(&n, &fields),
            DeviceRole::ReportPointer
        ));
    }

    // -- fields_have_pointer / fields_have_keyboard -------------------------

    #[test]
    fn pointer_detection_accepts_x_y_axes_and_buttons_only() {
        assert!(fields_have_pointer(&[field(
            USAGE_PAGE_GENERIC_DESKTOP,
            0x30
        )]));
        assert!(fields_have_pointer(&[field(
            USAGE_PAGE_GENERIC_DESKTOP,
            0x31
        )]));
        assert!(fields_have_pointer(&[field(USAGE_PAGE_BUTTON, 3)]));
        // Generic Desktop, but a non-axis usage (0x38 = Wheel) — a wheel alone
        // is not enough to call the interface a pointer.
        assert!(!fields_have_pointer(&[field(
            USAGE_PAGE_GENERIC_DESKTOP,
            0x38
        )]));
        assert!(!fields_have_pointer(&[]));
    }

    #[test]
    fn keyboard_detection_matches_only_the_keyboard_page() {
        assert!(fields_have_keyboard(&[field(USAGE_PAGE_KEYBOARD, 0x04)]));
        assert!(!fields_have_keyboard(&[field(USAGE_PAGE_CONSUMER, 0xE9)]));
        assert!(!fields_have_keyboard(&[]));
    }

    // -- hid_report_descriptor_len ------------------------------------------

    /// 9-byte interface descriptor for `iface`.
    fn iface_desc(iface: u8) -> [u8; 9] {
        [9, 0x04, iface, 0, 1, CLASS_HID, 0, 0, 0]
    }

    /// 9-byte HID descriptor declaring one Report (0x22) entry of `len` bytes.
    fn hid_desc(len: u16) -> [u8; 9] {
        [
            9,
            0x21,
            0x11,
            0x01, // bcdHID 1.11
            0,    // bCountryCode
            1,    // bNumDescriptors
            0x22, // Report descriptor
            (len & 0xff) as u8,
            (len >> 8) as u8,
        ]
    }

    /// 7-byte endpoint descriptor, so the walk has to step over a trailing TLV.
    const EP_DESC: [u8; 7] = [7, 0x05, 0x81, 0x03, 8, 0, 10];

    fn config_blob(parts: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for p in parts {
            v.extend_from_slice(p);
        }
        v
    }

    #[test]
    fn report_len_found_for_the_requested_interface() {
        let cfg = config_blob(&[&iface_desc(0), &hid_desc(52), &EP_DESC]);
        assert_eq!(hid_report_descriptor_len(&cfg, 0), Some(52));
    }

    /// The HID descriptor of interface 0 must not answer a query for
    /// interface 1 — `cur_iface` gates the match.
    #[test]
    fn report_len_is_scoped_to_its_interface() {
        let cfg = config_blob(&[&iface_desc(0), &hid_desc(52), &EP_DESC]);
        assert_eq!(hid_report_descriptor_len(&cfg, 1), None);
    }

    #[test]
    fn report_len_walks_past_a_preceding_interface() {
        let cfg = config_blob(&[
            &iface_desc(0),
            &hid_desc(52),
            &EP_DESC,
            &iface_desc(1),
            &hid_desc(0x00c2),
            &EP_DESC,
        ]);
        assert_eq!(hid_report_descriptor_len(&cfg, 0), Some(52));
        assert_eq!(hid_report_descriptor_len(&cfg, 1), Some(0x00c2));
    }

    #[test]
    fn report_len_handles_a_two_byte_length() {
        let cfg = config_blob(&[&iface_desc(0), &hid_desc(0x1234)]);
        assert_eq!(hid_report_descriptor_len(&cfg, 0), Some(0x1234));
    }

    /// A HID descriptor whose only class-descriptor entry is not a Report
    /// (0x22) yields nothing rather than a bogus length.
    #[test]
    fn report_len_skips_non_report_class_descriptors() {
        let mut hid = hid_desc(52);
        hid[6] = 0x23; // Physical descriptor, not Report
        let cfg = config_blob(&[&iface_desc(0), &hid, &EP_DESC]);
        assert_eq!(hid_report_descriptor_len(&cfg, 0), None);
    }

    /// `bNumDescriptors` claiming more entries than fit inside the HID
    /// descriptor's own `bLength` must fail closed, not read into the next TLV.
    #[test]
    fn report_len_fails_closed_on_overlong_num_descriptors() {
        let mut hid = hid_desc(52);
        hid[5] = 2; // bNumDescriptors = 2, but only one entry fits in bLength 9
        hid[6] = 0x23; // make the one entry that does fit a non-Report
        // The next descriptor is a Report-shaped decoy: an unbounded walk would
        // read a length out of it.
        let cfg = config_blob(&[&iface_desc(0), &hid, &[0x22, 0xff, 0xff]]);
        assert_eq!(hid_report_descriptor_len(&cfg, 0), None);
    }

    /// A descriptor whose `bLength` runs past the end of the blob terminates
    /// the walk instead of panicking.
    #[test]
    fn report_len_handles_a_truncated_blob() {
        let full = config_blob(&[&iface_desc(0), &hid_desc(52)]);
        let truncated = &full[..full.len() - 3];
        assert_eq!(hid_report_descriptor_len(truncated, 0), None);
    }

    /// A zero/one-byte `bLength` would make the walk spin forever; it must
    /// break out instead.
    #[test]
    fn report_len_rejects_a_degenerate_blength() {
        let cfg = config_blob(&[&iface_desc(0), &[0, 0x21, 0, 0, 0, 1, 0x22, 52, 0]]);
        assert_eq!(hid_report_descriptor_len(&cfg, 0), None);
        assert_eq!(hid_report_descriptor_len(&[], 0), None);
    }
}
