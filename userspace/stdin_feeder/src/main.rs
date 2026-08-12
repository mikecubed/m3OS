//! Userspace stdin feeder for m3OS (Phase 52d, Track C; Phase 100 Track D.1).
//!
//! Obtains input from two sources and forwards raw bytes to the kernel via
//! `push_raw_input`:
//!
//! 1. **PS/2 scancodes** (`KBD_TRY_READ`, label 4) — the original path.
//!    Translates set-1 make/break codes using the US-QWERTY table below.
//!
//! 2. **USB-keyboard `KeyEvent`s** (`KBD_EVENT_PULL`, label 2, Phase 100
//!    Track D.1) — typed `KeyEvent` structs injected by `usb-hid` into
//!    `kbd_server`'s bounded inject queue (`KBD_EVENT_INJECT`, label 5).
//!    `stdin_feeder` drains these on the same non-blocking poll cycle as the
//!    PS/2 path, converting each `KeyEvent` to stdin byte(s) using the same
//!    VT100 / control-code rules as `term::input::InputHandler::translate`
//!    (Phase 57 Track G.5) and
//!    `kernel_core::input::hid_poll::key_event_to_stdin` (Phase 100 D.1).
//!
//! Uses the non-blocking `KBD_TRY_READ` label (Phase 57d) rather than the
//! blocking `KBD_READ`, so that `display_server`'s concurrent
//! `KBD_EVENT_PULL` requests are not starved while this feeder polls.
//! `KBD_EVENT_PULL` is also non-blocking on the server side
//! (`MAX_PULL_POLLS == 1`): the server replies immediately with
//! `KBD_EVENT_NONE` (label 3) when no event is queued.
//!
//! When kbd_server reports both sources empty, stdin_feeder sleeps 5 ms
//! before retrying (matching the legacy kbd_server internal poll interval).
//!
//! If the `display.input-owner` service is present, stdin_feeder stands
//! down entirely: `display_server` registers that name only after the first
//! Toplevel surface is mapped, so input ownership transfers only when a real
//! graphical client (e.g. `term`) is actually up.  Boots that never reach a
//! Toplevel — text-mode fallback or a stalled graphical bring-up — keep both
//! PS/2 and USB input routed through this bridge to the kernel line discipline.
//!
//! All terminal policy (canonical editing, echo, signal generation, ICRNL)
//! is handled by the kernel-side `LineDiscipline` in `push_raw_input`.
//! This binary is a pure input-to-byte bridge.
#![no_std]
#![no_main]

use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

// Phase 100 D.1 — linking `kernel-core` for the host-tested
// `key_event_to_stdin` mapping pulls in `alloc`, so this binary now needs a
// global allocator. The `KeyEvent`→stdin path itself never allocates; the
// allocator is present only to satisfy the link.
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

// ---------------------------------------------------------------------------
// Scancode translation (US-QWERTY, ported from kernel/src/main.rs)
// ---------------------------------------------------------------------------

/// Translate a PS/2 scancode (make code, < 0x80) to an ASCII character.
///
/// Returns `None` for non-printable or unmapped scancodes.
fn scancode_to_char(sc: u8, shift: bool) -> Option<char> {
    let (lo, hi): (Option<char>, Option<char>) = match sc {
        0x02 => (Some('1'), Some('!')),
        0x03 => (Some('2'), Some('@')),
        0x04 => (Some('3'), Some('#')),
        0x05 => (Some('4'), Some('$')),
        0x06 => (Some('5'), Some('%')),
        0x07 => (Some('6'), Some('^')),
        0x08 => (Some('7'), Some('&')),
        0x09 => (Some('8'), Some('*')),
        0x0A => (Some('9'), Some('(')),
        0x0B => (Some('0'), Some(')')),
        0x0C => (Some('-'), Some('_')),
        0x0D => (Some('='), Some('+')),
        0x10 => (Some('q'), Some('Q')),
        0x11 => (Some('w'), Some('W')),
        0x12 => (Some('e'), Some('E')),
        0x13 => (Some('r'), Some('R')),
        0x14 => (Some('t'), Some('T')),
        0x15 => (Some('y'), Some('Y')),
        0x16 => (Some('u'), Some('U')),
        0x17 => (Some('i'), Some('I')),
        0x18 => (Some('o'), Some('O')),
        0x19 => (Some('p'), Some('P')),
        0x1A => (Some('['), Some('{')),
        0x1B => (Some(']'), Some('}')),
        0x1E => (Some('a'), Some('A')),
        0x1F => (Some('s'), Some('S')),
        0x20 => (Some('d'), Some('D')),
        0x21 => (Some('f'), Some('F')),
        0x22 => (Some('g'), Some('G')),
        0x23 => (Some('h'), Some('H')),
        0x24 => (Some('j'), Some('J')),
        0x25 => (Some('k'), Some('K')),
        0x26 => (Some('l'), Some('L')),
        0x27 => (Some(';'), Some(':')),
        0x28 => (Some('\''), Some('"')),
        0x29 => (Some('`'), Some('~')),
        0x2B => (Some('\\'), Some('|')),
        0x2C => (Some('z'), Some('Z')),
        0x2D => (Some('x'), Some('X')),
        0x2E => (Some('c'), Some('C')),
        0x2F => (Some('v'), Some('V')),
        0x30 => (Some('b'), Some('B')),
        0x31 => (Some('n'), Some('N')),
        0x32 => (Some('m'), Some('M')),
        0x33 => (Some(','), Some('<')),
        0x34 => (Some('.'), Some('>')),
        0x35 => (Some('/'), Some('?')),
        0x39 => (Some(' '), Some(' ')),
        _ => (None, None),
    };
    if shift { hi } else { lo }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

/// IPC operation label: non-blocking scancode probe (Phase 57d).
///
/// kbd_server replies immediately with the scancode byte, or 0 if the
/// buffer is empty.  Using this instead of the blocking `KBD_READ = 1`
/// keeps kbd_server's request queue drainable so `display_server`'s
/// concurrent `KBD_EVENT_PULL` requests are not starved.
const KBD_TRY_READ: u64 = 4;

/// IPC operation label: typed `KeyEvent` pull (Phase 56 Track D.1).
///
/// kbd_server replies with label `KBD_EVENT_PULL` (2) and a 20-byte
/// `KeyEvent` wire payload when an event is queued, or label
/// `KBD_EVENT_NONE` (3) when the queue is empty. The server is
/// non-blocking (`MAX_PULL_POLLS == 1`), so this call returns immediately
/// and never stalls `display_server`'s concurrent pulls.
const KBD_EVENT_PULL: u64 = 2;

/// Reply label from kbd_server when `KBD_EVENT_PULL` finds no event.
/// Distinct from `u64::MAX` (IPC transport error).
// The empty-queue path is detected by the label *not* equalling
// `KBD_EVENT_PULL` (see the pull loop), so this symbol is never read directly;
// kept to document the kbd_server reply-label wire ABI alongside its sibling.
#[allow(dead_code)]
const KBD_EVENT_NONE: u64 = 3;

/// Wire size of a serialised `KeyEvent` (bytes).
/// Sourced from the authoritative codec so the local buffer can never drift
/// from `KeyEvent::encode`/`decode`.
const KEY_EVENT_WIRE_SIZE: usize = kernel_core::input::events::KEY_EVENT_WIRE_SIZE;

/// How long to sleep when both kbd_server sources report an empty buffer
/// (5 ms, matching the legacy kbd_server internal poll interval).
const KBD_POLL_INTERVAL_NS: u32 = 5_000_000;

/// How often to re-check for graphical display ownership while falling back
/// to the text-mode input-to-stdin bridge. The check is capability-free, so it
/// can run on every empty poll without leaking handles.
const DISPLAY_PROBE_INTERVAL_EMPTY_POLLS: u32 = 1;

// ---------------------------------------------------------------------------
// KeyEvent wire decode (D.1)
//
// Decoding delegates to the authoritative, allocation-free codec
// `kernel_core::input::events::KeyEvent::decode` — no inline copy of the wire
// layout is kept here, so a layout change cannot silently desync this binary.
// The KeyEvent→stdin *mapping* likewise delegates to the host-tested
// `kernel_core::input::hid_poll::key_event_to_stdin`.
// ---------------------------------------------------------------------------

/// Convert a decoded `KeyEvent` to raw stdin byte(s) and push each via
/// `push_raw_input`.
///
/// This is a thin adapter over the single source of truth,
/// `kernel_core::input::hid_poll::key_event_to_stdin` (Phase 100 D.1) — the
/// same host-tested mapping used by the kernel-side line-discipline path. The
/// rules (Down/Repeat-only edges, navigation keysyms → VT100/CSI, Backspace →
/// DEL, Ctrl+letter → control code, 7-bit pass-through) all live there and are
/// covered by its unit tests; this binary no longer carries a hand-synced copy.
fn feed_key_event_to_stdin(symbol: u32, mods: u16, kind: u8) {
    kernel_core::input::hid_poll::key_event_to_stdin(symbol, mods, kind, |b| {
        syscall_lib::push_raw_input(b);
    });
}

fn lookup_kbd_service() -> u32 {
    loop {
        let handle = syscall_lib::ipc_lookup_service("kbd");
        if handle != u64::MAX {
            return handle as u32;
        }
        let _ = syscall_lib::nanosleep_for(0, 20_000_000); // 20 ms
    }
}

/// Probe for the `display.input-owner` marker. `display_server` registers
/// this name lazily, only after the first Toplevel surface is mapped —
/// so a "yes" here means a real graphical client is up and PS/2
/// scancodes belong to the focus dispatcher, not to this bridge.
/// Probing the marker (rather than the bare `display` service) avoids
/// the early-boot race where `display_server` is registered but no
/// graphical client has connected yet, which used to leave the
/// keyboard deaf in text-mode fallback boots.
fn display_input_owner_available() -> bool {
    syscall_lib::ipc_service_exists("display.input-owner")
}

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "stdin_feeder: starting\n");

    // Look up the "kbd" service to obtain an endpoint capability.
    // Retry indefinitely because service state is "running" as soon as init
    // forks the task, which still races service-registry publication.
    let mut kbd_handle = lookup_kbd_service();

    syscall_lib::write_str(STDOUT_FILENO, "stdin_feeder: ready\n");

    let mut shift = false;
    let mut ctrl = false;
    let mut graphical_input_owner = display_input_owner_available();
    let mut empty_polls_since_display_probe = 0u32;

    // Phase 100 Track D.1 — wire buffer for KeyEvent bulk replies.
    let mut kev_buf = [0u8; KEY_EVENT_WIRE_SIZE];

    loop {
        if graphical_input_owner {
            let _ = syscall_lib::nanosleep_for(0, 50_000_000);
            graphical_input_owner = display_input_owner_available();
            continue;
        }

        // ----------------------------------------------------------------
        // Source 1: PS/2 scancodes via the non-blocking KBD_TRY_READ probe
        // (label 4).  Returns the scancode byte, or 0 if the ring is empty,
        // or u64::MAX on IPC transport error.
        // ----------------------------------------------------------------
        let sc_rc = syscall_lib::ipc_call(kbd_handle, KBD_TRY_READ, 0);
        if sc_rc == u64::MAX {
            kbd_handle = lookup_kbd_service();
            continue;
        }
        let ps2_got_data = sc_rc != 0;

        if ps2_got_data {
            empty_polls_since_display_probe = 0;
            let sc = sc_rc as u8;

            // Key-release (break) codes: bit 7 set.
            if sc >= 0x80 {
                let make = sc & 0x7F;
                if make == 0x2A || make == 0x36 {
                    shift = false;
                }
                if make == 0x1D {
                    ctrl = false;
                }
                // Fall through to USB drain below without sleeping.
            } else {
                // Modifier make codes.
                if sc == 0x1D {
                    ctrl = true;
                } else if sc == 0x2A || sc == 0x36 {
                    shift = true;
                } else {
                    // VT100 escape sequences for special keys.
                    let escape_seq: Option<&[u8]> = match sc {
                        0x48 => Some(b"\x1b[A"),  // Arrow Up
                        0x50 => Some(b"\x1b[B"),  // Arrow Down
                        0x4D => Some(b"\x1b[C"),  // Arrow Right
                        0x4B => Some(b"\x1b[D"),  // Arrow Left
                        0x47 => Some(b"\x1b[H"),  // Home
                        0x4F => Some(b"\x1b[F"),  // End
                        0x53 => Some(b"\x1b[3~"), // Delete
                        0x49 => Some(b"\x1b[5~"), // Page Up
                        0x51 => Some(b"\x1b[6~"), // Page Down
                        0x01 => Some(b"\x1b"),    // Escape
                        _ => None,
                    };

                    if let Some(seq) = escape_seq {
                        for &b in seq {
                            syscall_lib::push_raw_input(b);
                        }
                    } else {
                        // Convert scancode to a raw byte.
                        let byte = if sc == 0x1C {
                            b'\r' // Enter key produces CR; kernel ICRNL translates to LF
                        } else if sc == 0x0F {
                            b'\t' // Tab
                        } else if sc == 0x0E {
                            0x7F // DEL / backspace
                        } else if ctrl {
                            // Ctrl + letter -> control character (0x01-0x1A).
                            match scancode_to_char(sc, false) {
                                Some(c) if c.is_ascii_alphabetic() => {
                                    (c.to_ascii_uppercase() as u8) - b'A' + 1
                                }
                                _ => {
                                    // Unrecognised Ctrl combo — fall through to USB drain.
                                    0
                                }
                            }
                        } else {
                            match scancode_to_char(sc, shift) {
                                Some(c) => {
                                    let mut buf = [0u8; 4];
                                    let s = c.encode_utf8(&mut buf);
                                    s.as_bytes()[0]
                                }
                                None => 0,
                            }
                        };
                        if byte != 0 {
                            syscall_lib::push_raw_input(byte);
                        }
                    }
                }
            }
        }

        // ----------------------------------------------------------------
        // Source 2: USB-keyboard typed KeyEvents via KBD_EVENT_PULL (label 2,
        // Phase 100 Track D.1).
        //
        // kbd_server drains the inject queue (populated by usb-hid via
        // KBD_EVENT_INJECT) before the PS/2 pipeline, so a USB keypress
        // that arrived while we were processing a PS/2 event is waiting.
        // The server is non-blocking (MAX_PULL_POLLS == 1) and replies:
        //   label KBD_EVENT_PULL (2) + 20-byte KeyEvent bulk — event ready.
        //   label KBD_EVENT_NONE (3)                          — queue empty.
        // u64::MAX indicates an IPC transport error; treat as empty (the
        // PS/2 path's reconnect handles service restarts on the next tick).
        // ----------------------------------------------------------------
        let kev_label = syscall_lib::ipc_call(kbd_handle, KBD_EVENT_PULL, 0);
        let usb_got_data = if kev_label == KBD_EVENT_PULL {
            let n = syscall_lib::ipc_take_pending_bulk(&mut kev_buf);
            if n as usize == KEY_EVENT_WIRE_SIZE {
                // Decode via the shared codec: it validates the `kind` tag and
                // keeps the wire layout in one place. `modifiers`/`kind` are
                // typed (`ModifierState(u16)` / `KeyEventKind: repr(u8)`), so
                // unwrap them to the raw bits `key_event_to_stdin` consumes.
                if let Ok((ev, _)) = kernel_core::input::events::KeyEvent::decode(&kev_buf) {
                    feed_key_event_to_stdin(ev.symbol, ev.modifiers.0, ev.kind as u8);
                }
                true
            } else {
                false // short/missing bulk — treat as no data
            }
        } else {
            false // KBD_EVENT_NONE (3) or error
        };

        // ----------------------------------------------------------------
        // If both sources were empty this iteration, sleep 5 ms to avoid
        // spinning; also re-check for a graphical input owner so we stand
        // down promptly once display_server takes the console.
        // ----------------------------------------------------------------
        if !ps2_got_data && !usb_got_data {
            empty_polls_since_display_probe = empty_polls_since_display_probe.saturating_add(1);
            if empty_polls_since_display_probe >= DISPLAY_PROBE_INTERVAL_EMPTY_POLLS {
                empty_polls_since_display_probe = 0;
                graphical_input_owner = display_input_owner_available();
            }
            let _ = syscall_lib::nanosleep_for(0, KBD_POLL_INTERVAL_NS);
        } else {
            empty_polls_since_display_probe = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "stdin_feeder: PANIC\n");
    syscall_lib::exit(101)
}
