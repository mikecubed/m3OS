//! TTY data structures matching the Linux x86_64 ABI.
//!
//! These types live in kernel-core so they can be unit-tested on the host
//! (`cargo test -p kernel-core`) without needing QEMU.

/// Number of control characters in a `Termios` struct.
pub const NCCS: usize = 19;

// ---------------------------------------------------------------------------
// Termios struct — Linux x86_64 layout (60 bytes)
// ---------------------------------------------------------------------------

/// Terminal I/O settings, binary-compatible with Linux `struct termios`.
///
/// Layout: c_iflag(4) + c_oflag(4) + c_cflag(4) + c_lflag(4) + c_line(1)
///         + c_cc(19) + padding(24 implicit from repr(C)) = 60 bytes total.
///
/// We use `repr(C)` so field order and alignment match the C ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    /// Input mode flags.
    pub c_iflag: u32,
    /// Output mode flags.
    pub c_oflag: u32,
    /// Control mode flags.
    pub c_cflag: u32,
    /// Local mode flags.
    pub c_lflag: u32,
    /// Line discipline (unused, always 0).
    pub c_line: u8,
    /// Control characters.
    pub c_cc: [u8; NCCS],
}

// Linux `struct termios` on x86_64 is exactly 60 bytes:
//   4 (c_iflag) + 4 (c_oflag) + 4 (c_cflag) + 4 (c_lflag) + 1 (c_line)
//   + 19 (c_cc) + 0 padding before next field ... but there is no next
//   field, and the overall struct size is rounded to alignment of the
//   largest member (u32 = 4 bytes), so 4+4+4+4+1+19 = 36, rounded up
//   to 36 (already aligned).  However, the Linux kernel header yields 60
//   bytes because the *kernel* `struct termios` includes c_ispeed and
//   c_ospeed (each u32) plus padding.  musl's `struct termios` is 60
//   bytes: it stores `__c_cc` as `cc_t[32]` (32 bytes) giving
//   4+4+4+4+1+pad(3)+32+4+4 = 60.
//
// We match musl's layout for binary compatibility:
//   The actual on-the-wire format that musl passes through ioctl(TCGETS)
//   is the *kernel* `struct termios` which is 36 bytes on x86_64.
//   But musl's TCGETS ioctl number is 0x5401 which maps to the 36-byte
//   kernel struct.  So we use 36 bytes for the ioctl copy.
//
// After testing, we'll assert the correct size.  For now, keep it simple:
// the ioctl handlers copy exactly `TERMIOS_SIZE` bytes.

/// Size of the termios struct as seen by the kernel ioctl interface.
/// On Linux x86_64, `ioctl(fd, TCGETS, &t)` copies 36 bytes.
pub const TERMIOS_SIZE: usize = 36;

// ---------------------------------------------------------------------------
// Winsize struct — Linux layout (8 bytes)
// ---------------------------------------------------------------------------

/// Terminal window size, binary-compatible with Linux `struct winsize`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Size of the winsize struct for ioctl copy.
pub const WINSIZE_SIZE: usize = 8;

// ---------------------------------------------------------------------------
// c_lflag constants
// ---------------------------------------------------------------------------

pub const ISIG: u32 = 0o000001;
pub const ICANON: u32 = 0o000002;
pub const ECHO: u32 = 0o000010;
pub const ECHOE: u32 = 0o000020;
pub const ECHOK: u32 = 0o000040;
pub const ECHONL: u32 = 0o000100;
pub const NOFLSH: u32 = 0o000200;
pub const TOSTOP: u32 = 0o000400;
pub const IEXTEN: u32 = 0o100000;

// ---------------------------------------------------------------------------
// c_iflag constants (Linux numeric values — match musl shims)
// ---------------------------------------------------------------------------

pub const IGNBRK: u32 = 0o000001;
pub const BRKINT: u32 = 0o000002;
pub const IGNPAR: u32 = 0o000004;
pub const PARMRK: u32 = 0o000010;
pub const INPCK: u32 = 0o000020;
pub const ISTRIP: u32 = 0o000040;
pub const INLCR: u32 = 0o000100;
pub const IGNCR: u32 = 0o000200;
pub const ICRNL: u32 = 0o000400;
pub const IUCLC: u32 = 0o001000;
pub const IXON: u32 = 0o002000;
pub const IXANY: u32 = 0o004000;
pub const IXOFF: u32 = 0o010000;
pub const IMAXBEL: u32 = 0o020000;
pub const IUTF8: u32 = 0o040000;

// ---------------------------------------------------------------------------
// c_oflag constants
// ---------------------------------------------------------------------------

pub const OPOST: u32 = 0o000001;
pub const ONLCR: u32 = 0o000004;

// ---------------------------------------------------------------------------
// c_cflag constants (minimal set for defaults)
// ---------------------------------------------------------------------------

/// Baud rate mask — not meaningful for virtual consoles but needed for
/// a valid default.
pub const B38400: u32 = 0o000017;
/// Character size mask.
pub const CS8: u32 = 0o000060;
/// Enable receiver.
pub const CREAD: u32 = 0o000200;
/// Hang up on last close.
pub const HUPCL: u32 = 0o002000;

// ---------------------------------------------------------------------------
// c_cc index constants (Linux x86_64 values)
// ---------------------------------------------------------------------------

pub const VINTR: usize = 0;
pub const VQUIT: usize = 1;
pub const VERASE: usize = 2;
pub const VKILL: usize = 3;
pub const VEOF: usize = 4;
pub const VTIME: usize = 5;
pub const VMIN: usize = 6;
pub const VSWTC: usize = 7;
pub const VSTART: usize = 8;
pub const VSTOP: usize = 9;
pub const VSUSP: usize = 10;
pub const VEOL: usize = 11;
pub const VREPRINT: usize = 12;
pub const VDISCARD: usize = 13;
pub const VWERASE: usize = 14;
pub const VLNEXT: usize = 15;
pub const VEOL2: usize = 16;

// ---------------------------------------------------------------------------
// Default termios constructor
// ---------------------------------------------------------------------------

impl Termios {
    /// Create a termios with sensible cooked-mode defaults matching Linux.
    ///
    /// Baseline: `ICRNL|IXON | OPOST|ONLCR | ICANON|ECHO|ECHOE|ECHOK|ISIG|IEXTEN`
    /// with the standard control characters.
    pub const fn cooked_default() -> Self {
        let mut c_cc = [0u8; NCCS];
        c_cc[VINTR] = 0x03; // ^C
        c_cc[VQUIT] = 0x1C; // ^\
        c_cc[VERASE] = 0x7F; // DEL
        c_cc[VKILL] = 0x15; // ^U
        c_cc[VEOF] = 0x04; // ^D
        c_cc[VTIME] = 0;
        c_cc[VMIN] = 1;
        c_cc[VSTART] = 0x11; // ^Q (XON)
        c_cc[VSTOP] = 0x13; // ^S (XOFF)
        c_cc[VSUSP] = 0x1A; // ^Z
        c_cc[VEOL] = 0;
        c_cc[VREPRINT] = 0x12; // ^R
        c_cc[VDISCARD] = 0x0F; // ^O
        c_cc[VWERASE] = 0x17; // ^W
        c_cc[VLNEXT] = 0x16; // ^V
        c_cc[VEOL2] = 0;

        Termios {
            c_iflag: ICRNL | IXON,
            c_oflag: OPOST | ONLCR,
            c_cflag: B38400 | CS8 | CREAD | HUPCL,
            c_lflag: ICANON | ECHO | ECHOE | ECHOK | ISIG | IEXTEN,
            c_line: 0,
            c_cc,
        }
    }

    /// Backwards-compatible alias for [`cooked_default`].
    pub const fn default_cooked() -> Self {
        Self::cooked_default()
    }

    /// Create a termios in raw mode, matching the POSIX `cfmakeraw` clearset.
    ///
    /// Clears the four `c_iflag` mapping bits + BRKINT/PARMRK/IGNBRK/ISTRIP,
    /// clears `OPOST`, clears the local-mode line editor / signal / extended
    /// processing bits, sets character size to `CS8`, and sets `VMIN = 1` /
    /// `VTIME = 0` for blocking byte-by-byte reads.
    pub const fn raw_default() -> Self {
        let mut t = Self::cooked_default();
        t.c_iflag &= !(IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL | IXON);
        t.c_oflag &= !OPOST;
        t.c_lflag &= !(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
        t.c_cflag = (t.c_cflag & !0o000060) | CS8;
        t.c_cc[VMIN] = 1;
        t.c_cc[VTIME] = 0;
        t
    }

    /// Returns true if ICANON is set (cooked / canonical mode).
    pub const fn is_canonical(&self) -> bool {
        self.c_lflag & ICANON != 0
    }

    /// Returns true if ECHO is set.
    pub const fn is_echo(&self) -> bool {
        self.c_lflag & ECHO != 0
    }

    /// Returns true if ISIG is set (signal characters enabled).
    pub const fn is_isig(&self) -> bool {
        self.c_lflag & ISIG != 0
    }
}

impl Winsize {
    /// Default 24x80 terminal.
    pub const fn default_console() -> Self {
        Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Line discipline — pure logic (no I/O, testable on host)
// ---------------------------------------------------------------------------

/// Edit buffer for canonical mode line editing.
pub struct EditBuffer {
    pub buf: [u8; 4096],
    pub len: usize,
}

impl Default for EditBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl EditBuffer {
    pub const fn new() -> Self {
        EditBuffer {
            buf: [0u8; 4096],
            len: 0,
        }
    }

    /// Push a byte into the edit buffer. Returns false if full.
    pub fn push(&mut self, b: u8) -> bool {
        if self.len < self.buf.len() {
            self.buf[self.len] = b;
            self.len += 1;
            true
        } else {
            false
        }
    }

    /// Erase the last byte from the buffer. Returns the erased byte
    /// or None when the buffer is empty.
    pub fn erase_char(&mut self) -> Option<u8> {
        if self.len > 0 {
            self.len -= 1;
            Some(self.buf[self.len])
        } else {
            None
        }
    }

    /// Phase 69b Track G — IUTF8-aware erase. When `iutf8` is set, the
    /// erase removes the whole most-recent codepoint *iff* the buffer
    /// tail is strictly well-formed UTF-8. With `iutf8` cleared, the
    /// behaviour matches the legacy [`EditBuffer::erase_char`] — one
    /// byte per call.
    ///
    /// Returns the number of bytes removed (0 when the buffer is
    /// empty). The kernel-core ldisc only echoes a single `\x08 \x08`
    /// sequence per VERASE today; the wider semantics here let the
    /// caller decide whether to echo more (e.g. a future double-width
    /// codepoint deserving two erase echoes).
    ///
    /// Validation flow: count trailing continuation bytes (capped at
    /// 4 — the legal UTF-8 maximum), then re-decode the candidate
    /// suffix through [`crate::utf8::Utf8Decoder`]. The suffix is
    /// removed as one codepoint only when the decoder reaches
    /// [`DecoderOutput::Codepoint`] exactly on the final byte;
    /// anything else (overlong encoding, UTF-16 surrogate, codepoint
    /// above U+10FFFF, stray continuation, mismatched length) falls
    /// back to removing the single trailing byte. This matches the
    /// Linux ldisc behaviour for the same edge cases and prevents
    /// the erase from consuming a preceding valid codepoint when its
    /// successor is malformed.
    pub fn erase_one_codepoint(&mut self, iutf8: bool) -> usize {
        if self.len == 0 {
            return 0;
        }
        if !iutf8 {
            self.len -= 1;
            return 1;
        }
        // Count trailing continuation bytes (`10xxxxxx`) up to the
        // legal UTF-8 maximum so a long malformed run cannot trick the
        // scan into walking arbitrarily far.
        const MAX_UTF8_LEN: usize = 4;
        let mut cont_count: usize = 0;
        while cont_count < self.len
            && cont_count < MAX_UTF8_LEN
            && self.buf[self.len - 1 - cont_count] & 0xC0 == 0x80
        {
            cont_count += 1;
        }
        // If the buffer is shorter than (cont_count + 1) bytes, the
        // continuations have no preceding leader — malformed; remove
        // exactly one byte.
        if cont_count >= self.len {
            self.len -= 1;
            return 1;
        }
        // Strict UTF-8 validation: feed the candidate suffix through
        // the decoder. Only remove the suffix as a whole codepoint if
        // every non-final byte was `Pending` and the final byte
        // produced `Codepoint(_)`. Overlong encodings, surrogates,
        // and out-of-range codepoints all fail this check at the
        // appropriate byte.
        let suffix_len = cont_count + 1;
        let suffix_start = self.len - suffix_len;
        let mut decoder = crate::utf8::Utf8Decoder::new();
        let mut suffix_well_formed = true;
        for (i, &b) in self.buf[suffix_start..self.len].iter().enumerate() {
            let out = decoder.decode_byte(b);
            let is_last = i + 1 == suffix_len;
            let ok = matches!(
                (out, is_last),
                (crate::utf8::DecoderOutput::Pending, false)
                    | (crate::utf8::DecoderOutput::Codepoint(_), true)
            );
            if !ok {
                suffix_well_formed = false;
                break;
            }
        }
        if suffix_well_formed {
            self.len -= suffix_len;
            suffix_len
        } else {
            self.len -= 1;
            1
        }
    }

    /// Kill (erase) the entire line. Returns the number of characters erased.
    pub fn kill_line(&mut self) -> usize {
        let n = self.len;
        self.len = 0;
        n
    }

    /// Word erase: erase back to previous whitespace boundary.
    /// Returns the number of characters erased.
    pub fn word_erase(&mut self) -> usize {
        let orig = self.len;
        // Skip trailing spaces.
        while self.len > 0 && self.buf[self.len - 1] == b' ' {
            self.len -= 1;
        }
        // Erase non-space characters.
        while self.len > 0 && self.buf[self.len - 1] != b' ' {
            self.len -= 1;
        }
        orig - self.len
    }

    /// Get the current contents as a slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Remove the first `n` bytes from the buffer, shifting remaining bytes.
    pub fn drain(&mut self, n: usize) {
        let n = n.min(self.len);
        if n < self.len {
            self.buf.copy_within(n..self.len, 0);
        }
        self.len -= n;
    }
}

// ---------------------------------------------------------------------------
// SmallVec — fixed-capacity byte buffer (no alloc)
// ---------------------------------------------------------------------------

/// A small fixed-capacity byte buffer for echo output.
/// Avoids heap allocation in the line discipline hot path.
pub struct SmallVec {
    buf: [u8; 8],
    len: u8,
}

impl Default for SmallVec {
    fn default() -> Self {
        Self::new()
    }
}

impl SmallVec {
    /// Create an empty SmallVec.
    pub const fn new() -> Self {
        SmallVec {
            buf: [0u8; 8],
            len: 0,
        }
    }

    /// Push a byte. Silently drops if full.
    pub fn push(&mut self, b: u8) {
        if (self.len as usize) < self.buf.len() {
            self.buf[self.len as usize] = b;
            self.len += 1;
        }
    }

    /// Current contents as a byte slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }

    /// Number of bytes stored.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns true if empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

// ---------------------------------------------------------------------------
// LdiscResult — outcome of processing one byte
// ---------------------------------------------------------------------------

/// Result of processing one byte through the line discipline.
pub enum LdiscResult {
    /// Byte consumed internally (e.g., IGNCR, edit buffer operation).
    Consumed,
    /// Signal should be generated (SIGINT=2, SIGTSTP=20, SIGQUIT=3).
    Signal(u8),
    /// Byte(s) pushed to stdin buffer. The `echo` field contains bytes to echo.
    Pushed { echo: SmallVec },
    /// Line completed (newline/EOF). Data has been pushed. Echo bytes provided.
    LineComplete { echo: SmallVec },
}

// ---------------------------------------------------------------------------
// Escape sequence parser state (canonical mode only)
// ---------------------------------------------------------------------------

/// Tracks multi-byte VT100 escape sequences so the line discipline can
/// silently discard them in canonical mode.  In raw/non-canonical mode
/// the state machine is bypassed entirely.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EscState {
    /// Normal input processing.
    Normal,
    /// Received ESC (0x1b); waiting for `[` (CSI) or `O` (SS3).
    Esc,
    /// Inside a CSI (`ESC [`) or SS3 (`ESC O`) sequence; consuming
    /// parameter/intermediate bytes until a final byte (0x40..=0x7E).
    Csi,
}

// ---------------------------------------------------------------------------
// LineDiscipline — unified line editing + termios processing
// ---------------------------------------------------------------------------

/// Unified line discipline that owns termios state and edit buffer.
///
/// All input processing (iflag transforms, signal generation, canonical
/// editing, echo generation) lives here so it can be unit-tested on the
/// host without QEMU.
pub struct LineDiscipline {
    pub termios: Termios,
    pub edit_buf: EditBuffer,
    /// Escape sequence parser state for canonical-mode filtering.
    esc_state: EscState,
    /// IXON: output is suspended after a VSTOP byte until VSTART arrives.
    /// Pure flag; the kernel side decides what "output suspended" means.
    pub output_suspended: bool,
    /// IEXTEN/VLNEXT: the next byte is delivered literally.
    lnext_pending: bool,
    /// VMIN/VTIME tracking — number of raw-mode bytes buffered since the
    /// reader last drained, plus the absolute deadline tick (1 ms / tick)
    /// at which a VTIME timer expires.  `deadline_ticks == None` means the
    /// timer is not armed.
    pub raw_buffered: usize,
    pub deadline_ticks: Option<u64>,
}

impl Default for LineDiscipline {
    fn default() -> Self {
        Self::new()
    }
}

impl LineDiscipline {
    /// Create a new LineDiscipline with default cooked-mode termios.
    pub const fn new() -> Self {
        LineDiscipline {
            termios: Termios::cooked_default(),
            edit_buf: EditBuffer::new(),
            esc_state: EscState::Normal,
            output_suspended: false,
            lnext_pending: false,
            raw_buffered: 0,
            deadline_ticks: None,
        }
    }

    /// Tell the discipline how many raw-mode bytes are currently buffered.
    /// Call from the kernel side after a successful read drains the ring.
    pub fn set_raw_buffered(&mut self, n: usize) {
        self.raw_buffered = n;
        if n == 0 {
            self.deadline_ticks = None;
        }
    }

    /// Arm the VTIME timer using `now_ticks` as the current monotonic tick.
    /// Used by the read path when it parks for VMIN/VTIME timing.
    pub fn arm_vtime_timer(&mut self, now_ticks: u64) {
        let vtime = self.termios.c_cc[VTIME] as u64;
        if vtime > 0 && self.deadline_ticks.is_none() {
            // VTIME unit is 0.1 s; ticks are 1 ms apart (TICKS_PER_SEC=1000).
            self.deadline_ticks = Some(now_ticks.saturating_add(vtime * 100));
        }
    }

    /// Returns true when a non-canonical read should complete *now*.
    ///
    /// The four POSIX cases:
    /// * `VMIN > 0, VTIME == 0` — block until ≥ VMIN bytes buffered.
    /// * `VMIN == 0, VTIME > 0` — return immediately if any data buffered;
    ///   otherwise wait until the deadline.
    /// * `VMIN > 0, VTIME > 0` — inter-byte timer; once any byte arrives,
    ///   the timer arms and the read completes when VMIN reached or the
    ///   deadline elapses.
    /// * `VMIN == 0, VTIME == 0` — poll: always ready (read returns whatever
    ///   is available, including zero bytes).
    pub fn poll_read_ready(&self, now_ticks: u64) -> bool {
        if self.termios.is_canonical() {
            // Canonical mode: ready when a complete line is buffered. The
            // caller already checks the edit buffer for `\n`; this branch
            // just provides a sane default.
            return self.edit_buf.as_slice().contains(&b'\n');
        }
        let vmin = self.termios.c_cc[VMIN] as usize;
        let vtime = self.termios.c_cc[VTIME] as usize;
        match (vmin, vtime) {
            (0, 0) => true,
            (0, _) => {
                if self.raw_buffered > 0 {
                    return true;
                }
                self.deadline_ticks.map(|d| now_ticks >= d).unwrap_or(false)
            }
            (n, 0) => self.raw_buffered >= n,
            (n, _) => {
                if self.raw_buffered >= n {
                    return true;
                }
                if self.raw_buffered == 0 {
                    // Inter-byte timer not armed yet.
                    return false;
                }
                self.deadline_ticks.map(|d| now_ticks >= d).unwrap_or(false)
            }
        }
    }

    /// Process one input byte through the line discipline.
    ///
    /// `push_fn` is called for each byte or slice that should be delivered
    /// to the stdin buffer. This callback decouples the discipline from
    /// kernel internals, making it testable on the host.
    ///
    /// Returns an `LdiscResult` indicating what the caller should do
    /// (deliver signal, echo bytes, etc.).
    pub fn process_byte(&mut self, byte: u8, push_fn: &mut dyn FnMut(&[u8])) -> LdiscResult {
        let c_lflag = self.termios.c_lflag;
        let c_iflag = self.termios.c_iflag;
        let c_oflag = self.termios.c_oflag;
        let c_cc = self.termios.c_cc;
        let canonical = c_lflag & ICANON != 0;
        let echo_on = c_lflag & ECHO != 0;
        let isig = c_lflag & ISIG != 0;
        let iexten = c_lflag & IEXTEN != 0;
        let ixon = c_iflag & IXON != 0;

        // VLNEXT (literal next): when IEXTEN is on and the previous byte
        // was VLNEXT, deliver this byte verbatim — no signal, no editing,
        // no IXON suspension.
        if self.lnext_pending {
            self.lnext_pending = false;
            push_fn(&[byte]);
            let mut echo = SmallVec::new();
            if echo_on {
                echo.push(byte);
            }
            return LdiscResult::Pushed { echo };
        }

        // 1. Apply iflag transforms (ICRNL, INLCR, IGNCR).
        let byte = if (c_iflag & INLCR != 0) && byte == b'\n' {
            b'\r'
        } else {
            byte
        };
        let byte = if (c_iflag & IGNCR != 0) && byte == b'\r' {
            return LdiscResult::Consumed;
        } else if (c_iflag & ICRNL != 0) && byte == b'\r' {
            b'\n'
        } else {
            byte
        };

        // 1a. IXON: VSTOP suspends output, VSTART resumes it.  In both cases
        // the byte is consumed (never delivered to the reader).
        if ixon {
            if byte == c_cc[VSTOP] {
                self.output_suspended = true;
                return LdiscResult::Consumed;
            }
            if byte == c_cc[VSTART] {
                self.output_suspended = false;
                return LdiscResult::Consumed;
            }
        }

        // 1b. IEXTEN: VLNEXT arms literal-next; VDISCARD toggles output
        //     discard.  Both bytes are consumed.
        if iexten && c_cc[VLNEXT] != 0 && byte == c_cc[VLNEXT] {
            self.lnext_pending = true;
            return LdiscResult::Consumed;
        }
        if iexten && c_cc[VDISCARD] != 0 && byte == c_cc[VDISCARD] {
            // Toggle output-discard; semantics are intentionally minimal —
            // the kernel side may inspect `output_suspended` if it wants
            // to honour VDISCARD as well.
            return LdiscResult::Consumed;
        }

        // 2. Check ISIG (signal generation from c_cc).
        if isig {
            let signal = if byte == c_cc[VINTR] {
                Some(2u8) // SIGINT
            } else if byte == c_cc[VSUSP] {
                Some(20u8) // SIGTSTP
            } else if byte == c_cc[VQUIT] {
                Some(3u8) // SIGQUIT
            } else {
                None
            };

            if let Some(sig) = signal {
                // Clear edit buffer in canonical mode before signal.
                if canonical {
                    self.edit_buf.clear();
                }
                return LdiscResult::Signal(sig);
            }
        }

        // 3. Canonical mode: discard VT100 escape sequences.
        //
        // When input arrives byte-by-byte (as from stdin_feeder), multi-byte
        // escape sequences (e.g. ESC [ A for Up Arrow) must not be buffered
        // as literal characters — that would corrupt canonical-mode input
        // for programs like login.  A small state machine silently consumes
        // complete CSI (ESC [) and SS3 (ESC O) sequences.  In raw mode the
        // state machine is never entered, preserving pass-through semantics.
        if canonical {
            match self.esc_state {
                EscState::Normal => {
                    if byte == 0x1b {
                        self.esc_state = EscState::Esc;
                        return LdiscResult::Consumed;
                    }
                    // Fall through to regular canonical processing.
                }
                EscState::Esc => {
                    if byte == b'[' || byte == b'O' {
                        self.esc_state = EscState::Csi;
                        return LdiscResult::Consumed;
                    }
                    // Not a recognised escape introducer.  The ESC was already
                    // consumed; let this byte receive normal processing.
                    self.esc_state = EscState::Normal;
                }
                EscState::Csi => {
                    // Parameter (0x30..=0x3F) and intermediate (0x20..=0x2F)
                    // bytes: stay inside the sequence.
                    if (0x20..=0x3F).contains(&byte) {
                        return LdiscResult::Consumed;
                    }
                    // Final byte (0x40..=0x7E): sequence complete.
                    if (0x40..=0x7E).contains(&byte) {
                        self.esc_state = EscState::Normal;
                        return LdiscResult::Consumed;
                    }
                    // Unexpected byte — abort sequence, process normally.
                    self.esc_state = EscState::Normal;
                }
            }
        }

        // 4. ICANON: canonical mode editing.
        if canonical {
            // VERASE (backspace/DEL/0x08)
            if byte == c_cc[VERASE] || byte == 0x7F || byte == 0x08 {
                // Phase 69b Track G — IUTF8-aware erase. When the
                // termios `IUTF8` bit is set, VERASE removes the
                // continuation bytes for the most recent codepoint
                // plus its leading byte (one whole codepoint per
                // press); when cleared, behaviour matches Phase 57
                // (one byte per press). A single backspace echo is
                // emitted per call: codepoint width on the visible
                // grid is a Phase 69b Track F concern handled by the
                // screen state machine, not the line discipline.
                let iutf8 = self.termios.c_iflag & IUTF8 != 0;
                let erased = self.edit_buf.erase_one_codepoint(iutf8);
                if erased > 0 && echo_on && (c_lflag & ECHOE != 0) {
                    let mut echo = SmallVec::new();
                    echo.push(0x08); // BS
                    echo.push(b' ');
                    echo.push(0x08); // BS
                    return LdiscResult::Pushed { echo };
                }
                return LdiscResult::Consumed;
            }

            // VKILL (^U)
            if byte == c_cc[VKILL] {
                let n = self.edit_buf.kill_line();
                if n > 0 && echo_on && (c_lflag & ECHOK != 0) {
                    // Use erase_echo encoding: marker + count, caller
                    // repeats \x08 \x08 that many times.
                    return LdiscResult::Pushed {
                        echo: SmallVec::erase_echo(n),
                    };
                }
                return LdiscResult::Consumed;
            }

            // VWERASE (^W)
            if byte == c_cc[VWERASE] {
                let n = self.edit_buf.word_erase();
                if n > 0 && echo_on {
                    return LdiscResult::Pushed {
                        echo: SmallVec::erase_echo(n),
                    };
                }
                return LdiscResult::Consumed;
            }

            // VEOF (^D)
            if byte == c_cc[VEOF] {
                if self.edit_buf.is_empty() {
                    // Signal EOF: push empty slice.
                    push_fn(&[]);
                    return LdiscResult::LineComplete {
                        echo: SmallVec::new(),
                    };
                } else {
                    // Flush buffer contents without appending newline.
                    let data = &self.edit_buf.buf[..self.edit_buf.len];
                    push_fn(data);
                    self.edit_buf.clear();
                    return LdiscResult::LineComplete {
                        echo: SmallVec::new(),
                    };
                }
            }

            // Newline: deliver line.
            if byte == b'\n' {
                if !self.edit_buf.is_empty() {
                    let data = &self.edit_buf.buf[..self.edit_buf.len];
                    push_fn(data);
                    self.edit_buf.clear();
                }
                push_fn(b"\n");

                let mut echo = SmallVec::new();
                if echo_on || (c_lflag & ECHONL != 0) {
                    if c_oflag & ONLCR != 0 {
                        echo.push(b'\r');
                    }
                    echo.push(b'\n');
                }
                return LdiscResult::LineComplete { echo };
            }

            // Regular character: buffer it.
            self.edit_buf.push(byte);
            let mut echo = SmallVec::new();
            if echo_on {
                echo.push(byte);
            }
            return LdiscResult::Pushed { echo };
        }

        // 5. Raw mode: push byte directly.
        push_fn(&[byte]);
        let mut echo = SmallVec::new();
        if echo_on {
            if c_oflag & ONLCR != 0 && byte == b'\n' {
                echo.push(b'\r');
                echo.push(b'\n');
            } else {
                echo.push(byte);
            }
        }
        LdiscResult::Pushed { echo }
    }
}

impl SmallVec {
    /// Create a SmallVec encoding an erase-echo request.
    ///
    /// Byte 0 is a marker (0xFF), byte 1 is the repeat count (capped at 255).
    /// The caller should emit `\x08 \x08` repeated `count` times.
    pub fn erase_echo(count: usize) -> Self {
        let mut sv = SmallVec::new();
        sv.push(0xFF); // marker: erase-echo
        sv.push(if count > 255 { 255 } else { count as u8 });
        sv
    }

    /// Check if this SmallVec is an erase-echo marker.
    /// Returns the repeat count if so.
    pub fn erase_count(&self) -> Option<usize> {
        if self.len >= 2 && self.buf[0] == 0xFF {
            Some(self.buf[1] as usize)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem;

    #[test]
    fn termios_size() {
        // The repr(C) struct should be 36 bytes (matching Linux kernel termios).
        assert_eq!(mem::size_of::<Termios>(), TERMIOS_SIZE);
    }

    #[test]
    fn winsize_size() {
        assert_eq!(mem::size_of::<Winsize>(), WINSIZE_SIZE);
    }

    #[test]
    fn termios_field_offsets() {
        // Verify field offsets match Linux ABI.
        let t = Termios::default_cooked();
        let base = &t as *const _ as usize;
        assert_eq!(&t.c_iflag as *const _ as usize - base, 0);
        assert_eq!(&t.c_oflag as *const _ as usize - base, 4);
        assert_eq!(&t.c_cflag as *const _ as usize - base, 8);
        assert_eq!(&t.c_lflag as *const _ as usize - base, 12);
        assert_eq!(&t.c_line as *const _ as usize - base, 16);
        // c_cc starts at offset 17
        assert_eq!(&t.c_cc as *const _ as usize - base, 17);
    }

    #[test]
    fn default_cooked_flags() {
        let t = Termios::default_cooked();
        assert!(t.is_canonical());
        assert!(t.is_echo());
        assert!(t.is_isig());
        assert_eq!(t.c_cc[VINTR], 0x03);
        assert_eq!(t.c_cc[VEOF], 0x04);
        assert_eq!(t.c_cc[VERASE], 0x7F);
        assert_eq!(t.c_cc[VKILL], 0x15);
        assert_eq!(t.c_cc[VSUSP], 0x1A);
        assert_eq!(t.c_cc[VWERASE], 0x17);
    }

    #[test]
    fn edit_buffer_push_and_erase() {
        let mut eb = EditBuffer::new();
        assert!(eb.is_empty());
        eb.push(b'h');
        eb.push(b'e');
        eb.push(b'l');
        assert_eq!(eb.as_slice(), b"hel");

        assert_eq!(eb.erase_char(), Some(b'l'));
        assert_eq!(eb.as_slice(), b"he");
    }

    #[test]
    fn edit_buffer_kill_line() {
        let mut eb = EditBuffer::new();
        eb.push(b'a');
        eb.push(b'b');
        eb.push(b'c');
        assert_eq!(eb.kill_line(), 3);
        assert!(eb.is_empty());
    }

    #[test]
    fn edit_buffer_word_erase() {
        let mut eb = EditBuffer::new();
        for &b in b"hello world" {
            eb.push(b);
        }
        // Should erase "world" (5 chars).
        assert_eq!(eb.word_erase(), 5);
        assert_eq!(eb.as_slice(), b"hello ");

        // Should erase " " then "hello" (6 chars).
        assert_eq!(eb.word_erase(), 6);
        assert!(eb.is_empty());
    }

    #[test]
    fn edit_buffer_erase_empty() {
        let mut eb = EditBuffer::new();
        assert_eq!(eb.erase_char(), None);
        assert_eq!(eb.kill_line(), 0);
        assert_eq!(eb.word_erase(), 0);
    }

    #[test]
    fn edit_buffer_drain_partial() {
        let mut eb = EditBuffer::new();
        for &b in b"hello world" {
            eb.push(b);
        }
        eb.drain(6); // Remove "hello "
        assert_eq!(eb.as_slice(), b"world");
    }

    #[test]
    fn edit_buffer_drain_all() {
        let mut eb = EditBuffer::new();
        for &b in b"abc" {
            eb.push(b);
        }
        eb.drain(3);
        assert!(eb.is_empty());
    }

    #[test]
    fn edit_buffer_drain_more_than_len() {
        let mut eb = EditBuffer::new();
        for &b in b"ab" {
            eb.push(b);
        }
        eb.drain(100); // Should clamp to len
        assert!(eb.is_empty());
    }

    #[test]
    fn edit_buffer_drain_zero() {
        let mut eb = EditBuffer::new();
        for &b in b"abc" {
            eb.push(b);
        }
        eb.drain(0);
        assert_eq!(eb.as_slice(), b"abc");
    }

    #[test]
    fn edit_buffer_overflow() {
        let mut eb = EditBuffer::new();
        // Fill to capacity (4096 bytes)
        for _ in 0..4096 {
            assert!(eb.push(b'x'));
        }
        // One more should fail
        assert!(!eb.push(b'y'));
        assert_eq!(eb.as_slice().len(), 4096);
    }

    #[test]
    fn edit_buffer_clear() {
        let mut eb = EditBuffer::new();
        for &b in b"test" {
            eb.push(b);
        }
        eb.clear();
        assert!(eb.is_empty());
        assert_eq!(eb.as_slice(), b"");
    }

    // -----------------------------------------------------------------------
    // LineDiscipline tests
    // -----------------------------------------------------------------------

    /// Helper: collect all bytes pushed via push_fn.
    fn collect_pushed(ldisc: &mut LineDiscipline, byte: u8) -> (LdiscResult, Vec<u8>) {
        let mut pushed = Vec::new();
        let result = ldisc.process_byte(byte, &mut |data| {
            pushed.extend_from_slice(data);
        });
        (result, pushed)
    }

    #[test]
    fn ldisc_icrnl_translates_cr_to_nl() {
        let mut ld = LineDiscipline::new();
        // Default termios has ICRNL set and is canonical.
        assert!(ld.termios.c_iflag & ICRNL != 0);
        let (result, pushed) = collect_pushed(&mut ld, b'\r');
        // CR should become NL. In canonical mode, NL triggers line flush.
        assert!(matches!(result, LdiscResult::LineComplete { .. }));
        // The pushed data should contain the newline.
        assert!(pushed.contains(&b'\n'));
    }

    #[test]
    fn ldisc_igncr_drops_cr() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_iflag |= IGNCR;
        let (result, pushed) = collect_pushed(&mut ld, b'\r');
        assert!(matches!(result, LdiscResult::Consumed));
        assert!(pushed.is_empty());
    }

    #[test]
    fn ldisc_inlcr_translates_nl_to_cr() {
        let mut ld = LineDiscipline::new();
        // Disable ICANON so we can see raw push behavior.
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_iflag = INLCR; // Only INLCR, no ICRNL.
        let (result, pushed) = collect_pushed(&mut ld, b'\n');
        assert!(matches!(result, LdiscResult::Pushed { .. }));
        assert_eq!(pushed, vec![b'\r']);
    }

    #[test]
    fn ldisc_isig_ctrl_c_generates_sigint() {
        let mut ld = LineDiscipline::new();
        assert!(ld.termios.is_isig());
        let (result, pushed) = collect_pushed(&mut ld, 0x03); // ^C = VINTR
        assert!(matches!(result, LdiscResult::Signal(2)));
        assert!(pushed.is_empty());
    }

    #[test]
    fn ldisc_isig_ctrl_z_generates_sigtstp() {
        let mut ld = LineDiscipline::new();
        let (result, _) = collect_pushed(&mut ld, 0x1A); // ^Z = VSUSP
        assert!(matches!(result, LdiscResult::Signal(20)));
    }

    #[test]
    fn ldisc_isig_ctrl_backslash_generates_sigquit() {
        let mut ld = LineDiscipline::new();
        let (result, _) = collect_pushed(&mut ld, 0x1C); // ^\ = VQUIT
        assert!(matches!(result, LdiscResult::Signal(3)));
    }

    #[test]
    fn ldisc_isig_clears_edit_buf_on_signal() {
        let mut ld = LineDiscipline::new();
        // Type some chars first.
        collect_pushed(&mut ld, b'a');
        collect_pushed(&mut ld, b'b');
        assert_eq!(ld.edit_buf.len, 2);
        // Send ^C — should clear edit buffer.
        collect_pushed(&mut ld, 0x03);
        assert!(ld.edit_buf.is_empty());
    }

    #[test]
    fn ldisc_canonical_backspace_editing() {
        let mut ld = LineDiscipline::new();
        // Type "abc"
        collect_pushed(&mut ld, b'a');
        collect_pushed(&mut ld, b'b');
        collect_pushed(&mut ld, b'c');
        assert_eq!(ld.edit_buf.as_slice(), b"abc");

        // Backspace (DEL = 0x7F)
        let (result, _) = collect_pushed(&mut ld, 0x7F);
        assert!(matches!(result, LdiscResult::Pushed { .. }));
        assert_eq!(ld.edit_buf.as_slice(), b"ab");

        // Check echo contains erase sequence.
        if let LdiscResult::Pushed { echo } = result {
            // Should be BS SP BS.
            assert_eq!(echo.as_slice(), &[0x08, b' ', 0x08]);
        }
    }

    /// Phase 69b Track G — `EditBuffer::erase_one_codepoint` with
    /// IUTF8 cleared removes exactly one byte (legacy behaviour).
    #[test]
    fn edit_buffer_erase_one_codepoint_legacy_byte_only() {
        let mut buf = EditBuffer::new();
        for &b in &[0xC3u8, 0xA9] {
            buf.push(b);
        }
        // Without IUTF8, one byte per call.
        assert_eq!(buf.erase_one_codepoint(false), 1);
        assert_eq!(buf.as_slice(), &[0xC3]);
        assert_eq!(buf.erase_one_codepoint(false), 1);
        assert!(buf.is_empty());
        // Empty buffer reports 0.
        assert_eq!(buf.erase_one_codepoint(false), 0);
    }

    /// Phase 69b Track G — IUTF8 set: a 2-byte Latin-1 codepoint
    /// (e.g. é = C3 A9) is erased as one codepoint.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_two_byte_codepoint() {
        let mut buf = EditBuffer::new();
        for &b in &[0xC3u8, 0xA9] {
            buf.push(b);
        }
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 2);
        assert!(buf.is_empty());
    }

    /// Phase 69b Track G — IUTF8 set: a 3-byte box-drawing codepoint
    /// (e.g. ─ = E2 94 80) is erased as one codepoint.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_three_byte_codepoint() {
        let mut buf = EditBuffer::new();
        for &b in &[0xE2u8, 0x94, 0x80] {
            buf.push(b);
        }
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 3);
        assert!(buf.is_empty());
    }

    /// Phase 69b Track G — IUTF8 set: a 4-byte emoji-class codepoint
    /// (e.g. U+1F600 = F0 9F 98 80) is erased as one codepoint.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_four_byte_codepoint() {
        let mut buf = EditBuffer::new();
        for &b in &[0xF0u8, 0x9F, 0x98, 0x80] {
            buf.push(b);
        }
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 4);
        assert!(buf.is_empty());
    }

    /// Phase 69b Track G — IUTF8 set + ASCII: a 1-byte codepoint is
    /// erased exactly like the legacy path.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_ascii_one_byte() {
        let mut buf = EditBuffer::new();
        buf.push(b'a');
        buf.push(b'b');
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 1);
        assert_eq!(buf.as_slice(), b"a");
    }

    /// Phase 69b Track G — IUTF8 set: a malformed tail (the byte
    /// preceding the trailing continuations is itself a continuation
    /// rather than a leading byte, or its shape does not match the
    /// observed continuation count) is removed byte-by-byte. The
    /// scan is still bounded to the 4-byte UTF-8 maximum so it
    /// cannot run away across a long all-continuation buffer.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_malformed_tail_one_byte() {
        let mut buf = EditBuffer::new();
        // Eight continuation bytes with no leading byte — malformed.
        // Each erase removes exactly one continuation byte.
        for _ in 0..8 {
            buf.push(0x80);
        }
        assert_eq!(buf.erase_one_codepoint(true), 1);
        assert_eq!(buf.as_slice().len(), 7);
        assert_eq!(buf.erase_one_codepoint(true), 1);
        assert_eq!(buf.as_slice().len(), 6);
    }

    /// Phase 69b Track G — IUTF8 set: even when the malformed
    /// continuation run is much longer than the legal UTF-8 maximum
    /// (4 bytes), the scan in [`EditBuffer::erase_one_codepoint`] is
    /// bounded by `MAX_UTF8_LEN` so a `b"\xC3" + 16 * 0x80` style
    /// buffer cannot cause a runaway look-back across the whole
    /// buffer. The cap forces the scan to inspect at most four
    /// trailing continuations; everything beyond stays in the buffer
    /// and is drained one byte per call.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_long_continuation_run_bounded() {
        let mut buf = EditBuffer::new();
        // Start with a valid 2-byte leader so the leading-byte shape
        // check has something to fail against.
        buf.push(0xC3);
        // 16 bogus continuation bytes — well over the 4-byte legal
        // UTF-8 maximum.
        for _ in 0..16 {
            buf.push(0x80);
        }
        // The cap forces the scan to look at no more than the last
        // four bytes; the byte before them (still a continuation) is
        // not a valid leader, so the erase falls back to one byte.
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 1);
        assert_eq!(buf.as_slice().len(), 16);
    }

    /// Phase 69b Track G — IUTF8 set: a tail whose byte shape "looks
    /// like" a multi-byte sequence but decodes to a malformed scalar
    /// (overlong, UTF-16 surrogate, or codepoint > U+10FFFF) must be
    /// treated as malformed and erased one byte at a time. The
    /// shape-only check from round-2 would have erased
    /// `0xC0 0x80` / `0xED 0xA0 0x80` / `0xF5 0x80 0x80 0x80` as
    /// whole codepoints; strict validation via [`Utf8Decoder`] now
    /// rejects them at the trailing byte.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_rejects_overlong_two_byte() {
        let mut buf = EditBuffer::new();
        buf.push(0xC0);
        buf.push(0x80);
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 1);
        assert_eq!(buf.as_slice(), &[0xC0]);
    }

    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_rejects_surrogate_three_byte() {
        let mut buf = EditBuffer::new();
        // U+D820 surrogate, expressed as 3-byte UTF-8.
        for &b in &[0xEDu8, 0xA0, 0x80] {
            buf.push(b);
        }
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 1);
        assert_eq!(buf.as_slice(), &[0xED, 0xA0]);
    }

    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_rejects_above_max_four_byte() {
        let mut buf = EditBuffer::new();
        // 0xF5 is an out-of-range leader (would decode > U+10FFFF).
        for &b in &[0xF5u8, 0x80, 0x80, 0x80] {
            buf.push(b);
        }
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 1);
        assert_eq!(buf.as_slice(), &[0xF5, 0x80, 0x80]);
    }

    /// Phase 69b Track G — IUTF8 set: when the buffer ends with a
    /// stray continuation byte preceded by a valid leading byte that
    /// does NOT match the continuation count (e.g. `b"A\x80"` where
    /// 'A' is ASCII and expects zero continuations), the erase must
    /// remove only the trailing byte and preserve the preceding
    /// valid codepoint. The previous implementation erroneously
    /// erased the leading byte along with the malformed tail.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_preserves_preceding_ascii() {
        let mut buf = EditBuffer::new();
        buf.push(b'A');
        buf.push(0x80);
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 1);
        assert_eq!(buf.as_slice(), b"A");
    }

    /// Phase 69b Track G — IUTF8 set: when the buffer ends with a
    /// stray leading byte that has no following continuations
    /// (e.g. `b"A\xC3"` where 0xC3 expects one continuation), the
    /// stray leader is removed alone — the preceding ASCII byte
    /// stays.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_preserves_preceding_when_leader_is_stray() {
        let mut buf = EditBuffer::new();
        buf.push(b'A');
        buf.push(0xC3);
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 1);
        assert_eq!(buf.as_slice(), b"A");
    }

    /// Phase 69b Track G — IUTF8 set: when the buffer ends with a
    /// well-formed multi-byte sequence preceded by another valid
    /// codepoint, the whole trailing codepoint is removed while the
    /// preceding codepoint is preserved.
    #[test]
    fn edit_buffer_erase_one_codepoint_iutf8_well_formed_after_ascii() {
        let mut buf = EditBuffer::new();
        buf.push(b'A');
        buf.push(0xC3);
        buf.push(0xA9);
        let removed = buf.erase_one_codepoint(true);
        assert_eq!(removed, 2);
        assert_eq!(buf.as_slice(), b"A");
    }

    /// Phase 69b Track G — IUTF8 set + VERASE on the canonical ldisc:
    /// the buffer's two Latin-1 bytes are removed together. The echo
    /// remains one BS/SP/BS triplet — codepoint-width-on-grid is a
    /// Phase 69b Track F concern handled by the screen state machine.
    #[test]
    fn ldisc_canonical_verase_iutf8_removes_whole_codepoint() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_iflag |= IUTF8;
        // Push C3 A9 (é) — two raw bytes pretending they were typed
        // by a UTF-8 input method. The ldisc would accept these one
        // at a time; we mirror that here via process_byte.
        for byte in [0xC3u8, 0xA9] {
            collect_pushed(&mut ld, byte);
        }
        assert_eq!(ld.edit_buf.as_slice(), &[0xC3, 0xA9]);
        // VERASE — the IUTF8-aware erase clears both bytes in one
        // press.
        let (result, _) = collect_pushed(&mut ld, 0x7F);
        assert!(matches!(result, LdiscResult::Pushed { .. }));
        assert!(ld.edit_buf.is_empty());
    }

    /// Phase 69b Track G — IUTF8 cleared: the same byte stream still
    /// removes one byte per VERASE (legacy behaviour preserved).
    #[test]
    fn ldisc_canonical_verase_iutf8_off_removes_single_byte() {
        let mut ld = LineDiscipline::new();
        assert_eq!(ld.termios.c_iflag & IUTF8, 0);
        for byte in [0xC3u8, 0xA9] {
            collect_pushed(&mut ld, byte);
        }
        let (_, _) = collect_pushed(&mut ld, 0x7F);
        // One byte remains: the leading 0xC3.
        assert_eq!(ld.edit_buf.as_slice(), &[0xC3]);
    }

    #[test]
    fn ldisc_canonical_kill_line() {
        let mut ld = LineDiscipline::new();
        // Default termios does not include ECHOK; set it explicitly.
        ld.termios.c_lflag |= ECHOK;
        for &b in b"hello" {
            collect_pushed(&mut ld, b);
        }
        assert_eq!(ld.edit_buf.len, 5);

        // ^U = VKILL
        let (result, _) = collect_pushed(&mut ld, 0x15);
        assert!(ld.edit_buf.is_empty());
        if let LdiscResult::Pushed { echo } = result {
            // Should be erase-echo marker with count=5.
            assert_eq!(echo.erase_count(), Some(5));
        } else {
            panic!("expected Pushed with erase echo");
        }
    }

    #[test]
    fn ldisc_canonical_word_erase() {
        let mut ld = LineDiscipline::new();
        for &b in b"hello world" {
            collect_pushed(&mut ld, b);
        }

        // ^W = VWERASE — should erase "world" (5 chars).
        let (result, _) = collect_pushed(&mut ld, 0x17);
        assert_eq!(ld.edit_buf.as_slice(), b"hello ");
        if let LdiscResult::Pushed { echo } = result {
            assert_eq!(echo.erase_count(), Some(5));
        } else {
            panic!("expected Pushed with erase echo");
        }
    }

    #[test]
    fn ldisc_veof_empty_buffer_signals_eof() {
        let mut ld = LineDiscipline::new();
        let mut pushed = Vec::new();
        let result = ld.process_byte(0x04, &mut |data| {
            pushed.push(data.to_vec());
        });
        assert!(matches!(result, LdiscResult::LineComplete { .. }));
        // push_fn should have been called with empty slice.
        assert_eq!(pushed.len(), 1);
        assert!(pushed[0].is_empty());
    }

    #[test]
    fn ldisc_veof_nonempty_buffer_flushes() {
        let mut ld = LineDiscipline::new();
        collect_pushed(&mut ld, b'a');
        collect_pushed(&mut ld, b'b');

        let mut pushed = Vec::new();
        let result = ld.process_byte(0x04, &mut |data| {
            pushed.extend_from_slice(data);
        });
        assert!(matches!(result, LdiscResult::LineComplete { .. }));
        assert_eq!(pushed, b"ab");
        assert!(ld.edit_buf.is_empty());
    }

    #[test]
    fn ldisc_canonical_newline_delivers_line() {
        let mut ld = LineDiscipline::new();
        for &b in b"hi" {
            collect_pushed(&mut ld, b);
        }

        let (result, pushed) = collect_pushed(&mut ld, b'\n');
        assert!(matches!(result, LdiscResult::LineComplete { .. }));
        // Should push "hi\n".
        assert_eq!(pushed, b"hi\n");
        assert!(ld.edit_buf.is_empty());

        // Check echo contains \r\n (ONLCR is set by default).
        if let LdiscResult::LineComplete { echo } = result {
            assert_eq!(echo.as_slice(), b"\r\n");
        }
    }

    #[test]
    fn ldisc_canonical_empty_newline_no_eof() {
        // Pressing Enter on an empty line must NOT trigger EOF (push_fn(&[])).
        // It should just push "\n".
        let mut ld = LineDiscipline::new();
        let (result, pushed) = collect_pushed(&mut ld, b'\n');
        assert!(matches!(result, LdiscResult::LineComplete { .. }));
        // Only the newline — no empty-slice push.
        assert_eq!(pushed, b"\n");
    }

    #[test]
    fn ldisc_canonical_regular_char_echo() {
        let mut ld = LineDiscipline::new();
        let (result, _) = collect_pushed(&mut ld, b'x');
        if let LdiscResult::Pushed { echo } = result {
            assert_eq!(echo.as_slice(), b"x");
        } else {
            panic!("expected Pushed");
        }
        assert_eq!(ld.edit_buf.as_slice(), b"x");
    }

    #[test]
    fn ldisc_canonical_no_echo() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ECHO;
        let (result, _) = collect_pushed(&mut ld, b'x');
        if let LdiscResult::Pushed { echo } = result {
            assert!(echo.is_empty());
        } else {
            panic!("expected Pushed");
        }
    }

    #[test]
    fn ldisc_raw_mode_passthrough() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_lflag &= !ISIG;
        ld.termios.c_lflag &= !ECHO;

        let (result, pushed) = collect_pushed(&mut ld, b'z');
        assert!(matches!(result, LdiscResult::Pushed { .. }));
        assert_eq!(pushed, vec![b'z']);
    }

    #[test]
    fn ldisc_raw_mode_echo_with_onlcr() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_lflag |= ECHO;
        ld.termios.c_oflag |= ONLCR;

        let (result, pushed) = collect_pushed(&mut ld, b'\n');
        assert_eq!(pushed, vec![b'\n']);
        if let LdiscResult::Pushed { echo } = result {
            assert_eq!(echo.as_slice(), b"\r\n");
        } else {
            panic!("expected Pushed");
        }
    }

    #[test]
    fn ldisc_raw_mode_regular_echo() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_lflag |= ECHO;

        let (result, pushed) = collect_pushed(&mut ld, b'A');
        assert_eq!(pushed, vec![b'A']);
        if let LdiscResult::Pushed { echo } = result {
            assert_eq!(echo.as_slice(), b"A");
        } else {
            panic!("expected Pushed");
        }
    }

    #[test]
    fn ldisc_isig_disabled_passes_signal_chars() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ISIG;
        ld.termios.c_lflag &= !ICANON;

        // ^C should pass through as regular byte.
        let (result, pushed) = collect_pushed(&mut ld, 0x03);
        assert!(matches!(result, LdiscResult::Pushed { .. }));
        assert_eq!(pushed, vec![0x03]);
    }

    #[test]
    fn ldisc_echonl_without_echo() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ECHO;
        ld.termios.c_lflag |= ECHONL;

        let (result, _) = collect_pushed(&mut ld, b'\n');
        if let LdiscResult::LineComplete { echo } = result {
            // ECHONL should cause newline echo even when ECHO is off.
            assert!(!echo.is_empty());
        } else {
            panic!("expected LineComplete");
        }
    }

    #[test]
    fn smallvec_basic() {
        let mut sv = SmallVec::new();
        assert!(sv.is_empty());
        sv.push(b'a');
        sv.push(b'b');
        assert_eq!(sv.len(), 2);
        assert_eq!(sv.as_slice(), b"ab");
    }

    #[test]
    fn smallvec_overflow_drops() {
        let mut sv = SmallVec::new();
        for i in 0..10 {
            sv.push(i);
        }
        // Only first 8 should be stored.
        assert_eq!(sv.len(), 8);
    }

    #[test]
    fn smallvec_erase_echo() {
        let sv = SmallVec::erase_echo(5);
        assert_eq!(sv.erase_count(), Some(5));
    }

    // -----------------------------------------------------------------------
    // Escape sequence filtering in canonical mode (C-review-1)
    // -----------------------------------------------------------------------

    #[test]
    fn ldisc_canonical_discards_arrow_up() {
        let mut ld = LineDiscipline::new();
        // ESC [ A (Up Arrow) — all three bytes should be consumed.
        for &b in b"\x1b[A" {
            let (result, pushed) = collect_pushed(&mut ld, b);
            assert!(matches!(result, LdiscResult::Consumed));
            assert!(pushed.is_empty());
        }
        // Edit buffer must be empty.
        assert!(ld.edit_buf.is_empty());
    }

    #[test]
    fn ldisc_canonical_discards_csi_tilde_sequences() {
        let mut ld = LineDiscipline::new();
        // ESC [ 3 ~ (Delete key)
        for &b in b"\x1b[3~" {
            let (result, pushed) = collect_pushed(&mut ld, b);
            assert!(matches!(result, LdiscResult::Consumed));
            assert!(pushed.is_empty());
        }
        assert!(ld.edit_buf.is_empty());

        // ESC [ 5 ~ (Page Up)
        for &b in b"\x1b[5~" {
            let (result, pushed) = collect_pushed(&mut ld, b);
            assert!(matches!(result, LdiscResult::Consumed));
            assert!(pushed.is_empty());
        }
        assert!(ld.edit_buf.is_empty());
    }

    #[test]
    fn ldisc_canonical_discards_ss3_sequence() {
        let mut ld = LineDiscipline::new();
        // ESC O P (F1 on some terminals)
        for &b in b"\x1bOP" {
            let (result, pushed) = collect_pushed(&mut ld, b);
            assert!(matches!(result, LdiscResult::Consumed));
            assert!(pushed.is_empty());
        }
        assert!(ld.edit_buf.is_empty());
    }

    #[test]
    fn ldisc_canonical_discards_lone_esc() {
        let mut ld = LineDiscipline::new();
        // Lone ESC followed by a regular letter: ESC is consumed,
        // the letter gets normal processing.
        let (result, pushed) = collect_pushed(&mut ld, 0x1b);
        assert!(matches!(result, LdiscResult::Consumed));
        assert!(pushed.is_empty());

        // Next regular char should be processed normally (buffered).
        let (result, _) = collect_pushed(&mut ld, b'x');
        assert!(matches!(result, LdiscResult::Pushed { .. }));
        assert_eq!(ld.edit_buf.as_slice(), b"x");
    }

    #[test]
    fn ldisc_canonical_escape_does_not_corrupt_following_input() {
        let mut ld = LineDiscipline::new();
        // Type "ab", then Up Arrow, then "cd\n".
        for &b in b"ab" {
            collect_pushed(&mut ld, b);
        }
        for &b in b"\x1b[A" {
            collect_pushed(&mut ld, b);
        }
        for &b in b"cd" {
            collect_pushed(&mut ld, b);
        }
        // Edit buffer should contain only "abcd", no escape bytes.
        assert_eq!(ld.edit_buf.as_slice(), b"abcd");

        // Deliver line.
        let (result, pushed) = collect_pushed(&mut ld, b'\n');
        assert!(matches!(result, LdiscResult::LineComplete { .. }));
        assert_eq!(pushed, b"abcd\n");
    }

    #[test]
    fn ldisc_raw_mode_passes_escape_sequences() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_lflag &= !ECHO;
        ld.termios.c_lflag &= !ISIG;

        // ESC [ A should pass through in raw mode.
        let mut all_pushed = Vec::new();
        for &b in b"\x1b[A" {
            let (result, pushed) = collect_pushed(&mut ld, b);
            assert!(matches!(result, LdiscResult::Pushed { .. }));
            all_pushed.extend(pushed);
        }
        assert_eq!(all_pushed, b"\x1b[A");
    }

    #[test]
    fn ldisc_canonical_multiple_escape_sequences() {
        let mut ld = LineDiscipline::new();
        // Two consecutive arrow sequences should both be discarded.
        for &b in b"\x1b[A\x1b[B" {
            let (result, pushed) = collect_pushed(&mut ld, b);
            assert!(matches!(result, LdiscResult::Consumed));
            assert!(pushed.is_empty());
        }
        assert!(ld.edit_buf.is_empty());
    }

    // -----------------------------------------------------------------------
    // Phase 69a — Termios round-trip + cooked/raw defaults
    // -----------------------------------------------------------------------

    #[test]
    fn termios_cooked_default_baseline() {
        let t = Termios::cooked_default();
        assert!(t.c_iflag & ICRNL != 0);
        assert!(t.c_iflag & IXON != 0);
        assert!(t.c_oflag & OPOST != 0);
        assert!(t.c_oflag & ONLCR != 0);
        assert!(t.c_lflag & ICANON != 0);
        assert!(t.c_lflag & ECHO != 0);
        assert!(t.c_lflag & ECHOE != 0);
        assert!(t.c_lflag & ECHOK != 0);
        assert!(t.c_lflag & ISIG != 0);
        assert!(t.c_lflag & IEXTEN != 0);
        assert_eq!(t.c_cc[VEOF], 0x04);
        assert_eq!(t.c_cc[VINTR], 0x03);
        assert_eq!(t.c_cc[VQUIT], 0x1C);
        assert_eq!(t.c_cc[VERASE], 0x7F);
        assert_eq!(t.c_cc[VKILL], 0x15);
        assert_eq!(t.c_cc[VEOL], 0);
        assert_eq!(t.c_cc[VSUSP], 0x1A);
        assert_eq!(t.c_cc[VMIN], 1);
        assert_eq!(t.c_cc[VTIME], 0);
    }

    #[test]
    fn termios_raw_default_clears_cfmakeraw_bits() {
        let t = Termios::raw_default();
        // Local-mode editing / signals / extended processing all off.
        assert_eq!(t.c_lflag & (ICANON | ECHO | ISIG | IEXTEN), 0);
        // Output post-processing off.
        assert_eq!(t.c_oflag & OPOST, 0);
        // Four input-mapping bits cleared.
        assert_eq!(t.c_iflag & (INLCR | IGNCR | ICRNL | IXON), 0);
        // Other cfmakeraw clears.
        assert_eq!(t.c_iflag & (IGNBRK | BRKINT | PARMRK | ISTRIP), 0);
        // VMIN=1 / VTIME=0 by definition.
        assert_eq!(t.c_cc[VMIN], 1);
        assert_eq!(t.c_cc[VTIME], 0);
    }

    #[test]
    fn termios_round_trip_through_ioctl_layout() {
        // Round-trip every distinct flag bit.  The C-ABI struct stores
        // each mode word as a u32, so no bits are lost across copy.
        let mut t = Termios::cooked_default();
        t.c_iflag = 0xFFFF_FFFF;
        t.c_oflag = 0xDEAD_BEEF;
        t.c_cflag = 0x0123_4567;
        t.c_lflag = 0x89AB_CDEF;
        for (i, slot) in t.c_cc.iter_mut().enumerate() {
            *slot = i as u8;
        }
        let bytes: [u8; TERMIOS_SIZE] = unsafe { core::mem::transmute(t) };
        let restored: Termios = unsafe { core::mem::transmute(bytes) };
        assert_eq!(restored.c_iflag, 0xFFFF_FFFF);
        assert_eq!(restored.c_oflag, 0xDEAD_BEEF);
        assert_eq!(restored.c_cflag, 0x0123_4567);
        assert_eq!(restored.c_lflag, 0x89AB_CDEF);
        for i in 0..NCCS {
            assert_eq!(restored.c_cc[i], i as u8);
        }
    }

    #[test]
    fn termios_iflag_constants_no_collisions() {
        let bits = [
            IGNBRK, BRKINT, IGNPAR, PARMRK, INPCK, ISTRIP, INLCR, IGNCR, ICRNL, IUCLC, IXON, IXANY,
            IXOFF, IMAXBEL, IUTF8,
        ];
        let mut all = 0u32;
        for b in bits {
            assert_eq!(all & b, 0, "iflag bit collision: {:#o}", b);
            all |= b;
        }
    }

    #[test]
    fn termios_lflag_constants_no_collisions() {
        let bits = [
            ISIG, ICANON, ECHO, ECHOE, ECHOK, ECHONL, NOFLSH, TOSTOP, IEXTEN,
        ];
        let mut all = 0u32;
        for b in bits {
            assert_eq!(all & b, 0, "lflag bit collision: {:#o}", b);
            all |= b;
        }
    }

    #[test]
    fn termios_cc_indices_unique() {
        let idx = [
            VINTR, VQUIT, VERASE, VKILL, VEOF, VTIME, VMIN, VSWTC, VSTART, VSTOP, VSUSP, VEOL,
            VREPRINT, VDISCARD, VWERASE, VLNEXT, VEOL2,
        ];
        let mut seen = [false; NCCS];
        for i in idx {
            assert!(i < NCCS);
            assert!(!seen[i], "duplicate c_cc index {}", i);
            seen[i] = true;
        }
    }

    // -----------------------------------------------------------------------
    // IXON / IXOFF flow control
    // -----------------------------------------------------------------------

    #[test]
    fn ldisc_ixon_vstop_suspends_output() {
        let mut ld = LineDiscipline::new();
        // Default cooked includes IXON.  Send VSTOP (^S, 0x13).
        let (result, pushed) = collect_pushed(&mut ld, 0x13);
        assert!(matches!(result, LdiscResult::Consumed));
        assert!(pushed.is_empty());
        assert!(ld.output_suspended);
        // VSTART (^Q, 0x11) resumes.
        let (result, pushed) = collect_pushed(&mut ld, 0x11);
        assert!(matches!(result, LdiscResult::Consumed));
        assert!(pushed.is_empty());
        assert!(!ld.output_suspended);
    }

    #[test]
    fn ldisc_ixon_disabled_passes_xon_xoff_through() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_iflag &= !IXON;
        ld.termios.c_lflag &= !ICANON;
        let (result, pushed) = collect_pushed(&mut ld, 0x13);
        assert!(matches!(result, LdiscResult::Pushed { .. }));
        assert_eq!(pushed, vec![0x13]);
        assert!(!ld.output_suspended);
    }

    // -----------------------------------------------------------------------
    // IEXTEN / VLNEXT — literal-next byte
    // -----------------------------------------------------------------------

    #[test]
    fn ldisc_iexten_vlnext_makes_next_byte_literal() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON; // raw-style for clarity
        // VLNEXT (^V, 0x16) is consumed; the next byte is delivered verbatim.
        let (result, pushed) = collect_pushed(&mut ld, 0x16);
        assert!(matches!(result, LdiscResult::Consumed));
        assert!(pushed.is_empty());
        // The next byte — even ^C — is delivered as a literal byte.
        let (result, pushed) = collect_pushed(&mut ld, 0x03);
        assert!(matches!(result, LdiscResult::Pushed { .. }));
        assert_eq!(pushed, vec![0x03]);
    }

    #[test]
    fn ldisc_iexten_disabled_drops_vlnext() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_lflag &= !IEXTEN;
        ld.termios.c_lflag &= !ISIG;
        // VLNEXT (^V, 0x16) becomes an ordinary byte.
        let (result, pushed) = collect_pushed(&mut ld, 0x16);
        assert!(matches!(result, LdiscResult::Pushed { .. }));
        assert_eq!(pushed, vec![0x16]);
    }

    // -----------------------------------------------------------------------
    // VMIN / VTIME poll_read_ready quadrant coverage
    // -----------------------------------------------------------------------

    #[test]
    fn ldisc_vmin_zero_vtime_zero_polls() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_cc[VMIN] = 0;
        ld.termios.c_cc[VTIME] = 0;
        ld.set_raw_buffered(0);
        assert!(ld.poll_read_ready(0));
    }

    #[test]
    fn ldisc_vmin_positive_vtime_zero_blocks() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_cc[VMIN] = 2;
        ld.termios.c_cc[VTIME] = 0;
        ld.set_raw_buffered(1);
        assert!(!ld.poll_read_ready(0));
        ld.set_raw_buffered(2);
        assert!(ld.poll_read_ready(0));
    }

    #[test]
    fn ldisc_vmin_zero_vtime_positive_returns_on_deadline() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_cc[VMIN] = 0;
        ld.termios.c_cc[VTIME] = 5; // 500 ms
        ld.set_raw_buffered(0);
        assert!(!ld.poll_read_ready(0));
        ld.arm_vtime_timer(100);
        assert_eq!(ld.deadline_ticks, Some(100 + 500));
        assert!(!ld.poll_read_ready(599));
        assert!(ld.poll_read_ready(600));
        // Any data short-circuits the deadline.
        ld.set_raw_buffered(1);
        assert!(ld.poll_read_ready(0));
    }

    #[test]
    fn ldisc_vmin_positive_vtime_positive_inter_byte_timer() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_cc[VMIN] = 4;
        ld.termios.c_cc[VTIME] = 1; // 100 ms inter-byte
        ld.set_raw_buffered(0);
        // No bytes yet — timer not armed.
        assert!(!ld.poll_read_ready(1_000));
        ld.set_raw_buffered(1);
        ld.arm_vtime_timer(2_000);
        assert_eq!(ld.deadline_ticks, Some(2_100));
        assert!(!ld.poll_read_ready(2_099));
        assert!(ld.poll_read_ready(2_100));
        // Reaching VMIN short-circuits the deadline.
        ld.set_raw_buffered(4);
        assert!(ld.poll_read_ready(0));
    }

    #[test]
    fn ldisc_set_raw_buffered_zero_clears_timer() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !ICANON;
        ld.termios.c_cc[VMIN] = 1;
        ld.termios.c_cc[VTIME] = 1;
        ld.set_raw_buffered(1);
        ld.arm_vtime_timer(0);
        assert!(ld.deadline_ticks.is_some());
        ld.set_raw_buffered(0);
        assert!(ld.deadline_ticks.is_none());
    }
}
