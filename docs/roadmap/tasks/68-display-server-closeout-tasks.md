# Phase 68 — Display Server Closeout: Task List

**Status:** Planned
**Source Ref:** phase-68
**Depends on:** Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Goal:** Close five Phase 56 completion gaps: wire subscription event push transmission; implement compositor damage tracking; add L/R modifier chord differentiation with versioned wire format; extend the init manifest parser to support comma-separated `depends=`; add a distinct `on-restart=` supervisor directive. Flip Phase 56 design-doc status to Complete; close audit Red Flag #7 for Phase 56.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Subscription event push: `flush_subscriber_ring` wired into all four `publish_*` functions | None | Planned |
| B | Compositor damage tracking: `DamageTracker`, cursor and surface damage integration | A | Planned |
| C | L/R modifier chord differentiation: `ModifierSide` field, versioned wire format bump | None | Planned |
| D | Init manifest `depends=` multi-service support and dependency-ordered start | None | Planned |
| E | Distinct `on-restart=` supervisor directive | D | Planned |
| F | Phase 56 design doc + task doc updated; status flipped Complete; Red Flag #7 closure | A, B, C, D, E | Planned |
| G | Documentation and Release | F | Planned |

---

## Track A — Subscription Event Push

### A.1 — Implement `flush_subscriber_ring`

**File:** `userspace/display_server/src/control.rs`
**Symbol:** `flush_subscriber_ring`
**Why it matters:** Without the flush step, queued events never leave the server — subscribers receive nothing.

**Acceptance:**
- [ ] `flush_subscriber_ring(endpoint, ring)` calls `sys_send` for each pending event in the ring until the ring is empty or `sys_send` returns `-EAGAIN`.
- [ ] On `-EAGAIN` the send is skipped for that event (event dropped, counter incremented); the loop continues for the next event.
- [ ] A named `events_dropped` counter is exported via the `display_server` debug control verb.

### A.2 — Wire `flush_subscriber_ring` into all four `publish_*` functions

**File:** `userspace/display_server/src/control.rs`
**Symbol:** `publish_surface_event`, `publish_focus_event`, `publish_layer_event`, `publish_cursor_event`
**Why it matters:** The four publish paths at lines 670, 690, 696, 703 each enqueue but never transmit; all must be updated.

**Acceptance:**
- [ ] Each `publish_*` function calls `flush_subscriber_ring` after enqueue.
- [ ] `cargo xtask test --test display_subscription_push` passes: subscriber receives a `SurfaceEvent` within 10 ms of a surface damage hint.
- [ ] Test confirms zero events dropped during the nominal test window.

---

## Track B — Compositor Damage Tracking

### B.1 — Implement `DamageTracker`

**File:** `userspace/display_server/src/compose.rs`
**Symbol:** `DamageTracker`
**Why it matters:** Without damage tracking every cursor motion repaints the entire framebuffer regardless of what changed.

**Acceptance:**
- [ ] `DamageTracker` holds a `Vec<DamageRect>` with a capacity cap (at most 16 rectangles before merging to a union).
- [ ] `mark_dirty(rect: DamageRect)` appends and merges overlapping rectangles.
- [ ] `union_rect() -> Option<DamageRect>` returns the bounding union of all dirty regions.
- [ ] `reset()` clears all rectangles.
- [ ] `is_full_repaint_needed()` returns `true` on first frame and after explicit invalidation.
- [ ] At least five unit tests: empty tracker, single rect, two non-overlapping rects, two overlapping rects (merged), full-repaint flag.

### B.2 — Clip blit operations in `compose` to dirty union

**File:** `userspace/display_server/src/compose.rs`
**Symbol:** `compose`
**Why it matters:** The blit reduction is only effective if the clipper is wired into every blit call in the compose path.

**Acceptance:**
- [ ] `compose` calls `DamageTracker::union_rect` and clips all surface and cursor blit operations to the returned rectangle.
- [ ] Cursor motion marks old and new cursor bounding boxes in `DamageTracker`.
- [ ] An instrumented test asserts that a cursor-motion-only frame blits fewer pixels than the full framebuffer resolution.

---

## Track C — L/R Modifier Chord Differentiation

### C.1 — Add `ModifierSide` field to `KeyEvent` wire format

**File:** `kernel-core/src/display/protocol.rs`
**Symbol:** `KeyEvent`, `ModifierSide`
**Why it matters:** Without side differentiation, a compositor cannot bind left-Meta separately from right-Meta.

**Acceptance:**
- [ ] `ModifierSide` enum has variants `Left`, `Right`, `Either`.
- [ ] `KeyEvent` struct has a `modifier_side: ModifierSide` field.
- [ ] Wire format version field is bumped from the current value; a named constant `KEY_EVENT_VERSION` is defined.
- [ ] Clients without a version handshake receive `ModifierSide::Either` for backward compatibility.

### C.2 — Emit `ModifierSide` from `kbd_server` PS/2 scanner

**File:** `userspace/kbd_server/src/ps2.rs`
**Symbol:** `scan_to_key_event`
**Why it matters:** The PS/2 extended scancode `0xE0` prefix distinguishes right-side modifier keys; `kbd_server` must use this information.

**Acceptance:**
- [ ] `scan_to_key_event` maps `0xE0 0x2A` (right-Shift) to `ModifierSide::Right`, `0x2A` (left-Shift) to `ModifierSide::Left`.
- [ ] Similar mappings for Ctrl and Alt.
- [ ] At least two unit tests: left-Shift → `ModifierSide::Left`, right-Shift → `ModifierSide::Right`.

---

## Track D — Init Manifest `depends=` Multi-Service Support

### D.1 — Extend manifest parser to support comma-separated `depends=`

**File:** `userspace/init/src/manifest.rs`
**Symbol:** `ServiceManifest::depends`, `parse_manifest`
**Why it matters:** Currently `depends=` accepts only a single service name; comma-separated lists are required for `mouse_server depends on kbd_server` without hardcoded ordering.

**Acceptance:**
- [ ] `ServiceManifest::depends` is `Vec<String>`, not `Option<String>`.
- [ ] `parse_manifest` splits the `depends=` value on commas and trims whitespace.
- [ ] A unit test parses `depends=kbd_server,display_server` and verifies both names in the vector.

### D.2 — Implement dependency-ordered start in the supervisor loop

**File:** `userspace/init/src/supervisor.rs`
**Symbol:** `start_services_ordered`
**Why it matters:** Without enforcement, declaring `depends=` has no runtime effect.

**Acceptance:**
- [ ] `start_services_ordered` defers starting a service until all named dependencies are in `ServiceState::Running`.
- [ ] Cyclic dependency detection logs an error at manifest load time and refuses to start the cycle's members.
- [ ] `mouse_server.conf` declares `depends=kbd_server`; the hardcoded ordering in `session_manager` for this pair is removed.
- [ ] `cargo xtask test --test manifest_depends` passes: `mouse_server` does not start until `kbd_server` is running.

---

## Track E — `on-restart=` Supervisor Directive

### E.1 — Add `on-restart=` field to manifest and supervisor

**Files:**
- `userspace/init/src/manifest.rs`
- `userspace/init/src/supervisor.rs`

**Symbol:** `ServiceManifest::on_restart`, `OnRestartAction`, `handle_budget_exhaustion`
**Why it matters:** Without a distinct `on-restart=` field, services that should trigger text-fallback on budget exhaustion cannot be distinguished from those that should log-and-continue.

**Acceptance:**
- [ ] `OnRestartAction` enum: `LogAndContinue` (default), `TextFallback`, `Panic`.
- [ ] `parse_manifest` reads `on-restart=` and maps to `OnRestartAction`; unknown values default to `LogAndContinue` with a log warning.
- [ ] `handle_budget_exhaustion` dispatches: `LogAndContinue` logs at ERROR; `TextFallback` calls `session_manager` text-fallback path; `Panic` calls `kernel::halt`.
- [ ] A test with `on-restart=text-fallback` and a crash-looping service verifies text-fallback is triggered after budget exhaustion.
- [ ] No existing manifests are broken (all default to `LogAndContinue` when the field is absent).

---

## Track F — Phase 56 Documentation Closure

### F.1 — Flip Phase 56 design doc status to Complete

**File:** `docs/roadmap/56-display-and-input-architecture.md`
**Symbol:** `**Status:**`
**Why it matters:** The audit's Red Flag #7 identified Phase 56 as one of five phases with a status drift between the design doc and the roadmap README.

**Acceptance:**
- [ ] `**Status:** Complete` appears in the design doc header.
- [ ] A `> **Phase 68 closure note:**` block is appended to `## Deferred Until Later` naming the five gaps closed.

### F.2 — Update Phase 56 task doc completion items

**File:** `docs/roadmap/tasks/56-display-and-input-architecture-tasks.md`
**Symbol:** (completion-gap track)
**Why it matters:** Task acceptance items for the five gaps must reference the Phase 68 closure.

**Acceptance:**
- [ ] The five completion-gap task items each note "(closed in Phase 68)".
- [ ] No other Phase 56 acceptance items are changed.

---

---

## Track G — Documentation and Release

### G.1 — Create the aligned legacy learning doc

**File:** `docs/68-display-server-closeout.md`
**Symbol:** (new document)
**Why it matters:** Learners need a focused reference for the five Phase 56 completion gaps — event push transmission, damage tracking, ModifierSide wire format, manifest depends=, on-restart= directive — without merging them into the broader Phase 56 display-server architecture narrative.

**Acceptance:**
- [ ] `docs/68-display-server-closeout.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 68`, `**Status:** Planned`, `**Source Ref:** phase-68`, `**Supersedes Legacy Doc:** new`).
- [ ] Overview is one learner-friendly paragraph explaining the five gaps closed and why Phase 56 was only nominally complete before this phase.
- [ ] Key Files table cites `userspace/display_server/src/control.rs`, `userspace/display_server/src/compose.rs`, `kernel-core/src/display/protocol.rs`, `userspace/kbd_server/src/ps2.rs`, and `userspace/init/src/manifest.rs`.
- [ ] Related Roadmap Docs links `docs/roadmap/68-display-server-closeout.md` and `docs/roadmap/tasks/68-display-server-closeout-tasks.md`.

### G.2 — Bump kernel version to 0.68.0

**Files:** `kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md`, `docs/roadmap/README.md`
**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-bump per shipped phase; keeping the version cursor accurate ensures `AGENTS.md` and the README reflect the real state of the kernel at any given phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.68.0"`
- [ ] `Cargo.lock` regenerated (run `cargo check` or `cargo xtask check` to trigger)
- [ ] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.68.0`
- [ ] `cargo xtask check` passes after the bump
- [ ] Git tag `v0.68.0` recommended at phase merge

---

## Documentation Notes

- `DamageTracker` is a pure in-process data structure; it does not touch kernel or IPC. It belongs in `userspace/display_server/src/compose.rs` or a sibling `damage.rs` file.
- The wire format version bump for `KeyEvent` must be documented in a `docs/appendix/` memo alongside the existing Phase 56 wire-format appendix.
- `depends=` cycle detection can use a simple DFS over the dependency graph at manifest load; no persistent graph structure is needed at runtime.
- The `on-restart=` field in `KNOWN_CONFIGS` fallback list in `userspace/init/src/main.rs` must be updated alongside the parser change.
