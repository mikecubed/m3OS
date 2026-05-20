//! Phase 64a — pure-logic parsers for init's `/run/services.status` file
//! and the step-name → init-manifest-name mapping. Host-testable.
//!
//! The syscall wrappers that actually open the file and write to
//! `/run/init.cmd` live in the binary-side sibling module
//! `crate::init_proxy` (binary-only because they call into
//! `syscall_lib::open` / `read` / `write` / `close`, which the host
//! test build does not link).

/// Map a `kernel_core::session_supervisor::declared_session_step_names`
/// entry to the `name=` value init uses in its manifest (and therefore
/// in `/run/init.cmd` and `/run/services.status`).
///
/// Keep this in lock-step with the `populate_ext2_files` config writer
/// in `xtask/src/main.rs`. Distinct from `session_manager`'s
/// `ipc_service_name`: that one maps to the IPC registry name each
/// daemon binds under (e.g. `display_server`'s IPC service is
/// `"display"`); this one maps to init's manifest name. The two happen
/// to agree for some services and disagree for others.
pub fn init_service_name(step_name: &str) -> &'static str {
    match step_name {
        "display_server" => "display",
        "kbd_server" => "kbd",
        "mouse_server" => "mouse_server",
        "audio_server" => "audio_server",
        "greeter" => "greeter",
        // Phase 72b — `term` is no longer a supervised step. It is a
        // user-facing app launched via `[autostart]` in default boot
        // or by `greeter::execve("/bin/term")` after auth.
        _ => "",
    }
}

/// Init's view of one supervised service, parsed from
/// `/run/services.status`. Mirrors init's `ServiceStatus` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitServiceState {
    NeverStarted,
    Starting,
    Running,
    Stopping,
    /// Service exited; `code` is positive for a normal exit code and
    /// negative for a signal-induced exit (per init's `stopped:-N`
    /// encoding).
    Stopped(i32),
    PermanentlyStopped,
}

impl InitServiceState {
    /// True when init considers this service no longer to be running
    /// for any reason (clean exit, signal exit, or permanent stop).
    /// Used by the deferred-reply machinery to know when a stop has
    /// completed.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            InitServiceState::Stopped(_) | InitServiceState::PermanentlyStopped
        )
    }
}

/// One line of `/run/services.status` as parsed by
/// [`parse_status_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitServiceStatus {
    pub state: InitServiceState,
    pub pid: i32,
    pub restarts: u32,
}

/// Maximum bytes a caller should read from `/run/services.status`
/// before invoking [`parse_status_for`]. Each service line is ~64
/// bytes; with ~20 services the file stays under 1500 bytes. 4 KiB
/// leaves headroom for future growth without touching this code.
pub const STATUS_BUF_BYTES: usize = 4096;

/// Parse `/run/services.status` content and return the row for
/// `init_name`, or `None` if no row matches.
///
/// Allocation-free and robust to init writing the file mid-read.
pub fn parse_status_for(bytes: &[u8], init_name: &str) -> Option<InitServiceStatus> {
    let target = init_name.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = start;
        while end < bytes.len() && bytes[end] != b'\n' {
            end += 1;
        }
        let line = &bytes[start..end];
        if let Some(parsed) = parse_status_line(line, target) {
            return Some(parsed);
        }
        start = end + 1;
    }
    None
}

fn parse_status_line(line: &[u8], target: &[u8]) -> Option<InitServiceStatus> {
    let (name, rest) = split_once(line, b' ')?;
    if name != target {
        return None;
    }
    let (state_tok, rest) = split_once(rest, b' ').unwrap_or((rest, b""));
    let state = parse_state(state_tok)?;
    let mut pid: i32 = 0;
    let mut restarts: u32 = 0;
    for field in split_fields(rest, b' ') {
        if let Some(val) = field.strip_prefix(b"pid=") {
            pid = parse_i32(val).unwrap_or(0);
        } else if let Some(val) = field.strip_prefix(b"restarts=") {
            restarts = parse_u32(val).unwrap_or(0);
        }
    }
    Some(InitServiceStatus {
        state,
        pid,
        restarts,
    })
}

fn parse_state(tok: &[u8]) -> Option<InitServiceState> {
    if tok == b"never-started" {
        Some(InitServiceState::NeverStarted)
    } else if tok == b"starting" {
        Some(InitServiceState::Starting)
    } else if tok == b"running" {
        Some(InitServiceState::Running)
    } else if tok == b"stopping" {
        Some(InitServiceState::Stopping)
    } else if tok == b"permanently-stopped" {
        Some(InitServiceState::PermanentlyStopped)
    } else if let Some(rest) = tok.strip_prefix(b"stopped:") {
        let code = parse_i32(rest)?;
        Some(InitServiceState::Stopped(code))
    } else {
        None
    }
}

fn split_once(bytes: &[u8], sep: u8) -> Option<(&[u8], &[u8])> {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == sep {
            return Some((&bytes[..i], &bytes[i + 1..]));
        }
        i += 1;
    }
    None
}

struct FieldIter<'a> {
    bytes: &'a [u8],
    sep: u8,
}

impl<'a> Iterator for FieldIter<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        if self.bytes.is_empty() {
            return None;
        }
        match split_once(self.bytes, self.sep) {
            Some((head, tail)) => {
                self.bytes = tail;
                Some(head)
            }
            None => {
                let head = self.bytes;
                self.bytes = &[];
                Some(head)
            }
        }
    }
}

fn split_fields(bytes: &[u8], sep: u8) -> FieldIter<'_> {
    FieldIter { bytes, sep }
}

fn parse_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    let mut n: u32 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(n)
}

fn parse_i32(bytes: &[u8]) -> Option<i32> {
    if bytes.is_empty() {
        return None;
    }
    if bytes[0] == b'-' {
        let val = parse_u32(&bytes[1..])?;
        if val > i32::MAX as u32 + 1 {
            return None;
        }
        Some(-(val as i32))
    } else {
        let val = parse_u32(bytes)?;
        if val > i32::MAX as u32 {
            return None;
        }
        Some(val as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_returns_running_with_pid() {
        let sample = b"display running pid=42 restarts=0 changed=1234\n\
                       term running pid=99 restarts=1 changed=1235\n";
        let s = parse_status_for(sample, "display").expect("display row");
        assert_eq!(s.state, InitServiceState::Running);
        assert_eq!(s.pid, 42);
        assert_eq!(s.restarts, 0);
    }

    #[test]
    fn parse_status_handles_signal_exit_with_negative_code() {
        let sample = b"audio_server stopped:-15 pid=0 restarts=2 changed=1234\n";
        let s = parse_status_for(sample, "audio_server").expect("row");
        assert_eq!(s.state, InitServiceState::Stopped(-15));
        assert_eq!(s.pid, 0);
        assert_eq!(s.restarts, 2);
    }

    #[test]
    fn parse_status_handles_permanently_stopped() {
        let sample = b"display permanently-stopped pid=0 restarts=3 changed=1234\n";
        let s = parse_status_for(sample, "display").expect("row");
        assert_eq!(s.state, InitServiceState::PermanentlyStopped);
        assert!(s.state.is_terminal());
    }

    #[test]
    fn parse_status_returns_none_for_missing_service() {
        let sample = b"display running pid=42 restarts=0 changed=1234\n";
        assert!(parse_status_for(sample, "term").is_none());
    }

    #[test]
    fn parse_status_returns_none_for_empty_buffer() {
        assert!(parse_status_for(b"", "display").is_none());
    }

    #[test]
    fn parse_status_tolerates_unterminated_last_line() {
        let sample = b"display running pid=42 restarts=0 changed=1234\nter";
        let s = parse_status_for(sample, "display").expect("display row");
        assert_eq!(s.state, InitServiceState::Running);
    }

    #[test]
    fn parse_status_distinguishes_starting_and_running() {
        let sample = b"display starting pid=0 restarts=0 changed=1\n\
                       term running pid=12 restarts=0 changed=2\n";
        assert_eq!(
            parse_status_for(sample, "display").unwrap().state,
            InitServiceState::Starting
        );
        assert_eq!(
            parse_status_for(sample, "term").unwrap().state,
            InitServiceState::Running
        );
    }

    #[test]
    fn init_service_name_maps_step_names_to_init_manifest_names() {
        assert_eq!(init_service_name("display_server"), "display");
        assert_eq!(init_service_name("kbd_server"), "kbd");
        assert_eq!(init_service_name("mouse_server"), "mouse_server");
        assert_eq!(init_service_name("audio_server"), "audio_server");
        assert_eq!(init_service_name("greeter"), "greeter");
        // Phase 72b — `term` is no longer a supervised step.
        assert_eq!(init_service_name("term"), "");
        assert_eq!(init_service_name("nonexistent"), "");
    }

    #[test]
    fn terminal_predicate_only_true_for_stopped_states() {
        assert!(!InitServiceState::Running.is_terminal());
        assert!(!InitServiceState::Starting.is_terminal());
        assert!(!InitServiceState::Stopping.is_terminal());
        assert!(InitServiceState::Stopped(0).is_terminal());
        assert!(InitServiceState::Stopped(-15).is_terminal());
        assert!(InitServiceState::PermanentlyStopped.is_terminal());
    }
}
