# Phase 68 — Display Server Closeout: Task List

**Status:** Complete
**Source Ref:** phase-68
**Depends on:** Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅, Phase 64 (Session Manager Lifecycle) ✅
**Goal:** Close the five Phase 56 completion gaps. Wire `flush_subscriber_ring` into the four existing `publish_*` functions and add two new event kinds (`LayerEvent`, `CursorEvent`) with matching publish + flush wiring. Implement `DamageTracker` in the compositor and clip blits to the dirty union. Add `ModifierSide` to `KeyEvent` and bump global `PROTOCOL_VERSION` from `1` to `2` (with a version-1 client compatibility shim). Extract `userspace/init/src/manifest.rs` and `userspace/init/src/supervisor.rs` from `main.rs`; add comma-separated `depends=` and a typed `on-restart=` directive. Create `mouse_server.conf` and register `mouse_server` as a managed init service declaring `depends=kbd_server`. Append a `> Phase 68 closure note:` block to Phase 56's design doc; bump kernel to `0.68.0`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Subscription event push: `flush_subscriber_ring`; wire into 4 existing `publish_*` functions; add `LayerEvent` + `CursorEvent` variants and matching `publish_*` + flush | None | Planned |
| B | Compositor damage tracking: `DamageTracker`, cursor and surface damage integration | A | Planned |
| C | `ModifierSide` field on `KeyEvent`; bump `PROTOCOL_VERSION` 1→2; update all 6 handshake sites; version-1 client compatibility shim; PS/2 emitter wiring | None | Planned |
| D | Extract `manifest.rs` + `supervisor.rs` from `userspace/init/src/main.rs`; add comma-separated `depends=`; dependency-ordered start; create `mouse_server.conf` + four-place registration | None | Planned |
| E | `on-restart=` supervisor directive: `OnRestartAction` enum, parser, `handle_budget_exhaustion` dispatch; text-fallback bridge to `session_manager` | D | Planned |
| F | Phase 56 closure note: append `> **Phase 68 closure note:**` block to Phase 56 design doc `## Deferred Until Later`; cross-reference from Phase 56 task doc completion-gap items | A, B, C, D, E | Planned |
| G | Kernel version bump to 0.68.0 + roadmap README row update | F | Planned |

---

## Track A — Subscription Event Push (4 existing + 2 new event kinds)

### A.1 — Implement `flush_subscriber_ring`

**File:** `userspace/display_server/src/control.rs`
**Symbol:** `flush_subscriber_ring`
**Why it matters:** Without the flush step, queued events never leave the server — subscribers receive nothing. Confirmed by the four `TODO(subscription-push)` markers at lines 670, 690, 696, 703.

**Acceptance:**
- [ ] `flush_subscriber_ring(endpoint, ring)` calls `sys_send` for each pending event in the ring until the ring is empty or `sys_send` returns `-EAGAIN`.
- [ ] On `-EAGAIN` the send is skipped for that event (event dropped, counter incremented); the loop continues for the next event.
- [ ] A named `events_dropped` counter is exported via the `display_server` debug control verb (or added if no counter-export verb exists yet).

### A.2 — Wire `flush_subscriber_ring` into the 4 existing `publish_*` functions

**File:** `userspace/display_server/src/control.rs`
**Symbols:** `publish_surface_created` (`:665`), `publish_surface_destroyed` (`:689`), `publish_focus_changed` (`:694`), `publish_bind_triggered` (`:700`)
**Why it matters:** All four publish paths currently enqueue but never transmit (TODO markers at `:670,690,696,703`); each must be updated.

**Acceptance:**
- [ ] Each of the four `publish_*` functions calls `flush_subscriber_ring` after enqueue.
- [ ] The `TODO(subscription-push)` markers are removed.
- [ ] `cargo xtask test --test display_subscription_push` passes: a subscriber receives a `SurfaceCreated` event within 10 ms of a surface-creation control call.
- [ ] Test confirms zero events dropped during the nominal test window.

### A.3 — Add `LayerEvent` and `CursorEvent` variants + publish + flush

**Files:**
- `userspace/display_server/src/control.rs`
- `kernel-core/src/display/control.rs`
- `kernel-core/src/display/protocol.rs`

**Symbols:** `ControlEvent::LayerEvent`, `ControlEvent::CursorEvent`, `EventKind::LayerEvent`, `EventKind::CursorEvent`, `publish_layer_event`, `publish_cursor_event`
**Why it matters:** The Phase 56 layer-shell-equivalent surface roles (anchor, exclusive zone, keyboard-interactivity) and cursor visibility transitions have no corresponding subscription event today; clients cannot react to them.

**Acceptance:**
- [ ] `ControlEvent::LayerEvent { surface_id, anchor, exclusive_zone, keyboard_interactivity }` and `ControlEvent::CursorEvent { visible, hot_x, hot_y }` (or equivalent shapes) are added to the enum near the existing variants (`control.rs:397-400`).
- [ ] `EventKind::LayerEvent = 4` and `EventKind::CursorEvent = 5` extend the existing 0..=3 mapping at `control.rs:184-200,197-200`.
- [ ] `publish_layer_event` and `publish_cursor_event` follow the same enqueue-then-flush pattern as the four existing functions; no `TODO(subscription-push)` markers remain anywhere in `control.rs`.
- [ ] Encode/decode for the two new variants is added to the matching protocol module and round-trips in a host-side unit test.
- [ ] `cargo xtask test --test display_subscription_push` is extended to cover all six event kinds.

---

## Track B — Compositor Damage Tracking

### B.1 — Implement `DamageTracker`

**File:** `userspace/display_server/src/compose.rs` (or sibling `userspace/display_server/src/damage.rs`)
**Symbol:** `DamageTracker`
**Why it matters:** `compose.rs:164-175` documents that cursor-only frames trigger a full framebuffer blit; without damage tracking every cursor motion repaints everything mapped.

**Acceptance:**
- [ ] `DamageTracker` holds a `Vec<DamageRect>` with a capacity cap (at most 16 rectangles before merging to a union).
- [ ] `mark_dirty(rect: DamageRect)` appends and merges overlapping rectangles.
- [ ] `union_rect() -> Option<DamageRect>` returns the bounding union of all dirty regions.
- [ ] `reset()` clears all rectangles.
- [ ] `is_full_repaint_needed()` returns `true` on first frame, after explicit invalidation, and on capacity-cap overflow.
- [ ] At least five unit tests: empty tracker, single rect, two non-overlapping rects, two overlapping rects (merged), capacity overflow → full-repaint flag.

### B.2 — Clip blit operations in `compose` to dirty union

**File:** `userspace/display_server/src/compose.rs`
**Symbol:** `run_compose` (around `:118-362`)
**Why it matters:** The blit reduction is only effective if the clipper is wired into every blit call in the compose path.

**Acceptance:**
- [ ] `run_compose` calls `DamageTracker::union_rect` and clips all surface and cursor blit operations to the returned rectangle when `is_full_repaint_needed()` is `false`.
- [ ] Cursor motion marks old and new cursor bounding boxes in `DamageTracker`.
- [ ] An instrumented test asserts that a cursor-motion-only frame blits strictly fewer pixels than the full framebuffer resolution.
- [ ] The deferred-fast-path note at `compose.rs:164-175` is removed or updated to reflect that the gap is closed.

---

## Track C — `ModifierSide` + Global `PROTOCOL_VERSION` Bump

### C.1 — Add `ModifierSide` field to `KeyEvent`

**File:** `kernel-core/src/input/events.rs`
**Symbols:** `KeyEvent` (`:82-88`), `ModifierSide`, `KEY_EVENT_WIRE_SIZE` (`:91`)
**Why it matters:** Without side differentiation, a compositor cannot bind left-Meta separately from right-Meta. The current 19-byte wire size omits the field entirely.

**Acceptance:**
- [ ] `ModifierSide` enum has variants `Left`, `Right`, `Either`.
- [ ] `KeyEvent` struct gains a `modifier_side: ModifierSide` field.
- [ ] `KEY_EVENT_WIRE_SIZE` is updated to reflect the new field width.
- [ ] At least two host-side unit tests round-trip `Left` and `Right` through encode/decode.

### C.2 — Bump global `PROTOCOL_VERSION` from 1 to 2 across all handshake sites

**Files:**
- `kernel-core/src/display/protocol.rs` (constant at `:44`; handshake sites at `:1478,1682,1883`)
- `kernel-core/src/display/control.rs` (handshake site at `:297`)
- `userspace/display_server/src/control.rs` (handshake mirror at `:469`)

**Symbol:** `PROTOCOL_VERSION`
**Why it matters:** All in-tree clients handshake; bumping the global version is the clean coordinated upgrade path. Any per-event header is fragile because misaligned readers misparse downstream fields.

**Acceptance:**
- [ ] `PROTOCOL_VERSION = 2` in `protocol.rs:44`.
- [ ] All six handshake sites announce `2` (verified by grep).
- [ ] A version-1-handshake-from-client compatibility shim returns `KeyEvent` records with `modifier_side: ModifierSide::Either`; the shim's behavior is exercised by a host-side unit test in `kernel-core`.
- [ ] All in-tree clients (`kbd_server`, `display_server`, host tests, any `m3ctl`-style consumers) handshake at version 2 in the same change set; `cargo xtask check` passes.

### C.3 — Emit `ModifierSide` from `kbd_server` PS/2 scanner

**File:** `userspace/kbd_server/src/ps2.rs`
**Symbol:** `scan_to_key_event` (or the existing scan-to-event entry point — verify name during implementation)
**Why it matters:** The PS/2 extended scancode `0xE0` prefix distinguishes right-side modifier keys; `kbd_server` must use this information.

**Acceptance:**
- [ ] `scan_to_key_event` maps `0xE0 0x2A` (right-Shift) to `ModifierSide::Right`, bare `0x2A` (left-Shift) to `ModifierSide::Left`.
- [ ] Similar mappings for Ctrl (`0x1D` left / `0xE0 0x1D` right) and Alt (`0x38` left / `0xE0 0x38` right).
- [ ] Non-modifier keys emit `ModifierSide::Either`.
- [ ] At least two unit tests: left-Shift → `ModifierSide::Left`, right-Shift → `ModifierSide::Right`.

---

## Track D — Extract `manifest.rs` + `supervisor.rs`; multi-service `depends=`; `mouse_server.conf`

### D.1 — Extract `manifest.rs` from `userspace/init/src/main.rs`

**Files:**
- `userspace/init/src/manifest.rs` (new)
- `userspace/init/src/main.rs`

**Symbols:** `ServiceManifest`, `parse_manifest`, `OnRestartAction`
**Why it matters:** `userspace/init/src/main.rs` is currently the only file in init's `src/`; the inline parser and the resolved-index `[[usize; MAX_DEPS]; MAX_SERVICES]` array (`:437`) cannot represent multi-service `depends=` cleanly. Extraction is a prerequisite for tracks D.2 and E.1.

**Acceptance:**
- [ ] `manifest.rs` exists and exports `ServiceManifest`, `OnRestartAction`, `parse_manifest`, and `detect_cycles`.
- [ ] `ServiceManifest::depends` is `Vec<String>` (replacing the resolved-index array).
- [ ] `parse_manifest` splits `depends=` on commas, trims whitespace, and rejects empty names with a logged warning.
- [ ] A unit test parses `depends=kbd_server,display_server` and verifies both names appear in the vector.
- [ ] `main.rs` imports from `manifest.rs`; the inline parser is removed; `cargo xtask check` passes.

### D.2 — Extract `supervisor.rs`; implement dependency-ordered start

**Files:**
- `userspace/init/src/supervisor.rs` (new)
- `userspace/init/src/main.rs`

**Symbols:** `start_services_ordered`, `init::supervisor::ServiceState` (small enum, parallel to but independent of `session_manager::ServiceState`)
**Why it matters:** Without enforcement, declaring `depends=` has no runtime effect.

**Acceptance:**
- [ ] `supervisor.rs` exists and exports `start_services_ordered` plus the small `ServiceState` enum (variants: `Pending`, `Starting`, `Running`, `Failed`).
- [ ] `start_services_ordered` defers starting a service until all named dependencies are in `ServiceState::Running`.
- [ ] `detect_cycles` (from D.1) logs an error at manifest load time and refuses to start cycle members.
- [ ] `main.rs` calls `start_services_ordered`; the previously implicit kbd→mouse ordering in `KNOWN_CONFIGS` is removed.

### D.3 — Create `mouse_server.conf` and register the binary (four-place rule)

**Files:**
- `kernel/initrd/etc/services.d/mouse_server.conf` (new)
- `xtask/src/main.rs` (`bins` array around `:141`; `populate_ext2_files`)
- `kernel/src/fs/ramdisk.rs` (`include_bytes!` static + `BIN_ENTRIES` tuple)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS` at `:126`)

**Symbol:** `mouse_server.conf`
**Why it matters:** `mouse_server` is currently an unmanaged binary. Without registration in all four places, the manifest-declared `depends=kbd_server` cannot take effect.

**Acceptance:**
- [ ] `kernel/initrd/etc/services.d/mouse_server.conf` exists with `name=mouse_server`, `exec=/bin/mouse_server`, `depends=kbd_server`, `restart=on-failure`, `max_restart=10`.
- [ ] `mouse_server` appears in xtask `bins` (`needs_alloc` set correctly per actual crate deps).
- [ ] `mouse_server` `include_bytes!` static and `BIN_ENTRIES` tuple are added to `kernel/src/fs/ramdisk.rs`.
- [ ] `mouse_server.conf` is added to `KNOWN_CONFIGS`.
- [ ] `cargo xtask clean && cargo xtask test --test manifest_depends` confirms `mouse_server` does not start until `kbd_server` is in `Running`.

---

## Track E — `on-restart=` Supervisor Directive

### E.1 — Add `on-restart=` field and `handle_budget_exhaustion` dispatch

**Files:**
- `userspace/init/src/manifest.rs`
- `userspace/init/src/supervisor.rs`

**Symbols:** `ServiceManifest::on_restart`, `OnRestartAction`, `handle_budget_exhaustion`
**Why it matters:** Without a typed `on-restart=` directive, the supervisor cannot distinguish services that should escalate to text-fallback from those that should log-and-continue when the restart budget is exhausted.

**Acceptance:**
- [ ] `OnRestartAction` enum: `LogAndContinue` (default), `TextFallback`, `Panic`.
- [ ] `parse_manifest` reads `on-restart=` and maps to `OnRestartAction`; unknown values default to `LogAndContinue` with a log warning.
- [ ] `handle_budget_exhaustion` dispatches: `LogAndContinue` logs at ERROR; `TextFallback` calls into the `session_manager` text-fallback control verb; `Panic` calls `kernel::halt` (or the userspace-side fatal-exit path).
- [ ] An integration test with `on-restart=text-fallback` and a crash-looping service verifies text-fallback is triggered after budget exhaustion.
- [ ] No existing manifests are broken (all default to `LogAndContinue` when the field is absent).

---

## Track F — Phase 56 Closure Note

### F.1 — Append `> Phase 68 closure note:` block to Phase 56 design doc

**File:** `docs/roadmap/56-display-and-input-architecture.md`
**Symbol:** `## Deferred Until Later` section
**Why it matters:** Phase 56's design doc is already `Status: Complete`; what's missing is a forward-pointer that names the five gaps closed in Phase 68 so future readers can trace the actual completion path.

**Acceptance:**
- [ ] A `> **Phase 68 closure note:**` blockquote is appended to `## Deferred Until Later` in Phase 56's design doc, naming the five gaps closed (subscription event push + 2 new event kinds; compositor damage tracking; `ModifierSide` + `PROTOCOL_VERSION` bump to 2; managed `mouse_server` with `depends=kbd_server`; `on-restart=` directive with extracted `manifest.rs`/`supervisor.rs`).
- [ ] The note links to `docs/roadmap/68-display-server-closeout.md`.
- [ ] No other Phase 56 design doc content is modified.

### F.2 — Annotate Phase 56 task doc completion-gap items

**File:** `docs/roadmap/tasks/56-display-and-input-architecture-tasks.md`
**Symbol:** (completion-gap items)
**Why it matters:** Task acceptance items that referenced the five gaps must point readers to the Phase 68 closure for the actual implementation.

**Acceptance:**
- [ ] Each Phase 56 task item that referenced a deferred gap closed by Phase 68 carries a `(closed in Phase 68)` annotation linking to `docs/roadmap/68-display-server-closeout.md`.
- [ ] No other Phase 56 acceptance items are changed.

---

## Track G — Kernel Version Bump and README

### G.1 — Bump kernel version to 0.68.0; update roadmap README

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-bump per shipped phase; keeping the version cursor accurate ensures `AGENTS.md` and the README reflect the real state of the kernel.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.68.0"`.
- [ ] `Cargo.lock` regenerated (`cargo xtask check` triggers it).
- [ ] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.68.0`.
- [ ] `docs/roadmap/README.md` Phase 68 row status moves from `Planned` to `Complete` at merge; the row description is updated to reflect the actual shipped scope (no longer mentions "flips Phase 56 to Complete" since that drift never existed).
- [ ] `cargo xtask check` passes after the bump.
- [ ] Git tag `v0.68.0` recommended at phase merge.

---

## Documentation Notes

- `DamageTracker` is a pure in-process data structure; it does not touch kernel or IPC. It belongs in `userspace/display_server/src/compose.rs` or a sibling `damage.rs` file.
- The `PROTOCOL_VERSION` 1→2 bump should be documented in a short memo under `docs/appendix/` alongside the existing Phase 56 wire-format reference. The memo should enumerate every handshake site (six known: `protocol.rs:44,1478,1682,1883`; `control.rs:297`; `userspace/display_server/src/control.rs:469`) and the version-1 compatibility shim's contract.
- `depends=` cycle detection can use a simple DFS over the dependency graph at manifest load; no persistent graph structure is needed at runtime.
- `init::supervisor::ServiceState` is intentionally separate from `session_manager::ServiceState` (`userspace/session_manager/src/table.rs:40-57`). The two supervisors have different lifecycles (init owns boot + restart budget; session_manager owns the graphical session). Coupling them is deferred until a concrete need emerges.
- The four-place rule for `mouse_server` registration (xtask `bins`, ramdisk `BIN_ENTRIES`, `KNOWN_CONFIGS`, `services.d/mouse_server.conf`) follows AGENTS.md "Adding a New Userspace Binary". Run `cargo xtask clean` after adding the conf to force ext2 disk recreation.
- `on-restart=text-fallback` dispatch crosses a process boundary (init → session_manager via the existing control socket); use the smallest viable verb rather than introducing a new IPC channel.
