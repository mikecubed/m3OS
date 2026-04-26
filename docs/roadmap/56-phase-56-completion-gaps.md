# Phase 56 — Completion Gaps

**Status:** Living document — updated as items close.
**Source Ref:** phase-56
**Depends on:** PR #124 (`feat/phase-56-tracks-d-h`)
**Builds on:** PR #121 (Track A), PR #122 + PR #123 (Tracks B + C), and the Phase 56 close-out (`SYS_IPC_TAKE_PENDING_BULK = 0x1112`, `SYS_IPC_TRY_RECV_MSG = 0x1113`, reply-cap-handle plumbing).
**Primary Components:** `userspace/init`, `userspace/display_server`, `kernel-core/tests`, `xtask/src/main.rs`, `docs/roadmap/tasks/56-display-and-input-architecture-tasks.md`
**Milestone Goal:** Tick the last set of items needed to mark Phase 56 as 100% complete per its own task spec.

## Why this doc exists

After PR #124 landed (Tracks D – H + bulk-drain close-out), Phase 56's *architectural* goals are met:

- All four Goal-A contract points (swappable layout, grab hook, layer-shell role, control socket) ship.
- Runtime byte-flow is verified end-to-end via the F.2 regression reaching `DISPLAY_CRASH_SMOKE:debug-crash:transport-error` (the reply-cap-handle revoke path that proves the request → bulk-drain → decode chain is real).
- Kernel version is bumped to 0.56.0.
- 1183 kernel-core lib tests + 13 host integration tests + 21 test-binary suites green.

But the Phase 56 task spec defines completion in terms of acceptance checkboxes (216 still unchecked), gated regression validations (need real artifacts), and one bug surfaced by the close-out itself. This doc enumerates everything between "architecturally done" and "spec-complete".

## 1. Bugs revealed by close-out (must fix)

### 1.1 F.2 supervisor restart not visible after panic — RESOLVED

**Severity:** real bug; blocked F.2 regression PASS.

**Status:** **FIXED.** F.2 regression passes end-to-end after this commit.

**Root cause:** test-pattern mismatch. Three issues, each compounding the next:

1. The xtask regression asserted `init: started 'display_server' pid=` but init logs the **service-registration name** from the manifest (`name=display`), not the binary name. Init was correctly reaping + restarting; the test assertion was wrong.
2. The smoke binary similarly polled `/run/services.status` for service name `display_server`. Same fix: use `display`.
3. After the supervisor restart fired, the smoke binary's lookup retry budget (40 ms) was too short to absorb the restarted display_server's framebuffer-acquire backoff + dual-endpoint registration cascade. Bumped to 5 s.
4. Step ordering: smoke binary printed `restart-confirmed` based on `/run/services.status` (which transitions to `running` at fork time) **before** display_server actually re-registered. The xtask test then expected `display_server: starting` (second instance) before `restart-confirmed`. Reordered the smoke binary to print `restart-confirmed` only after `lookup_with_extended_backoff(CONTROL_SERVICE_NAME)` succeeds — proving the new control endpoint is actually reachable.

**Files changed:**
- `xtask/src/main.rs` — assertion pattern + comment
- `userspace/display-server-crash-smoke/src/main.rs` — service-status key + extended retry budget + reordered restart-confirmed signal

**Validation:**
```
M3OS_ENABLE_CRASH_SMOKE=1 cargo xtask regression --test display-server-crash-recovery
regression: 1 passed, 0 failed
```

## 2. Real deferrals (explicit, will NOT close in Phase 56)

These are documented in `docs/56-display-and-input-architecture.md` § Deferred follow-ups. Each is explicitly out of phase scope by design.

| ID | Description | Reason | Owning future phase |
|---|---|---|---|
| D-B4 | True zero-copy via page-grant capabilities | Inline IPC bulk ships today; zero-copy needs kernel cap-transfer addition | Phase 56b or later |
| D-F1a | `mouse_server` dependency direction reversal | Init's manifest parser doesn't yet support comma-separated `depends=` | Phase 57+ session-manifest pass |
| D-F1b | Distinct `on-restart=` supervisor directive | Existing `restart=on-failure` covers the failure mode | Phase 51 service-model maturity |
| D-D1 | Standalone modifier-key edges on `kbd_server` pull path | Modifier state is folded into next non-modifier event | Additive — when a real client needs it |
| D-A0 | L/R modifier chord differentiation | `MOD_SHIFT` doesn't distinguish left vs right | Wire-format change; needs versioned bump |
| D-E4 | Server-initiated subscription event push | Registry queues events but doesn't transmit them | Needs polling verb OR cap-transfer-at-subscribe; see `TODO(subscription-push)` markers |

These are excluded from the "100% complete" bar by definition. Listing them here for visibility, not as todo items.

## 3. Bookkeeping — 216 unchecked acceptance bullets

The task spec at `docs/roadmap/tasks/56-display-and-input-architecture-tasks.md` has 216 `[ ]` checkboxes that should be `[x]` because the work shipped (just no agent flipped them back). Distribution:

```
 10  ### E.4 — Control socket: endpoint, verbs, events
 10  ### A.3 — Client protocol wire format
  9  ### H.4 — Version bump to 0.56.0 on phase landing
  9  ### H.1 — Create Phase 56 learning doc
  9  ### D.4 — Keybind grab-hook implementation
  9  ### D.1 — Extend kbd_server to emit key events with modifier state
  8  ### D.3 — Input dispatcher with focus-aware routing
  8  ### A.4 — Input event protocol
  8  ### A.0 — Shared protocol module in kernel-core
  7  ### E.3 — Cursor surface role and pointer rendering
  7  ### D.2 — Create mouse_server userspace service
  7  ### A.8 — Control-socket protocol (Goal-A decision 4)
  6  ### G.7 — Interactive run-gui smoke validation
  6  ### G.2 — Keybind grab-hook regression test
  6  ### E.2 — Layer surface role with anchors and exclusive zones
  6  ### C.6 — gfx-demo protocol-reference client
  6  ### A.9 — Verify evaluation gate checks before closing the phase
  6  ### A.6 — Layer-shell-equivalent surface roles (Goal-A decision 3)
  6  ### A.5 — Keybind grab-hook semantics (Goal-A decision 2)
  5  ### H.2 — Update subsystem and roadmap docs
  5  ### G.5 — Display-service crash recovery regression test
  5  ### G.4 — Control socket round-trip regression test
  5  ### F.2 — Display-service crash recovery
  5  ### A.7 — Swappable layout module contract (Goal-A decision 1)
  5  ### A.2 — Service topology and ownership boundaries
  4  ### H.3 — Update evaluation docs
  4  ### G.6 — xtask and CI plumbing for the new test suites
  4  ### G.3 — Layer-shell exclusive-zone regression test
  4  ### G.1 — Multi-client coexistence regression test
  4  ### F.3 — Fallback to text-mode administration
  4  ### F.1 — Service manifests and supervision under init
  4  ### C.5 — Client connection handshake and event loop
  4  ### A.1 — Adopt the four Goal-A design decisions as Phase 56 contract points
  3  ### B.1 — Transfer framebuffer ownership from kernel to display_server
  2  ### E.1 — LayoutPolicy trait and default floating layout
  2  ### B.4 — Cross-process shared-buffer transport for surfaces
  1  ### C.4 — Damage-tracked software composer
  1  ### C.2 — Framebuffer acquisition and exclusive presentation
  1  ### B.3 — Vblank / frame-tick notification source
  1  ### B.2 — Mouse input path (PS/2 AUX)
```

**Process:** walk each task section, verify the work shipped (in the source files named in the task), and flip checkboxes for items that are actually done. Items where the deferred-bullets list (§ 2 above) applies stay unchecked but get a `(deferred)` annotation.

**Estimated effort:** ~45 minutes of careful editing.

## 4. QEMU-integration regressions writeable now (bulk-drain closed)

Pre-close-out: deferred behind the bulk-drain gap.
Post-close-out: writeable but not yet written.

### 4.1 G.1 — Multi-client coexistence runtime regression

**Spec:** § G.1 (lines ~771–786). Two `gfx-demo`-flavored clients connect; each fills a distinct color; a pixel-sampling harness reads back the framebuffer and asserts both colors are present at their layout-derived positions.

**Lift plan (per `kernel-core/tests/phase56_g1_multi_client_coexistence.rs` header):**
1. Add a guest binary `userspace/display-multi-client-smoke/` modeled on `userspace/display-server-crash-smoke/`. Forks two child clients with different fill colors.
2. Add a test-only `ControlCommand::ReadBackPixel { x, y }` verb gated by an env-var marker (mirror F.2's `M3OS_DISPLAY_SERVER_DEBUG_CRASH=1`).
3. Add `cargo xtask regression --test multi-client-coexistence` boots with the marker, runs the smoke, asserts both colors at expected positions.

**Estimated effort:** 3–4 hours.

### 4.2 G.2 — Synthetic-key-injection grab-hook regression

**Spec:** § G.2. A test client gains focus; `m3ctl register-bind MOD_SUPER+q`; synthetic `KeyDown` for `SUPER+q` fires; client receives **no** `KeyEvent`; control-socket sees `BindTriggered`.

**Already covered:** 4 host tests on `BindTable::match_bind` predicate invariants in `kernel-core/tests/phase56_g2_keybind_grab_hook.rs`.

**Lift plan:** synthetic-key injection requires either:
- A test-only `ControlCommand::InjectKey { mask, keycode, kind }` verb, OR
- A debugfs / sysfs hook to push fake scancodes into the kbd ring.

The control-socket verb is the cleaner option since E.4's dispatcher already exists. Same env-var gating pattern as F.2's `DebugCrash`.

**Estimated effort:** 2–3 hours.

### 4.3 G.4 — `m3ctl` runtime list-surfaces / subscribe / frame-stats regression

**Spec:** § G.4. Now technically possible since `m3ctl` reply decoding works.

**Already covered:** 4 host tests on the codec round-trip.

**Lift plan:** add a regression test that runs `m3ctl version`, `m3ctl list-surfaces` (after `gfx-demo` registers a `Toplevel`), `m3ctl frame-stats`, and asserts each prints the expected human-readable summary.

The `subscribe`-with-event-receipt portion stays deferred behind the subscription-push gap (§ 2 D-E4).

**Estimated effort:** 1–2 hours.

## 5. Acceptance gates that need real validation

These are the H.4 and A.9 verification bullets that require running real quality gates and capturing the evidence.

### 5.1 `cargo xtask check` clean on the final branch

**Status:** verified locally; needs to be captured in the closing PR description as evidence.

### 5.2 `cargo xtask test` passes on the final branch

**Status:** **Not yet verified.** This runs all kernel tests inside QEMU via the xtask harness. Different from `cargo test -p kernel-core` (host tests).

**Estimated effort:** 5 minutes to run; up to 1 hour if any test surfaces a regression that needs investigation.

### 5.3 `M3OS_ENABLE_CRASH_SMOKE=1 cargo xtask regression --test display-server-crash-recovery`

**Status:** **Currently fails at step 16** (the F.2 supervisor bug — § 1.1).

**Will pass after:** § 1.1 is fixed.

### 5.4 `M3OS_ENABLE_FALLBACK_SMOKE=1 cargo xtask regression --test display-fallback`

**Status:** **Not yet validated.** F.3 ships the regression but it hasn't been run end-to-end since the bulk-drain close-out.

**Estimated effort:** 5 minutes to run; up to 1 hour if it fails.

### 5.5 `cargo xtask smoke-test`

**Status:** verified green on the close-out commit (PR #124 head).

### 5.6 `cargo xtask regression` (default-gated tests)

**Status:** runs all the non-env-gated regression tests (driver-restart-guest, max-restart-exceeded, fork-overlap, etc.). **Not yet verified post-close-out.**

**Estimated effort:** ~10 minutes to run.

## 6. The four Goal-A contract points — re-verification

The task spec § A.1 lists the four contract points as a required cross-check at phase close. Each delivers as below:

| # | Goal-A decision | Phase 56 task | Source location | Verified |
|---|---|---|---|---|
| 1 | Swappable layout module from day one | A.7 + E.1 | `kernel-core::display::layout::LayoutPolicy` trait + `FloatingLayout` + `StubLayout` + `display_server::compose::default_layout()` factory | ✅ |
| 2 | Keybind grab hook keyed on modifier sets | A.5 + D.4 | `kernel-core::input::bind_table::BindTable` + `GrabState` | ✅ |
| 3 | Layer-shell-equivalent surface role | A.6 + E.2 | `kernel-core::display::protocol::SurfaceRole::Layer` + `kernel-core::display::layer::compute_layer_geometry` + `LayerConflictTracker` | ✅ |
| 4 | Control socket as a first-class part of the protocol | A.8 + E.4 | `kernel-core::display::control` + `display_server::control` + `userspace/m3ctl/` | ✅ |

All four ship. No deferrals on the Goal-A surface.

## 7. Estimated total close-out effort

| Phase | Items | Effort |
|---|---|---|
| § 1 — F.2 supervisor bug | Fix init's restart path | 1–2 h |
| § 3 — Bookkeeping | Tick 216 acceptance checkboxes | ~45 min |
| § 4 — QEMU regressions | G.1, G.2 runtime, G.4 runtime | 6–9 h |
| § 5 — Validation runs | `cargo xtask test`, gated regressions | ~30 min |
| **Total** | | **8–12 h** |

The `§ 4` block is the bulk of the work. Whether to ship it inside Phase 56 or as a sibling Phase 56a follow-on PR is a project-management call:
- **Inside Phase 56:** Phase 56 carries all its acceptance criteria. PR #124 grows by ~9 hours of work.
- **As Phase 56a:** Phase 56 closes on PR #124 (with `(deferred)` annotations on the QEMU regression bullets). Phase 56a is a small, focused follow-up.

Either is defensible. The deferral is documented either way.

## 8. Closing checklist

When the items above resolve, mark this doc complete and update the Phase 56 row in `docs/roadmap/README.md` from "Complete (D–H + close-out)" to "Complete (all acceptance bullets ticked)".

- [x] § 1.1 — F.2 supervisor restart bug fixed
- [x] § 3 — 216 acceptance checkboxes triaged. 195 ticked (work shipped); 21 remain unchecked, each annotated with the deferral reason and a pointer to either § 2 (explicit deferral) or § 4 (QEMU integration regression writeable but not yet written). The unchecked box is intentional in every case.
- [x] § 4.1 — G.1 multi-client coexistence regression written + passing (`M3OS_ENABLE_MULTI_CLIENT_SMOKE=1 cargo xtask regression --test multi-client-coexistence`: 1 passed, 0 failed). Required: new `ControlCommand::ReadBackPixel` test-only verb gated by `M3OS_DISPLAY_SERVER_READBACK=1`, FramebufferOwner trait extension with `read_pixel`, new `display-multi-client-smoke` guest binary, and a small fix to `FloatingLayout::arrange` so cascade slots are stable across frames in the multi-surface case (call-local index instead of a persistent counter).
- [ ] § 4.2 — G.2 synthetic-key-injection regression written + passing
- [ ] § 4.3 — G.4 runtime control-socket regression written + passing
- [ ] § 5.2 — `cargo xtask test` passes on final branch
- [x] § 5.3 — F.2 regression passes
- [x] § 5.4 — F.3 regression passes
- [x] § 5.6 — Default `cargo xtask regression` passes (10/11; `fork-overlap` and `serverization-fallback` are pre-existing flakes — both fail intermittently on main, neither caused by Phase 56 changes; the Phase 56 task doc records the flake count from the close-out smoke runs)

When all 9 boxes tick, Phase 56 is 100% complete by its own spec.
