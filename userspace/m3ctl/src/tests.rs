//! Phase 57 Track I.2 host tests for the m3ctl verb parser.
//!
//! TDD discipline: this file commits **before** the implementation
//! that makes the session-* arms pass. The RED commit's
//! `parse_verb` does not yet recognize `session-state` /
//! `session-stop` / `session-restart`; the tests below assert the
//! correct typed `ParsedVerb::Session(_)` outcome and therefore fail
//! until the GREEN commit lands the arms.
//!
//! Tests run on the host via
//! `cargo test -p m3ctl --target x86_64-unknown-linux-gnu`.
//!
//! # Coverage
//!
//! - **Phase 57 I.2 verbs.** Every new verb maps to the correct
//!   [`ControlVerb`] variant; the dispatch target is the session
//!   service, not the display service.
//! - **DRY check.** The session codec (`encode_verb` / `decode_verb`)
//!   is the single source of truth — the m3ctl crate must not redefine
//!   the verb tags. We assert the parsed verb round-trips through the
//!   codec.
//! - **Phase 56 verb regression.** The display verbs that already
//!   worked must keep working unchanged — the I.2 refactor must not
//!   regress the Phase 56 verb surface.

extern crate std;

use super::*;
use kernel_core::display::control::{ControlCommand, EventKind, SurfaceId};
use kernel_core::session_control::{ControlVerb, decode_verb, encode_verb};

// ---------------------------------------------------------------------------
// Phase 57 I.2 — session control verbs (RED until the GREEN commit lands)
// ---------------------------------------------------------------------------

#[test]
fn session_state_parses_to_session_state_verb() {
    let parsed = parse_verb("session-state", &[]).expect("session-state should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Session(ControlVerb::SessionState),
        "session-state must produce ParsedVerb::Session(ControlVerb::SessionState)"
    );
}

#[test]
fn session_stop_parses_to_session_stop_verb() {
    let parsed = parse_verb("session-stop", &[]).expect("session-stop should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Session(ControlVerb::SessionStop),
        "session-stop must produce ParsedVerb::Session(ControlVerb::SessionStop)"
    );
}

#[test]
fn session_restart_parses_to_session_restart_verb() {
    let parsed = parse_verb("session-restart", &[]).expect("session-restart should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Session(ControlVerb::SessionRestart),
        "session-restart must produce ParsedVerb::Session(ControlVerb::SessionRestart)"
    );
}

#[test]
fn session_verbs_take_no_arguments() {
    // Extra arguments are tolerated (parser is permissive — argument
    // parsing is per-verb). The three session verbs do not consume
    // their args; passing extras must not change the parsed verb.
    let parsed = parse_verb("session-state", &["unused", "args"])
        .expect("extra args should not block session-state");
    assert_eq!(parsed, ParsedVerb::Session(ControlVerb::SessionState));
}

#[test]
fn session_state_detailed_flag_selects_detailed_verb() {
    // Phase 64a: `session-state --detailed` opts into the per-service
    // `ServiceStates` reply by dispatching the `SessionStateDetailed`
    // verb. The bare `session-state` form continues to return the
    // session-wide `SessionState` for back-compat.
    let parsed = parse_verb("session-state", &["--detailed"])
        .expect("session-state --detailed should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Session(ControlVerb::SessionStateDetailed)
    );
}

#[test]
fn session_state_detailed_flag_tolerates_position() {
    // The flag is recognized anywhere in the trailing argv, not just
    // as the first arg, so extra unrelated tokens before or after do
    // not block detection.
    let before = parse_verb("session-state", &["--detailed", "extra"])
        .expect("session-state --detailed extra should parse");
    let after = parse_verb("session-state", &["extra", "--detailed"])
        .expect("session-state extra --detailed should parse");
    assert_eq!(
        before,
        ParsedVerb::Session(ControlVerb::SessionStateDetailed)
    );
    assert_eq!(
        after,
        ParsedVerb::Session(ControlVerb::SessionStateDetailed)
    );
}

#[test]
fn session_restart_with_no_arg_keeps_whole_session_semantic() {
    // Phase 64a: `session-restart` with no arg preserves the Phase 57
    // whole-session restart contract for back-compat.
    let parsed =
        parse_verb("session-restart", &[]).expect("session-restart with no arg should parse");
    assert_eq!(parsed, ParsedVerb::Session(ControlVerb::SessionRestart));
}

#[test]
fn session_restart_with_name_dispatches_per_service_verb() {
    // Phase 64a: `session-restart <name>` switches to the per-service
    // verb. The codec-level `restart_service_name` accessor lets us
    // assert on the embedded name without exposing the raw buffer.
    let parsed = parse_verb("session-restart", &["display_server"])
        .expect("session-restart display_server should parse");
    match parsed {
        ParsedVerb::Session(verb) => {
            assert_eq!(verb.restart_service_name(), Some("display_server"));
        }
        other => panic!("expected Session, got {:?}", other),
    }
}

#[test]
fn session_restart_oversized_name_returns_bad_argument() {
    let big = "x".repeat(64);
    let err =
        parse_verb("session-restart", &[big.as_str()]).expect_err("oversized name should fail");
    assert!(matches!(err, ParseError::BadArgument(_)));
}

#[test]
fn session_restart_per_service_round_trips_through_codec() {
    let parsed = parse_verb("session-restart", &["audio_server"])
        .expect("session-restart audio_server should parse");
    let verb = match parsed {
        ParsedVerb::Session(v) => v,
        other => panic!("expected Session, got {:?}", other),
    };
    let mut buf = [0u8; 64];
    let n = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..n]).expect("decode_verb");
    assert_eq!(decoded.restart_service_name(), Some("audio_server"));
}

// ---------------------------------------------------------------------------
// DRY — the session-control codec lives once in kernel_core. We verify
// every parsed session verb round-trips through encode_verb /
// decode_verb. If anyone reintroduces a parallel byte definition in
// m3ctl, this test catches it because it would diverge from the
// kernel_core codec output.
// ---------------------------------------------------------------------------

#[test]
fn session_state_round_trips_through_codec() {
    let parsed = parse_verb("session-state", &[]).expect("session-state should parse");
    let verb = match parsed {
        ParsedVerb::Session(v) => v,
        other => panic!("expected Session, got {:?}", other),
    };
    let mut buf = [0u8; 4];
    let n = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..n]).expect("decode_verb");
    assert_eq!(decoded, ControlVerb::SessionState);
}

#[test]
fn session_stop_round_trips_through_codec() {
    let parsed = parse_verb("session-stop", &[]).expect("session-stop should parse");
    let verb = match parsed {
        ParsedVerb::Session(v) => v,
        other => panic!("expected Session, got {:?}", other),
    };
    let mut buf = [0u8; 4];
    let n = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..n]).expect("decode_verb");
    assert_eq!(decoded, ControlVerb::SessionStop);
}

#[test]
fn session_restart_round_trips_through_codec() {
    let parsed = parse_verb("session-restart", &[]).expect("session-restart should parse");
    let verb = match parsed {
        ParsedVerb::Session(v) => v,
        other => panic!("expected Session, got {:?}", other),
    };
    let mut buf = [0u8; 4];
    let n = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..n]).expect("decode_verb");
    assert_eq!(decoded, ControlVerb::SessionRestart);
}

// ---------------------------------------------------------------------------
// Phase 56 regression — pre-existing display verbs must keep working
// after the I.2 refactor (lib + bin split, parse_verb relocation).
// ---------------------------------------------------------------------------

#[test]
fn display_version_parses_to_display_command() {
    let parsed = parse_verb("version", &[]).expect("version should parse");
    assert_eq!(parsed, ParsedVerb::Display(ControlCommand::Version));
}

#[test]
fn display_focus_parses_with_surface_id() {
    let parsed = parse_verb("focus", &["7"]).expect("focus 7 should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Display(ControlCommand::Focus {
            surface_id: SurfaceId(7)
        })
    );
}

#[test]
fn display_focus_accepts_hex_surface_id() {
    let parsed = parse_verb("focus", &["0x2a"]).expect("focus 0x2a should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Display(ControlCommand::Focus {
            surface_id: SurfaceId(42)
        })
    );
}

#[test]
fn display_focus_missing_id_returns_missing_argument() {
    let err = parse_verb("focus", &[]).expect_err("focus with no id should fail");
    assert!(matches!(err, ParseError::MissingArgument(_)));
}

#[test]
fn display_focus_bad_id_returns_bad_argument() {
    let err = parse_verb("focus", &["not-a-number"]).expect_err("non-numeric id should fail");
    assert!(matches!(err, ParseError::BadArgument(_)));
}

#[test]
fn display_register_bind_parses_with_mask_and_keycode() {
    let parsed = parse_verb("register-bind", &["0x0008", "65"]).expect("register-bind");
    assert_eq!(
        parsed,
        ParsedVerb::Display(ControlCommand::RegisterBind {
            modifier_mask: 8,
            keycode: 65,
        })
    );
}

#[test]
fn display_subscribe_parses_event_kind() {
    let parsed = parse_verb("subscribe", &["focus-changed"]).expect("subscribe focus-changed");
    assert_eq!(
        parsed,
        ParsedVerb::Display(ControlCommand::Subscribe {
            event_kind: EventKind::FocusChanged
        })
    );
}

#[test]
fn display_subscribe_unknown_kind_returns_unknown_event_kind() {
    let err =
        parse_verb("subscribe", &["bogus"]).expect_err("subscribe with unknown kind should fail");
    assert!(matches!(err, ParseError::UnknownEventKind(_)));
}

// ---------------------------------------------------------------------------
// Phase 72 — workspace verb range checks
// ---------------------------------------------------------------------------

#[test]
fn workspace_switch_accepts_one_through_nine() {
    for n in 1u8..=9 {
        let arg = format!("{n}");
        let parsed = parse_verb("workspace", &["switch", arg.as_str()])
            .expect("workspace switch <1..=9> should parse");
        match parsed {
            ParsedVerb::Display(ControlCommand::SwitchWorkspace { n: got }) => {
                assert_eq!(got, n);
            }
            other => panic!("expected SwitchWorkspace, got {:?}", other),
        }
    }
}

#[test]
fn workspace_switch_rejects_zero_and_ten() {
    for arg in ["0", "10", "255"] {
        let err = parse_verb("workspace", &["switch", arg])
            .expect_err("workspace switch outside 1..=9 should fail");
        match err {
            ParseError::BadArgument(_) => {}
            other => panic!("expected BadArgument, got {:?}", other),
        }
    }
}

#[test]
fn move_to_workspace_rejects_zero_and_ten() {
    for arg in ["0", "10", "255"] {
        let err = parse_verb("move-to-workspace", &[arg])
            .expect_err("move-to-workspace outside 1..=9 should fail");
        match err {
            ParseError::BadArgument(_) => {}
            other => panic!("expected BadArgument, got {:?}", other),
        }
    }
}

// ---------------------------------------------------------------------------
// Unknown verb regression
// ---------------------------------------------------------------------------

#[test]
fn unknown_verb_returns_unknown_verb_error() {
    let err = parse_verb("not-a-real-verb", &[]).expect_err("unknown verb should fail");
    match err {
        ParseError::UnknownVerb(s) => assert_eq!(s, "not-a-real-verb"),
        other => panic!("expected UnknownVerb, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Service name / IPC label invariants — must match the daemons.
// ---------------------------------------------------------------------------

#[test]
fn session_control_service_name_matches_daemon() {
    // The `userspace::session_manager::control::CONTROL_SERVICE_NAME`
    // constant is "session-control"; the m3ctl client must use the
    // same value. If anyone renames either side they MUST keep them
    // aligned — this test catches divergence at host-test time, before
    // a runtime lookup fails.
    assert_eq!(SESSION_CONTROL_SERVICE_NAME, "session-control");
}

#[test]
fn display_control_service_name_matches_daemon() {
    // Mirror invariant for the Phase 56 surface.
    assert_eq!(DISPLAY_CONTROL_SERVICE_NAME, "display-control");
}

// ---------------------------------------------------------------------------
// Phase 81 (D.2) — `m3ctl wifi status` parsing + formatter.
// ---------------------------------------------------------------------------

#[test]
fn wifi_status_parses_to_wifi_status_verb() {
    assert_eq!(parse_verb("wifi", &["status"]), Ok(ParsedVerb::WifiStatus));
}

#[test]
fn wifi_without_subcommand_is_missing_argument() {
    assert!(matches!(
        parse_verb("wifi", &[]),
        Err(ParseError::MissingArgument(_))
    ));
    assert!(matches!(
        parse_verb("wifi", &["bogus"]),
        Err(ParseError::BadArgument(_))
    ));
}

#[test]
fn wifi_status_format() {
    use alloc::vec::Vec;
    let status = wifi_core::control::WifiStatus {
        ssid: Vec::from(&b"HomeNet"[..]),
        rssi: -47,
        ipv4: [192, 168, 1, 42],
    };
    let rendered = format_wifi_status(&status);
    assert!(rendered.contains("wifi: associated"));
    assert!(rendered.contains("ssid: HomeNet"));
    assert!(rendered.contains("signal: -47 dBm"));
    assert!(rendered.contains("ipv4: 192.168.1.42"));
}

#[test]
fn wifi_not_associated_message_is_stable() {
    assert_eq!(WIFI_NOT_ASSOCIATED_MSG, "wifi: not associated");
    assert_eq!(WIFI_CONTROL_SERVICE_NAME, "wifi.control");
}

// ---------------------------------------------------------------------------
// Phase 84 (D.3) — `m3ctl mitigations status` parsing + formatter.
// ---------------------------------------------------------------------------

#[test]
fn mitigations_status_parses_to_verb() {
    assert_eq!(
        parse_verb("mitigations", &["status"]),
        Ok(ParsedVerb::MitigationsStatus)
    );
    assert!(matches!(
        parse_verb("mitigations", &[]),
        Err(ParseError::MissingArgument(_))
    ));
    assert!(matches!(
        parse_verb("mitigations", &["bogus"]),
        Err(ParseError::BadArgument(_))
    ));
}

#[test]
fn mitigations_status_format_is_honest() {
    use kernel_core::spectre::{IbrsMode, MitigationLevel, MitigationReport};

    // Full level but KPTI NOT enforcing → Meltdown must read Vulnerable
    // (never a false "Mitigation: PTI"), and the honesty notes are present.
    let report = MitigationReport {
        level: MitigationLevel::Full,
        level_recognized: true,
        kpti_active: false,
        ibpb_active: true,
        ibrs_mode: IbrsMode::None,
        leaf7_edx: 0,
        arch_caps: 0,
        // No-PKU boot (the default lane): W^X v1, PKU absent.
        wx_v2: false,
        pku_present: false,
        pku_active: false,
        pcid_active: false,
        // No-CET boot (the default lane): not-supported.
        cet_present: false,
        cet_active: false,
    };
    let r = format_mitigations(&report);
    assert!(r.contains("level=full"));
    assert!(r.contains("Meltdown: Vulnerable"));
    assert!(r.contains("retpoline): compiled-in"));
    assert!(r.contains("UNADDRESSED"));
    // The honesty note must enumerate Spectre-v1 (report_map marks it
    // Status::Unaddressed) — the note is not an exhaustive list otherwise.
    assert!(r.contains("UNADDRESSED — Spectre-v1, MDS"));
    assert!(r.contains("Grimsdal"));
    // Phase 90a C.2 — the no-PKU boot prints the v1 / PKU-absent W^X line.
    assert!(r.contains("W^X: v1 (PKU absent)"));
    // Phase 110 A.5 — KPTI is NOT enforcing here, so no PCID line is printed.
    assert!(!r.contains("KPTI PCID:"));
    // Phase 110 B.3 — the no-CET boot prints the not-supported CET line.
    assert!(r.contains("CET: not-supported"));

    // KPTI enforcing → Meltdown reads "Mitigation: PTI".
    let report2 = MitigationReport {
        kpti_active: true,
        ..report
    };
    let r2 = format_mitigations(&report2);
    assert!(r2.contains("Meltdown: Mitigation: PTI"));
    // Phase 110 A.5 — KPTI enforcing without PCID (the default QEMU lane) prints
    // the fallback PCID posture line.
    assert!(r2.contains("KPTI PCID: fallback (full TLB flush; no PCID/INVPCID)"));

    // KPTI enforcing WITH the PCID scheme (bare-metal PCID silicon) prints the
    // active posture line instead.
    let report_pcid = MitigationReport {
        kpti_active: true,
        pcid_active: true,
        ..report
    };
    assert!(
        format_mitigations(&report_pcid).contains("KPTI PCID: active (kernel/user PCID, no-flush)")
    );

    // RDCL_NO silicon → "Not affected" regardless of kpti_active.
    let report3 = MitigationReport {
        leaf7_edx: 1 << 29,
        arch_caps: 0b01,
        ..report
    };
    assert!(format_mitigations(&report3).contains("Meltdown: Not affected"));
}

/// Phase 90a C.2 — the W^X / PKU posture line renders all three boot states.
#[test]
fn mitigations_status_wx_pku_line() {
    use kernel_core::spectre::{IbrsMode, MitigationLevel, MitigationReport};

    let base = MitigationReport {
        level: MitigationLevel::Auto,
        level_recognized: true,
        kpti_active: false,
        ibpb_active: false,
        ibrs_mode: IbrsMode::None,
        leaf7_edx: 0,
        arch_caps: 0,
        wx_v2: false,
        pku_present: false,
        pku_active: false,
        pcid_active: false,
        cet_present: false,
        cet_active: false,
    };

    // No-PKU boot (the default TCG lane): v1, PKU absent.
    assert!(format_mitigations(&base).contains("W^X: v1 (PKU absent)"));

    // PKU active (e.g. under KVM on a PKU host): v2, present + active.
    let active = MitigationReport {
        wx_v2: true,
        pku_present: true,
        pku_active: true,
        ..base
    };
    assert!(format_mitigations(&active).contains("W^X: v2 (PKU present, active)"));

    // PKU silicon present but the kernel did not enable it: v1, present but
    // inactive (so the v2 exception is unavailable).
    let inactive = MitigationReport {
        pku_present: true,
        ..base
    };
    assert!(format_mitigations(&inactive).contains("W^X: v1 (PKU present, inactive)"));
}

/// Phase 110 B.3 — the CET posture line renders all three boot states.
#[test]
fn mitigations_status_cet_line() {
    use kernel_core::spectre::{IbrsMode, MitigationLevel, MitigationReport};

    let base = MitigationReport {
        level: MitigationLevel::Auto,
        level_recognized: true,
        kpti_active: false,
        ibpb_active: false,
        ibrs_mode: IbrsMode::None,
        leaf7_edx: 0,
        arch_caps: 0,
        wx_v2: false,
        pku_present: false,
        pku_active: false,
        pcid_active: false,
        cet_present: false,
        cet_active: false,
    };

    // No-CET boot (the default TCG lane): not-supported.
    assert!(format_mitigations(&base).contains("CET: not-supported"));

    // CET silicon, policy on (the Dell): enabled.
    let active = MitigationReport {
        cet_present: true,
        cet_active: true,
        ..base
    };
    assert!(format_mitigations(&active).contains("CET: enabled (user shadow stacks)"));

    // CET silicon but mitigations=off: supported, inactive.
    let inactive = MitigationReport {
        cet_present: true,
        ..base
    };
    assert!(format_mitigations(&inactive).contains("CET: supported, inactive"));
}

// ---------------------------------------------------------------------------
// Phase 103 A.5 — `m3ctl power status` / `m3ctl battery`
// ---------------------------------------------------------------------------

#[test]
fn power_status_parses_to_power_status_verb() {
    assert_eq!(
        parse_verb("power", &["status"]),
        Ok(ParsedVerb::PowerStatus)
    );
    assert_eq!(parse_verb("battery", &[]), Ok(ParsedVerb::Battery));
}

#[test]
fn power_without_subcommand_is_missing_argument() {
    assert!(matches!(
        parse_verb("power", &[]),
        Err(ParseError::MissingArgument(_))
    ));
    assert!(matches!(
        parse_verb("power", &["frobnicate"]),
        Err(ParseError::BadArgument(_))
    ));
}

#[test]
fn backlight_parses_pct_up_down_and_show() {
    // Phase 103 B.3 acceptance: the <pct> / up / down argument forms.
    assert!(matches!(
        parse_verb("backlight", &[]),
        Ok(ParsedVerb::BacklightShow)
    ));
    assert!(matches!(
        parse_verb("backlight", &["50"]),
        Ok(ParsedVerb::BacklightSet(50))
    ));
    assert!(matches!(
        parse_verb("backlight", &["0"]),
        Ok(ParsedVerb::BacklightSet(0))
    ));
    assert!(matches!(
        parse_verb("backlight", &["100"]),
        Ok(ParsedVerb::BacklightSet(100))
    ));
    assert!(matches!(
        parse_verb("backlight", &["up"]),
        Ok(ParsedVerb::BacklightStep(10))
    ));
    assert!(matches!(
        parse_verb("backlight", &["down"]),
        Ok(ParsedVerb::BacklightStep(-10))
    ));
    for bad in ["101", "-5", "bright", "10%"] {
        assert!(
            matches!(
                parse_verb("backlight", &[bad]),
                Err(ParseError::BadArgument(_))
            ),
            "{bad} must be rejected"
        );
    }
}

#[test]
fn power_off_and_suspend_parse() {
    // Phase 103 D.3 — the poweroff verb and the Track F suspend stub.
    assert!(matches!(
        parse_verb("power", &["off"]),
        Ok(ParsedVerb::PowerOff)
    ));
    assert!(matches!(
        parse_verb("power", &["suspend"]),
        Ok(ParsedVerb::PowerSuspend)
    ));
}

#[test]
fn power_status_format_renders_vm_and_battery_cases() {
    use kernel_core::power::control::{AcState, CpufreqMech, PowerStatusWire, ThermalWire};
    use kernel_core::power::governor::GovernorMode;

    let vm = PowerStatusWire::no_battery();
    let rendered = format_power_status(&vm);
    assert!(rendered.contains("ac: assumed-online"));
    assert!(rendered.contains("battery: none"));
    // Slice-2/B posture lines on a zone-less, HWP-less, panel-less VM.
    assert!(rendered.contains("thermal: none (no zones)"));
    assert!(rendered.contains("governor: conservative (mech none, target 0)"));
    assert!(rendered.contains("backlight: none (no device)"));
    assert!(rendered.contains("sleep: none declared"));

    let laptop = PowerStatusWire {
        battery_present: true,
        percent: 50,
        ac: AcState::Offline,
        state: kernel_core::power::battery::BST_STATE_DISCHARGING,
        rate: 8_760,
        temp_deci_c: 421,
        thermal: ThermalWire::Normal,
        governor: GovernorMode::Conservative,
        mech: CpufreqMech::Hwp,
        perf: 96,
        backlight_pct: 75,
        sleep_bits: kernel_core::power::control::SLEEP_S3 | kernel_core::power::control::SLEEP_S4,
    };
    let rendered = format_power_status(&laptop);
    assert!(rendered.contains("ac: offline"));
    assert!(rendered.contains("50% discharging"));
    assert!(rendered.contains("thermal: normal, 42.1 C"));
    assert!(rendered.contains("governor: conservative (mech hwp, target 96)"));
    assert!(rendered.contains("backlight: 75%"));
    assert!(rendered.contains("sleep: S3+S4 (firmware-declared)"));
    assert!(format_battery(&laptop).contains("battery: 50% discharging"));

    // A passive-cooling laptop just under boiling renders its state.
    let hot = PowerStatusWire {
        temp_deci_c: 953,
        thermal: ThermalWire::Passive,
        ..laptop
    };
    assert!(format_power_status(&hot).contains("thermal: passive, 95.3 C"));
}
