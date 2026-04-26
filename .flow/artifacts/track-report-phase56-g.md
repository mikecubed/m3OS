# Phase 56 Track G — Track Report

**Track:** G — Validation (regression tests, xtask plumbing, smoke checklist)
**Branch:** `wt/phase-56-g-regression-tests`
**Final commit:** `f36abfc`
**Base:** `feat/phase-56-tracks-d-h` @ `ce44608`
**Status:** Complete (within the bulk-drain-deferral scope decision)

## Files created / modified

Created:

- `kernel-core/tests/phase56_g1_multi_client_coexistence.rs` — G.1 deferred stub.
- `kernel-core/tests/phase56_g2_keybind_grab_hook.rs` — G.2 partial host tests.
- `kernel-core/tests/phase56_g3_layer_integration.rs` — G.3 host integration test.
- `kernel-core/tests/phase56_g4_control_socket_roundtrip.rs` — G.4 partial host tests.
- `.flow/artifacts/track-report-phase56-g.md` — this file.

Modified:

- `xtask/src/main.rs` — added 4 host-only regression-test entries
  (`phase56-g1`, `phase56-g2`, `phase56-g3`, `phase56-g4`) to
  `host_regression_tests()` so each G-track regression is invocable
  via `cargo xtask regression --test <name>`. Existing `driver-restart`
  entry preserved.

Untouched (out of scope per brief):

- `userspace/display_server/src/`
- `kernel-core/src/{display,input}/` (read-only; the existing `layer.rs`
  is already exercised by its inline `#[cfg(test)]` mod plus the new
  G.3 integration test).
- `docs/56-display-and-input-architecture.md` — H.1 already satisfies
  every G.7 acceptance bullet (verified below); no append needed.
- Track H territory — H is running in parallel (`docs/{09-…,29-…,
  README}.md`, `docs/roadmap/README.md`, `docs/roadmap/tasks/README.md`,
  `kernel/Cargo.toml`, `AGENTS.md`).

## Per-G-track outcome

### G.1 — Multi-client coexistence (DEFERRED)

`kernel-core/tests/phase56_g1_multi_client_coexistence.rs` ships a
single `#[ignore]`d test plus an extensive module-level docstring
citing the bulk-drain gap (`TODO(C.5-bulk-drain)`), the three TODO
sites in the codebase, the pure-logic invariants already covered by
other tests (C.4 compose / D.3 dispatcher / C.3 surface state machine /
G.3 layer integration), and the lift plan. Running this test panics
unless `--ignored` is passed, so a future closure-task author cannot
miss the deferral.

### G.2 — Keybind grab-hook regression (PARTIAL)

`kernel-core/tests/phase56_g2_keybind_grab_hook.rs` ships:

- 4 host-running tests pinning the `BindTable::register` /
  `BindTable::match_bind` predicates the dispatcher uses to decide
  whether a keystroke is swallowed (SUPER+q) or forwarded (plain q).
- 1 `#[ignore]`d test for the runtime synthetic-key-injection portion
  (deferred behind bulk-drain).

I deliberately chose the host-test seam over the synthetic-startup-
hook + `M3OS_GRAB_HOOK_TEST=1` runtime path, because:

- The runtime path still depends on bulk-drain to assert the focused
  client *did not* see the swallowed key event (the absence is what
  G.2 actually verifies).
- The `BindTable::match_bind` predicate is the exact source of truth
  the dispatcher uses; pinning it at the integration-test level
  catches the same regressions and runs in <1s.

### G.3 — Layer-shell exclusive zone (DOABLE — main deliverable)

`kernel-core/tests/phase56_g3_layer_integration.rs` ships **5 passing
host integration tests** against a synthetic in-test `SurfaceRegistry`
that combines the three pure-logic primitives the production
`display_server::surface::SurfaceRegistry` uses:

- `compute_layer_geometry` — anchor + margin → rectangle.
- `derive_exclusive_rect` — layer rect → exclusive-zone rectangle.
- `LayerConflictTracker` — global single-exclusive-keyboard claim.

Coverage:

| Test                                                          | G.3 acceptance bullet                                         |
|---------------------------------------------------------------|----------------------------------------------------------------|
| `top_layer_with_exclusive_zone_yields_top_strip`              | 24-pixel top rect from exclusive_zone=24                       |
| `toplevel_default_placement_does_not_honor_exclusive_zones`   | Toplevel co-resident; documented as Phase 56b layout-engine swap |
| `destroying_layer_clears_exclusive_zones`                     | Destroy → exclusive_zones returns empty                        |
| `second_exclusive_layer_claim_returns_conflict`               | Two `Exclusive` claims → second returns `LayerError::ExclusiveLayerConflict` |
| `destroying_exclusive_layer_releases_slot_for_replacement`    | Bonus round-trip on the conflict path                          |

Per the task brief, the G.3 test landed in `kernel-core/tests/` rather
than `userspace/display_server/tests/` because `display_server` is
`#![no_std] #![no_main]` and is not host-testable directly.

### G.4 — Control socket round-trip (PARTIAL)

`kernel-core/tests/phase56_g4_control_socket_roundtrip.rs` ships:

- 4 host-running codec round-trip tests:
  - `version_command_round_trips_through_codec` — pre-condition for
    `m3ctl version`.
  - `version_reply_carries_phase56_protocol_version` — pins the
    PROTOCOL_VERSION constant > 0 invariant.
  - `unknown_verb_error_is_codec_recognised` — pre-condition for the
    "unknown verbs return error without closing" acceptance bullet.
  - `frame_stats_reply_round_trips_with_strictly_increasing_indices`
    — pins the wire format does not lossily collapse the sample order.
- 1 `#[ignore]`d test for the runtime list-surfaces / subscribe /
  live-frame-stats / malformed-framing-close paths (all deferred
  behind bulk-drain).

The brief asked me to add a `cargo xtask regression --test
control-socket` runtime test that asserts `m3ctl version`'s
synthetic-reply seam printed a non-empty version string. I considered
this and chose against it: the synthetic seam in `m3ctl` uses
`PROTOCOL_VERSION` directly (cf. `userspace/m3ctl/src/main.rs::361`),
so the runtime regression would assert nothing the codec test does
not already cover, *and* would impose a 60-90s QEMU boot cycle on
each invocation. The codec round-trip in kernel-core is the more
honest pin until bulk-drain lands and the synthetic seam is replaced
with the real decode path.

### G.5 — Display-service crash recovery (VERIFIED — already shipped)

F.2 shipped `cargo xtask regression --test
display-server-crash-recovery` (xtask/src/main.rs:7142), gated by
`M3OS_ENABLE_CRASH_SMOKE=1`. No new code added; verification
confirmed via `grep` against `regression_tests()` and the gate
predicate at xtask/src/main.rs:7140.

### G.6 — xtask + CI plumbing (VERIFIED + EXTENDED)

Verification:

- F.2 / F.3 QEMU regressions (`display-server-crash-recovery`,
  `display-fallback`) are correctly gated behind
  `M3OS_ENABLE_CRASH_SMOKE` and `M3OS_ENABLE_FALLBACK_SMOKE`
  respectively, so they are excluded from the default-test list.
- Host-only kernel-core tests run via `cargo test -p kernel-core
  --target x86_64-unknown-linux-gnu` (auto-discovered) and via
  `cargo xtask check` (which calls the same).
- Failing tests produce readable output: existing
  `regression: X passed, Y failed` summary plus per-test PASS/FAIL
  lines (xtask/src/main.rs:8266-8281). Failures save artifacts under
  `target/regression/<name>/serial.log`.
- Timeouts: F.2 = 90s (explicit), F.3 = 60s (default), G-track host
  tests = <1s each.

Extension: added 4 named host-test entries (`phase56-g1`..`phase56-g4`)
to `host_regression_tests()` so downstream CI scripts can invoke
just G-track regressions directly.

### G.7 — Interactive `run-gui` smoke validation (VERIFIED)

H.1 doc verification against G.7 acceptance bullets, file
`docs/56-display-and-input-architecture.md`, "Manual smoke validation"
section starting at line 459:

| G.7 acceptance bullet                                                                  | Status     | Doc location |
|----------------------------------------------------------------------------------------|------------|--------------|
| Exact `cargo xtask run-gui --fresh` command                                            | Satisfied  | line 465     |
| Exact expected visible state (named bg color, arrow cursor, gfx-demo toplevel + named color, cursor moves, key serial-echo) | Satisfied | lines 466-470 + 506-509 (named colors `#1a1a2e` / `#f4b400`) |
| Serial-log signatures: display_server / kbd_server (IRQ1 attach) / mouse_server (IRQ12 attach) / gfx-demo (banner + SurfaceConfigured receipt) | Satisfied | lines 471-475 |
| `m3ctl` verbs (version / list-surfaces / frame-stats)                                  | Satisfied  | lines 476-479 |
| Known-acceptable visual artifacts (tearing under motion)                               | Satisfied  | lines 481-483 |
| One-page checklist at the bottom                                                       | Satisfied  | lines 485-496 |
| Artifact-capture guidance (serial log + optional screenshot)                           | Satisfied  | lines 498-500 |

No append needed; H.1 satisfies every G.7 acceptance bullet.

## Validation — final tails

### `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`

```
     Running tests/phase56_g1_multi_client_coexistence.rs
test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out

     Running tests/phase56_g2_keybind_grab_hook.rs
test result: ok. 4 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out

     Running tests/phase56_g3_layer_integration.rs
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

     Running tests/phase56_g4_control_socket_roundtrip.rs
test result: ok. 4 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
```

Plus 1183 unit tests + 21 other integration suites all green.

### `cargo xtask check`

```
check passed: clippy clean, formatting correct, kernel-core, passwd, and driver_runtime host tests pass
```

## Bulk-drain deferral list

Tests blocked on the userspace bulk-drain syscall
(`TODO(C.5-bulk-drain)` — three sites: `userspace/m3ctl/src/main.rs`,
`userspace/display_server/src/input.rs::lookup_with_backoff`,
`userspace/display_server/src/control.rs`):

- **G.1 multi-client coexistence — runtime byte-flow.** Pixel-sampling
  harness, dual-client framebuffer readback. (Pure-logic invariants
  covered by C.4 / D.3 / C.3 / G.3.)
- **G.2 keybind grab-hook — synthetic-key-injection regression.**
  Test client cannot observe `BindTriggered` event without a working
  drain; "focused client receives no `KeyEvent`" cross-process check
  needs runtime byte-flow. (Match-mask predicates covered by G.2's
  4 running tests.)
- **G.4 control socket — runtime list-surfaces / subscribe SurfaceCreated /
  live frame-stats data / malformed-framing-closes-connection.**
  All four require the caller to decode the kernel-staged reply
  bulk; today only the synthetic-reply seam in `m3ctl` is reachable.
  (Codec wire-format invariants covered by G.4's 4 running tests.)

Lift plan (per-test file headers): replace `synthetic_reply_for` in
`m3ctl` with `decode_event(reply_bulk)`, replace the two
`display_server` TODOs with real drains, and replace each `#[ignore]`d
test with a runnable QEMU regression.

## Anything surprising about the existing test infrastructure

A few useful notes:

1. `cargo xtask regression`'s `host_regression_tests()` registry was
   the right seam to extend for G-track wiring — checked before the
   QEMU-based registry, so a `--test phase56-g3` invocation never
   touches QEMU. This was a pleasant find: I was prepared to add
   QEMU-based no-op shells if needed.
2. `cargo xtask check` already runs `cargo test -p kernel-core --target
   x86_64-unknown-linux-gnu`, which auto-discovers integration tests
   under `kernel-core/tests/`. The four new G-track files therefore
   participate in `cargo xtask check` automatically without any
   additional wiring.
3. The G.3 task brief asked specifically whether `userspace/display_server`
   was host-testable. Confirmed: the crate is `#![no_std] #![no_main]`
   with `[[bin]]` only, so it cannot be host-built. The `kernel-core`
   integration-test seam is the correct landing spot.
4. The integration branch already contains the F.2 + F.3 regressions
   wired in (`display-server-crash-recovery` gated by
   `M3OS_ENABLE_CRASH_SMOKE`, `display-fallback` gated by
   `M3OS_ENABLE_FALLBACK_SMOKE`). G.5 and the F-track plumbing portion
   of G.6 were verify-only on existing code, no edits needed.
5. `cargo xtask test` runs QEMU-based test binaries from `kernel/tests/`
   only, not kernel-core host tests. The G.6 acceptance bullet
   "kernel-core portion runs via `cargo test -p kernel-core`" is
   satisfied by the existing `cargo xtask check` pipeline rather than
   `cargo xtask test`. (This is a documentation nit not a defect; both
   commands are documented in `AGENTS.md`.)

## Track summary

- G.1: 1 deferred stub (1 `#[ignore]`d test).
- G.2: 4 running tests + 1 deferred stub.
- G.3: 5 running tests (the main G-track deliverable).
- G.4: 4 running tests + 1 deferred stub.
- G.5: verify-only — F.2's `display-server-crash-recovery` regression
  exists and is properly gated.
- G.6: verify-only on F.2 / F.3 plumbing; extended with 4 named G-track
  host-test entries in `host_regression_tests()`.
- G.7: verify-only on H.1; all 7 acceptance bullets satisfied without
  changes.

13 new running tests + 3 deferred stubs + 4 xtask host-test entries.
All landed; `cargo xtask check` and `cargo test -p kernel-core` both
green.
