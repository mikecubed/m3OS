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
    /// `usb-mouse`. Values are in QEMU's normalized 0..0x7FFF absolute range.
    pub fn send_pointer_abs(&mut self, x: i32, y: i32) -> Result<(), QmpError> {
        let _ = self.execute(
            "input-send-event",
            json!({
                "events": [
                    {"type": "abs", "data": {"axis": "x", "value": x}},
                    {"type": "abs", "data": {"axis": "y", "value": y}},
                ]
            }),
        )?;
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
    /// different spot would select nothing. Coordinates are in QEMU's
    /// normalized 0..0x7FFF absolute range (see [`Self::send_pointer_abs`]).
    pub fn drag_abs(&mut self, from: (i32, i32), to: (i32, i32)) -> Result<(), QmpError> {
        const STEPS: i32 = 8;
        self.send_pointer_abs(from.0, from.1)?;
        self.send_button("left", true)?;
        for step in 1..=STEPS {
            let x = from.0 + (to.0 - from.0) * step / STEPS;
            let y = from.1 + (to.1 - from.1) * step / STEPS;
            self.send_pointer_abs(x, y)?;
        }
        self.send_button("left", false)?;
        Ok(())
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

/// Allocate a fresh QMP socket path for the current `xtask` run.
/// The path is *not* created; we hand it to QEMU which `bind`s it,
/// and the connect helper waits for the listener to come up. See
/// `QMP_SOCKET_COUNTER` for the cleanup contract.
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
}
