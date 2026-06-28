//! Phase 100 Track D — HID-polling adaptive-backoff helpers and
//! KeyEvent-to-stdin translation.
//!
//! These pure functions are factored out of the `usb-hid` and `usbhub`
//! drivers so they are host-testable (the `std` feature of `kernel-core`
//! enables the test harness) and shared between drivers that already
//! depend on `kernel-core`.
//!
//! # Adaptive backoff — D.2 / D.3 design note
//!
//! `usb-hid` and `usbhub` currently poll on a fixed cadence (5 ms / 50 ms).
//! At idle this wakes the core ~200× / 20× per second with nothing to do.
//! The backoff helpers below implement the Phase 100 bring-up step: **while
//! reports are arriving**, the fast cadence is preserved (≤ one report
//! period of latency); **at idle**, the sleep doubles every few consecutive
//! empty polls up to a cap of ~50–200 ms, reducing wake frequency ≈10×.
//!
//! Full xHCI transfer-event notification (blocking on the controller's
//! IRQ-driven wakeup instead of polling at all) is deferred to **Phase 103**
//! (USB runtime power management). The adaptive backoff is the bring-up step:
//! it lowers idle CPU occupancy without requiring changes to the xHCI server
//! or the `usb-core` IPC protocol.

// ---- HID interrupt-IN poll backoff (D.2) -----------------------------------

/// Fast-path poll interval for `usb-hid` — matches the current boot-device
/// report period of ~10 ms (Boot Protocol `bInterval`).  A 5 ms poll keeps
/// latency below one report period while events are actively arriving.
pub const HID_POLL_FAST_NS: u32 = 5_000_000; // 5 ms

/// Maximum idle sleep for `usb-hid`.  At this cadence the core wakes ~10×/s
/// at idle instead of 200×/s — a 20× improvement in idle CPU occupancy.
pub const HID_POLL_MAX_IDLE_NS: u32 = 100_000_000; // 100 ms

/// Number of consecutive empty polls per backoff doubling step.
///
/// At `HID_POLL_FAST_NS` (5 ms) each doubling step is 20 ms of wall-clock
/// idle before the sleep lengthens.  Six steps reach `HID_POLL_MAX_IDLE_NS`.
const HID_BACKOFF_STEP: u32 = 4;

/// Compute the next `nanosleep_for` duration for `usb-hid`'s poll loop.
///
/// Returns [`HID_POLL_FAST_NS`] while `consecutive_empty` is below one
/// step — i.e. while reports are arriving (the caller resets
/// `consecutive_empty` to 0 on any non-empty poll).  Doubles every
/// [`HID_BACKOFF_STEP`] consecutive empties, capped at
/// [`HID_POLL_MAX_IDLE_NS`].
///
/// # Example
/// ```
/// use kernel_core::input::hid_poll::{next_hid_backoff_ns, HID_POLL_FAST_NS, HID_POLL_MAX_IDLE_NS};
/// assert_eq!(next_hid_backoff_ns(0), HID_POLL_FAST_NS);
/// assert!(next_hid_backoff_ns(100) <= HID_POLL_MAX_IDLE_NS);
/// ```
pub fn next_hid_backoff_ns(consecutive_empty: u32) -> u32 {
    if consecutive_empty < HID_BACKOFF_STEP {
        return HID_POLL_FAST_NS;
    }
    let doublings = (consecutive_empty / HID_BACKOFF_STEP).min(5); // ≤ 32× = 160 ms, capped below
    let ns = HID_POLL_FAST_NS.saturating_mul(1u32 << doublings);
    ns.min(HID_POLL_MAX_IDLE_NS)
}

// ---- Hub-port monitoring backoff (D.3) -------------------------------------

/// Base poll interval for `usbhub` steady-state port monitoring.
/// After initial enumeration, the daemon re-checks port status at this
/// cadence (growing with adaptive backoff), so a hot-plug is detected
/// within one backoff period.
pub const HUB_POLL_BASE_NS: u32 = 50_000_000; // 50 ms

/// Maximum idle sleep for `usbhub`.
pub const HUB_POLL_MAX_IDLE_NS: u32 = 200_000_000; // 200 ms

/// Compute the next `nanosleep_for` duration for `usbhub`'s port-monitoring
/// loop.  Starts at [`HUB_POLL_BASE_NS`], doubles every 4 consecutive idle
/// polls, capped at [`HUB_POLL_MAX_IDLE_NS`].
pub fn hub_next_backoff_ns(consecutive_idle: u32) -> u32 {
    if consecutive_idle < 4 {
        return HUB_POLL_BASE_NS;
    }
    let doublings = (consecutive_idle / 4).min(2); // ≤ 4× = 200 ms, capped
    let ns = HUB_POLL_BASE_NS.saturating_mul(1u32 << doublings);
    ns.min(HUB_POLL_MAX_IDLE_NS)
}

// ---- KeyEvent→stdin translation (D.1) -------------------------------------

// Private-use KeySym values for navigation keys; must stay in sync with
// `kernel_core::input::keymap::KEYSYM_*` constants (Phase 56 Track D.1).
const KEYSYM_LEFT: u32 = 0xE010;
const KEYSYM_RIGHT: u32 = 0xE011;
const KEYSYM_UP: u32 = 0xE012;
const KEYSYM_DOWN: u32 = 0xE013;
const KEYSYM_HOME: u32 = 0xE014;
const KEYSYM_END: u32 = 0xE015;
const KEYSYM_PAGEUP: u32 = 0xE016;
const KEYSYM_PAGEDOWN: u32 = 0xE017;
// 0xE018 = KEYSYM_INSERT (no VT100 output needed for stdin_feeder)
const KEYSYM_DELETE: u32 = 0xE019;

/// `MOD_CTRL` bit in `ModifierState::0`; mirrors `kernel_core::input::events::MOD_CTRL`.
const MOD_CTRL_BIT: u16 = 1 << 1;

/// Translate a decoded `KeyEvent`'s fields to raw stdin byte(s), calling
/// `push` for each output byte.  Returns `true` if any bytes were pushed.
///
/// Only `Down` (kind=0) and `Repeat` (kind=2) events produce output;
/// `Up` (kind=1) events are ignored.
///
/// Logic mirrors `term::input::InputHandler::translate` (Phase 57 Track G.5):
/// - Private-use navigation keysyms → VT100/CSI escape sequences.
/// - Backspace (U+0008) → DEL (0x7F).
/// - Ctrl + ASCII letter → control code (0x01–0x1A).
/// - All other 7-bit values pass through verbatim (including CR 0x0D,
///   TAB 0x09, ESC 0x1B, and printable ASCII 0x20–0x7E).
/// - Unknown private-use keysyms (0xE000+) produce no output.
///
/// This function is `no_std`-compatible and allocation-free.  The `push`
/// callback is called at most `~5` times (for the longest CSI sequence).
pub fn key_event_to_stdin<F: FnMut(u8)>(
    symbol: u32,
    mods_bits: u16,
    kind: u8,
    mut push: F,
) -> bool {
    // Only Down (0) and Repeat (2) edges generate stdin bytes.
    if kind != 0 && kind != 2 {
        return false;
    }
    // symbol==0 means a modifier-key-only event (no character).
    if symbol == 0 {
        return false;
    }

    // Navigation keys in the Unicode private-use area → VT100/CSI sequences.
    let escape_seq: Option<&'static [u8]> = match symbol {
        s if s == KEYSYM_UP => Some(b"\x1b[A"),
        s if s == KEYSYM_DOWN => Some(b"\x1b[B"),
        s if s == KEYSYM_RIGHT => Some(b"\x1b[C"),
        s if s == KEYSYM_LEFT => Some(b"\x1b[D"),
        s if s == KEYSYM_HOME => Some(b"\x1b[H"),
        s if s == KEYSYM_END => Some(b"\x1b[F"),
        s if s == KEYSYM_DELETE => Some(b"\x1b[3~"),
        s if s == KEYSYM_PAGEUP => Some(b"\x1b[5~"),
        s if s == KEYSYM_PAGEDOWN => Some(b"\x1b[6~"),
        _ => None,
    };
    if let Some(seq) = escape_seq {
        for &b in seq {
            push(b);
        }
        return true;
    }

    // Backspace (U+0008) → DEL (0x7F), matching the kernel line-discipline
    // convention used by the existing PS/2 path in `stdin_feeder`.
    if symbol == 0x08 {
        push(0x7F);
        return true;
    }

    // Ctrl + ASCII letter → control code (0x01–0x1A).
    if mods_bits & MOD_CTRL_BIT != 0 && symbol <= 0x7F {
        let lower = (symbol as u8).to_ascii_lowercase();
        if lower.is_ascii_lowercase() {
            push(lower - b'a' + 1);
            return true;
        }
        // Ctrl held but key is not A-Z: fall through to the 7-bit path
        // only if the symbol itself is a printable ASCII byte.
    }

    // All 7-bit values (printable ASCII, CR, TAB, ESC, and other C0
    // control codes the keymap assigns directly) pass through verbatim.
    // Private-use keysyms outside the recognised navigation set (> 0x7F
    // and not matched above) produce no output.
    if symbol <= 0x7F {
        push(symbol as u8);
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Host-side tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- next_hid_backoff_ns -----------------------------------------------

    #[test]
    fn hid_backoff_fast_while_active() {
        // consecutive_empty < HID_BACKOFF_STEP → always fast
        for n in 0..HID_BACKOFF_STEP {
            assert_eq!(
                next_hid_backoff_ns(n),
                HID_POLL_FAST_NS,
                "expected fast at consecutive_empty={n}"
            );
        }
    }

    #[test]
    fn hid_backoff_grows_with_empty_count() {
        let first_step = next_hid_backoff_ns(HID_BACKOFF_STEP);
        let second_step = next_hid_backoff_ns(HID_BACKOFF_STEP * 2);
        assert!(
            first_step > HID_POLL_FAST_NS,
            "first step must be longer than fast cadence"
        );
        assert!(second_step >= first_step, "backoff must be non-decreasing");
    }

    #[test]
    fn hid_backoff_capped_at_max() {
        // A very large consecutive_empty must not exceed the cap.
        assert_eq!(next_hid_backoff_ns(u32::MAX), HID_POLL_MAX_IDLE_NS);
        assert_eq!(next_hid_backoff_ns(1_000_000), HID_POLL_MAX_IDLE_NS);
    }

    #[test]
    fn hid_backoff_snaps_to_fast_at_zero() {
        // Simulates a report arriving: caller resets consecutive_empty to 0.
        assert_eq!(next_hid_backoff_ns(0), HID_POLL_FAST_NS);
    }

    // ---- hub_next_backoff_ns -----------------------------------------------

    #[test]
    fn hub_backoff_starts_at_base() {
        assert_eq!(hub_next_backoff_ns(0), HUB_POLL_BASE_NS);
        assert_eq!(hub_next_backoff_ns(3), HUB_POLL_BASE_NS);
    }

    #[test]
    fn hub_backoff_capped_at_max() {
        assert_eq!(hub_next_backoff_ns(u32::MAX), HUB_POLL_MAX_IDLE_NS);
    }

    // ---- key_event_to_stdin ------------------------------------------------

    fn collect(symbol: u32, mods_bits: u16, kind: u8) -> Vec<u8> {
        let mut out = Vec::new();
        key_event_to_stdin(symbol, mods_bits, kind, |b| out.push(b));
        out
    }

    #[test]
    fn printable_ascii_passes_through() {
        assert_eq!(collect(b'a' as u32, 0, 0), b"a");
        assert_eq!(collect(b'Z' as u32, 0, 0), b"Z");
        assert_eq!(collect(b'0' as u32, 0, 0), b"0");
        assert_eq!(collect(b'!' as u32, 0, 0), b"!");
    }

    #[test]
    fn up_event_produces_no_bytes() {
        // kind=1 (Up) must always produce no output.
        assert!(collect(b'a' as u32, 0, 1).is_empty());
        assert!(collect(KEYSYM_UP, 0, 1).is_empty());
    }

    #[test]
    fn repeat_event_produces_output_like_down() {
        // kind=2 (Repeat) must behave like kind=0 (Down).
        assert_eq!(collect(b'x' as u32, 0, 2), b"x");
        assert_eq!(collect(KEYSYM_LEFT, 0, 2), b"\x1b[D");
    }

    #[test]
    fn modifier_only_event_produces_no_bytes() {
        // symbol==0 means modifier-only; no output.
        assert!(collect(0, 0, 0).is_empty());
    }

    #[test]
    fn backspace_becomes_del() {
        assert_eq!(collect(0x08, 0, 0), &[0x7F]);
    }

    #[test]
    fn enter_cr_passes_through() {
        assert_eq!(collect(b'\r' as u32, 0, 0), b"\r");
    }

    #[test]
    fn tab_passes_through() {
        assert_eq!(collect(b'\t' as u32, 0, 0), b"\t");
    }

    #[test]
    fn ctrl_c_produces_etx() {
        const MOD_CTRL: u16 = 1 << 1;
        assert_eq!(collect(b'c' as u32, MOD_CTRL, 0), &[0x03]);
    }

    #[test]
    fn ctrl_d_produces_eot() {
        const MOD_CTRL: u16 = 1 << 1;
        assert_eq!(collect(b'd' as u32, MOD_CTRL, 0), &[0x04]);
    }

    #[test]
    fn arrow_up_produces_csi_a() {
        assert_eq!(collect(KEYSYM_UP, 0, 0), b"\x1b[A");
    }

    #[test]
    fn arrow_down_produces_csi_b() {
        assert_eq!(collect(KEYSYM_DOWN, 0, 0), b"\x1b[B");
    }

    #[test]
    fn arrow_right_produces_csi_c() {
        assert_eq!(collect(KEYSYM_RIGHT, 0, 0), b"\x1b[C");
    }

    #[test]
    fn arrow_left_produces_csi_d() {
        assert_eq!(collect(KEYSYM_LEFT, 0, 0), b"\x1b[D");
    }

    #[test]
    fn home_produces_csi_h() {
        assert_eq!(collect(KEYSYM_HOME, 0, 0), b"\x1b[H");
    }

    #[test]
    fn end_produces_csi_f() {
        assert_eq!(collect(KEYSYM_END, 0, 0), b"\x1b[F");
    }

    #[test]
    fn delete_produces_csi_3_tilde() {
        assert_eq!(collect(KEYSYM_DELETE, 0, 0), b"\x1b[3~");
    }

    #[test]
    fn pageup_produces_csi_5_tilde() {
        assert_eq!(collect(KEYSYM_PAGEUP, 0, 0), b"\x1b[5~");
    }

    #[test]
    fn pagedown_produces_csi_6_tilde() {
        assert_eq!(collect(KEYSYM_PAGEDOWN, 0, 0), b"\x1b[6~");
    }

    #[test]
    fn unknown_private_use_keysym_produces_no_bytes() {
        // A private-use codepoint not in our navigation table must produce nothing.
        assert!(collect(0xE0FF, 0, 0).is_empty());
        assert!(collect(0xE000, 0, 0).is_empty());
    }

    #[test]
    fn ctrl_with_private_use_sym_produces_no_bytes() {
        // Ctrl held + private-use keysym must NOT silently produce a control byte.
        const MOD_CTRL: u16 = 1 << 1;
        assert!(collect(0xE061, MOD_CTRL, 0).is_empty()); // low byte = 'a', must not emit 0x01
    }

    #[test]
    fn returns_true_iff_bytes_emitted() {
        let mut emitted = false;
        let got = key_event_to_stdin(b'a' as u32, 0, 0, |_| {
            emitted = true;
        });
        assert!(got);
        assert!(emitted);

        let got_none = key_event_to_stdin(0xE0FF, 0, 0, |_| {});
        assert!(!got_none);

        let got_up = key_event_to_stdin(b'a' as u32, 0, 1, |_| {});
        assert!(!got_up);
    }
}
