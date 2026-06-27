# Phase 98 — Roadmap Audit & Re-Charter: Task List

**Status:** Planned
**Source Ref:** phase-98
**Depends on:** the full Phase 1→97 arc being nominally "done"
**Goal:** Reconcile every Phase 1→97 Status against falsifiable evidence and repair the rotted index layer (Track A); charter the GUI-workstation arc (Phases 99–110) as template-conformant docs that schedule every open deferral and handoff (Track B); and specify two repository-hygiene reforms — a single unified workspace version (Track C) and a slimmed `AGENTS.md` (Track D) — precisely enough to execute mechanically in follow-on PRs. This PR is planning docs only.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Backward audit (1→97 verdict matrix) + index-layer repair + 1.0/version reconciliation + bare-metal validation strategy | — | Planned |
| B | Forward re-charter: design + task docs for Phases 99–110 + README rows + map every deferral/handoff onto a phase or backlog | A (the verdict matrix informs sequencing) | Planned |
| C | Versioning reform **spec** (single `[workspace.package]` version) for follow-on execution | — | Planned |
| D | Rules / `AGENTS.md` slimming **spec** for follow-on execution | — | Planned |

---

## Track A — Backward Audit + Index Repair

### A.1 — Phase 1→97 evidence-reconciliation matrix

**File:** `docs/appendix/audit-status/09-recharter-audit-2026-06.md` (new)
**Symbol:** the per-phase verdict table
**Why it matters:** The existing audit corpus stops at 57e (2026-05) and the pre-1.0 audit at 75; ~50 "Complete" rows (mostly pre-Phase-63, predating the gate-citation convention) cite no inline evidence, and the index layer drifted. A current matrix is what makes the roadmap a trustworthy map for the next arc.

**Acceptance:**
- [ ] Every Phase 1→97 (incl. lettered sub-phases) has a verdict: **Validated** (cite gate name + last PASS/HW run/host test), **Claimed-unvalidated** (Complete, no current evidence — flagged, with a risk note), or **Regressed** (gate now fails).
- [ ] The verdict for phases whose gates exist in `AGENTS.md` but are not cited inline (e.g. `hda-smoke`/P80, `ahci-smoke`/P82, `userspace-simd-smoke`/P86f, `clang-stress`/P88, `tiling-smoke`/P72) cites the gate.
- [ ] The matrix's verdict scheme + method are documented, mirroring the prior audit's severity-tag structure.

### A.2 — Fix stale README Status fields

**File:** `docs/roadmap/README.md`
**Symbol:** the per-phase status table rows; the `## Suggested Delivery Rhythm` gantt
**Why it matters:** Stale rows actively mislead. Phase 54a reads `Planned` while Phase 66 claims to *close* it; the Phase 98 row's own motivating example ("Phase 96's stale `Planned` while HW-validated") is itself stale — the live Phase 96 row already reads `✅ Complete`; and the gantt mislabels P78–81 with pre-renumbering themes.

**Acceptance:**
- [ ] Phase 54a Status reconciled against Phase 66's closure claim (set to its true state with a cited pointer).
- [ ] The gantt's `section Post-1.0 Platform Growth` nodes (P78–81 labelled "Cross-Compiled Toolchains / Networking and GitHub / Node.js / Claude Code") are corrected to the renumbered themes.
- [ ] A status-marker legend is added (the table mixes `Complete`, `**Complete**`, `Complete ✅`, `✅ Complete`, `🟢 Landed`, `🟡 Implemented` with no key).

### A.3 — Regenerate the rotted index layer

**Files:**
- `docs/appendix/codebase-map.md`
- `docs/README.md`
- `docs/roadmap/tasks/README.md`
- `docs/appendix/file-backed-mmap.md`

**Symbol:** the Workspace-Crates / Kernel-Source-Layout / Ports-Tree sections; the Documentation Index; the task-doc index
**Why it matters:** The next-arc work (I2C/LPSS, ACPI, GUI, power) is navigated via `codebase-map.md`, which is the most stale doc in the tree — frozen at ~Phase 55, listing ~40 of ~116 crates and omitting the entire GUI/audio/driver/toolchain trees.

**Acceptance:**
- [ ] `codebase-map.md` lists all current workspace members (cross-checked against `Cargo.toml`) including `display_server`/`term`/`greeter`/compositor clients, `audio_server`/`hda`/`ac97`, the `userspace/drivers/*` tree, `pkg`/`m3ctl`/`wifi-core`, the kernel modules added since (`epoll.rs`/`eventfd.rs`/`flock.rs`/`mitigations.rs`/`timerfd.rs`/`trace.rs`/`iommu/`), and every toolchain port. Prefer a generated manifest over a hand-list where feasible.
- [ ] `docs/README.md` Documentation Index includes the ~34 currently-omitted top-level learning docs.
- [ ] `docs/roadmap/tasks/README.md` is current (no broken P51 link; P13 not marked "not yet created"; the "57+ deferred" claim removed).
- [ ] `file-backed-mmap.md` is corrected or superseded to reflect Phase 95b's landed `MAP_LAZY_FILE` demand-paged mmap.

### A.4 — Reconcile the 1.0 / versioning posture

**Files:**
- `docs/release/1.0-release-gate.md`
- `docs/roadmap/README.md` (Phase 83 row)

**Symbol:** the 1.0 blocker set; the Phase 59 / Phase 65 dependency claims
**Why it matters:** Phase 83 declared a "Release 1.0 Gate" (Complete) yet the kernel stayed `<1.0` and the gate names still-`Planned` deps (Phase 59 Validation Backlog, Phase 65 fat_server) — a release-gate-atop-incomplete-deps inconsistency. The new arc is "post-1.0 platform growth," so the version-cut policy must be explicit.

**Acceptance:**
- [ ] The Phase 83 gate's still-`Planned` dependencies (59, 65) are closed/superseded or the gate is downgraded, with the disposition recorded.
- [ ] An explicit version-cut policy for the GUI-workstation arc is written (pairs with Track C: the single `[workspace.package]` version is an OS release version; 1.0 is cut when the gate's matrix is green).

### A.5 — Bare-metal validation strategy

**File:** `docs/appendix/bare-metal-validation.md` (new)
**Symbol:** the per-HW-phase validation protocol + the "Validated-on-HW (run N, date)" status convention
**Why it matters:** The entire forward arc (ACPI, I2C-HID, AX201, bare-metal GUI, power, laptop audio) is HW-only — QEMU models none of it — so the always-on-gate quality story does not transfer. Without a repeatable protocol, HW phases get marked "Complete" on one manual run with no regression coverage, recreating the exact drift this phase retires.

**Acceptance:**
- [ ] A reusable protocol generalized from `scripts/ure-vfio-validate.md`: USB/VFIO passthrough where possible, AMT Serial-over-LAN pre-network, `usb-logsink` boot.log + network sink post-network, and a "the screen shows X" method (photo or an on-device render assertion shipped over the log sink, since QMP screendump is QEMU-only).
- [ ] A recorded-evidence convention (where captures live) and the **"Validated-on-HW (run N, date)"** status string the HW phases adopt instead of bare "Complete".

---

## Track B — Forward Re-Charter (Phases 99–110)

### B.1 — Author the next-arc design + task docs

**Files:** `docs/roadmap/99-…` through `docs/roadmap/110-…` + their `docs/roadmap/tasks/*-tasks.md` companions
**Symbol:** the design-doc + task-doc pair for each phase
**Why it matters:** The re-charter's value is a sequenced, dependency-correct map of the GUI-workstation arc; ad-hoc accretion is what produced the audit debt.

**Acceptance:**
- [ ] Each of Phases 99–110 has a design doc conforming to `docs/appendix/doc-templates.md` (all required sections; Milestone Goal + measurable Acceptance Criteria) and a companion task doc (Track Layout + per-track File/Symbol/Why-it-matters/Acceptance tasks).
- [ ] The dependency edges are explicit and consistent across docs: `99→100`; `101→102`, `101→103`; `103+104→105` (settings panel); `106` gated on bare-metal NVMe root; `108` after the Dell line.
- [ ] The two sequencing traps are encoded: ACPI (101) precedes I2C-HID (102); SMP robustness (99) precedes multi-core bare-metal GUI (100).
- [ ] Each HW-only phase references the Track A.5 bare-metal validation protocol and uses the "Validated-on-HW" status convention.

### B.2 — Schedule every open deferral and handoff

**Files:** the Phase 99–110 docs; `docs/roadmap/98-roadmap-audit-and-recharter.md` "Deferred Until Later" backlog
**Symbol:** the deferral→phase mapping
**Why it matters:** The audit found ~55 still-open deferral themes and 7 open-unscheduled handoffs; "ensure followups are completed or scheduled" requires each to land on a phase or be consciously accepted-deferred.

**Acceptance:**
- [ ] The 7 open-unscheduled handoffs are assigned: lost-wakeup single-state-word refactor + kstack/`PROCESS_TABLE`-across-faults audit + 4 GiB SMP panic-quiesce + the step-25 demand-fault NULL-deref flake + `copyfile→EFAULT` + 55c `net::remote` test-encoder bug → **Phase 99**; the USB-kbd-text-mode / `usb-hid`-`usbhub` CPU-hog → **Phase 100** input polish.
- [ ] The GUI/pointer/power/Wi-Fi/installer/packaging/apps/audio/security deferral themes are mapped to Phases 100–110 as above.
- [ ] Items not scheduled into a phase (on-device cargo, networking depth, ext4 journaling, NUMA/swap, crates.io, broader Spectre family, the PS/2-mouse dev-path bug, …) are listed in the Phase 98 accepted-deferred backlog with a future home or acceptance gate.

### B.3 — README rows + next-arc section + gantt

**File:** `docs/roadmap/README.md`
**Symbol:** the status table; a new "Next Arc — GUI Workstation (99→110)" section; the delivery gantt
**Why it matters:** The README is the canonical index; the new phases must be discoverable and sequenced there per the documentation policy.

**Acceptance:**
- [ ] A new status-table section lists Phases 99–110 with Theme / Primary Outcome / Status (`Planned`) / Source Ref / Milestone link / Tasks link.
- [ ] A gantt section (or dependency note) reflects the `99→100→{101→102, 101→103}→104→105→106→107→108/109/110` sequence.
- [ ] The Phase 98 row is updated from `Proposed` to `Planned` and its summary cell matches the rewritten design doc (no self-stale Phase 96 example).

---

## Track C — Versioning Reform (Spec Only)

### C.1 — Specify the single-workspace-version migration

**Files:**
- `Cargo.toml` (root — the target `[workspace.package]` block)
- all ~116 member `Cargo.toml` files except `sunset-local`
- `AGENTS.md` (the version-bump policy)

**Symbol:** `[workspace.package] version`; `version.workspace = true`
**Why it matters:** Phase-encoded per-crate versions cause meaningless merge conflicts when two phases land in parallel (both edit the same `version = "…"` line); a single workspace version makes phase branches touch zero version lines.

**Acceptance:**
- [ ] The spec gives the exact root block (`[workspace.package] version = "0.98.0"` + `edition = "2024"`) and the mass-conversion rule (replace each member's standalone `^version = "…"` with `version.workspace = true` and `^edition = "…"` with `edition.workspace = true`; inline dep version specs are untouched because they are inline tables).
- [ ] `sunset-local` (vendored, edition 2021) is explicitly excluded.
- [ ] The behavior change is noted: `env!("CARGO_PKG_VERSION")` makes the boot banner / `uname` report `0.98.0`; no source changes needed.
- [ ] The `AGENTS.md` policy rewrite is specified: "phase number → `docs/roadmap/` + a `phase-NN` git tag + commit message; do NOT bump Cargo versions per phase; the single `[workspace.package]` version is an OS release version bumped only at release."
- [ ] A verification step is named for the follow-on PR: `cargo xtask check` + `git grep -nE '^version = "0\.'` returns only the root block + `sunset-local`.

---

## Track D — Rules / `AGENTS.md` Slimming (Spec Only)

### D.1 — Specify the `AGENTS.md` cut + gate-table relocation

**Files:**
- `AGENTS.md`
- `docs/appendix/regression-gates.md` (new — the relocation target)
- `.github/copilot-instructions.md`

**Symbol:** the regression-gate table; the capability inventory; the version-bump policy
**Why it matters:** `AGENTS.md` is ~83 KB (~37 K tokens) loaded every session and violates its own "keep it small" policy — 58 % is a gate table whose cells are 200–600-word phase diaries.

**Acceptance:**
- [ ] The spec defines the lean inline `Gate | Env var | one-line purpose` table that stays in `AGENTS.md` and the new `docs/appendix/regression-gates.md` that receives every full per-gate description verbatim (one section per gate), linked once.
- [ ] The capability-inventory bullets are specified to collapse to one line per capability *class* (the ~9 KB "Package management" multi-toolchain saga → one line pointing at `docs/roadmap/README.md`).
- [ ] The version-bump policy fix (Track C) + the `v0.97.0`-header / `0.96.0`-`Cargo.toml` drift reconciliation are included.
- [ ] Stale content flagged for removal: the phase-annotated `cargo xtask check` crate list, the stale ASCII architecture diagram (claims FAT32 root + IPv4-only), the duplicated doc-template rules.
- [ ] `.github/copilot-instructions.md` is specified to become a thin pointer to `AGENTS.md` (it currently duplicates Build&Run with a staler "toy OS" framing).
- [ ] Target size recorded: ~83 KB → ~28–30 KB (~63 % reduction), zero operational info lost (the Gate→env-var mapping stays inline).

---

## Documentation Notes

- This PR adds planning docs only; Tracks C and D are **specs** executed in follow-on PRs (the user's chosen scope keeps this PR reviewable).
- The audit (Track A) extends — does not replace — the `docs/appendix/audit-status/` corpus; cite the 57e (2026-05) and 75 (`74a`) cutoffs it builds on.
- Prefer a generated manifest over hand-maintained crate lists in `codebase-map.md` so it cannot rot again.
- Every chartered phase doc must name the bare-metal validation protocol (Track A.5) and adopt the "Validated-on-HW (run N, date)" status convention — the HW-only arc has no CI safety net.
