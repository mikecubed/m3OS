//! Phase 68 Track D.1 — Service manifest parser.
//!
//! `ServiceManifest` is the heap-backed Phase 68 reshape of the
//! fixed-size `ServiceDef` that lived inline in
//! `userspace/init/src/main.rs`. The legacy parser used a
//! `[FixedStr<MAX_NAME>; MAX_DEPS]` array for `depends`, which cannot
//! represent comma-separated dependency lists cleanly. The Phase 68
//! shape uses `Vec<String>` so a service can declare
//! `depends=kbd_server,display_server` and both names round-trip
//! unchanged.
//!
//! The parser accepts the same `key=value` config-file shape the
//! legacy init code consumes: one assignment per line, `#`
//! single-line comments, blank lines ignored. Unknown keys are
//! logged-and-skipped (the caller decides whether to surface the
//! warning).

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Default per-service restart budget used when the manifest does not
/// set `max_restart=`. Matches the legacy default in
/// `userspace/init/src/main.rs`.
pub const DEFAULT_MAX_RESTART: u32 = 10;

/// Default stop-timeout (seconds) before SIGKILL.
pub const DEFAULT_STOP_TIMEOUT_SECS: u32 = 5;

/// Service execution model.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ServiceType {
    /// Long-running daemon. Restart policy applies.
    #[default]
    Daemon,
    /// One-shot init action; not restarted.
    Oneshot,
}

/// Restart policy applied when a service exits.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RestartPolicy {
    /// Restart unconditionally.
    Always,
    /// Restart only when the exit status was non-zero.
    OnFailure,
    /// Never restart.
    #[default]
    Never,
}

/// Phase 68 Track E.1 — typed action the supervisor takes when a
/// service exhausts its restart budget. Distinct from
/// [`RestartPolicy`]: `RestartPolicy` decides whether to restart on a
/// single exit; `OnRestartAction` decides what to do after
/// `max_restart` failures.
///
/// The default is `LogAndContinue` — failing to set `on-restart=` in
/// the manifest yields the conservative behaviour the legacy init
/// code already applied.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OnRestartAction {
    /// Log an ERROR and leave the service in `PermanentlyStopped`.
    /// Matches the legacy init's behaviour.
    #[default]
    LogAndContinue,
    /// Escalate to `session_manager`'s text-fallback recovery path.
    /// Used for display-critical services so the system stays usable
    /// after a compositor crash.
    TextFallback,
    /// Treat exhaustion as a fatal init-stage failure. The supervisor
    /// calls the userspace-side fatal-exit path; on PID 1 this halts
    /// the kernel.
    Panic,
}

/// Heap-backed service manifest. Replaces the legacy fixed-size
/// `ServiceDef` for the Phase 68 manifest path.
#[derive(Clone, Debug)]
pub struct ServiceManifest {
    pub name: String,
    pub command: String,
    pub service_type: ServiceType,
    pub restart_policy: RestartPolicy,
    pub max_restart: u32,
    pub stop_timeout_secs: u32,
    pub run_as_uid: u32,
    /// Phase 68 — comma-separated dependency names. Empty `Vec` if no
    /// `depends=` line was present.
    pub depends: Vec<String>,
    /// Phase 68 Track E.1 — action when the supervisor exhausts the
    /// per-service restart budget.
    pub on_restart: OnRestartAction,
}

impl ServiceManifest {
    /// Build an empty manifest (every field at its default). Used by
    /// [`parse_manifest`] to incrementally populate fields as the
    /// parser walks the config file.
    pub fn empty() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            service_type: ServiceType::default(),
            restart_policy: RestartPolicy::default(),
            max_restart: DEFAULT_MAX_RESTART,
            stop_timeout_secs: DEFAULT_STOP_TIMEOUT_SECS,
            run_as_uid: 0,
            depends: Vec::new(),
            on_restart: OnRestartAction::default(),
        }
    }

    /// True iff the manifest has the minimum required fields
    /// (`name=` and `command=`).
    pub fn is_complete(&self) -> bool {
        !self.name.is_empty() && !self.command.is_empty()
    }
}

impl Default for ServiceManifest {
    fn default() -> Self {
        Self::empty()
    }
}

/// Diagnostic kinds the parser surfaces alongside a successful parse.
///
/// `parse_manifest` returns `(ServiceManifest, Vec<ParseWarning>)`;
/// the caller can route warnings to syslog without aborting on a
/// best-effort parse. Hard parse failures (missing `name=` /
/// `command=`) return `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseWarning {
    /// An `on-restart=` value did not match `log` / `text-fallback`
    /// / `panic`. The field defaults to [`OnRestartAction::LogAndContinue`].
    UnknownOnRestart(String),
    /// A `depends=` entry was empty (e.g. `depends=a,,b`).
    EmptyDependencyName,
    /// An unknown key=value pair was ignored.
    UnknownKey(String),
}

/// Parse a manifest config file. Returns `None` if `name=` or
/// `command=` is missing (the legacy contract); otherwise returns the
/// manifest plus any non-fatal warnings the caller may want to log.
///
/// `depends=` splits on commas with whitespace trimming, rejecting
/// empty names with [`ParseWarning::EmptyDependencyName`].
pub fn parse_manifest(buf: &[u8]) -> Option<(ServiceManifest, Vec<ParseWarning>)> {
    let mut svc = ServiceManifest::empty();
    let mut warnings: Vec<ParseWarning> = Vec::new();
    for line in buf.split(|b| *b == b'\n') {
        let line = trim_ascii(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let eq = match line.iter().position(|b| *b == b'=') {
            Some(i) => i,
            None => continue,
        };
        let key = trim_ascii(&line[..eq]);
        let val = trim_ascii(&line[eq + 1..]);
        let key_str = match core::str::from_utf8(key) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let val_str = match core::str::from_utf8(val) {
            Ok(s) => s,
            Err(_) => continue,
        };
        match key_str {
            "name" => svc.name = val_str.to_string(),
            "command" | "exec" => svc.command = val_str.to_string(),
            "type" => {
                svc.service_type = match val_str {
                    "oneshot" => ServiceType::Oneshot,
                    _ => ServiceType::Daemon,
                };
            }
            "restart" => {
                svc.restart_policy = match val_str {
                    "always" => RestartPolicy::Always,
                    "on-failure" => RestartPolicy::OnFailure,
                    _ => RestartPolicy::Never,
                };
            }
            "max_restart" => {
                if let Ok(v) = val_str.parse::<u32>() {
                    svc.max_restart = v;
                }
            }
            "stop_timeout" => {
                if let Ok(v) = val_str.parse::<u32>()
                    && v > 0
                {
                    svc.stop_timeout_secs = v;
                }
            }
            "user" => {
                if let Ok(v) = val_str.parse::<u32>() {
                    svc.run_as_uid = v;
                }
            }
            "depends" => {
                for entry in val_str.split(',') {
                    let trimmed = entry.trim();
                    if trimmed.is_empty() {
                        warnings.push(ParseWarning::EmptyDependencyName);
                        continue;
                    }
                    svc.depends.push(trimmed.to_string());
                }
            }
            "on-restart" => {
                svc.on_restart = match val_str {
                    "log" | "log-and-continue" => OnRestartAction::LogAndContinue,
                    "text-fallback" => OnRestartAction::TextFallback,
                    "panic" => OnRestartAction::Panic,
                    other => {
                        warnings.push(ParseWarning::UnknownOnRestart(other.to_string()));
                        OnRestartAction::LogAndContinue
                    }
                };
            }
            other => {
                warnings.push(ParseWarning::UnknownKey(other.to_string()));
            }
        }
    }
    if !svc.is_complete() {
        return None;
    }
    Some((svc, warnings))
}

fn trim_ascii(s: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && (s[start] == b' ' || s[start] == b'\t' || s[start] == b'\r') {
        start += 1;
    }
    while end > start && (s[end - 1] == b' ' || s[end - 1] == b'\t' || s[end - 1] == b'\r') {
        end -= 1;
    }
    &s[start..end]
}

/// Run DFS over the dependency graph implied by a slice of manifests
/// and report indices of every service that participates in a cycle
/// (including unresolvable references to a non-existent service).
///
/// Returns a `Vec<usize>` of indices whose manifests should not be
/// started — the caller marks each one `PermanentlyStopped`.
pub fn detect_cycles(manifests: &[ServiceManifest]) -> Vec<usize> {
    // Build name → index map.
    let mut name_idx: Vec<(String, usize)> = manifests
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name.clone(), i))
        .collect();
    name_idx.sort_by(|a, b| a.0.cmp(&b.0));
    let find = |name: &str| -> Option<usize> {
        name_idx
            .binary_search_by(|(n, _)| n.as_str().cmp(name))
            .ok()
            .map(|i| name_idx[i].1)
    };

    let n = manifests.len();
    let mut state: Vec<u8> = alloc::vec![0u8; n]; // 0 = unvisited, 1 = on stack, 2 = done
    let mut bad: Vec<usize> = Vec::new();
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        dfs(manifests, &find, start, &mut state, &mut bad);
    }
    bad.sort();
    bad.dedup();
    bad
}

fn dfs<F>(
    manifests: &[ServiceManifest],
    find: &F,
    node: usize,
    state: &mut [u8],
    bad: &mut Vec<usize>,
) where
    F: Fn(&str) -> Option<usize>,
{
    state[node] = 1;
    let m = &manifests[node];
    for dep_name in &m.depends {
        let dep_idx = match find(dep_name.as_str()) {
            Some(i) => i,
            None => {
                // Unresolvable dependency — the service cannot run.
                bad.push(node);
                continue;
            }
        };
        if state[dep_idx] == 1 {
            // Cycle: both nodes on the stack participate.
            bad.push(node);
            bad.push(dep_idx);
        } else if state[dep_idx] == 0 {
            dfs(manifests, find, dep_idx, state, bad);
            // If the dependency was marked bad downstream, this node is
            // also unrootable.
            if bad.contains(&dep_idx) {
                bad.push(node);
            }
        }
    }
    state[node] = 2;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Option<(ServiceManifest, Vec<ParseWarning>)> {
        parse_manifest(input.as_bytes())
    }

    #[test]
    fn parses_minimum_required_fields() {
        let (m, warns) = parse("name=kbd_server\ncommand=/bin/kbd_server\n").expect("complete");
        assert_eq!(m.name, "kbd_server");
        assert_eq!(m.command, "/bin/kbd_server");
        assert!(warns.is_empty());
    }

    #[test]
    fn missing_name_returns_none() {
        assert!(parse("command=/bin/x\n").is_none());
    }

    #[test]
    fn missing_command_returns_none() {
        assert!(parse("name=foo\n").is_none());
    }

    // Phase 68 Track D.1 acceptance — multi-service `depends=` parses
    // into a `Vec<String>` with both names preserved.
    #[test]
    fn parses_multi_service_depends() {
        let input =
            "name=mouse_server\ncommand=/bin/mouse_server\ndepends=kbd_server,display_server\n";
        let (m, warns) = parse(input).expect("complete");
        assert_eq!(m.depends.len(), 2);
        assert_eq!(m.depends[0], "kbd_server");
        assert_eq!(m.depends[1], "display_server");
        assert!(warns.is_empty());
    }

    #[test]
    fn depends_trims_whitespace_around_entries() {
        let (m, _w) = parse("name=a\ncommand=/x\ndepends= kbd_server , display_server \n").unwrap();
        assert_eq!(m.depends, ["kbd_server", "display_server"]);
    }

    #[test]
    fn empty_depends_entries_emit_warning_and_are_skipped() {
        let (m, warns) = parse("name=a\ncommand=/x\ndepends=a,,b\n").unwrap();
        assert_eq!(m.depends, ["a", "b"]);
        assert!(warns.contains(&ParseWarning::EmptyDependencyName));
    }

    #[test]
    fn on_restart_log_and_continue_is_default() {
        let (m, _w) = parse("name=a\ncommand=/x\n").unwrap();
        assert_eq!(m.on_restart, OnRestartAction::LogAndContinue);
    }

    #[test]
    fn on_restart_text_fallback_parses() {
        let (m, _w) = parse("name=a\ncommand=/x\non-restart=text-fallback\n").unwrap();
        assert_eq!(m.on_restart, OnRestartAction::TextFallback);
    }

    #[test]
    fn on_restart_panic_parses() {
        let (m, _w) = parse("name=a\ncommand=/x\non-restart=panic\n").unwrap();
        assert_eq!(m.on_restart, OnRestartAction::Panic);
    }

    #[test]
    fn on_restart_unknown_falls_back_to_log_and_warns() {
        let (m, warns) = parse("name=a\ncommand=/x\non-restart=teleport\n").unwrap();
        assert_eq!(m.on_restart, OnRestartAction::LogAndContinue);
        assert!(
            warns
                .iter()
                .any(|w| matches!(w, ParseWarning::UnknownOnRestart(s) if s == "teleport"))
        );
    }

    #[test]
    fn restart_on_failure_parses() {
        let (m, _w) = parse("name=a\ncommand=/x\nrestart=on-failure\n").unwrap();
        assert_eq!(m.restart_policy, RestartPolicy::OnFailure);
    }

    #[test]
    fn unknown_key_yields_warning() {
        let (_m, warns) = parse("name=a\ncommand=/x\nfoo=bar\n").unwrap();
        assert!(
            warns
                .iter()
                .any(|w| matches!(w, ParseWarning::UnknownKey(k) if k == "foo"))
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let input = "# header\nname=a\n\ncommand=/x\n# trailing\n";
        let (m, _w) = parse(input).unwrap();
        assert_eq!(m.name, "a");
    }

    #[test]
    fn exec_alias_for_command() {
        let (m, _w) = parse("name=a\nexec=/bin/y\n").unwrap();
        assert_eq!(m.command, "/bin/y");
    }

    // ---- detect_cycles --------------------------------------------------

    fn manifest(name: &str, deps: &[&str]) -> ServiceManifest {
        let mut m = ServiceManifest::empty();
        m.name = name.to_string();
        m.command = "/x".to_string();
        m.depends = deps.iter().map(|s| s.to_string()).collect();
        m
    }

    #[test]
    fn detect_cycles_on_acyclic_graph_is_empty() {
        let ms = [
            manifest("a", &[]),
            manifest("b", &["a"]),
            manifest("c", &["a", "b"]),
        ];
        assert!(detect_cycles(&ms).is_empty());
    }

    #[test]
    fn detect_cycles_flags_self_cycle() {
        let ms = [manifest("a", &["a"])];
        assert_eq!(detect_cycles(&ms), [0]);
    }

    #[test]
    fn detect_cycles_flags_two_node_cycle() {
        let ms = [manifest("a", &["b"]), manifest("b", &["a"])];
        let bad = detect_cycles(&ms);
        assert_eq!(bad, [0, 1]);
    }

    #[test]
    fn detect_cycles_flags_unresolvable_dependency() {
        let ms = [manifest("a", &["missing"])];
        assert_eq!(detect_cycles(&ms), [0]);
    }

    // ---- Phase 100 Track A.1 — builtin graphical-stack entries ----------
    //
    // Each test feeds the exact byte string from `BUILTIN_CONFIGS` in
    // `userspace/init/src/main.rs` through `parse_manifest` (the kernel-core
    // heap-backed equivalent of `parse_service_def` used in init). Both
    // parsers accept the same `key=value` line format, so this gives
    // host-testable coverage that the new entries are well-formed and carry
    // the correct dependency edges before a QEMU boot is needed.
    //
    // Rationale for placing tests here rather than in init/main.rs: init is
    // a `no_std` binary with no `#[cfg(test)]` module; kernel-core's
    // `parse_manifest` is the canonical, host-testable parser for the same
    // format. The byte strings below are kept byte-for-byte identical to the
    // BUILTIN_CONFIGS literals so a mismatch would surface immediately.

    #[test]
    fn builtin_display_server_parses() {
        // name=display (registered service name), command=/bin/display_server,
        // depends=kbd (matching data-disk display_server.conf).
        let cfg = "name=display\ncommand=/bin/display_server\ntype=daemon\nrestart=on-failure\nmax_restart=5\ndepends=kbd\n";
        let (m, warns) = parse(cfg).expect("display_server builtin must parse");
        assert_eq!(m.name, "display");
        assert_eq!(m.command, "/bin/display_server");
        assert_eq!(m.service_type, ServiceType::Daemon);
        assert_eq!(m.restart_policy, RestartPolicy::OnFailure);
        assert_eq!(m.depends, ["kbd"]);
        assert!(warns.is_empty());
    }

    #[test]
    fn builtin_mouse_server_parses() {
        // mouse_server: peer of kbd, no graphical dependency on the builtin
        // path (Phase 100 Track A.1 — starts before display_server so pointer
        // events are buffered early).
        let cfg = "name=mouse_server\ncommand=/bin/mouse_server\ntype=daemon\nrestart=on-failure\nmax_restart=5\n";
        let (m, warns) = parse(cfg).expect("mouse_server builtin must parse");
        assert_eq!(m.name, "mouse_server");
        assert_eq!(m.command, "/bin/mouse_server");
        assert_eq!(m.service_type, ServiceType::Daemon);
        assert_eq!(m.restart_policy, RestartPolicy::OnFailure);
        assert!(
            m.depends.is_empty(),
            "mouse_server must have no deps on builtin path"
        );
        assert!(warns.is_empty());
    }

    #[test]
    fn builtin_session_manager_parses() {
        // session_manager: session orchestrator, no explicit deps.
        let cfg = "name=session_manager\ncommand=/bin/session_manager\ntype=daemon\nrestart=on-failure\nmax_restart=3\n";
        let (m, warns) = parse(cfg).expect("session_manager builtin must parse");
        assert_eq!(m.name, "session_manager");
        assert_eq!(m.command, "/bin/session_manager");
        assert_eq!(m.service_type, ServiceType::Daemon);
        assert_eq!(m.restart_policy, RestartPolicy::OnFailure);
        assert!(m.depends.is_empty());
        assert!(warns.is_empty());
    }

    #[test]
    fn builtin_audio_server_parses() {
        // audio_server: ring-3 AC97/HDA driver; depends on display so the
        // compositor is up before audio claims the device.
        let cfg = "name=audio_server\ncommand=/drivers/audio_server\ntype=daemon\nrestart=on-failure\nmax_restart=3\ndepends=display\n";
        let (m, warns) = parse(cfg).expect("audio_server builtin must parse");
        assert_eq!(m.name, "audio_server");
        assert_eq!(m.command, "/drivers/audio_server");
        assert_eq!(m.depends, ["display"]);
        assert!(warns.is_empty());
    }

    #[test]
    fn builtin_greeter_parses_with_four_deps() {
        // greeter: GUI login manager. Four dependency edges — the exact MAX_DEPS
        // boundary for init's fixed-size dep array. All four must round-trip.
        let cfg = "name=greeter\ncommand=/bin/greeter\ntype=daemon\nrestart=on-failure\nmax_restart=3\ndepends=display,kbd,mouse_server,audio_server\n";
        let (m, warns) = parse(cfg).expect("greeter builtin must parse");
        assert_eq!(m.name, "greeter");
        assert_eq!(m.command, "/bin/greeter");
        assert_eq!(m.service_type, ServiceType::Daemon);
        assert_eq!(m.restart_policy, RestartPolicy::OnFailure);
        assert_eq!(m.max_restart, 3);
        // All four dependency edges must be present and in order.
        assert_eq!(m.depends.len(), 4);
        assert_eq!(m.depends[0], "display");
        assert_eq!(m.depends[1], "kbd");
        assert_eq!(m.depends[2], "mouse_server");
        assert_eq!(m.depends[3], "audio_server");
        assert!(warns.is_empty());
    }

    #[test]
    fn builtin_graphical_stack_dep_graph_has_no_cycles() {
        // Feed all five graphical-stack BUILTIN_CONFIGS entries into
        // detect_cycles together with their upstream dependencies. The dep
        // graph must be acyclic and every dependency reference must resolve.
        let ms = [
            manifest("console", &[]),
            manifest("kbd", &["console"]),
            manifest("display", &["kbd"]),
            manifest("mouse_server", &[]),
            manifest("session_manager", &[]),
            manifest("audio_server", &["display"]),
            manifest(
                "greeter",
                &["display", "kbd", "mouse_server", "audio_server"],
            ),
        ];
        let bad = detect_cycles(&ms);
        assert!(
            bad.is_empty(),
            "graphical-stack dep graph must be acyclic; bad indices: {bad:?}"
        );
    }
}
