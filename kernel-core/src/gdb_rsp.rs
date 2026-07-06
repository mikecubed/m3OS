//! GDB Remote Serial Protocol (RSP) wire codec — Phase 111 Track C.1.
//!
//! Pure logic, host-testable: the framing every GDB client speaks — `$<payload>#<cc>`
//! with a mod-256 checksum, `+`/`-` ack/nak, the `0x03` async-interrupt byte,
//! run-length `*` expansion on decode, and the hex helpers the `g`/`m`/`M`
//! commands use. The in-kernel stub (`kernel/src/debug/gdbstub.rs`) drives its
//! polled COM2 bytes through here so the protocol layer is validated without
//! QEMU.

/// GDB async-interrupt byte (`Ctrl-C` on the remote link).
pub const INTERRUPT: u8 = 0x03;
/// Packet-ack byte.
pub const ACK: u8 = b'+';
/// Packet-nak byte.
pub const NAK: u8 = b'-';

/// Mod-256 checksum of an RSP payload (the sum of its bytes).
pub fn checksum(payload: &[u8]) -> u8 {
    let mut sum = 0u8;
    for &b in payload {
        sum = sum.wrapping_add(b);
    }
    sum
}

/// Lower-case hex digit for a nibble (0..16).
#[inline]
fn hex_digit(n: u8) -> u8 {
    match n & 0xf {
        d @ 0..=9 => b'0' + d,
        d => b'a' + (d - 10),
    }
}

/// Parse one hex digit; `None` if not a hex char.
#[inline]
pub fn parse_hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Frame `payload` as a full `$<payload>#<cc>` packet into `out`, returning the
/// byte length. `out` must hold `payload.len() + 4` bytes.
pub fn encode_packet(payload: &[u8], out: &mut [u8]) -> Option<usize> {
    let need = payload.len() + 4;
    if out.len() < need {
        return None;
    }
    out[0] = b'$';
    out[1..1 + payload.len()].copy_from_slice(payload);
    let cs = checksum(payload);
    out[1 + payload.len()] = b'#';
    out[2 + payload.len()] = hex_digit(cs >> 4);
    out[3 + payload.len()] = hex_digit(cs);
    Some(need)
}

/// Hex-encode `bytes` (two lower-case digits each) into `out`. Returns the
/// number of hex chars written, or `None` if `out` is too small.
pub fn hex_encode(bytes: &[u8], out: &mut [u8]) -> Option<usize> {
    if out.len() < bytes.len() * 2 {
        return None;
    }
    for (i, &b) in bytes.iter().enumerate() {
        out[i * 2] = hex_digit(b >> 4);
        out[i * 2 + 1] = hex_digit(b);
    }
    Some(bytes.len() * 2)
}

/// Hex-decode `hex` into `out`. Returns the number of bytes written, or `None`
/// on odd length / non-hex / undersized output.
pub fn hex_decode(hex: &[u8], out: &mut [u8]) -> Option<usize> {
    if !hex.len().is_multiple_of(2) || out.len() < hex.len() / 2 {
        return None;
    }
    for i in 0..hex.len() / 2 {
        let hi = parse_hex_digit(hex[2 * i])?;
        let lo = parse_hex_digit(hex[2 * i + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(hex.len() / 2)
}

/// Parse a big-endian hex integer (as GDB writes addresses/lengths). Stops at
/// the first non-hex char; returns `(value, consumed)`.
pub fn parse_hex_prefix(s: &[u8]) -> (u64, usize) {
    let mut v = 0u64;
    let mut n = 0;
    for &c in s {
        match parse_hex_digit(c) {
            Some(d) => {
                v = (v << 4) | d as u64;
                n += 1;
            }
            None => break,
        }
    }
    (v, n)
}

/// Incremental RSP packet reader. Fed raw bytes from the transport, it yields
/// one [`RspEvent`] per meaningful unit: a validated packet payload, an
/// ack/nak, or the `0x03` interrupt. Run-length `*` sequences are expanded
/// inside the payload.
#[derive(Debug)]
pub struct PacketReader {
    buf: [u8; MAX_PACKET],
    len: usize,
    state: State,
    cksum_hi: Option<u8>,
    /// Running checksum of the **raw** body bytes as transmitted (the RSP
    /// checksum covers the on-wire bytes, including `*` + RLE count — not the
    /// expanded payload).
    raw_sum: u8,
    /// Set after a `*` in the body; the next byte is the RLE repeat count.
    rle: bool,
}

/// Max decoded packet payload the stub accepts (generous for a `G`/`M` write of
/// the full register set + a memory block).
pub const MAX_PACKET: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
enum State {
    Idle,
    Body,
    Checksum,
}

/// One event surfaced by [`PacketReader::feed`].
#[derive(Debug, PartialEq, Eq)]
pub enum RspEvent {
    /// A framed packet whose checksum validated. `len` bytes of the reader's
    /// payload buffer are valid (retrieve with [`PacketReader::payload`]).
    Packet(usize),
    /// A packet arrived but its checksum was wrong (the caller should NAK).
    BadChecksum,
    /// Peer acked (`+`).
    Ack,
    /// Peer nak'd (`-`).
    Nak,
    /// Async interrupt (`0x03`).
    Interrupt,
}

impl Default for PacketReader {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketReader {
    pub const fn new() -> Self {
        PacketReader {
            buf: [0u8; MAX_PACKET],
            len: 0,
            state: State::Idle,
            cksum_hi: None,
            raw_sum: 0,
            rle: false,
        }
    }

    /// The decoded payload of the most recently completed packet.
    pub fn payload(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Feed one byte. Returns `Some(event)` when a unit completes.
    pub fn feed(&mut self, byte: u8) -> Option<RspEvent> {
        match self.state {
            State::Idle => match byte {
                b'$' => {
                    self.len = 0;
                    self.raw_sum = 0;
                    self.rle = false;
                    self.state = State::Body;
                    None
                }
                ACK => Some(RspEvent::Ack),
                NAK => Some(RspEvent::Nak),
                INTERRUPT => Some(RspEvent::Interrupt),
                _ => None, // ignore stray bytes between packets
            },
            State::Body => {
                if byte == b'#' {
                    self.state = State::Checksum;
                    self.cksum_hi = None;
                    None
                } else {
                    self.raw_sum = self.raw_sum.wrapping_add(byte);
                    self.push_body(byte);
                    None
                }
            }
            State::Checksum => {
                let Some(d) = parse_hex_digit(byte) else {
                    self.state = State::Idle;
                    return Some(RspEvent::BadChecksum);
                };
                match self.cksum_hi.take() {
                    None => {
                        self.cksum_hi = Some(d);
                        None
                    }
                    Some(hi) => {
                        self.state = State::Idle;
                        let want = (hi << 4) | d;
                        if want == self.raw_sum {
                            Some(RspEvent::Packet(self.len))
                        } else {
                            Some(RspEvent::BadChecksum)
                        }
                    }
                }
            }
        }
    }

    /// Append a body byte, expanding a run-length `*<count>` sequence. RSP RLE:
    /// the byte before `*` is repeated `(count_char - 29)` **additional** times.
    fn push_body(&mut self, byte: u8) {
        if self.rle {
            // `byte` is the repeat count following a `*`.
            self.rle = false;
            let repeat = byte.wrapping_sub(29);
            let last = if self.len > 0 {
                self.buf[self.len - 1]
            } else {
                0
            };
            for _ in 0..repeat {
                if self.len < MAX_PACKET {
                    self.buf[self.len] = last;
                    self.len += 1;
                }
            }
            return;
        }
        if byte == b'*' {
            self.rle = true;
            return;
        }
        if self.len < MAX_PACKET {
            self.buf[self.len] = byte;
            self.len += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(r: &mut PacketReader, bytes: &[u8]) -> Vec<RspEvent> {
        let mut ev = Vec::new();
        for &b in bytes {
            if let Some(e) = r.feed(b) {
                ev.push(e);
            }
        }
        ev
    }

    #[test]
    fn checksum_matches_gdb() {
        // GDB's canonical example: checksum of "?" is 0x3f.
        assert_eq!(checksum(b"?"), 0x3f);
        // "vCont?" checksum.
        assert_eq!(checksum(b"OK"), (b'O' as u16 + b'K' as u16) as u8);
    }

    #[test]
    fn encode_frames_with_checksum() {
        let mut out = [0u8; 16];
        let n = encode_packet(b"OK", &mut out).unwrap();
        assert_eq!(&out[..n], b"$OK#9a");
        let n = encode_packet(b"?", &mut out).unwrap();
        assert_eq!(&out[..n], b"$?#3f");
    }

    #[test]
    fn encode_rejects_small_buffer() {
        let mut out = [0u8; 3];
        assert!(encode_packet(b"OK", &mut out).is_none());
    }

    #[test]
    fn decode_valid_packet() {
        let mut r = PacketReader::new();
        let ev = feed_all(&mut r, b"$g#67");
        assert_eq!(ev, vec![RspEvent::Packet(1)]);
        assert_eq!(r.payload(), b"g");
    }

    #[test]
    fn decode_ack_nak_interrupt() {
        let mut r = PacketReader::new();
        assert_eq!(
            feed_all(&mut r, &[ACK, NAK, INTERRUPT]),
            vec![RspEvent::Ack, RspEvent::Nak, RspEvent::Interrupt]
        );
    }

    #[test]
    fn decode_bad_checksum() {
        let mut r = PacketReader::new();
        // "$g#00" — wrong checksum (g is 0x67).
        assert_eq!(feed_all(&mut r, b"$g#00"), vec![RspEvent::BadChecksum]);
    }

    #[test]
    fn decode_multi_char_and_ack_prefix() {
        // A stray leading ack followed by a real packet — the reader ignores the
        // ack's framing and still decodes the packet payload.
        let mut r = PacketReader::new();
        let payload = b"m1a0,4";
        let mut pkt = [0u8; 16];
        let n = encode_packet(payload, &mut pkt).unwrap();
        let mut with_ack = vec![ACK];
        with_ack.extend_from_slice(&pkt[..n]);
        let ev = feed_all(&mut r, &with_ack);
        assert_eq!(ev, vec![RspEvent::Ack, RspEvent::Packet(payload.len())]);
        assert_eq!(r.payload(), payload);
    }

    #[test]
    fn hex_roundtrips() {
        let data = [0x00u8, 0x67, 0xff, 0x10, 0xab];
        let mut hex = [0u8; 10];
        let n = hex_encode(&data, &mut hex).unwrap();
        assert_eq!(&hex[..n], b"0067ff10ab");
        let mut back = [0u8; 5];
        let m = hex_decode(&hex[..n], &mut back).unwrap();
        assert_eq!(&back[..m], &data);
    }

    #[test]
    fn hex_decode_rejects_odd_and_nonhex() {
        let mut out = [0u8; 4];
        assert!(hex_decode(b"abc", &mut out).is_none()); // odd
        assert!(hex_decode(b"xy", &mut out).is_none()); // non-hex
    }

    #[test]
    fn parse_hex_prefix_stops_at_delimiter() {
        assert_eq!(parse_hex_prefix(b"1a0,4"), (0x1a0, 3));
        assert_eq!(parse_hex_prefix(b"deadbeef"), (0xdeadbeef, 8));
        assert_eq!(parse_hex_prefix(b",4"), (0, 0));
    }

    #[test]
    fn rle_expands_on_decode() {
        // RSP RLE: 'a' followed by '*' then count char. "0* " => '0' repeated
        // (0x20 - 29 = 3) additional times = "0000". Build a packet with that
        // body and a correct checksum.
        let body = b"0* "; // ' ' = 0x20 → repeat 3 more
        let mut pkt = [0u8; 16];
        let n = encode_packet(body, &mut pkt).unwrap();
        let mut r = PacketReader::new();
        let ev = feed_all(&mut r, &pkt[..n]);
        // The decoded payload expands to "0000".
        assert_eq!(ev, vec![RspEvent::Packet(4)]);
        assert_eq!(r.payload(), b"0000");
    }
}
