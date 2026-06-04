# Phase 83 — Release 1.0 Gate: Task List

**Status:** In progress
**Source Ref:** phase-83
**Depends on:** Phase 53 (Headless Hardening) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 75 (W^X Enforcement) ✅, Phase 77 (Pre-1.0 Correctness) ✅, Phase 78 (USB Host Foundation) ✅, Phase 79 (Modern Intel/Realtek NIC) ✅, Phase 80 (Intel HDA Audio) ✅; capability evidence from Phase 56/57 (display/input + audio + session) ✅, Phase 81 (Wi-Fi mt792x) and Phase 82 (AHCI/SATA) ✅. *(Phase 65 `fat_server` is **not** a blocker — FAT32 remains an ENOSYS stub and is recorded as an A.5 known-limitation, not consumed by the release gate.)*
**Goal:** Turn m3OS's broad capability surface into an **honest, evidence-backed 1.0 release promise** without writing a line of data-path code. The phase produces one authoritative release artifact (`docs/release/1.0-release-gate.md`) carrying a closed-vocabulary status legend, a target×workflow support matrix (with the **local-system/graphical stack in scope**), a recommended-configuration line, a system-requirements block, a first-class non-goals/known-limitations section, a must-pass validation **gate bundle** with the repo's PASS-not-SKIP discipline, a claim→gate **evidence trail**, and a maintainer-runnable release checklist. Two project decisions are settled and recorded here: (1) **1.0 includes the graphical/local-system branch** (greeter → login → compositor → `term`/launcher/bar, USB-HID input, HDA/AC'97 audio), screenshot-validated, with SSH-first headless remaining the *recommended admin* path; (2) the kernel crate stays **phase-tracked at `0.83.0`** — "1.0" is quality-bar/milestone language, **not** a SemVer `1.0.0` commitment, because no public syscall/userspace ABI is frozen yet. The phase closes with the version bump, the Phase 83 learning doc, and full documentation alignment.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | **Release contract** in `docs/release/1.0-release-gate.md`: closed status-tier legend, target×workflow support matrix (incl. the graphical/local-system rows now in scope), recommended-configuration line, system-requirements block, and a first-class non-goals/known-limitations section | Phase 53 support boundary; Phases 55/55c/78/79/80/81/82 capability evidence | ✅ Complete |
| B | **Validation gate bundle + evidence trail**: enumerate the must-pass `cargo xtask` gates with PASS-not-SKIP discipline; map every Supported claim → a named runnable gate (mutually exhaustive); fold in the screenshot-validated graphical gates; tier QEMU-validated vs host-tested vs bare-metal-only; publish a maintainer-runnable checklist | A | ✅ Complete |
| C | **Release decision + versioning posture**: record the local-system-in-scope decision with phase evidence; codify the phase-tracked `0.83.0` policy and document the frozen-ABI-is-the-real-1.0 blocker; resolve the design doc's open decision and version contradiction | A, B | ✅ Complete |
| D | **Documentation + version alignment**: bump kernel `0.82.0` → `0.83.0`; create + index the learning doc `docs/83-release-1-0-gate.md`; flip the roadmap README row + Tasks link; align top-level `README.md`/`docs/README.md`; reconcile the evaluation R10 doc + stale release cross-refs; design-doc + AGENTS.md closeout | A, B, C | 🔄 In progress |

> **Ordering note.** Track A writes the **promise** (the support matrix is the central artifact); Track B writes the **proof** (the gate bundle and the claim→gate evidence trail); Track C records the two **decisions** (scope = local-system-in; version = phase-tracked `0.83.0`) and reconciles the design doc; Track D **aligns every other doc** and performs the version bump + learning-doc cut. The A↔B relationship carries the phase's core integrity invariant: the matrix and the evidence trail must be **mutually exhaustive** — every `Supported`/`QEMU-validated` row names a backing gate, and every listed gate maps to a matrix row, with no orphans on either side.

> **No-new-runtime-code rule.** This is a release-contract + validation-discipline phase, not a feature phase. The only source change on the critical path is the `kernel/Cargo.toml` version bump (D.1); the validation gates it references already exist (the AGENTS.md gate table + the QMP/PPM screenshot plumbing in `xtask/src/{qmp,ppm}.rs`). If a gate the matrix needs does **not** yet exist, that is a finding to record as a non-goal or a follow-up — not new driver code smuggled into the release gate.

> **Versioning decision (settled).** Per the SemVer 2.0.0 contract — item 4 (*"Major version zero (0.y.z) is for initial development. Anything MAY change at any time. The public API SHOULD NOT be considered stable."*) and item 5 (*"Version 1.0.0 defines the public API."*) — and the nearest-peer precedent (Redox deliberately stays `0.x`, gating its 1.0 on a frozen `relibc` ABI; SerenityOS ships **no** numbered release and stays perpetually pre-1.0; Haiku ships "feature-complete" milestones as *beta*), m3OS bumps the kernel crate to **`0.83.0`** and uses "1.0" only as a quality-bar milestone. Item 4 licenses staying `0.x` while the public ABI is unfrozen; declaring `1.0.0` (item 5) would assert a public-API definition the system does not hold (Wi-Fi and AHCI landed in Phases 81–82 — it is still adding whole subsystems) and would break the `0.NN.0 = Phase NN` mapping. C.2 records the exact future work an eventual SemVer `1.0.0` requires.

> **Planning-doc reconciliation already landed.** The design doc (`docs/roadmap/83-release-1-0-gate.md`) and the roadmap README Phase 83 row were reconciled to both settled decisions **together with this task list** (same PR): the design doc now records local-system-in-scope and `0.83.0` (its earlier `1.0.0` Related-Documentation line and open headless-vs-local framing are gone), and the README row links this task list. Tasks **C.2 / D.3 / D.6** below therefore **verify and finish** that alignment (two residual design-doc fragments, the Phase 53 support-boundary supersession, the top-level README, and the evaluation docs) rather than starting from the pre-reconciliation state.

---

## Track A — Release contract & support matrix (`docs/release/1.0-release-gate.md`)

### A.1 — Closed status-tier legend

**File:** `docs/release/1.0-release-gate.md` (new)
**Symbol:** `## Status Legend` — a closed set of status words, each with a one-sentence definition: `Supported`, `Experimental`, `Host-tested-only`, `QEMU-validated`, `Bare-metal-validated`, `Out-of-scope`
**Why it matters:** a support matrix is only honest if its status words are *defined*; free-text "works"/"should work" is unfalsifiable. seL4's verified-platform table draws every cell from a closed four-level legend (Unverified / Ongoing / FC / Verified) — borrow that discipline so no cell can over-claim, and so a QEMU-only proof can never be silently presented as full hardware support.

**Acceptance:**
- [x] A `## Status Legend` section lists exactly the closed status set, each with a one-line definition; every later matrix cell uses one of these words and nothing else (grep-verifiable: no matrix cell contains the free-text "works"/"should work").
- [x] The legend separates `QEMU-validated`, `Host-tested-only`, and `Bare-metal-validated`, so QEMU-blind capabilities (mt76 radio, AHCI hot-plug/BOHC, RTL8125 2.5G) cannot be folded into plain `Supported`.

### A.2 — Target × workflow support matrix (graphical/local-system rows in scope)

**File:** `docs/release/1.0-release-gate.md`
**Symbol:** `## Support Matrix` — a table with columns `Capability / Workflow | Target | Status (A.1) | Backing gate (→ Track B) | Notes / limitation`, one row per shipped capability class from the AGENTS.md inventory
**Why it matters:** the matrix is the central release artifact — it *is* the written promise. Because the graphics stack is in 1.0 scope (decision C.1), the local-system rows (greeter, compositor clients, `term`, USB-HID input, HDA/AC'97 audio) appear as first-class rows with honest status tiers rather than being deferred to 1.x; every claim carries a target and a backing-gate reference so the document cannot drift from what is actually tested.

**Acceptance:**
- [x] The matrix has one row per shipped capability class (headless boot+login, SSH/telnet remote admin, IPv4 TCP/UDP, AF_UNIX, NVMe + AHCI/SATA root, multi-NIC e1000/e1000e/igb/igc/r8169/RTL8125, Wi-Fi mt792x, USB-HID input, graphical session, HDA/AC'97 audio, dynamic linking, multi-user), each with a Target, an A.1 Status word, and a Track-B backing-gate reference.
- [x] The graphical/local-system rows (greeter, compositor clients, `term`, launcher/bar, USB-HID input, audio) are present with the screenshot-validated status from B.3 — **not** an `Out-of-scope`/deferred marker.
- [x] No row claims a status with no Track-B evidence; the matrix is mutually exhaustive with the B.2 evidence trail (every `Supported`/`QEMU-validated` cell names a reproducible gate).
- [x] Each `Supported`/`QEMU-validated` row resolves to a **single copy-pasteable `cargo xtask` command** (inline, or via an unambiguous B.1 gate name) — the support claim is executable per-row, mirroring seL4's per-config `*_verified.cmake` target, not merely a pointer into a separate table.
- [x] The doc states it **extends** (does not replace) the Phase 53 [support boundary](../53-headless-hardening.md#support-boundary) and [gate bundle](../53-headless-hardening.md#gate-bundle), and supersedes Phase 53's now-stale "GUI / compositor / graphical session" and "Mouse input or audio" *Out-of-scope* rows (delivered in Phase 56/57) — the reconciliation of the Phase 53 file itself is tracked in D.6.

### A.3 — Recommended-configuration line

**File:** `docs/release/1.0-release-gate.md`
**Symbol:** `## Recommended Configuration` — one paragraph naming the exact validated config (QEMU machine type, OVMF firmware, NIC model, storage controller, RAM)
**Why it matters:** Redox publishes a single "most feature-complete experience" config (e.g. VirtualBox + Intel PRO/1000) so users land on the validated path. m3OS should name the exact `cargo xtask run-gui` / QEMU device set the project actually exercises, so "supported" has a concrete reference target a user can reproduce.

**Acceptance:**
- [x] A `## Recommended Configuration` section names the exact machine type, firmware (OVMF/UEFI), NIC model, storage controller, and minimum RAM that the Track-B gate bundle actually exercises.
- [x] The recommended config corresponds to a real, copy-pasteable `cargo xtask` invocation (grep-verifiable command, not prose).

### A.4 — System-requirements block

**File:** `docs/release/1.0-release-gate.md`
**Symbol:** `## System Requirements` — concrete minimums: architecture, firmware, RAM floor, assumed CPU features
**Why it matters:** Haiku's release notes carry a concrete CPU/RAM/firmware floor so a user knows the minimum machine the system is claimed to run on. A 1.0 promise needs the same: m3OS is UEFI-only x86_64, depends on FPU/XSAVE state handling and SMEP/SMAP, and needs a RAM floor for the heap/allocator.

**Acceptance:**
- [x] A `## System Requirements` section states concrete minimums: architecture (x86_64), firmware (**UEFI/OVMF, not legacy BIOS**), a numeric RAM floor, and the assumed CPU features (SSE/XSAVE state handling, SMEP/SMAP enforcement).

### A.5 — First-class non-goals / known-limitations section

**File:** `docs/release/1.0-release-gate.md`
**Symbol:** `## Non-Goals and Known Limitations` — a standalone bulleted section, each item bounded and specific
**Why it matters:** the strongest release precedents treat non-goals as first-class (Genode states a single-vCPU bound inline per feature; seL4 marks exactly which proofs are `Ongoing` vs done; Haiku lists known issues per feature). Vague "not production-ready" is useless — each non-goal must name the exact missing capability so the promise is honest and so post-1.0 phases are clearly framed as 1.x growth, not hidden release debt.

**Acceptance:**
- [x] A standalone `## Non-Goals and Known Limitations` section exists; every item is specific and bounded (names the exact missing capability) — vague phrasing like "not production-ready" is disallowed.
- [x] The **"no frozen public syscall/userspace ABI"** limitation is listed explicitly as the headline reason 1.0 is a quality-bar milestone rather than a SemVer `1.0.0` (cross-refs the C.2 versioning posture).
- [x] Concrete bounded non-goals are enumerated (e.g. NCQ, AHCI hot-plug/staggered-spin-up, IPv6/DHCPv6, SMP CPU hot-unplug, the QEMU-absent mt76 radio model, non-`Sata` AHCI signatures, GPU acceleration, **FAT32 writes — `fat_server` is a permanent ENOSYS stub (Phase 65); ext2 is the supported on-disk filesystem**), each with its one-line reason.
- [x] Post-1.0 phases (84 Spectre/KPTI, 85 toolchains, 86 GitHub, 87 Node.js, 88 Claude Code, 89 IPv6/DHCPv6) are listed as explicit 1.x scope, **not** 1.0 blockers.

---

## Track B — Validation gate bundle & evidence trail

### B.1 — Must-pass gate bundle (with PASS-not-SKIP discipline)

**Files:**
- `docs/release/1.0-release-gate.md`
- `AGENTS.md` (the existing opt-in regression gate table is the source of truth for env-var triggers)

**Symbol:** `## Gate Bundle` — a checklist of the exact `cargo xtask` commands that must pass, in **two classes**: (1) **env-gated opt-in regression gates** from the AGENTS.md table, each with its `M3OS_*_REGRESSION` trigger (`ssh-e1000-banner-check`, `doom-audio-smoke`, `termios-smoke`, `tui-app-smoke`, `doom-concurrent-smoke`, `tiling-smoke`, `htop-render-probe`, the `xhci-*`/`usb-smoke` set, `tls-smoke`, `dns-smoke`, `multi-nic-smoke`, `hda-smoke`, `wifi-smoke`, `ahci-smoke` + `ahci-root-smoke`); and (2) **always-on / non-env probes** that have **no** `M3OS_*_REGRESSION` var (`cargo xtask check` (clippy + fmt + host-test crates), `smoke-test`, `regression`, and the screenshot probes `compositor-stress`, `less-render-probe`, `session-smoke`) — listed with command, class, trigger, and PASS condition each
**Why it matters:** the gate bundle is the release "definition of done" — it turns the A.2 promise into something evidence-backed and repeatable. The repo already encodes the PASS-not-SKIP pattern (`tls-smoke`/`dns-smoke` must PASS, not SKIP, or the musl-dependent fixes ride unverified); generalize it so a 1.0 claim never counts a SKIP (musl-absent, no QEMU mt76 model) as a pass.

**Acceptance:**
- [x] A `## Gate Bundle` checklist lists each must-pass gate with its exact `cargo xtask` command, its trigger (always-on vs the env var from the AGENTS.md table), and its PASS condition.
- [x] The bundle states the **PASS-not-SKIP** rule explicitly: gates whose backing test can SKIP (musl-absent → `tls-smoke`/`dns-smoke` SKIP; no QEMU mt76 model → `wifi-smoke` skip-with-reason) must be run on a platform where they PASS before 1.0 is claimed, **or** the corresponding A.2 row is downgraded (e.g. to `Bare-metal-validated`).
- [x] For **class (1)** gates the bundle references the `M3OS_*_REGRESSION` env vars from the AGENTS.md gate table verbatim (grep-verifiable against `AGENTS.md`), so the two never drift; **class (2)** probes are explicitly marked always-on/non-env (no env var), so the verbatim-env-var check is never falsely applied to them. *(Promoting a class-(2) probe such as `compositor-stress` to an env-gated release gate would require adding it to the AGENTS.md table — itself in-scope doc work for this task.)*

### B.2 — Evidence trail: claim → gate map (mutually exhaustive)

**File:** `docs/release/1.0-release-gate.md`
**Symbol:** `## Evidence Trail` — a table mapping every A.2 matrix capability → the exact B.1 gate(s) that prove it
**Why it matters:** seL4 pins each release's claims to a `verification-manifest` that ties "this is verified" to a reproducible artifact; m3OS's analog is a claim→gate table so no `Supported` row is unfalsifiable and no gate is orphaned. This mutual-exhaustiveness is the phase's integrity invariant — if the matrix lists a capability with no gate (or a gate exercises nothing in the matrix), the document will lie on the next change.

**Acceptance:**
- [x] An `## Evidence Trail` table maps each `Supported`/`QEMU-validated` capability in A.2 to ≥ 1 named gate from B.1.
- [x] The mapping is **mutually exhaustive**: no `Supported`/`QEMU-validated` capability lacks a gate, and no B.1 gate is absent from the matrix — a reviewer can diff the two lists and find zero orphans on either side. *(Forward + Reverse tables in the doc make both directions diffable.)*

### B.3 — Graphical / local-system evidence gates (screenshot-validated)

**Files:**
- `docs/release/1.0-release-gate.md`
- `xtask/src/main.rs` (existing gates: `cmd_compositor_stress`, `htop-render-probe`, `tiling-smoke`, `usb-smoke`, `hda-smoke`, `less-render-probe`)
- `xtask/src/qmp.rs`, `xtask/src/ppm.rs` (the QMP keystroke + PPM screenshot plumbing)

**Symbol:** the QMP/PPM screenshot-probe rule — every "the screen shows X" claim cites a `QmpClient::screendump` (`xtask/src/qmp.rs`) + `ppm` pixel assertion (`xtask/src/ppm.rs`), the path `compositor-stress` / `htop-render-probe` / `less-render-probe` / `session-smoke` already use, **never** a serial `Wait` on a sentinel
**Why it matters:** the GUI is in 1.0 scope (C.1), and the serial-only smoke harness is **blind to rendering** — a serial sentinel proves a program ran, not that it drew anything (SerenityOS's CI takes the same posture: it boots the system under QEMU and asserts on framebuffer screenshots). Every graphical `Supported` row must be backed by a framebuffer screenshot (or WAV-capture, for audio) probe, per the research's "no serial-sentinel proofs for graphical claims" rule and this repo's existing headless-screenshot methodology.

**Acceptance:**
- [x] Every graphical/local-system row in A.2 (greeter, compositor, `term`, launcher/bar, USB-HID input, audio) cites a QMP/PPM screenshot gate (or the `hda-smoke` non-silent-WAV capture) as its backing evidence — not a serial-only `Wait`.
- [x] The B.1 bundle's screenshot-validated set is drawn **only from gates that already exist** (`compositor-stress`, `htop-render-probe`, `tiling-smoke`, `session-smoke`, `less-render-probe`) plus the `hda-smoke` non-silent-WAV check — **no gate is invented**.
- [x] The **greeter/login render path has no dedicated screenshot probe today** (xtask ships `session-smoke`/`compositor-stress`, but no greeter render gate) — so the greeter row is backed by `session-smoke` + `compositor-stress` and marked `Experimental` (not `Supported`) unless/until a greeter render probe is built; building that probe is recorded as an A.5 follow-up, **not** smuggled into this documentation phase (honoring the no-new-runtime-code rule).
- [x] Any graphical capability with **no** screenshot/WAV probe is marked `Experimental`/`Host-tested-only` in A.2 — not `Supported`.

### B.4 — Hardware-honesty tiering (QEMU vs host vs bare-metal)

**File:** `docs/release/1.0-release-gate.md`
**Symbol:** the per-row Status-assignment rules separating `QEMU-validated`, `Host-tested-only`, and `Bare-metal-validated` for the QEMU-blind hardware (mt792x Wi-Fi radio, AHCI hot-plug/BOHC/SSS, RTL8125 2.5G, the e1000-family INTx path)
**Why it matters:** several capabilities are inherently un-provable in QEMU (no mt76 device model; AHCI hot-plug/BOHC left as no-ops by `ich9-ahci`; RTL8125 real-silicon `ping`). The matrix must never collapse host-tested / QEMU-validated / bare-metal into "supported" — that is the research's "over-claiming hardware from QEMU alone" anti-pattern and would make the headline promise dishonest.

**Acceptance:**
- [x] Wi-Fi (mt792x) is recorded as `Host-tested-only` (soft-MAC/handshake logic) **plus** `Bare-metal-validated` (radio via VFIO), never plain `Supported`, with the "no QEMU mt76 model" reason inline.
- [x] AHCI hot-plug/BOHC/SSS and RTL8125 2.5G are recorded as `Bare-metal-validated` with their QEMU-skip reason; the AHCI root-mount + IDENTIFY/RW/flush/recover path is `QEMU-validated` (backed by the `ahci-smoke` + `ahci-root-smoke` gates).
- [x] Every `Bare-metal-validated` row names the validation method (VFIO / real hardware runbook) rather than implying CI coverage.

### B.5 — Maintainer-runnable release checklist

**File:** `docs/release/1.0-release-gate.md`
**Symbol:** `## Release Checklist` — an ordered, copy-pasteable command list (the gate bundle in run order + the manual operator checks) that certifies a build off-CI, **plus** the 1.0-cut release artifacts: a release-notes/CHANGELOG entry, a reproducible-image step (`cargo xtask image --sign` + verification), and a known-good/rollback note (the commit/tag the validated image corresponds to)
**Why it matters:** "a gate only CI can run is theater" — the release evidence must be reproducible by the person cutting the release with named `cargo xtask` commands, mirroring seL4's buildable verified-config artifact and Redox's maintainer-run CI; and a 1.0 cut needs the standard release artifacts (notes, a verifiable image, a rollback anchor) a user can act on, not just a green test run.

**Acceptance:**
- [x] A `## Release Checklist` lists the gate bundle as an ordered set of copy-pasteable `cargo xtask` commands plus the manual operator checks (e.g. attach VNC viewer, SSH-in banner check), runnable on a maintainer machine without CI.
- [x] The checklist enumerates the `M3OS_*_REGRESSION` env vars that enable the opt-in gates (cross-ref the AGENTS.md table) so a release run turns them all on, and states the single-line PASS verdict the maintainer is certifying.
- [x] The checklist includes a **release-notes / CHANGELOG** artifact for the 1.0 milestone — component-grouped with a "Known issues" section (the Redox release-note shape), drawing its known-issues from the A.5 non-goals.
- [x] The checklist includes a **reproducible-image** step — `cargo xtask image --sign` (cross-referencing A.3's recommended configuration) plus how a user verifies the produced image — and a **known-good / rollback** note recording the exact commit/tag the validated image corresponds to.

---

## Track C — Release decision & versioning posture

### C.1 — Record the local-system-in-scope decision (with phase evidence)

**Files:**
- `docs/release/1.0-release-gate.md`
- `docs/roadmap/83-release-1-0-gate.md` (design-doc reconciliation — see D.6)

**Symbol:** `## Release Scope Decision` — 1.0 **includes** the local-system/graphical branch, backed by named phase evidence; SSH-first headless remains the *recommended admin* path
**Why it matters:** the design doc was written **before the GUI stack was properly planned** and framed headless-vs-local as an open decision. The graphics stack now exists in-tree (display_server, greeter, compositor clients, USB-HID input, HDA/AC'97 audio), so the settled decision is to include it — but it must be recorded with the evidence (which phases back it) rather than merely asserted.

**Acceptance:**
- [x] A `## Release Scope Decision` section states explicitly that 1.0 **includes** the local-system/graphical branch and names the phases that constitute the evidence (Phase 47 full-screen workload; Phase 56 display+input; Phase 57 audio + `session_manager`; Phase 78 USB-HID; Phase 80 HDA).
- [x] The section states that the **recommended admin path remains SSH-first/headless** while the graphical session is a `Supported` (screenshot-validated) workflow — resolving the design doc's open headless-vs-local question.
- [x] The decision is reflected back into the design doc (D.6): no remaining "if 1.0 includes the local-system branch" conditional language survives. *(Verified: design doc Critical-Items row reads "Recorded scope decision (local-system in scope)"; no conditional survives.)*

### C.2 — Versioning posture: phase-tracked crate + the real-1.0 (ABI) blocker

**Files:**
- `docs/release/1.0-release-gate.md`
- `docs/roadmap/83-release-1-0-gate.md` (resolve the internal contradiction — see D.6)

**Symbol:** `## Versioning Posture` — the kernel crate stays phase-tracked at `0.NN.0` (→ `0.83.0`); "1.0" is a quality-bar milestone, **not** SemVer `1.0.0`; the headline blocker for an eventual SemVer `1.0.0` is a frozen public syscall/userspace ABI
**Why it matters:** SemVer 2.0.0 item 5 says `1.0.0` *defines a stable public API* — a promise m3OS does not hold and is not ready to make (it is still adding whole subsystems: Wi-Fi and AHCI landed in Phases 81–82). Declaring `1.0.0` would over-promise and break the `0.NN.0 = Phase NN` mapping. The honest posture mirrors Redox (stays `0.x` until the `relibc` ABI is frozen) and isolates any future stability promise to a narrow surface.

**Acceptance:**
- [x] A `## Versioning Posture` section states the kernel crate is bumped to `0.83.0` (phase-tracked) and that "1.0" is quality-bar language, not a SemVer commitment; it cites SemVer 2.0.0 **item 4** (0.y.z = initial development, public API SHOULD NOT be considered stable) and **item 5** (1.0.0 defines the public API), and the Redox / SerenityOS / Haiku precedent (Redox gates 1.0 on a frozen `relibc` ABI; SerenityOS ships no numbered release; Haiku ships feature-complete cuts as *beta*).
- [x] The section names the concrete future work an eventual SemVer `1.0.0` requires (a frozen syscall/userspace ABI, optionally versioned in a narrow `syscall-abi`-style surface à la Redox's `relibc`) and links it from the A.5 non-goals.
- [x] **Verification (the design-doc reconciliation already landed in this PR):** confirm the design doc's Related-Documentation line and Versioning-baseline gate **both** read `0.83.0` and that no `1.0.0` crate-version language survives (grep) — the earlier internal contradiction is resolved, not still pending. *(Verified: design doc reads `0.83.0`; only explanatory "not a SemVer 1.0.0" usage remains.)*

---

## Track D — Documentation & version alignment

### D.1 — Bump kernel crate `0.82.0` → `0.83.0`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.83.0"`
**Why it matters:** the `0.NN.0 = Phase NN` convention (0.82.0 = Phase 82) requires the Phase 83 release-gate cut to land as `0.83.0`; per Track C the crate stays phase-tracked even though the public milestone language is "1.0". This is the explicit version bump the release closeout performs, mirroring Phase 82 Track F's `0.81.0` → `0.82.0`.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version` reads `0.83.0`; `cargo xtask check` builds clean and the boot banner / version reporting reflects `0.83.0`.
- [x] No reference bumps the kernel crate to `1.0.0` (grep-verifiable: `1.0.0` does not appear as a crate version); "1.0" appears only as milestone/public language per Track C.

### D.2 — Create + index the learning doc `docs/83-release-1-0-gate.md`

**Files:**
- `docs/83-release-1-0-gate.md` (new)
- `docs/README.md` (the `### Phase-Aligned Learning Docs` table)

**Symbol:** a phase learning doc following the aligned learning-doc template (`docs/appendix/doc-templates.md`); a new `Release 1.0 Gate | 83 | …` row in the learning-docs table
**Why it matters:** every phase ships a learning doc (the design doc's "Learning Documentation Requirement"); this one teaches why release engineering is architecture — the support matrix, the gate bundle, the headless-vs-local decision, and the versioning posture — and links the authoritative `docs/release/1.0-release-gate.md`.

**Acceptance:**
- [ ] `docs/83-release-1-0-gate.md` exists and follows the learning-doc template (Overview / What This Doc Covers / Core Implementation / Key Files / How This Phase Differs / Related Roadmap Docs / Deferred), explaining the matrix, gate bundle, scope decision, and versioning posture in learner-friendly terms.
- [ ] `docs/README.md`'s `### Phase-Aligned Learning Docs` table has a `Release 1.0 Gate | 83 | …` row linking the new doc.
- [ ] The learning doc links the authoritative `docs/release/1.0-release-gate.md` and the design + task docs.

### D.3 — Flip the roadmap README row + Tasks link

**File:** `docs/roadmap/README.md`
**Symbol:** the Phase 83 Release-Gate row — the Tasks cell and Primary-Outcome text were **already aligned in this PR** (Tasks links `./tasks/83-release-1-0-gate-tasks.md`; Primary-Outcome states local-system-in-scope + `0.83.0`); the only **remaining** action is flipping Status `Planned` → `Complete` (kernel `0.83.0`) when the phase lands
**Why it matters:** the roadmap README is the authoritative phase index; the Tasks-link + scope/version reconciliation landed with this task list, so the residual work is just the on-landing status flip.

**Acceptance:**
- [ ] (Verify — landed in this PR) the Phase 83 row's Tasks cell links `./tasks/83-release-1-0-gate-tasks.md` and the Primary-Outcome reflects the local-system-in-scope decision and `0.83.0` (not "headless-only").
- [ ] On landing, the row's Status flips `Planned` → `Complete` (kernel `0.83.0`).

### D.4 — Align top-level `README.md` + `docs/README.md` release language

**Files:**
- `README.md`
- `docs/README.md`

**Symbol:** the project-positioning paragraph(s) + a link to `docs/release/1.0-release-gate.md`
**Why it matters:** the design doc requires top-level docs to tell the same release story; once the support matrix exists, the top-level README must point to it and describe the system as the matrix does (graphical-capable, SSH-first-recommended, `0.83.0` / "1.0 quality-bar") — neither over- nor under-claiming.

**Acceptance:**
- [ ] `README.md` links the authoritative release-gate doc and its one-line system description matches the A.2 matrix scope (every claim it makes appears in the matrix).
- [ ] `docs/README.md` links `docs/release/1.0-release-gate.md` from its documentation index.

### D.5 — Reconcile the evaluation R10 doc + stale release cross-refs

**Files:**
- `docs/evaluation/roadmap/R10-release-1-0-and-beyond.md`
- `docs/evaluation/roadmap/README.md`

**Symbol:** R10's "Official roadmap phases covered" + "Key Cross-Links" lists (which still reference the pre-renumber `Phase 58 — Release 1.0 Gate` / `Phase 62 — Claude Code`, now Phase 83 / 88) and the Open-Questions headless-vs-GUI item (now resolved by C.1)
**Why it matters:** R10 still points at the pre-renumber phase IDs (58–62) and frames the GUI-vs-headless question as open; aligning it to the current IDs (83, 84–89) and recording the resolved decision keeps the evaluation roadmap from contradicting the shipped gate.

**Acceptance:**
- [ ] R10's phase cross-references point to the current IDs (Phase 83 release gate; 85/86/87/88/89 post-1.0), not the stale 58–62.
- [ ] R10's Open-Question about headless-vs-GUI is marked **resolved** (local-system in scope) or removed, consistent with C.1.

### D.6 — Design-doc reconciliation + AGENTS.md closeout

**Files:**
- `docs/roadmap/83-release-1-0-gate.md`
- `AGENTS.md`

**Symbol:** the design-doc residual fragments + AGENTS.md closeout — the major reconciliation (Companion-Task-List link; Feature Scope / Evaluation Gate / Implementation Outline / Acceptance Criteria / Related-Documentation → local-system-in-scope + `0.83.0`) **already landed in this PR**; the **residual** deltas are two stale fragments, the Phase 53 boundary supersession, and the on-landing AGENTS.md version bump
**Why it matters:** the design doc was reconciled alongside this task list, but two open-decision fragments survived — the **Critical-and-Non-Deferrable-Items** row "Explicit headless vs local-system decision" and the **Deferred-Until-Later** bullet "A full desktop claim if the local-system branch is not yet ready" — and the canonical Phase 53 [support boundary](../53-headless-hardening.md#support-boundary) still lists "GUI / compositor / graphical session" and "Mouse input or audio" as *Out of scope* (delivered in Phase 56/57); AGENTS.md is the always-loaded inventory and must reflect the version bump on landing.

**Acceptance:**
- [ ] (Verify — landed in this PR) the design doc's Companion Task List links the task doc, and Feature Scope / Evaluation Gate / Implementation Outline / Acceptance Criteria / Related-Documentation read local-system-in-scope + `0.83.0` with no `1.0.0` crate bump.
- [ ] The two **residual** design-doc fragments are rewritten to the settled posture: the Critical-Items "Explicit headless vs local-system decision" row → "Recorded scope decision: local-system in scope, SSH-first headless recommended"; and the Deferred-Until-Later "full desktop claim if … not yet ready" → a scope boundary (a full general-purpose desktop beyond the screenshot-validated greeter → compositor → `term`/launcher/bar session), not a readiness contingency.
- [ ] Phase 53's now-superseded support-boundary rows ("GUI / compositor / graphical session"; "Mouse input or audio" = *Out of scope*) are reconciled — struck or footnoted as **superseded by Phase 56/57/83** — and the new release-gate doc states it **extends** (does not replace) the Phase 53 support boundary + gate bundle.
- [ ] On landing, AGENTS.md's kernel version reads `0.83.0`; a release-gate capability bullet is added **only** if the gate introduces a new capability class (otherwise the inventory is left unchanged per the maintenance policy).

---

## Documentation Notes

- **What changed relative to the design doc.** This task list settles two questions the design doc originally left open or self-contradictory, and the **design-doc + roadmap-README reconciliation already landed in this same PR**: (1) the headless-vs-local-system decision is settled as **local-system-in-scope** (the design doc predated the GUI stack); (2) the version posture is settled as **phase-tracked `0.83.0`**, not `1.0.0` — "1.0" is quality-bar language only. The design doc no longer contains the earlier `1.0.0` Related-Documentation line or open headless-vs-local framing; C.2 / D.3 / D.6 therefore **verify and finish** the alignment (two residual design-doc fragments, the Phase 53 boundary supersession, the top-level README, and the evaluation docs) rather than starting from the pre-reconciliation state.
- **No data-path code.** The phase ships no new runtime/driver code; the only source change is the `kernel/Cargo.toml` bump (D.1). Every validation gate it references already exists (the AGENTS.md gate table + the `xtask/src/{qmp,ppm}.rs` screenshot plumbing). A gate the matrix needs but that does not exist is a finding for A.5 non-goals or a follow-up phase — never new driver code inside the release gate.
- **The A↔B integrity invariant.** The support matrix (A.2) and the evidence trail (B.2) must be mutually exhaustive — every `Supported`/`QEMU-validated` claim has a backing gate and every gate maps to a claim. A reviewer should be able to diff the two and find no orphans; this is the single most important review check for the phase.
- **Honesty over breadth.** Status tiers are a closed vocabulary (A.1); QEMU-blind hardware is tiered `Bare-metal-validated`/`Host-tested-only` (B.4); graphical claims require screenshot proof (B.3); SKIPs never count as PASS (B.1). A small, honest 1.0 promise is the goal — not the widest matrix.
- **Prefer exact targets.** Reference exact doc sections (`## Support Matrix`), exact `cargo xtask` command names, and the exact `M3OS_*_REGRESSION` env vars over directories or "the gates".
