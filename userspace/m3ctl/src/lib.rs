//! `m3ctl` — Phase 56 / 57 control-socket client library.
//!
//! The binary is a thin shell that parses argv, dispatches via
//! [`parse_verb`], and prints the reply. Every parser path that does
//! not depend on `_start` lives here so it is host-testable via
//! `cargo test -p m3ctl --target x86_64-unknown-linux-gnu`.
//!
//! # Phase 57 Track I.2 — session control verbs
//!
//! Phase 57 closes the F.5 → I.2 client-side deferral by adding three
//! new verbs that reach the `session_manager` daemon's control socket
//! at `/run/m3os/session.sock`:
//!
//! - `m3ctl session-state`   — returns the current
//!   [`kernel_core::session::SessionState`] as a printable string.
//! - `m3ctl session-stop`    — graceful shutdown (falls through to
//!   `text-fallback`).
//! - `m3ctl session-restart` — graceful stop + start.
//!
//! All three reuse [`kernel_core::session_control`] for the codec — no
//! parallel byte definitions live in this crate (DRY).
//!
//! # Capability gate
//!
//! Per the Phase 57 F.5 design, the session control surface is gated
//! by [`kernel_core::session_control::ControlSocketCap`], minted at
//! `session_manager` startup and granted only to `m3ctl`. The binary
//! presents the cap implicitly — possession of the cap is the gate;
//! the parser surfaced here is cap-agnostic so it stays
//! host-testable.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use kernel_core::display::control::{ControlCommand, EventKind, SurfaceId};
use kernel_core::session_control::ControlVerb;

// ---------------------------------------------------------------------------
// Service-registry names + IPC labels — must match the daemons.
// ---------------------------------------------------------------------------

/// Service-registry name of the `display_server` control endpoint.
pub const DISPLAY_CONTROL_SERVICE_NAME: &str = "display-control";

/// Service-registry name of the `session_manager` control endpoint
/// (Phase 57 F.5). Mirrors
/// `userspace::session_manager::control::CONTROL_SERVICE_NAME`.
pub const SESSION_CONTROL_SERVICE_NAME: &str = "session-control";

/// IPC label for an encoded `display_server::control::ControlCommand`.
/// Mirrors `display_server::control::LABEL_CTL_CMD`.
pub const LABEL_DISPLAY_CTL_CMD: u64 = 1;

/// IPC label for an encoded
/// `kernel_core::session_control::ControlVerb`. Mirrors
/// `userspace::session_manager::control::LABEL_CTL_CMD`.
pub const LABEL_SESSION_CTL_CMD: u64 = 1;

// ---------------------------------------------------------------------------
// Parsed verb — unified across display and session targets.
// ---------------------------------------------------------------------------

/// Parsed CLI verb. Each variant carries the typed payload the binary
/// dispatcher needs to emit the correct IPC `call`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedVerb {
    /// A Phase 56 `display-control` verb. The binary looks up the
    /// `display-control` service, encodes via
    /// `kernel_core::display::control::encode_command`, and parses the
    /// reply via `decode_event`.
    Display(ControlCommand),
    /// A Phase 57 `session-control` verb. The binary looks up the
    /// `session-control` service, encodes via
    /// `kernel_core::session_control::encode_verb`, and parses the
    /// reply via `decode_reply`.
    Session(ControlVerb),
    /// Phase 73 — `m3ctl lock`. Spawns `/bin/lockscreen` directly via
    /// `fork + execve`. The lockscreen client requests an exclusive-
    /// keyboard Layer surface from the compositor; the compositor's
    /// existing focus dispatcher honours the grab. A second `m3ctl
    /// lock` while a lockscreen is already running is observed by the
    /// new lockscreen exiting immediately when it cannot register the
    /// singleton `"lockscreen"` IPC service name.
    LockScreen,
    /// Phase 81 (D.2) — `m3ctl wifi status`. The binary looks up the
    /// `wifi.control` service the mt792x driver exposes, sends a
    /// `WIFI_STATUS` query over the userspace Wi-Fi control protocol
    /// (`wifi_core::control`), and prints the associated SSID, RSSI, and
    /// assigned IPv4 — or "not associated" when no link is up.
    WifiStatus,
    /// Phase 84 (D.3) — `m3ctl mitigations status`. The binary calls the
    /// m3OS-native `SYS_MITIGATIONS_STATUS` syscall, decodes the boot
    /// [`kernel_core::spectre::MitigationReport`], and prints the per-vuln
    /// status, the compiled-in retpoline line, the UNADDRESSED classes, and
    /// the Grimsdal microkernel-isolation caveat.
    MitigationsStatus,
    /// Phase 103 (A.5) — `m3ctl power status`. Queries `powerd`'s
    /// `power` service (`kernel_core::power::control`) and prints the
    /// AC/battery snapshot.
    PowerStatus,
    /// Phase 103 (A.5) — `m3ctl battery`: the battery-focused view of
    /// the same `POWER_STATUS` query.
    Battery,
}

/// Service-registry name of the mt792x Wi-Fi driver's userspace control
/// endpoint (Phase 81 D.2). The scan/connect/status protocol flows
/// driver ↔ `m3ctl` here, never through the kernel `RemoteNic` facade.
pub const WIFI_CONTROL_SERVICE_NAME: &str = "wifi.control";

/// Message printed by `m3ctl wifi status` when the driver reports it is not
/// associated (or no Wi-Fi driver is present).
pub const WIFI_NOT_ASSOCIATED_MSG: &str = "wifi: not associated";

/// Message printed by the power verbs when `powerd` is not running.
pub const POWER_UNAVAILABLE_MSG: &str = "power: powerd not running";

/// Render a [`kernel_core::power::control::PowerStatusWire`] for
/// `m3ctl power status` (pure formatter — host-tested).
pub fn format_power_status(status: &kernel_core::power::control::PowerStatusWire) -> String {
    use kernel_core::power::control::AcState;
    let mut out = String::new();
    out.push_str("power:\n  ac: ");
    out.push_str(match status.ac {
        AcState::Online => "online",
        AcState::Offline => "offline",
        AcState::AssumedOnline => "assumed-online (no adapter device)",
    });
    out.push('\n');
    out.push_str("  battery: ");
    if status.battery_present {
        out.push_str(&format_battery_line(status));
    } else {
        out.push_str("none");
    }
    out.push('\n');
    out.push_str("  thermal: ");
    out.push_str(&format_thermal_field(status));
    out.push('\n');
    out.push_str("  governor: ");
    out.push_str(&format_governor_field(status));
    out.push('\n');
    out
}

/// The Phase 103 C thermal field: `none (no zones)` on QEMU/desktop, or
/// `<state>, 42.1 C` with the hottest zone's reading.
fn format_thermal_field(status: &kernel_core::power::control::PowerStatusWire) -> String {
    use alloc::string::ToString;
    use kernel_core::power::control::{TEMP_UNKNOWN_DECI_C, ThermalWire};
    if status.thermal == ThermalWire::NoZones {
        return String::from("none (no zones)");
    }
    let mut line = String::from(status.thermal.as_str());
    if status.temp_deci_c != TEMP_UNKNOWN_DECI_C {
        line.push_str(", ");
        line.push_str(&(status.temp_deci_c / 10).to_string());
        line.push('.');
        line.push_str(&(status.temp_deci_c % 10).unsigned_abs().to_string());
        line.push_str(" C");
    }
    line
}

/// The Phase 103 E governor field: mode, mechanism, and the last target
/// on the abstract 1–255 scale.
fn format_governor_field(status: &kernel_core::power::control::PowerStatusWire) -> String {
    use alloc::string::ToString;
    let mut line = String::from(status.governor.as_str());
    line.push_str(" (mech ");
    line.push_str(status.mech.as_str());
    line.push_str(", target ");
    line.push_str(&status.perf.to_string());
    line.push(')');
    line
}

/// Render the battery view for `m3ctl battery`.
pub fn format_battery(status: &kernel_core::power::control::PowerStatusWire) -> String {
    let mut out = String::new();
    out.push_str("battery: ");
    if status.battery_present {
        out.push_str(&format_battery_line(status));
    } else {
        out.push_str("none");
    }
    out.push('\n');
    out
}

fn format_battery_line(status: &kernel_core::power::control::PowerStatusWire) -> String {
    use alloc::string::ToString;
    use kernel_core::power::battery::{BST_STATE_CHARGING, BST_STATE_DISCHARGING};
    use kernel_core::power::control::PERCENT_UNKNOWN;
    let mut line = String::new();
    if status.percent == PERCENT_UNKNOWN {
        line.push_str("present (percent unknown)");
    } else {
        line.push_str(&status.percent.to_string());
        line.push('%');
    }
    if status.state & BST_STATE_CHARGING != 0 {
        line.push_str(" charging");
    } else if status.state & BST_STATE_DISCHARGING != 0 {
        line.push_str(" discharging");
    }
    line
}

/// Render a [`wifi_core::control::WifiStatus`] into the human-readable lines
/// `m3ctl wifi status` prints. Pure + host-tested (`wifi_status_format`).
pub fn format_wifi_status(status: &wifi_core::control::WifiStatus) -> String {
    use alloc::string::ToString;
    let ssid = match core::str::from_utf8(&status.ssid) {
        Ok(s) if !s.is_empty() => s,
        _ => "(hidden)",
    };
    let mut out = String::new();
    out.push_str("wifi: associated\n");
    out.push_str("  ssid: ");
    out.push_str(ssid);
    out.push('\n');
    out.push_str("  signal: ");
    out.push_str(&status.rssi.to_string());
    out.push_str(" dBm\n");
    out.push_str("  ipv4: ");
    out.push_str(&status.ipv4[0].to_string());
    out.push('.');
    out.push_str(&status.ipv4[1].to_string());
    out.push('.');
    out.push_str(&status.ipv4[2].to_string());
    out.push('.');
    out.push_str(&status.ipv4[3].to_string());
    out.push('\n');
    out
}

/// Render a [`kernel_core::spectre::MitigationReport`] into the human-readable
/// `m3ctl mitigations status` output. Pure + host-tested
/// (`mitigations_status_format`).
///
/// Per-vuln status comes from the host-tested `report_map` (so Meltdown tracks
/// the *actual* KPTI state, never a false "Mitigated"). Retpoline is reported
/// separately as compiled-in (it cannot be disabled at boot, B.1). The
/// UNADDRESSED classes and the Grimsdal caveat are always printed so a deferred
/// class can never silently read as covered.
pub fn format_mitigations(report: &kernel_core::spectre::MitigationReport) -> String {
    use kernel_core::spectre::{MitigationLevel, Status};
    let mut out = String::new();

    out.push_str("mitigations: level=");
    out.push_str(match report.level {
        MitigationLevel::Off => "off",
        MitigationLevel::Auto => "auto",
        MitigationLevel::Full => "full",
    });
    if !report.level_recognized {
        out.push_str(" (unrecognized value → auto)");
    }
    out.push('\n');

    for (vuln, status) in report.vuln_map().iter() {
        out.push_str("  ");
        out.push_str(vuln.name());
        out.push_str(": ");
        match status {
            Status::NotAffected => out.push_str("Not affected"),
            Status::Vulnerable => out.push_str("Vulnerable"),
            Status::Mitigated(name) => {
                out.push_str("Mitigation: ");
                out.push_str(name);
            }
            Status::Unaddressed => out.push_str("UNADDRESSED"),
        }
        out.push('\n');
    }

    // Retpoline is compile-time-unconditional (B.1): reported separately from
    // the runtime-gated KPTI/IBRS lines so a reader does not expect a switch.
    out.push_str("  Spectre-v2 (retpoline): compiled-in (cannot disable at boot)\n");

    // Phase 90a C.2 — W^X policy version + PKU posture. The W^X enforcement
    // points are always active (Phase 75); the *version* reflects whether the
    // pkey-guarded W+X exception is available, which is exactly "PKU active on
    // this boot". On the default no-PKU lane this reads `v1 (PKU absent)`; on a
    // PKU host (e.g. under KVM) it reads `v2 (PKU present, active)`. A
    // present-but-inactive CPU (PKU silicon the kernel did not enable) reads
    // `v1 (PKU present, inactive)`.
    out.push_str("  W^X: ");
    out.push_str(if report.wx_v2 { "v2" } else { "v1" });
    out.push_str(" (PKU ");
    match (report.pku_present, report.pku_active) {
        (true, true) => out.push_str("present, active"),
        (true, false) => out.push_str("present, inactive"),
        // `pku_active` without `pku_present` is not a state the kernel can
        // produce (active implies present), but render it defensively.
        (false, true) => out.push_str("active"),
        (false, false) => out.push_str("absent"),
    }
    out.push_str(")\n");

    // Honesty: enumerate the UNADDRESSED classes + the microkernel caveat.
    out.push_str(
        "note: UNADDRESSED — Spectre-v1, MDS, L1TF, SSB, Retbleed, Downfall/GDS are not mitigated.\n",
    );
    out.push_str(
        "note: ring-3 driver isolation does not by itself mitigate Spectre between userspace \
         components (Grimsdal et al., NordSec 2019); m3OS makes no claim of freedom from \
         microarchitectural timing channels (seL4 verification-scope framing).\n",
    );
    out
}

/// Parser-level error. Variants are *data*; callers `match` to surface
/// the right human-readable diagnostic. `String` is the parser's
/// surface-level error message — the binary prints it then exits with
/// code 2 (usage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The verb name was not recognized (e.g., a typo). Carries the
    /// offending string so the diagnostic can echo it back.
    UnknownVerb(String),
    /// A required argument was missing.
    MissingArgument(&'static str),
    /// An argument failed to parse (e.g., not a u32).
    BadArgument(&'static str),
    /// Unknown event-kind name to `subscribe`.
    UnknownEventKind(String),
}

// ---------------------------------------------------------------------------
// Top-level parser
// ---------------------------------------------------------------------------

/// Parse `verb` + `args` into a typed [`ParsedVerb`].
///
/// Phase 57 I.2 adds the three `session-*` verbs alongside the existing
/// Phase 56 `display-control` verbs. The session arms produce
/// [`ParsedVerb::Session`] payloads carrying the matching
/// [`ControlVerb`] discriminant; the binary's session dispatcher
/// encodes via [`kernel_core::session_control::encode_verb`].
pub fn parse_verb(verb: &str, args: &[&str]) -> Result<ParsedVerb, ParseError> {
    match verb {
        // Phase 57 I.2 — session control verbs. Phase 64a adds the
        // `--detailed` flag to `session-state` to opt into the
        // per-service `ServiceStates` reply; the bare form continues
        // to return the session-wide `SessionState` for back-compat.
        "session-state" => {
            if args.iter().any(|a| *a == "--detailed") {
                Ok(ParsedVerb::Session(ControlVerb::SessionStateDetailed))
            } else {
                Ok(ParsedVerb::Session(ControlVerb::SessionState))
            }
        }
        "session-stop" => Ok(ParsedVerb::Session(ControlVerb::SessionStop)),
        // Phase 64a: `session-restart <name>` restarts a single
        // declared service via init delegation. `session-restart` with
        // no arg keeps the Phase 57 whole-session semantics.
        "session-restart" => match args.iter().find(|a| !a.starts_with("--")) {
            None => Ok(ParsedVerb::Session(ControlVerb::SessionRestart)),
            Some(name) => ControlVerb::new_session_restart_service(name)
                .map(ParsedVerb::Session)
                .map_err(|_| {
                    ParseError::BadArgument("session-restart: service name must be 1..=32 bytes")
                }),
        },
        // Phase 73 — local "lock" verb spawns the lockscreen client.
        // Implemented as a direct fork+execve rather than a control
        // command so it works on any boot mode without extending the
        // wire protocol.
        "lock" => Ok(ParsedVerb::LockScreen),
        // Phase 81 (D.2) — `m3ctl wifi status` (read-only diagnostics).
        "wifi" => match args.first().copied() {
            Some("status") => Ok(ParsedVerb::WifiStatus),
            Some(_) => Err(ParseError::BadArgument("wifi: only `status` is supported")),
            None => Err(ParseError::MissingArgument("wifi: expected `status`")),
        },
        // Phase 84 (D.3) — `m3ctl mitigations status` (read-only diagnostics).
        "mitigations" => match args.first().copied() {
            Some("status") => Ok(ParsedVerb::MitigationsStatus),
            Some(_) => Err(ParseError::BadArgument(
                "mitigations: only `status` is supported",
            )),
            None => Err(ParseError::MissingArgument(
                "mitigations: expected `status`",
            )),
        },
        // Phase 103 (A.5) — `m3ctl power status` / `m3ctl battery`.
        "power" => match args.first().copied() {
            Some("status") => Ok(ParsedVerb::PowerStatus),
            Some(_) => Err(ParseError::BadArgument("power: only `status` is supported")),
            None => Err(ParseError::MissingArgument("power: expected `status`")),
        },
        "battery" => Ok(ParsedVerb::Battery),
        // Phase 56 — display control verbs.
        "version" => Ok(ParsedVerb::Display(ControlCommand::Version)),
        "list-surfaces" => Ok(ParsedVerb::Display(ControlCommand::ListSurfaces)),
        "frame-stats" => Ok(ParsedVerb::Display(ControlCommand::FrameStats)),
        "focus" => {
            let id_str = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("focus requires <surface-id>"))?;
            let id = parse_u32(id_str)
                .ok_or(ParseError::BadArgument("focus: surface-id must be a u32"))?;
            Ok(ParsedVerb::Display(ControlCommand::Focus {
                surface_id: SurfaceId(id),
            }))
        }
        "register-bind" => {
            let mask_str = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("register-bind requires <mask>"))?;
            let mask = parse_u16(mask_str).ok_or(ParseError::BadArgument(
                "register-bind: mask must fit in u16",
            ))?;
            let kc_str = args.get(1).copied().ok_or(ParseError::MissingArgument(
                "register-bind requires <keycode>",
            ))?;
            let kc = parse_u32(kc_str).ok_or(ParseError::BadArgument(
                "register-bind: keycode must be a u32",
            ))?;
            Ok(ParsedVerb::Display(ControlCommand::RegisterBind {
                modifier_mask: mask,
                keycode: kc,
            }))
        }
        "unregister-bind" => {
            let mask_str = args.first().copied().ok_or(ParseError::MissingArgument(
                "unregister-bind requires <mask>",
            ))?;
            let mask = parse_u16(mask_str).ok_or(ParseError::BadArgument(
                "unregister-bind: mask must fit in u16",
            ))?;
            let kc_str = args.get(1).copied().ok_or(ParseError::MissingArgument(
                "unregister-bind requires <keycode>",
            ))?;
            let kc = parse_u32(kc_str).ok_or(ParseError::BadArgument(
                "unregister-bind: keycode must be a u32",
            ))?;
            Ok(ParsedVerb::Display(ControlCommand::UnregisterBind {
                modifier_mask: mask,
                keycode: kc,
            }))
        }
        "subscribe" => {
            let name = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("subscribe requires <kind>"))?;
            let kind = parse_event_kind(name)
                .ok_or_else(|| ParseError::UnknownEventKind(String::from(name)))?;
            Ok(ParsedVerb::Display(ControlCommand::Subscribe {
                event_kind: kind,
            }))
        }
        // Phase 72 — tiling / workspace verbs.
        //
        // Each maps to a single `ControlCommand` variant whose
        // codec round-trips through `kernel-core::display::protocol`.
        "layout" => {
            let name = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("layout requires <name>"))?;
            let kind = parse_policy_kind(name)
                .ok_or(ParseError::BadArgument("layout: unknown policy name"))?;
            Ok(ParsedVerb::Display(ControlCommand::SetLayout { kind }))
        }
        "workspace" => {
            let action = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("workspace requires <action>"))?;
            match action {
                "switch" => {
                    let n_str = args
                        .get(1)
                        .copied()
                        .ok_or(ParseError::MissingArgument("workspace switch requires <n>"))?;
                    let n = parse_u8(n_str)
                        .ok_or(ParseError::BadArgument("workspace switch: n must be 1..=9"))?;
                    if !(1..=9).contains(&n) {
                        return Err(ParseError::BadArgument("workspace switch: n must be 1..=9"));
                    }
                    Ok(ParsedVerb::Display(ControlCommand::SwitchWorkspace { n }))
                }
                _ => Err(ParseError::BadArgument(
                    "workspace: unknown action (expected 'switch')",
                )),
            }
        }
        "move-to-workspace" => {
            let n_str = args.first().copied().ok_or(ParseError::MissingArgument(
                "move-to-workspace requires <n>",
            ))?;
            let n = parse_u8(n_str).ok_or(ParseError::BadArgument(
                "move-to-workspace: n must be 1..=9",
            ))?;
            if !(1..=9).contains(&n) {
                return Err(ParseError::BadArgument(
                    "move-to-workspace: n must be 1..=9",
                ));
            }
            let follow = if args.iter().any(|a| *a == "--follow") {
                1
            } else {
                0
            };
            Ok(ParsedVerb::Display(ControlCommand::MoveToWorkspace {
                n,
                follow,
            }))
        }
        "reload" => Ok(ParsedVerb::Display(ControlCommand::Reload)),
        "query" => {
            let what = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("query requires <what>"))?;
            match what {
                "windows" => Ok(ParsedVerb::Display(ControlCommand::QueryWindows)),
                "workspaces" => Ok(ParsedVerb::Display(ControlCommand::QueryWorkspaces)),
                _ => Err(ParseError::BadArgument(
                    "query: expected 'windows' or 'workspaces'",
                )),
            }
        }
        "tile" => {
            let sub = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("tile requires <subcommand>"))?;
            match sub {
                "fullscreen" => Ok(ParsedVerb::Display(ControlCommand::TileFullscreen)),
                "set-master-ratio" => {
                    let ratio_str = args.get(1).copied().ok_or(ParseError::MissingArgument(
                        "tile set-master-ratio requires <ratio>",
                    ))?;
                    let ratio = parse_ratio(ratio_str).ok_or(ParseError::BadArgument(
                        "tile set-master-ratio: ratio must be 0.0..=1.0",
                    ))?;
                    Ok(ParsedVerb::Display(ControlCommand::SetMasterRatio {
                        ratio_x100: ratio,
                    }))
                }
                _ => Err(ParseError::BadArgument(
                    "tile: unknown subcommand (expected 'fullscreen' or 'set-master-ratio')",
                )),
            }
        }
        other => Err(ParseError::UnknownVerb(String::from(other))),
    }
}

/// Parse a policy name into the wire-side `kind` byte expected by
/// `ControlCommand::SetLayout`. Mirrors the byte mapping in
/// `display_server::main::policy_kind_to_byte`.
fn parse_policy_kind(name: &str) -> Option<u8> {
    match name {
        "master-stack" | "master_stack" | "master" => Some(0),
        "dwindle" => Some(1),
        "spiral" => Some(2),
        "grid" => Some(3),
        "tabbed" => Some(4),
        "fullscreen" => Some(5),
        _ => None,
    }
}

/// Parse a ratio string (`"0.55"`) into the u16 `ratio * 100` wire
/// encoding.
fn parse_ratio(s: &str) -> Option<u16> {
    let f: f32 = s.parse().ok()?;
    if !(0.0..=1.0).contains(&f) {
        return None;
    }
    Some((f * 100.0) as u16)
}

/// Parse a `u8` written either as decimal or with a `0x` hex prefix.
pub fn parse_u8(s: &str) -> Option<u8> {
    if let Some(rest) = s.strip_prefix("0x") {
        u8::from_str_radix(rest, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Map a subscribe `<kind>` argument to the typed [`EventKind`].
pub fn parse_event_kind(name: &str) -> Option<EventKind> {
    match name {
        "surface-created" | "SurfaceCreated" => Some(EventKind::SurfaceCreated),
        "surface-destroyed" | "SurfaceDestroyed" => Some(EventKind::SurfaceDestroyed),
        "focus-changed" | "FocusChanged" => Some(EventKind::FocusChanged),
        "bind-triggered" | "BindTriggered" => Some(EventKind::BindTriggered),
        _ => None,
    }
}

/// Parse a `u32` written either as decimal or with a `0x` hex prefix.
pub fn parse_u32(s: &str) -> Option<u32> {
    if let Some(rest) = s.strip_prefix("0x") {
        u32::from_str_radix(rest, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Parse a `u16` written either as decimal or with a `0x` hex prefix.
pub fn parse_u16(s: &str) -> Option<u16> {
    if let Some(rest) = s.strip_prefix("0x") {
        u16::from_str_radix(rest, 16).ok()
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests;
