//! Minimal QMP (QEMU Machine Protocol) client used by the render-probe
//! subcommand to drive keystrokes into the guest and capture the
//! framebuffer state via `screendump`.
//!
//! The smoke-test harness used by `tui-app-smoke`, `termios-smoke`,
//! etc. is structurally blind to render disappearance because it only
//! reads serial output. The bug investigated in
//! `docs/handoffs/2026-05-17-less-render-disappearance.md` corrupts
//! pixels with no serial side-effect, so the gate passes a black
//! screen. This module closes that hole: it talks to QEMU's QMP
//! socket, sends `send-key` events through the emulated PS/2 path
//! (the same path real key presses take), and reads back PPM
//! framebuffer dumps that downstream code hashes / diffs.
//!
//! The protocol is text-based JSON over a Unix socket, one message
//! per line. We deliberately avoid a thread / async runtime: the
//! probe is sequential — issue a command, wait for the reply, move
//! on — so a blocking `BufReader::read_line` is enough.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Default deadline for any single QMP command. QEMU should reply to
/// `qmp_capabilities` / `send-key` essentially instantly; `screendump`
/// can take longer when the framebuffer is large (1280×800 BGRA8888 is
/// ~4 MiB of pixel data) but still completes in tens of milliseconds.
/// 30 s gives enough headroom for TCG under heavy CI load without
/// hanging the probe forever on a wedged guest.
pub const DEFAULT_CMD_TIMEOUT: Duration = Duration::from_secs(30);

/// Gap left between the individual edges of a pointer gesture
/// ([`QmpClient::drag_abs_with_mods`] and everything built on it).
///
/// This is load-bearing for any *modifier-qualified* gesture, and the
/// reason is a guest-side polling race rather than anything QEMU does
/// wrong. QMP `input-send-event` delivers each edge faithfully, but the
/// keyboard and the pointer are two independent USB interrupt-IN
/// endpoints, and the guest's `usb-hid` daemon drains them from one poll
/// loop — one report per device per tick. QEMU's `usb-tablet` hands out
/// its queued motion/button reports one per poll, while `usb-kbd` drains
/// its whole scancode queue in a couple of polls. Fire the twelve edges
/// of a `shift`-held drag back to back and the keyboard timeline runs
/// *ahead* of the pointer timeline: the guest reads the Shift make, then
/// the first motion report, then the Shift **break** — which was sent
/// last, but is already queued — and only then the button press and the
/// remaining motion. Empirically (headless repro on the
/// `term-daily-driver-smoke` lane) the release edge lands between the
/// first `MoveAbs` and the button-down, so `display_server` stamps the
/// entire drag with *no* modifiers and `term`'s Shift-drag override never
/// fires.
///
/// Pacing every edge fixes it: each step is consumed before the next is
/// queued, so the two device timelines cannot invert. The value must
/// exceed the guest poller's worst-case sleep — `usb-hid` backs off to
/// `kernel_core::input::hid_poll::HID_POLL_MAX_IDLE_NS` (100 ms) when
/// idle, and only snaps back to its 5 ms cadence once a report arrives —
/// so the *first* edge of a gesture can sit in QEMU's queue for up to
/// 100 ms before anyone looks at it. 150 ms clears that with margin on
/// TCG. Cost is ~2 s per 13-step drag, which the gate's budget absorbs.
///
/// Applied uniformly, including to unmodified drags: arm 6 of
/// `term-daily-driver-smoke` compares an unshifted drag against a
/// Shift-held one, and that comparison is only honest if the two
/// gestures differ *only* in the modifier.
pub const GESTURE_STEP_PACING: Duration = Duration::from_millis(150);

/// One QMP client. Wraps the underlying `UnixStream` plus a buffered
/// reader. Dropping closes the socket; QEMU treats that as a benign
/// monitor disconnect.
pub struct QmpClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

#[derive(Debug)]
pub enum QmpError {
    Connect(std::io::Error, PathBuf),
    Io(std::io::Error),
    Decode(String),
    Server(String),
    Timeout(Duration, String),
}

impl std::fmt::Display for QmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QmpError::Connect(e, path) => write!(f, "qmp: connect to {}: {e}", path.display()),
            QmpError::Io(e) => write!(f, "qmp: io: {e}"),
            QmpError::Decode(msg) => write!(f, "qmp: decode: {msg}"),
            QmpError::Server(msg) => write!(f, "qmp: server error: {msg}"),
            QmpError::Timeout(d, msg) => write!(f, "qmp: timeout after {d:?} while {msg}"),
        }
    }
}

impl std::error::Error for QmpError {}

impl QmpClient {
    /// Connect to a QMP Unix-socket server, perform the
    /// `qmp_capabilities` handshake, and return a ready client.
    ///
    /// QEMU's QMP server starts in *negotiation* mode: it emits a
    /// `QMP` greeting line and refuses every command except
    /// `qmp_capabilities` until the client opts in. The greeting must
    /// be consumed and the handshake completed before any real
    /// command is issued, otherwise QEMU replies with
    /// `CommandNotFound`. We bundle both into `connect` so callers
    /// always get a usable client.
    ///
    /// `connect_deadline` covers retries: QEMU's socket comes up
    /// asynchronously after launch, so the first few `connect` calls
    /// may see `ENOENT` / `ECONNREFUSED` before the listener binds.
    /// We retry on those errors until the deadline expires.
    pub fn connect(socket: &Path, connect_deadline: Instant) -> Result<Self, QmpError> {
        let stream = loop {
            match UnixStream::connect(socket) {
                Ok(s) => break s,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    if Instant::now() >= connect_deadline {
                        return Err(QmpError::Connect(e, socket.to_path_buf()));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(QmpError::Connect(e, socket.to_path_buf())),
            }
        };
        // Per-read timeout for the recv path. The handshake response
        // and command replies must arrive within a small window;
        // QEMU's QMP server is a single-threaded poll loop so the
        // typical reply latency is sub-millisecond. A 30s read
        // timeout protects against a wedged guest without hanging
        // the probe forever.
        stream
            .set_read_timeout(Some(DEFAULT_CMD_TIMEOUT))
            .map_err(QmpError::Io)?;
        let writer = stream.try_clone().map_err(QmpError::Io)?;
        let reader = BufReader::new(stream);
        let mut client = Self { writer, reader };

        // 1. Consume the greeting. Format:
        //    {"QMP":{"version":{...},"capabilities":[...]}}
        let greeting = client.read_one_message(DEFAULT_CMD_TIMEOUT, "qmp greeting")?;
        if greeting.get("QMP").is_none() {
            return Err(QmpError::Decode(format!(
                "expected QMP greeting, got: {greeting}"
            )));
        }

        // 2. Send the capabilities handshake. The empty arguments map
        //    keeps OOB mode disabled — synchronous request / response is
        //    all we need.
        let _ = client.execute("qmp_capabilities", json!({}))?;
        Ok(client)
    }

    /// Send `{"execute": cmd, "arguments": args}` and return the
    /// `return` field of the reply. Events that arrive before the
    /// reply (e.g. `RESET`, `SHUTDOWN`) are silently dropped — the
    /// probe does not consume them today.
    pub fn execute(&mut self, cmd: &str, args: Value) -> Result<Value, QmpError> {
        let request = json!({"execute": cmd, "arguments": args});
        let line = format!("{}\n", request);
        self.writer
            .write_all(line.as_bytes())
            .map_err(QmpError::Io)?;
        self.writer.flush().map_err(QmpError::Io)?;

        // Drain until we see a `return` or `error` envelope. Events
        // carry an `event` field; we skip those.
        let deadline = Instant::now() + DEFAULT_CMD_TIMEOUT;
        loop {
            let msg =
                self.read_one_message(deadline.saturating_duration_since(Instant::now()), cmd)?;
            if msg.get("event").is_some() {
                continue;
            }
            if let Some(ret) = msg.get("return") {
                return Ok(ret.clone());
            }
            if let Some(err) = msg.get("error") {
                let cls = err.get("class").and_then(Value::as_str).unwrap_or("?");
                let desc = err.get("desc").and_then(Value::as_str).unwrap_or("?");
                return Err(QmpError::Server(format!("{cls}: {desc}")));
            }
            return Err(QmpError::Decode(format!("unexpected envelope: {msg}")));
        }
    }

    /// Press a single PS/2 key by QEMU `qcode` name (e.g. `"down"`,
    /// `"ret"`, `"a"`). Honors `hold-time` in milliseconds so a
    /// shell that requires a measurable key-down duration (most do
    /// not) still sees a clean press / release pair.
    ///
    /// QEMU's `send-key` synthesises both press and release; we don't
    /// need to manage state.
    pub fn press_key(&mut self, qcode: &str, hold_ms: u32) -> Result<(), QmpError> {
        let _ = self.execute(
            "send-key",
            json!({
                "keys": [{"type": "qcode", "data": qcode}],
                "hold-time": hold_ms,
            }),
        )?;
        Ok(())
    }

    /// Press a chord (multiple keys at once) by qcode list.
    /// QEMU's `send-key` accepts an array of key objects and presses
    /// them all simultaneously then releases; chord ordering inside
    /// the array doesn't matter to QEMU. Used for compositor
    /// keybinds like SUPER+RETURN (`&["meta_l", "ret"]`) or
    /// SUPER+1..9 workspace switches (`&["meta_l", "1"]`).
    pub fn press_chord(&mut self, qcodes: &[&str], hold_ms: u32) -> Result<(), QmpError> {
        let keys: Vec<Value> = qcodes
            .iter()
            .map(|qc| json!({"type": "qcode", "data": qc}))
            .collect();
        let _ = self.execute(
            "send-key",
            json!({
                "keys": keys,
                "hold-time": hold_ms,
            }),
        )?;
        Ok(())
    }

    /// Press *or* release a single PS/2 key by QEMU `qcode` name, as one
    /// half of a make/break pair.
    ///
    /// [`Self::press_key`] and [`Self::press_chord`] both go through
    /// `send-key`, which synthesises the release for you — so neither can
    /// leave a key held down across *other* events. That is exactly what a
    /// modifier-qualified pointer gesture needs: the compositor stamps its
    /// live keyboard-modifier snapshot onto every pointer event it
    /// delivers, so `term`'s Shift-drag override and Alt block-select are
    /// only reachable if Shift/Alt is physically down while the button and
    /// motion events arrive. `input-send-event`'s `key` event carries an
    /// explicit `down` flag and does not synthesise the other edge, which
    /// is what makes the hold possible.
    ///
    /// Every `true` must be paired with a matching `false` — a leaked
    /// key-down stays latched in the guest's modifier state for the rest
    /// of the run and corrupts every later arm.
    pub fn send_key_state(&mut self, qcode: &str, down: bool) -> Result<(), QmpError> {
        let _ = self.execute("input-send-event", key_state_args(qcode, down))?;
        Ok(())
    }

    /// Inject a relative pointer motion into the guest via QMP
    /// `input-send-event`. Drives the emulated `usb-mouse` (QEMU routes the
    /// pointer event through its input subsystem to the active mouse device).
    /// Used by the `usb-smoke` gate to assert a live USB mouse report.
    pub fn send_pointer_rel(&mut self, dx: i32, dy: i32) -> Result<(), QmpError> {
        let _ = self.execute(
            "input-send-event",
            json!({
                "events": [
                    {"type": "rel", "data": {"axis": "x", "value": dx}},
                    {"type": "rel", "data": {"axis": "y", "value": dy}},
                ]
            }),
        )?;
        Ok(())
    }

    /// Inject an **absolute** pointer position into the guest via QMP
    /// `input-send-event`. QEMU routes `abs` events to a device that registered
    /// absolute axes — the emulated `usb-tablet` — so this drives the
    /// Report-Protocol pointer path (Phase 92b B.2) distinct from the relative
    /// `usb-mouse`.
    ///
    /// `x` / `y` are **screen pixels**, not the 0..0x7FFF normalized range the
    /// name "absolute" usually implies. QEMU maps an `input-send-event` abs
    /// value into the target device's logical axis range, and `usb-tablet`'s
    /// logical range *is* 0..0x7FFF — so the mapping is the identity and the
    /// literal number here reaches the device. m3OS's `usb-hid` then reports
    /// the decoded value unscaled (`abs_position = (x, y)` raw), so the
    /// compositor hit-tests against exactly what was passed. Sending 0x4000
    /// would land at (16384, 16384): outside any real framebuffer, hitting no
    /// surface, and the event is silently dropped.
    pub fn send_pointer_abs(&mut self, x: i32, y: i32) -> Result<(), QmpError> {
        let _ = self.execute("input-send-event", abs_move_args(x, y))?;
        Ok(())
    }

    /// Phase 112 Track C.1 — press or release a pointer button in the
    /// guest via QMP `input-send-event`.
    ///
    /// `button` is a QEMU button name (`"left"`, `"middle"`, `"right"`).
    /// Press and release are separate calls so a caller can hold the
    /// button down across intervening motion events — which is exactly
    /// what a selection drag is, and why [`Self::press_key`]-style
    /// press-and-release-in-one would not work here.
    pub fn send_button(&mut self, button: &str, down: bool) -> Result<(), QmpError> {
        let _ = self.execute("input-send-event", button_event_args(button, down))?;
        Ok(())
    }

    /// Phase 112 Track C.1 — inject `|notches|` wheel events via QMP
    /// `input-send-event`. Positive `notches` is wheel-**up**, matching
    /// the `PointerEvent::wheel_dy` convention the guest sees.
    ///
    /// QEMU models a wheel notch as a button press+release of the
    /// pseudo-buttons `wheel-up` / `wheel-down`, so each notch is two
    /// events. A wheel only reaches the guest through a Report-protocol
    /// pointer (`usb-tablet`); the PS/2 path never carries one.
    pub fn send_wheel(&mut self, notches: i32) -> Result<(), QmpError> {
        for _ in 0..notches.unsigned_abs() {
            let _ = self.execute("input-send-event", wheel_notch_args(notches))?;
        }
        Ok(())
    }

    /// Phase 112 Track C.1 — drag from one absolute position to another
    /// with the left button held: press, a few interpolated motion
    /// samples, release.
    ///
    /// The intermediate samples matter — `term` extends a selection on
    /// motion events, so a press followed immediately by a release at a
    /// different spot would select nothing. Coordinates are screen pixels
    /// (see [`Self::send_pointer_abs`] for why the tablet path takes them
    /// literally).
    pub fn drag_abs(&mut self, from: (i32, i32), to: (i32, i32)) -> Result<(), QmpError> {
        self.drag_abs_with_mods(&[], from, to)
    }

    /// [`Self::drag_abs`] with one or more modifier keys held down for the
    /// whole gesture — press the modifiers, run the drag, release them.
    ///
    /// `mods` is a list of QEMU qcodes (`&["shift"]`, `&["alt"]`,
    /// `&["ctrl", "shift"]`). They go down before the button press and come
    /// back up after the release, so every button and motion event in
    /// between is stamped by the compositor with those modifiers — which is
    /// what selects `term`'s Shift-drag override (force a selection even
    /// while the application has mouse reporting on) and Alt block-select.
    ///
    /// Every step is separated by [`GESTURE_STEP_PACING`] — see that
    /// constant for why a back-to-back gesture cannot hold a modifier
    /// even though QEMU delivers every edge exactly as asked.
    ///
    /// The release half runs **even when the drag fails**. A modifier left
    /// latched in the guest is not a local failure: it silently corrupts
    /// every subsequent arm of the same gate run (a held Shift turns the
    /// next `type_text` into uppercase, the next wheel into a page scroll,
    /// and so on), so an error in the body must not skip the cleanup. The
    /// body's error is the one reported; a cleanup error only surfaces when
    /// the body succeeded.
    pub fn drag_abs_with_mods(
        &mut self,
        mods: &[&str],
        from: (i32, i32),
        to: (i32, i32),
    ) -> Result<(), QmpError> {
        let plan = modifier_drag_plan(mods, from, to);
        // Prologue and body share a fate: if a modifier press fails, the
        // gesture would not be the one the caller asked for, so don't drag.
        let mut outcome = Ok(());
        for step in plan.prologue.iter().chain(&plan.body) {
            if let Err(e) = self.run_gesture_step(step) {
                outcome = Err(e);
                break;
            }
        }
        // Unconditionally run the whole epilogue, including releases for
        // modifiers whose press never landed: a key-up for a key that is
        // not down is a no-op in the guest's modifier tracking, whereas
        // skipping one that *is* down leaks it. The extra release is the
        // strictly safer side.
        for step in &plan.epilogue {
            if let Err(e) = self.run_gesture_step(step)
                && outcome.is_ok()
            {
                outcome = Err(e);
            }
        }
        outcome
    }

    /// Dispatch one [`GestureStep`] to the injection method that speaks it,
    /// then wait [`GESTURE_STEP_PACING`] so the guest observes this edge
    /// before the next one is queued behind it.
    fn run_gesture_step(&mut self, step: &GestureStep) -> Result<(), QmpError> {
        let sent = match step {
            GestureStep::Key { qcode, down } => self.send_key_state(qcode, *down),
            GestureStep::MoveAbs { x, y } => self.send_pointer_abs(*x, *y),
            GestureStep::Button { button, down } => self.send_button(button, *down),
        };
        // Pace even after a failed step: a partially-emitted gesture still
        // has edges in flight, and the epilogue that follows must not race
        // them either.
        std::thread::sleep(GESTURE_STEP_PACING);
        sent
    }

    /// Type a literal ASCII string, one PS/2 keypress per character.
    /// Whitespace, punctuation, and shift-modified letters are mapped
    /// through [`ascii_to_qkeys`]. Characters outside the supported
    /// set are skipped with a warning written to stderr — the
    /// alternative (silently dropping) made the prior smoke harness
    /// pass with no keystrokes during a regression two phases ago.
    pub fn type_text(&mut self, text: &str) -> Result<(), QmpError> {
        for ch in text.chars() {
            let keys = match ascii_to_qkeys(ch) {
                Some(k) => k,
                None => {
                    eprintln!("qmp.type_text: skipping unsupported character {ch:?}");
                    continue;
                }
            };
            // Chord: shift+letter etc. all keys press together.
            let key_objs: Vec<Value> = keys
                .iter()
                .map(|qc| json!({"type": "qcode", "data": qc}))
                .collect();
            let _ = self.execute(
                "send-key",
                json!({
                    "keys": key_objs,
                    "hold-time": 30,
                }),
            )?;
            // Gap between keys. The m3OS graphical input path
            // (PS/2 controller's 1-byte output buffer → kbd_server →
            // display_server → term → PTY) RE-RENDERS a glyph per
            // keystroke; at a 5 ms gap a burst of scancodes (especially
            // the 4-scancode make/break of two CONSECUTIVE shift-chords
            // like `$(`) overruns the controller's single-byte buffer and
            // a shift-BREAK is lost → stuck shift → the rest of the line
            // corrupts (observed garbling `claude` into `CLAUDE` etc.).
            // 80 ms gives the guest time to drain each scancode before the
            // next, which empirically types long `export FOO="$(cat …)"`
            // command lines + a prompt into the claude TUI without drops.
            // Cost to the short less/htop probe commands is ~1–3 s — fine.
            std::thread::sleep(Duration::from_millis(80));
        }
        Ok(())
    }

    /// Capture the current framebuffer state to `path` as PPM. QEMU
    /// writes a P6 (binary) PPM with one byte per channel, no alpha;
    /// callers can read it back with [`crate::ppm::read_ppm`].
    pub fn screendump(&mut self, path: &Path) -> Result<(), QmpError> {
        // `format` was added in QEMU 7.1; older builds default to PPM
        // anyway, so omitting it is the more compatible choice. The
        // device selector defaults to the primary VGA console, which
        // is what `display_server` writes to via SYS_FB_*.
        let _ = self.execute(
            "screendump",
            json!({
                "filename": path.to_string_lossy(),
            }),
        )?;
        Ok(())
    }

    fn read_one_message(&mut self, timeout: Duration, what: &str) -> Result<Value, QmpError> {
        // Apply the dynamic timeout for this read. UnixStream's
        // `set_read_timeout` is per-stream, not per-read, but reading
        // a single line via `read_line` will return when the line ends
        // or the timeout fires — whichever comes first.
        if timeout.is_zero() {
            return Err(QmpError::Timeout(timeout, what.to_string()));
        }
        // Install the caller-supplied timeout on the stream. The
        // command path (`QmpClient::execute`) passes the remaining
        // budget from a 30 s deadline via
        // `deadline.saturating_duration_since(Instant::now())`, so
        // this value naturally shortens as the deadline approaches.
        // Surfaces a real `QmpError::Io` if setsockopt itself fails
        // instead of silently dropping the error.
        self.reader
            .get_ref()
            .set_read_timeout(Some(timeout))
            .map_err(QmpError::Io)?;

        let mut line = String::new();
        let n = self.reader.read_line(&mut line).map_err(|e| {
            // Per-stream read-timeout expiration surfaces as WouldBlock
            // on most Unixes (EAGAIN/EWOULDBLOCK from the underlying
            // recv) but is documented to surface as TimedOut on some
            // platforms; map both to QmpError::Timeout so callers get a
            // consistent diagnostic when the deadline expires.
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) {
                QmpError::Timeout(timeout, what.to_string())
            } else {
                QmpError::Io(e)
            }
        })?;
        if n == 0 {
            return Err(QmpError::Decode(format!("EOF while {what}")));
        }
        serde_json::from_str(line.trim()).map_err(|e| {
            QmpError::Decode(format!("bad JSON {what}: {e}; raw line: {}", line.trim()))
        })
    }
}

/// Translate one ASCII char into the chord of QEMU qcode names that
/// types it. Returns `None` for characters that have no fixed
/// QWERTY mapping.
///
/// The harness only needs to type shell commands like
/// `less /etc/passwd\n`. We cover lowercase ASCII letters, digits,
/// the punctuation that appears in canonical guest paths, and the
/// shift-modified versions where needed. Anything outside this set
/// (Unicode, control sequences) is the caller's responsibility to
/// inject via [`QmpClient::press_key`] directly.
pub fn ascii_to_qkeys(ch: char) -> Option<Vec<&'static str>> {
    match ch {
        'a'..='z' => letter(ch),
        'A'..='Z' => letter(ch.to_ascii_lowercase()).map(|mut v| {
            v.insert(0, "shift");
            v
        }),
        '0'..='9' => Some(vec![digit(ch)]),
        ' ' => Some(vec!["spc"]),
        '\n' | '\r' => Some(vec!["ret"]),
        '\t' => Some(vec!["tab"]),
        '/' => Some(vec!["slash"]),
        '.' => Some(vec!["dot"]),
        ',' => Some(vec!["comma"]),
        '-' => Some(vec!["minus"]),
        '=' => Some(vec!["equal"]),
        ';' => Some(vec!["semicolon"]),
        '\'' => Some(vec!["apostrophe"]),
        // Shift-modified punctuation we actually need.
        ':' => Some(vec!["shift", "semicolon"]),
        '_' => Some(vec!["shift", "minus"]),
        '"' => Some(vec!["shift", "apostrophe"]),
        '?' => Some(vec!["shift", "slash"]),
        '!' => Some(vec!["shift", "1"]),
        // Shell + prompt punctuation needed to type an `export FOO="$(cat …)"`
        // command and a `<<<NUMBER>>>`-style prompt over QMP (the claude-TUI
        // OpenRouter round-trip arm). Command substitution `$(cat …)` keeps the
        // credential off the captured framebuffer — only `$(cat path)` is typed.
        '$' => Some(vec!["shift", "4"]),
        '(' => Some(vec!["shift", "9"]),
        ')' => Some(vec!["shift", "0"]),
        '*' => Some(vec!["shift", "8"]),
        '<' => Some(vec!["shift", "comma"]),
        '>' => Some(vec!["shift", "dot"]),
        '@' => Some(vec!["shift", "2"]),
        '#' => Some(vec!["shift", "3"]),
        '%' => Some(vec!["shift", "5"]),
        '&' => Some(vec!["shift", "7"]),
        '+' => Some(vec!["shift", "equal"]),
        _ => None,
    }
}

fn letter(ch: char) -> Option<Vec<&'static str>> {
    Some(vec![LOWER_LETTER_QCODES[(ch as u8 - b'a') as usize]])
}

fn digit(ch: char) -> &'static str {
    DIGIT_QCODES[(ch as u8 - b'0') as usize]
}

const LOWER_LETTER_QCODES: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];

const DIGIT_QCODES: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// Counter used to generate per-run unique QMP socket paths so two
/// concurrent xtask invocations don't fight over the same path. The
/// path lives under `$TMPDIR`. Unlinking is the caller's
/// responsibility: the QEMU-launcher in `xtask/src/main.rs` removes
/// any stale file *before* spawning QEMU and removes the live socket
/// *after* QEMU exits on the happy path. A crashed harness or a
/// `kill -9` can leave the socket file behind; stray `.sock` files
/// in `$TMPDIR` are harmless and get reaped by the next `tmpwatch`
/// or reboot.
static QMP_SOCKET_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Phase 112 Track C.1 — build the `input-send-event` argument object for
/// one pointer-button edge. Split out from
/// [`QmpClient::send_button`] so the wire shape is assertable without a
/// live QEMU socket.
pub fn button_event_args(button: &str, down: bool) -> Value {
    json!({
        "events": [
            {"type": "btn", "data": {"down": down, "button": button}}
        ]
    })
}

/// Phase 112 Track C.1 — build the `input-send-event` argument object for
/// a single wheel notch. Positive `notches` selects `wheel-up`, negative
/// `wheel-down`; the magnitude is the caller's loop count, not part of
/// this payload (QEMU models one notch as a press+release pair).
pub fn wheel_notch_args(notches: i32) -> Value {
    let button = if notches >= 0 {
        "wheel-up"
    } else {
        "wheel-down"
    };
    json!({
        "events": [
            {"type": "btn", "data": {"down": true, "button": button}},
            {"type": "btn", "data": {"down": false, "button": button}},
        ]
    })
}

/// Phase 112 Track C.1 — build the `input-send-event` argument object for
/// one key edge. Unlike `send-key`, this event carries an explicit `down`
/// flag and emits only the edge asked for, so a modifier can stay held
/// across intervening pointer events. Split out from
/// [`QmpClient::send_key_state`] so the wire shape is assertable without a
/// live QEMU socket.
pub fn key_state_args(qcode: &str, down: bool) -> Value {
    json!({
        "events": [
            {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": qcode}}}
        ]
    })
}

/// One step of a pointer gesture, in the vocabulary [`QmpClient`]'s
/// injection methods speak.
///
/// A gesture is described as a list of these rather than as raw JSON so
/// [`QmpClient::run_gesture_step`] drives it through the same typed methods a
/// hand-written gate arm would call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GestureStep {
    /// One keyboard edge — a modifier going down or coming back up.
    Key { qcode: String, down: bool },
    /// An absolute pointer move, in screen pixels.
    MoveAbs { x: i32, y: i32 },
    /// One pointer-button edge.
    Button { button: String, down: bool },
}

/// The `input-send-event` `arguments` object a step puts on the wire.
///
/// Mirrors [`QmpClient::run_gesture_step`]'s dispatch: that method routes each
/// variant to the injection method which builds the same object. Keep the two
/// matches in step — this one is what lets a host test assert the exact bytes
/// of an ordered gesture without a live QEMU socket.
impl From<&GestureStep> for Value {
    fn from(step: &GestureStep) -> Value {
        match step {
            GestureStep::Key { qcode, down } => key_state_args(qcode, *down),
            GestureStep::MoveAbs { x, y } => abs_move_args(*x, *y),
            GestureStep::Button { button, down } => button_event_args(button, *down),
        }
    }
}

/// The ordered step sequence a modifier-held drag puts on the wire, split
/// into the three groups [`QmpClient::drag_abs_with_mods`] treats
/// differently.
///
/// Keeping the ordering in one pure function — rather than inline in the
/// executor — is what lets a host test assert it. Order is the whole
/// contract here: a drag that releases its modifier before the button
/// release still *emits* every event, but the compositor stamps the release
/// with no modifiers and `term` sees a plain drag.
pub struct DragPlan {
    /// Modifier key-downs, in the order the caller listed them.
    pub prologue: Vec<GestureStep>,
    /// The gesture itself: park, button down, interpolated motion, button up.
    pub body: Vec<GestureStep>,
    /// Modifier key-ups, in reverse press order (innermost released first,
    /// mirroring how a human lets go of a chord).
    pub epilogue: Vec<GestureStep>,
}

/// Phase 112 Track C.1 — build the step sequence for a left-button drag
/// from `from` to `to` with `mods` held down throughout.
///
/// The body parks the pointer first so the button press lands at `from`
/// rather than wherever the pointer happened to be, then walks `STEPS`
/// interpolated samples to `to`: `term` extends a selection on motion, so
/// the intermediate samples are what make the selection cover the span
/// instead of collapsing to a point. Coordinates are screen pixels (see
/// [`QmpClient::send_pointer_abs`]).
pub fn modifier_drag_plan(mods: &[&str], from: (i32, i32), to: (i32, i32)) -> DragPlan {
    const STEPS: i32 = 8;
    let key = |qc: &&str, down: bool| GestureStep::Key {
        qcode: (*qc).to_string(),
        down,
    };
    let button = |down: bool| GestureStep::Button {
        button: "left".to_string(),
        down,
    };

    let prologue = mods.iter().map(|qc| key(qc, true)).collect();
    let epilogue = mods.iter().rev().map(|qc| key(qc, false)).collect();

    let mut body: Vec<GestureStep> = Vec::with_capacity(STEPS as usize + 3);
    body.push(GestureStep::MoveAbs {
        x: from.0,
        y: from.1,
    });
    body.push(button(true));
    for step in 1..=STEPS {
        body.push(GestureStep::MoveAbs {
            x: from.0 + (to.0 - from.0) * step / STEPS,
            y: from.1 + (to.1 - from.1) * step / STEPS,
        });
    }
    body.push(button(false));

    DragPlan {
        prologue,
        body,
        epilogue,
    }
}

/// Build the `input-send-event` argument object for one absolute pointer
/// move. Shared by [`QmpClient::send_pointer_abs`] and
/// [`modifier_drag_plan`] so the two cannot drift apart.
fn abs_move_args(x: i32, y: i32) -> Value {
    json!({
        "events": [
            {"type": "abs", "data": {"axis": "x", "value": x}},
            {"type": "abs", "data": {"axis": "y", "value": y}},
        ]
    })
}

/// Allocate a fresh QMP socket path for the current `xtask` run.
/// The path is *not* created; we hand it to QEMU which `bind`s it,
/// and the connect helper waits for the listener to come up. See
/// `QMP_SOCKET_COUNTER` for the cleanup contract.
pub fn fresh_socket_path() -> PathBuf {
    let pid = std::process::id();
    let seq = QMP_SOCKET_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!("m3os-xtask-qmp-{pid}-{seq}.sock"));
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_lowercase_letters_decode() {
        for ch in 'a'..='z' {
            let keys = ascii_to_qkeys(ch).expect("letter");
            assert_eq!(keys.len(), 1);
        }
    }

    #[test]
    fn ascii_uppercase_letters_carry_shift() {
        let keys = ascii_to_qkeys('A').expect("upper A");
        assert_eq!(keys, vec!["shift", "a"]);
    }

    #[test]
    fn punctuation_for_path_and_command_lines() {
        assert_eq!(ascii_to_qkeys('/'), Some(vec!["slash"]));
        assert_eq!(ascii_to_qkeys('.'), Some(vec!["dot"]));
        assert_eq!(ascii_to_qkeys(' '), Some(vec!["spc"]));
        assert_eq!(ascii_to_qkeys('\n'), Some(vec!["ret"]));
        assert_eq!(ascii_to_qkeys(':'), Some(vec!["shift", "semicolon"]));
        assert_eq!(ascii_to_qkeys('_'), Some(vec!["shift", "minus"]));
    }

    #[test]
    fn shell_and_prompt_punctuation_carry_shift() {
        // The `export FOO="$(cat path)"` + `<<<NUMBER>>>` set used by the
        // claude-TUI OpenRouter round-trip arm.
        assert_eq!(ascii_to_qkeys('$'), Some(vec!["shift", "4"]));
        assert_eq!(ascii_to_qkeys('('), Some(vec!["shift", "9"]));
        assert_eq!(ascii_to_qkeys(')'), Some(vec!["shift", "0"]));
        assert_eq!(ascii_to_qkeys('*'), Some(vec!["shift", "8"]));
        assert_eq!(ascii_to_qkeys('<'), Some(vec!["shift", "comma"]));
        assert_eq!(ascii_to_qkeys('>'), Some(vec!["shift", "dot"]));
    }

    #[test]
    fn unknown_ascii_returns_none() {
        // Backslash and backtick remain unmapped (not needed by any arm).
        assert_eq!(ascii_to_qkeys('\\'), None);
        assert_eq!(ascii_to_qkeys('`'), None);
    }

    #[test]
    fn fresh_socket_paths_are_unique() {
        let a = fresh_socket_path();
        let b = fresh_socket_path();
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains("m3os-xtask-qmp-"));
    }

    // -----------------------------------------------------------------
    // Phase 112 Track C.1 — pointer button / wheel injection
    // -----------------------------------------------------------------

    /// C.1 acceptance: a button edge is one `btn` event carrying the
    /// button name and the press/release flag.
    #[test]
    fn button_event_wire_shape() {
        let down = button_event_args("left", true);
        assert_eq!(
            down,
            json!({"events": [{"type": "btn", "data": {"down": true, "button": "left"}}]})
        );
        let up = button_event_args("left", false);
        assert_eq!(up["events"][0]["data"]["down"], json!(false));
        // Middle-click is the other button the selection path may use.
        assert_eq!(
            button_event_args("middle", true)["events"][0]["data"]["button"],
            json!("middle")
        );
    }

    /// C.1 acceptance: a wheel notch is a press+release pair of QEMU's
    /// `wheel-up` / `wheel-down` pseudo-buttons, with the direction taken
    /// from the sign.
    #[test]
    fn wheel_notch_wire_shape_and_direction() {
        let up = wheel_notch_args(1);
        let events = up["events"].as_array().expect("events array");
        assert_eq!(events.len(), 2, "one notch is a press + a release");
        assert_eq!(events[0]["data"]["button"], json!("wheel-up"));
        assert_eq!(events[0]["data"]["down"], json!(true));
        assert_eq!(events[1]["data"]["button"], json!("wheel-up"));
        assert_eq!(events[1]["data"]["down"], json!(false));

        let down = wheel_notch_args(-3);
        assert_eq!(
            down["events"][0]["data"]["button"],
            json!("wheel-down"),
            "negative notches scroll down"
        );

        // Zero is treated as up; `send_wheel` loops |notches| times, so a
        // zero-notch call emits nothing at all.
        assert_eq!(
            wheel_notch_args(0)["events"][0]["data"]["button"],
            json!("wheel-up")
        );
    }

    /// A held modifier is one `key` event per edge, with the qcode nested
    /// under `key` (not flat like the `btn` events) and the `down` flag
    /// carrying the edge. `send-key` cannot express this — it always
    /// synthesises the release — which is why the helper exists.
    #[test]
    fn key_state_wire_shape() {
        assert_eq!(
            key_state_args("shift", true),
            json!({"events": [
                {"type": "key", "data": {"down": true, "key": {"type": "qcode", "data": "shift"}}}
            ]})
        );
        assert_eq!(
            key_state_args("alt", false),
            json!({"events": [
                {"type": "key", "data": {"down": false, "key": {"type": "qcode", "data": "alt"}}}
            ]})
        );
    }

    /// The exact ordered wire sequence a successful
    /// [`QmpClient::drag_abs_with_mods`] emits: prologue, body, epilogue.
    fn emitted_args(plan: &DragPlan) -> Vec<Value> {
        plan.prologue
            .iter()
            .chain(&plan.body)
            .chain(&plan.epilogue)
            .map(Value::from)
            .collect()
    }

    /// The modifier must go down before the button press and come up only
    /// after the button release — a plan that releases it early still emits
    /// every event, but the compositor stamps the button-up with no
    /// modifiers and `term` never sees the Shift-drag override.
    #[test]
    fn modifier_drag_holds_the_modifier_across_the_whole_gesture() {
        let plan = modifier_drag_plan(&["shift"], (10, 20), (90, 100));
        let seq = emitted_args(&plan);
        // 1 modifier down + park + button down + 8 motion samples
        // + button up + 1 modifier up.
        assert_eq!(seq.len(), 13, "sequence: {seq:#?}");

        assert_eq!(seq[0], key_state_args("shift", true), "modifier down first");
        assert_eq!(seq[1], abs_move_args(10, 20), "park at the drag origin");
        assert_eq!(
            seq[2],
            button_event_args("left", true),
            "button down after the modifier"
        );
        // Motion samples interpolate to the endpoint; the last one lands
        // exactly on `to` so the selection reaches the requested cell.
        assert_eq!(seq[3], abs_move_args(20, 30));
        assert_eq!(seq[10], abs_move_args(90, 100), "last sample hits `to`");
        assert_eq!(
            seq[11],
            button_event_args("left", false),
            "button up before the modifier"
        );
        assert_eq!(seq[12], key_state_args("shift", false), "modifier up last");
    }

    /// The release half is a mirror of the press half, so a chord unwinds
    /// innermost-first the way a human lets go of one.
    #[test]
    fn modifier_drag_releases_a_chord_in_reverse_press_order() {
        let plan = modifier_drag_plan(&["ctrl", "shift"], (0, 0), (8, 8));
        assert_eq!(
            plan.prologue.iter().map(Value::from).collect::<Vec<_>>(),
            vec![key_state_args("ctrl", true), key_state_args("shift", true)]
        );
        assert_eq!(
            plan.epilogue.iter().map(Value::from).collect::<Vec<_>>(),
            vec![
                key_state_args("shift", false),
                key_state_args("ctrl", false)
            ]
        );
    }

    /// `drag_abs` is the no-modifier case of the same plan, so an unqualified
    /// drag must emit exactly the pointer events and nothing else — no stray
    /// key edges that would perturb the guest's modifier state.
    #[test]
    fn unmodified_drag_emits_no_key_events() {
        let plan = modifier_drag_plan(&[], (60, 200), (1800, 1000));
        assert!(plan.prologue.is_empty());
        assert!(plan.epilogue.is_empty());
        let seq = emitted_args(&plan);
        assert_eq!(seq.len(), 11, "park + down + 8 samples + up");
        assert!(
            seq.iter()
                .all(|args| args["events"][0]["type"] != json!("key")),
            "an unqualified drag must not touch the keyboard"
        );
        assert_eq!(seq[0], abs_move_args(60, 200));
        assert_eq!(seq[10], button_event_args("left", false));
    }

    /// The gesture pacing must outlast the guest HID poller's worst-case
    /// sleep, or a modifier-qualified drag silently degrades into an
    /// unmodified one (see [`GESTURE_STEP_PACING`] for the full race).
    ///
    /// `usb-hid` backs off to `HID_POLL_MAX_IDLE_NS` = 100 ms when idle and
    /// only returns to its 5 ms cadence after a report lands, so the first
    /// edge of a gesture can wait a full 100 ms to be read. `xtask` does not
    /// depend on `kernel-core`, so the bound is restated here — if that
    /// backoff cap is ever raised, this test is the tripwire.
    #[test]
    fn gesture_pacing_outlasts_the_guest_hid_poll_backoff() {
        /// `kernel_core::input::hid_poll::HID_POLL_MAX_IDLE_NS`, in ms.
        const GUEST_HID_MAX_IDLE: Duration = Duration::from_millis(100);
        assert!(
            GESTURE_STEP_PACING > GUEST_HID_MAX_IDLE,
            "a gesture step must outlive the guest's {GUEST_HID_MAX_IDLE:?} idle poll \
             backoff, else the guest can read a later edge before an earlier one; \
             GESTURE_STEP_PACING is {GESTURE_STEP_PACING:?}"
        );
    }

    /// A 13-step Shift-drag costs 13 pacing gaps. Keep that visible: three
    /// drags is the whole gesture budget `term-daily-driver-smoke` spends,
    /// and a pacing bump that pushes it past a few seconds would start
    /// competing with the gate's own timeout rather than with the guest's
    /// poll loop.
    #[test]
    fn a_paced_drag_stays_within_a_few_seconds() {
        let plan = modifier_drag_plan(&["shift"], (60, 200), (1800, 1000));
        let steps = plan.prologue.len() + plan.body.len() + plan.epilogue.len();
        assert_eq!(steps, 13);
        assert!(
            GESTURE_STEP_PACING * (steps as u32) < Duration::from_secs(4),
            "a single paced drag must stay well under the per-arm settle budget"
        );
    }
}
