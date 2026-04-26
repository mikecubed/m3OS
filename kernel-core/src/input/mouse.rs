//! Phase 56 Track B.2 — PS/2 AUX (mouse) packet decoder.
//!
//! Pure-logic, host-testable. Consumes raw bytes from the kernel's PS/2 AUX
//! ring buffer and emits one `MousePacket` per complete packet. Two packet
//! shapes are supported: the legacy 3-byte standard packet and the 4-byte
//! IntelliMouse extension (with wheel). The decoder enters IntelliMouse mode
//! only after `enable_wheel_mode()` is called by the driver glue (typically
//! after the Magic Knock handshake succeeds in ring 0).

/// One fully decoded PS/2 mouse packet.
///
/// Internal kernel-side representation; userspace `mouse_server` lifts this
/// into the public `PointerEvent` (timestamping, button-edge tracking, Y-axis
/// flip). Holding both shapes here keeps the wire-codec types in `events.rs`
/// free of PS/2-specific quirks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MousePacket {
    /// Sign-extended X delta (PS/2 9-bit two's-complement).
    pub dx: i16,
    /// Sign-extended Y delta (PS/2 native: +Y = up; caller flips).
    pub dy: i16,
    /// Wheel delta: 0 in 3-byte mode, signed 8-bit in 4-byte mode.
    pub wheel: i8,
    /// Left button currently pressed.
    pub left: bool,
    /// Right button currently pressed.
    pub right: bool,
    /// Middle button currently pressed.
    pub middle: bool,
    /// PS/2 status bit 6 — X delta overflowed and is unreliable.
    pub x_overflow: bool,
    /// PS/2 status bit 7 — Y delta overflowed and is unreliable.
    pub y_overflow: bool,
}

/// Errors observable from outside the decoder.
///
/// Today only `LostSync` is defined; additional variants would be `#[non_exhaustive]`-gated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecoderError {
    /// First byte of a packet must have status bit 3 set; if it isn't,
    /// the byte is dropped and the decoder reports this so callers can
    /// log the resync.
    LostSync,
}

/// Event surfaced by `Ps2MouseDecoder::feed`.
///
/// Either a complete packet was assembled, or a stray byte was discarded
/// while hunting for the next aligned status byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecoderEvent {
    /// One fully assembled PS/2 mouse packet.
    Packet(MousePacket),
    /// A misaligned byte was dropped during resync recovery.
    Resync,
}

/// PS/2 AUX byte-stream decoder.
///
/// Stateful but allocation-free: framing position lives in three bytes of
/// state plus a fixed 4-byte buffer. Safe to drive from the kernel's PS/2
/// IRQ bottom half because no method allocates or panics.
pub struct Ps2MouseDecoder {
    /// True when the IntelliMouse 4-byte protocol is active.
    wheel_mode: bool,
    /// 0..=3 — number of bytes already accumulated for the current packet.
    cursor: u8,
    /// Buffer for the in-progress packet (max 4 bytes).
    bytes: [u8; 4],
}

/// Status-byte mask for the always-1 sync bit (bit 3).
const STATUS_SYNC_BIT: u8 = 1 << 3;
/// Status-byte mask for the X-sign bit (bit 4).
const STATUS_X_SIGN: u8 = 1 << 4;
/// Status-byte mask for the Y-sign bit (bit 5).
const STATUS_Y_SIGN: u8 = 1 << 5;
/// Status-byte mask for the X-overflow bit (bit 6).
const STATUS_X_OVERFLOW: u8 = 1 << 6;
/// Status-byte mask for the Y-overflow bit (bit 7).
const STATUS_Y_OVERFLOW: u8 = 1 << 7;
/// Status-byte mask for the left-button bit (bit 0).
const STATUS_LEFT_BUTTON: u8 = 1 << 0;
/// Status-byte mask for the right-button bit (bit 1).
const STATUS_RIGHT_BUTTON: u8 = 1 << 1;
/// Status-byte mask for the middle-button bit (bit 2).
const STATUS_MIDDLE_BUTTON: u8 = 1 << 2;

impl Default for Ps2MouseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Ps2MouseDecoder {
    /// Construct a decoder in 3-byte standard mode with empty framing.
    pub const fn new() -> Self {
        Self {
            wheel_mode: false,
            cursor: 0,
            bytes: [0; 4],
        }
    }

    /// Switch to the IntelliMouse 4-byte packet shape.
    ///
    /// Resets framing so a half-decoded 3-byte packet does not bleed into
    /// the new shape.
    pub fn enable_wheel_mode(&mut self) {
        self.wheel_mode = true;
        self.cursor = 0;
    }

    /// Switch back to the legacy 3-byte packet shape.
    ///
    /// Resets framing for the same reason as `enable_wheel_mode`.
    pub fn disable_wheel_mode(&mut self) {
        self.wheel_mode = false;
        self.cursor = 0;
    }

    /// Whether the decoder is currently expecting 4-byte (wheel) packets.
    pub const fn wheel_mode(&self) -> bool {
        self.wheel_mode
    }

    /// Number of bytes accumulated toward the next packet (`0..=3`).
    pub const fn pending_bytes(&self) -> u8 {
        self.cursor
    }

    /// Reset framing without dropping wheel mode; used when the AUX ring
    /// has been observed to skip bytes (overrun).
    pub fn resync(&mut self) {
        self.cursor = 0;
        self.bytes = [0; 4];
    }

    /// Feed a single byte and optionally produce an event.
    ///
    /// Returns `Some(Packet)` exactly when a packet boundary is crossed,
    /// `Some(Resync)` when a byte is dropped to recover framing, and `None`
    /// while accumulating the middle bytes of a packet.
    pub fn feed(&mut self, byte: u8) -> Option<DecoderEvent> {
        if self.cursor == 0 && (byte & STATUS_SYNC_BIT) == 0 {
            // Misaligned status byte — drop it and report the resync.
            return Some(DecoderEvent::Resync);
        }

        // Safe indexing: cursor is always 0..=3 at this point.
        let idx = self.cursor as usize;
        if idx < self.bytes.len() {
            self.bytes[idx] = byte;
        }
        self.cursor = self.cursor.saturating_add(1);

        let packet_len: u8 = if self.wheel_mode { 4 } else { 3 };
        if self.cursor < packet_len {
            return None;
        }

        let status = self.bytes[0];
        let dx_low = self.bytes[1] as i16;
        let dy_low = self.bytes[2] as i16;
        let dx = if (status & STATUS_X_SIGN) != 0 {
            dx_low - 256
        } else {
            dx_low
        };
        let dy = if (status & STATUS_Y_SIGN) != 0 {
            dy_low - 256
        } else {
            dy_low
        };
        let wheel = if self.wheel_mode {
            self.bytes[3] as i8
        } else {
            0
        };

        let packet = MousePacket {
            dx,
            dy,
            wheel,
            left: (status & STATUS_LEFT_BUTTON) != 0,
            right: (status & STATUS_RIGHT_BUTTON) != 0,
            middle: (status & STATUS_MIDDLE_BUTTON) != 0,
            x_overflow: (status & STATUS_X_OVERFLOW) != 0,
            y_overflow: (status & STATUS_Y_OVERFLOW) != 0,
        };

        self.cursor = 0;
        Some(DecoderEvent::Packet(packet))
    }
}

// ---------------------------------------------------------------------------
// Phase 56 Track D.2 — Pointer button-edge tracker (test-first stubs)
// ---------------------------------------------------------------------------
//
// These declarations exist in their failing-test-friendly form: the type
// surface compiles so the unit tests can be written against the public API
// before any behavior lands. `update` returns the empty transition set
// regardless of input, so every meaningful test in the test module fails
// at runtime. The follow-up commit replaces these stubs with the real
// state machine.

/// Three-button state snapshot (left, right, middle).
///
/// Used by [`ButtonTracker`] as both the input (state at the end of a fresh
/// `MousePacket`) and the cached previous state used to detect transitions.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ButtonState {
    /// Left mouse button currently pressed.
    pub left: bool,
    /// Right mouse button currently pressed.
    pub right: bool,
    /// Middle mouse button currently pressed.
    pub middle: bool,
}

impl ButtonState {
    /// Project a freshly decoded [`MousePacket`] onto its 3-button state.
    pub const fn from_packet(_packet: &MousePacket) -> Self {
        // Stub: real impl reads packet.{left, right, middle}.
        Self {
            left: false,
            right: false,
            middle: false,
        }
    }
}

/// Stable button index used in [`ButtonTransition`].
///
/// Indices follow the m3OS convention agreed in the Phase 56 D.2 design note:
/// `0 = left`, `1 = right`, `2 = middle`. This mirrors how PS/2 reports the
/// three buttons in its status byte and is what `display_server` (D.3)
/// expects when interpreting `PointerButton::Down(idx)` / `Up(idx)`.
pub const BUTTON_INDEX_LEFT: u8 = 0;
/// Stable button index for right mouse button. See [`BUTTON_INDEX_LEFT`].
pub const BUTTON_INDEX_RIGHT: u8 = 1;
/// Stable button index for middle mouse button. See [`BUTTON_INDEX_LEFT`].
pub const BUTTON_INDEX_MIDDLE: u8 = 2;

/// A single button-edge transition emitted by [`ButtonTracker::update`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonTransition {
    /// Button at the given index just transitioned to pressed.
    Down(u8),
    /// Button at the given index just transitioned to released.
    Up(u8),
}

/// Fixed-capacity holder for button transitions emitted by one packet.
///
/// At most three buttons can change per packet (one per left/right/middle).
/// We surface a `[Option<ButtonTransition>; 3]` array rather than allocating
/// so `mouse_server` can iterate without touching the heap on the hot path.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ButtonTransitions {
    transitions: [Option<ButtonTransition>; 3],
}

impl ButtonTransitions {
    /// Iterate over the present transitions in stable left-right-middle order.
    pub fn iter(&self) -> impl Iterator<Item = ButtonTransition> + '_ {
        self.transitions.iter().filter_map(|t| *t)
    }

    /// Number of present transitions (0..=3).
    pub fn len(&self) -> usize {
        self.transitions.iter().filter(|t| t.is_some()).count()
    }

    /// True when no transitions were emitted on this packet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Pure-logic button-edge state machine for `mouse_server`.
///
/// PS/2 packets carry the *current* button-down state, not edges. The
/// userspace mouse pipeline must compute edges itself so `display_server` and
/// downstream clients see explicit `PointerButton::Down(idx)` /
/// `PointerButton::Up(idx)` events instead of having to diff button bits
/// across packets.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ButtonTracker {
    prev: ButtonState,
}

impl ButtonTracker {
    /// Construct a tracker assuming all buttons are released.
    pub const fn new() -> Self {
        Self {
            prev: ButtonState {
                left: false,
                right: false,
                middle: false,
            },
        }
    }

    /// Snapshot of the last observed state.
    pub const fn state(&self) -> ButtonState {
        self.prev
    }

    /// Feed the freshly decoded button state and return the resulting edges.
    ///
    /// Stub returns an empty transition set; tests fail until the real
    /// state-machine impl lands in the follow-up commit.
    pub fn update(&mut self, _new_state: ButtonState) -> ButtonTransitions {
        // Intentional stub: do not mutate self.prev, do not compare states.
        // The follow-up impl commit makes the pure-logic tests green.
        ButtonTransitions::default()
    }
}

// ---------------------------------------------------------------------------
// Wire encoding for `sys_read_mouse_packet` (Phase 56 Track B.2)
// ---------------------------------------------------------------------------

/// Wire-format size of one [`MousePacket`] returned by the
/// `sys_read_mouse_packet` syscall (0x1015). 8 bytes, layout documented on
/// [`encode_packet`].
pub const MOUSE_PACKET_WIRE_SIZE: usize = 8;

/// Encode a [`MousePacket`] into the 8-byte little-endian wire layout used
/// by `sys_read_mouse_packet`.
///
/// Wire layout (8 bytes total):
/// * `[0..2]` — `dx: i16` little-endian
/// * `[2..4]` — `dy: i16` little-endian
/// * `[4..5]` — `wheel: i8`
/// * `[5..6]` — buttons bitfield: bit 0 left, bit 1 right, bit 2 middle,
///   bit 3 x_overflow, bit 4 y_overflow, bits 5..7 reserved
/// * `[6..8]` — reserved (zero)
pub fn encode_packet(packet: &MousePacket, out: &mut [u8; MOUSE_PACKET_WIRE_SIZE]) {
    out[0..2].copy_from_slice(&packet.dx.to_le_bytes());
    out[2..4].copy_from_slice(&packet.dy.to_le_bytes());
    out[4] = packet.wheel as u8;
    let mut buttons: u8 = 0;
    if packet.left {
        buttons |= 1 << 0;
    }
    if packet.right {
        buttons |= 1 << 1;
    }
    if packet.middle {
        buttons |= 1 << 2;
    }
    if packet.x_overflow {
        buttons |= 1 << 3;
    }
    if packet.y_overflow {
        buttons |= 1 << 4;
    }
    out[5] = buttons;
    out[6] = 0;
    out[7] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn feed_all(decoder: &mut Ps2MouseDecoder, bytes: &[u8]) -> alloc::vec::Vec<DecoderEvent> {
        let mut out = alloc::vec::Vec::new();
        for &b in bytes {
            if let Some(ev) = decoder.feed(b) {
                out.push(ev);
            }
        }
        out
    }

    #[test]
    fn decodes_3_byte_zero_motion_packet() {
        let mut d = Ps2MouseDecoder::new();
        assert_eq!(d.feed(0x08), None);
        assert_eq!(d.feed(0), None);
        let ev = d.feed(0).expect("packet emitted");
        match ev {
            DecoderEvent::Packet(p) => {
                assert_eq!(
                    p,
                    MousePacket {
                        dx: 0,
                        dy: 0,
                        wheel: 0,
                        left: false,
                        right: false,
                        middle: false,
                        x_overflow: false,
                        y_overflow: false,
                    }
                );
            }
            DecoderEvent::Resync => panic!("unexpected resync"),
        }
    }

    #[test]
    fn decodes_button_state() {
        let mut d = Ps2MouseDecoder::new();
        let events = feed_all(&mut d, &[0x0F, 0, 0]);
        assert_eq!(events.len(), 1);
        match events[0] {
            DecoderEvent::Packet(p) => {
                assert!(p.left);
                assert!(p.right);
                assert!(p.middle);
            }
            DecoderEvent::Resync => panic!("unexpected resync"),
        }
    }

    #[test]
    fn decodes_negative_x_delta() {
        let mut d = Ps2MouseDecoder::new();
        let events = feed_all(&mut d, &[0x18, 0xFE, 0]);
        assert_eq!(events.len(), 1);
        match events[0] {
            DecoderEvent::Packet(p) => assert_eq!(p.dx, -2),
            DecoderEvent::Resync => panic!("unexpected resync"),
        }
    }

    #[test]
    fn decodes_negative_y_delta() {
        let mut d = Ps2MouseDecoder::new();
        let events = feed_all(&mut d, &[0x28, 0, 0xFD]);
        assert_eq!(events.len(), 1);
        match events[0] {
            DecoderEvent::Packet(p) => assert_eq!(p.dy, -3),
            DecoderEvent::Resync => panic!("unexpected resync"),
        }
    }

    #[test]
    fn decodes_overflow_bits() {
        let mut d = Ps2MouseDecoder::new();
        let events = feed_all(&mut d, &[0xC8, 0, 0]);
        assert_eq!(events.len(), 1);
        match events[0] {
            DecoderEvent::Packet(p) => {
                assert!(p.x_overflow);
                assert!(p.y_overflow);
            }
            DecoderEvent::Resync => panic!("unexpected resync"),
        }
    }

    #[test]
    fn lost_sync_drops_byte_and_emits_resync_event() {
        let mut d = Ps2MouseDecoder::new();
        let ev = d.feed(0x00).expect("resync emitted");
        assert_eq!(ev, DecoderEvent::Resync);
        assert_eq!(d.pending_bytes(), 0);

        // After recovery a normal packet decodes cleanly.
        assert_eq!(d.feed(0x08), None);
        assert_eq!(d.feed(0), None);
        let pkt = d.feed(0).expect("packet emitted");
        assert!(matches!(pkt, DecoderEvent::Packet(_)));
    }

    #[test]
    fn wheel_mode_decodes_4_byte_packet_with_signed_wheel() {
        let mut d = Ps2MouseDecoder::new();
        d.enable_wheel_mode();
        assert_eq!(d.feed(0x08), None);
        assert_eq!(d.feed(5), None);
        assert_eq!(d.feed(5), None);
        let ev = d.feed(0xFF).expect("packet emitted");
        match ev {
            DecoderEvent::Packet(p) => {
                assert_eq!(p.wheel, -1);
                assert_eq!(p.dx, 5);
                assert_eq!(p.dy, 5);
            }
            DecoderEvent::Resync => panic!("unexpected resync"),
        }
    }

    #[test]
    fn wheel_mode_handles_zero_wheel() {
        let mut d = Ps2MouseDecoder::new();
        d.enable_wheel_mode();
        let events = feed_all(&mut d, &[0x08, 0, 0, 0]);
        assert_eq!(events.len(), 1);
        match events[0] {
            DecoderEvent::Packet(p) => assert_eq!(p.wheel, 0),
            DecoderEvent::Resync => panic!("unexpected resync"),
        }
    }

    #[test]
    fn resync_clears_partial_packet() {
        let mut d = Ps2MouseDecoder::new();
        assert_eq!(d.feed(0x08), None);
        assert_eq!(d.feed(0x42), None);
        assert_eq!(d.pending_bytes(), 2);
        d.resync();
        assert_eq!(d.pending_bytes(), 0);

        let events = feed_all(&mut d, &[0x08, 0, 0]);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], DecoderEvent::Packet(_)));
    }

    #[test]
    fn disable_wheel_mode_returns_to_3_byte_packets() {
        let mut d = Ps2MouseDecoder::new();
        d.enable_wheel_mode();
        d.disable_wheel_mode();
        assert!(!d.wheel_mode());
        let events = feed_all(&mut d, &[0x08, 1, 2]);
        assert_eq!(events.len(), 1);
        match events[0] {
            DecoderEvent::Packet(p) => {
                assert_eq!(p.dx, 1);
                assert_eq!(p.dy, 2);
                assert_eq!(p.wheel, 0);
            }
            DecoderEvent::Resync => panic!("unexpected resync"),
        }
    }

    #[test]
    fn cursor_starts_zero_after_completed_packet() {
        let mut d = Ps2MouseDecoder::new();
        let _ = feed_all(&mut d, &[0x08, 0, 0]);
        assert_eq!(d.pending_bytes(), 0);
    }

    proptest! {
        #[test]
        fn proptest_decoder_never_panics(stream in proptest::collection::vec(any::<u8>(), 0..1024)) {
            let mut d = Ps2MouseDecoder::new();
            for b in stream {
                let _ = d.feed(b);
            }
        }

        #[test]
        fn proptest_decoded_packet_has_status_bit_set(
            stream in proptest::collection::vec(any::<u8>(), 0..1024)
        ) {
            // Mirror the decoder's framing logic to track which byte became
            // the status byte for each emitted packet, then assert bit 3 was
            // set in that captured status byte.
            let mut d = Ps2MouseDecoder::new();
            // Randomly enable wheel mode to exercise both packet shapes.
            if stream.first().copied().unwrap_or(0) & 1 == 1 {
                d.enable_wheel_mode();
            }
            let mut pending: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
            for b in stream {
                // Predict whether this byte will form a packet.
                let was_idle = pending.is_empty();
                let dropped = was_idle && (b & STATUS_SYNC_BIT) == 0;
                if !dropped {
                    pending.push(b);
                }
                let packet_len: usize = if d.wheel_mode() { 4 } else { 3 };
                let event = d.feed(b);
                if pending.len() == packet_len {
                    // Decoder must have just emitted a Packet.
                    match event {
                        Some(DecoderEvent::Packet(_)) => {
                            prop_assert!((pending[0] & STATUS_SYNC_BIT) != 0);
                        }
                        other => {
                            prop_assert!(false, "expected Packet, got {:?}", other);
                        }
                    }
                    pending.clear();
                } else if dropped {
                    prop_assert_eq!(event, Some(DecoderEvent::Resync));
                } else {
                    prop_assert_eq!(event, None);
                }
            }
        }

        #[test]
        fn proptest_internal_state_bounded(
            stream in proptest::collection::vec(any::<u8>(), 0..4096)
        ) {
            let mut d = Ps2MouseDecoder::new();
            for b in stream {
                let _ = d.feed(b);
                prop_assert!(d.pending_bytes() <= 3);
            }
        }
    }

    #[test]
    fn encode_packet_zero_packet_round_trip() {
        let p = MousePacket::default();
        let mut buf = [0u8; MOUSE_PACKET_WIRE_SIZE];
        encode_packet(&p, &mut buf);
        assert_eq!(buf, [0u8; 8]);
    }

    #[test]
    fn encode_packet_motion_and_buttons() {
        let p = MousePacket {
            dx: -3,
            dy: 7,
            wheel: -1,
            left: true,
            right: false,
            middle: true,
            x_overflow: false,
            y_overflow: true,
        };
        let mut buf = [0u8; MOUSE_PACKET_WIRE_SIZE];
        encode_packet(&p, &mut buf);
        assert_eq!(buf[0..2], [0xFD, 0xFF]);
        assert_eq!(buf[2..4], [0x07, 0x00]);
        assert_eq!(buf[4], 0xFF);
        assert_eq!(buf[5], 0b0001_0101);
        assert_eq!(buf[6..8], [0u8, 0u8]);
    }

    // ---- Phase 56 D.2 — ButtonTracker -------------------------------------

    fn state(left: bool, right: bool, middle: bool) -> ButtonState {
        ButtonState {
            left,
            right,
            middle,
        }
    }

    #[test]
    fn button_tracker_starts_with_no_buttons_pressed() {
        let t = ButtonTracker::new();
        assert_eq!(t.state(), ButtonState::default());
    }

    #[test]
    fn button_tracker_emits_no_transitions_when_state_unchanged() {
        let mut t = ButtonTracker::new();
        let out = t.update(state(false, false, false));
        assert!(out.is_empty());
        assert_eq!(out.len(), 0);
        assert_eq!(out.iter().count(), 0);
    }

    #[test]
    fn button_tracker_left_press_emits_down_edge_with_left_index() {
        let mut t = ButtonTracker::new();
        let out = t.update(state(true, false, false));
        assert_eq!(out.len(), 1);
        let collected: alloc::vec::Vec<_> = out.iter().collect();
        assert_eq!(
            collected,
            alloc::vec![ButtonTransition::Down(BUTTON_INDEX_LEFT)]
        );
    }

    #[test]
    fn button_tracker_left_release_emits_up_edge() {
        let mut t = ButtonTracker::new();
        let _ = t.update(state(true, false, false));
        let out = t.update(state(false, false, false));
        let collected: alloc::vec::Vec<_> = out.iter().collect();
        assert_eq!(
            collected,
            alloc::vec![ButtonTransition::Up(BUTTON_INDEX_LEFT)]
        );
    }

    #[test]
    fn button_tracker_holding_emits_no_repeat_edges() {
        let mut t = ButtonTracker::new();
        let _ = t.update(state(true, false, false));
        let out_a = t.update(state(true, false, false));
        let out_b = t.update(state(true, false, false));
        assert!(out_a.is_empty());
        assert!(out_b.is_empty());
    }

    #[test]
    fn button_tracker_right_and_middle_have_distinct_indices() {
        let mut t = ButtonTracker::new();

        let out_r = t.update(state(false, true, false));
        let collected_r: alloc::vec::Vec<_> = out_r.iter().collect();
        assert_eq!(
            collected_r,
            alloc::vec![ButtonTransition::Down(BUTTON_INDEX_RIGHT)]
        );

        let out_m = t.update(state(false, true, true));
        let collected_m: alloc::vec::Vec<_> = out_m.iter().collect();
        assert_eq!(
            collected_m,
            alloc::vec![ButtonTransition::Down(BUTTON_INDEX_MIDDLE)]
        );
    }

    #[test]
    fn button_tracker_simultaneous_press_emits_in_left_right_middle_order() {
        let mut t = ButtonTracker::new();
        let out = t.update(state(true, true, true));
        let collected: alloc::vec::Vec<_> = out.iter().collect();
        assert_eq!(
            collected,
            alloc::vec![
                ButtonTransition::Down(BUTTON_INDEX_LEFT),
                ButtonTransition::Down(BUTTON_INDEX_RIGHT),
                ButtonTransition::Down(BUTTON_INDEX_MIDDLE),
            ]
        );
    }

    #[test]
    fn button_tracker_mixed_press_release_emits_correct_edges() {
        let mut t = ButtonTracker::new();
        // Start with left+middle held.
        let _ = t.update(state(true, false, true));
        // Now release left, press right; middle stays held.
        let out = t.update(state(false, true, true));
        let collected: alloc::vec::Vec<_> = out.iter().collect();
        assert_eq!(
            collected,
            alloc::vec![
                ButtonTransition::Up(BUTTON_INDEX_LEFT),
                ButtonTransition::Down(BUTTON_INDEX_RIGHT),
            ]
        );
    }

    #[test]
    fn button_tracker_state_reflects_last_observation() {
        let mut t = ButtonTracker::new();
        let _ = t.update(state(true, true, false));
        assert_eq!(t.state(), state(true, true, false));
        let _ = t.update(state(false, true, true));
        assert_eq!(t.state(), state(false, true, true));
    }

    #[test]
    fn button_state_from_packet_extracts_three_button_bits_only() {
        let p = MousePacket {
            dx: 5,
            dy: -2,
            wheel: 1,
            left: true,
            right: false,
            middle: true,
            x_overflow: true,
            y_overflow: false,
        };
        assert_eq!(ButtonState::from_packet(&p), state(true, false, true));
    }

    proptest! {
        #[test]
        fn proptest_button_tracker_produces_at_most_three_edges(
            seq in proptest::collection::vec((any::<bool>(), any::<bool>(), any::<bool>()), 0..256)
        ) {
            let mut t = ButtonTracker::new();
            for (l, r, m) in seq {
                let out = t.update(state(l, r, m));
                prop_assert!(out.len() <= 3);
                prop_assert!(out.iter().count() == out.len());
                // After update, the cached state must equal the input.
                prop_assert_eq!(t.state(), state(l, r, m));
            }
        }

        #[test]
        fn proptest_button_tracker_idempotent_under_no_change(
            initial in (any::<bool>(), any::<bool>(), any::<bool>()),
            repeats in 1usize..=10
        ) {
            let (l, r, m) = initial;
            let mut t = ButtonTracker::new();
            // Prime tracker.
            let _ = t.update(state(l, r, m));
            for _ in 0..repeats {
                let out = t.update(state(l, r, m));
                prop_assert!(out.is_empty());
            }
        }
    }
}
