# Phase 98 — Roadmap Audit & Re-Charter (toward a real-hardware GUI workstation)

**Status:** Complete
**Source Ref:** phase-98
**Depends on:** the full Phase 1→97 arc being nominally "done" (hardware substrate → drivers → toolchains → bare-metal bring-up)
**Builds on:** Phase 58 (Documentation Reconciliation Pass) and `docs/appendix/audit-status/` established the precedent that a phase's *claimed* status and its *validated* status drift over time. That audit corpus stopped at Phase 57e (2026-05-08); the pre-1.0 audit (`74a-pre-1.0-audit.md`) extended it to Phase 75. This phase brings the evidence standard up to **Phase 97**, institutionalizes it (a gate / recorded run / host test behind every Status), and pairs the backward audit with a forward re-charter of the next arc.
**Primary Components:** `docs/roadmap/` (every phase doc + the README status table + the stale delivery-gantt), `docs/appendix/audit-status/` (the audit report), `docs/appendix/codebase-map.md` + `docs/README.md` + `docs/roadmap/tasks/README.md` (the stale index layer), the root `Cargo.toml` + ~116 member `Cargo.toml` files (versioning reform), `AGENTS.md` (rules slimming), and — for Track B scoping — `userspace/init` (the bare-metal boot manifest), `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_framebuffer_mmap` WC gap), the `display_server`/`greeter`/`session_manager` graphical stack, the SMP scheduler block/wake path, and the absent ACPI / I2C / power / Wi-Fi-supplicant / installer / networked-`pkg` substrates.

## Milestone Goal

Three deliverables that turn the end of the original roadmap into a trustworthy map plus a deliberate next arc:

1. **An honest, evidence-backed audit** of the completed roadmap (Phases 1→97). Every phase's Status is reconciled against falsifiable evidence — a named passing gate, a recorded hardware run, or a host test — and the **index layer** (`codebase-map.md`, the two `README` indexes, the delivery gantt) is repaired so the map matches the tree. Output: a per-phase **Validated / Claimed-unvalidated / Regressed** verdict and the corrected README.
2. **A re-chartered roadmap (Phases 99–110)** for the next arc: a **usable GUI workstation on the real Dell Tiger Lake laptop**, then the **HP OmniBook (AMD Ryzen AI 9 365 / Strix Point)**. The arc is sequenced by dependency and each phase ships as a template-conformant design + task doc.
3. **Two repository-hygiene reforms specified for execution in follow-on PRs:** a **single unified workspace version** (kill the phase-encoded per-crate versions that conflict when phases land in parallel) and a **slimmed `AGENTS.md`** (it loads every session and currently violates its own "keep it small" policy by ~63%).

This phase **charters and specifies**; the chartered phases and the two reforms **execute as their own PRs**. This PR is planning docs only.

## Why This Phase Exists

The project reached the end of its original roadmap: the hardware substrate (55–55c), display/input/audio (56–57), the long toolchain series (85–95: Python, clang, Go, Node, Rust, Claude Code), the dynamic-C runtime (93), and bare-metal networking on real silicon (96). At this inflection point three things are worth doing *before* piling on features:

- **Verify before extending.** The audit found the evidence convention is strongly time-stratified: from ~Phase 63 onward nearly every "Complete" row cites a concrete gate (`git-https-smoke` 36/36, `node-jit-smoke` 20/20, `claude-smoke` 27/27, `ipv6-smoke`, `ure-smoke`, …), but ~50 earlier "Complete" rows cite **no inline evidence**, and several rows are flatly stale (Phase 54a reads `Planned` while Phase 66 claims to *close* it). The index layer is worse: `codebase-map.md` is frozen at ~Phase 55 (lists ~40 of ~116 crates, omits the entire GUI/audio/driver/toolchain trees), and `file-backed-mmap.md` still says demand-paged mmap is unimplemented though Phase 95b landed `MAP_LAZY_FILE`. A deliberate audit + index repair turns the roadmap back into a trustworthy map — exactly where the *next* arc (I2C/LPSS, ACPI, GUI, power) will be navigated from.
- **Re-charter with intent, and scope the WHOLE workstation.** The original Phase 98 stub charted only GUI-pointer-keyboard-Wi-Fi. A completeness review surfaced that "real-hardware GUI **workstation**" is much larger: there is **zero** laptop power/ACPI code (no battery, brightness, suspend, lid) — and ACPI device/IRQ enumeration is a **hidden prerequisite** of the charted I2C-HID touchpad; the recurring **lost-wakeup / SMP-fault** bug class is open and the laptop is 8-core (it cannot pin `-smp 1` the way the toolchain gates do); the userspace framebuffer is **still mapped write-back** (only the kernel console got the Phase 96 WC fix), so the charter's "now-fast framebuffer" premise is false; and there is **no installer, no networked/signed packaging, and no GUI apps**. This phase scopes all of it.
- **Stop bleeding tokens and merge-conflicts on bookkeeping.** Crate versions encode the phase number (`kernel=0.96.0`, `kernel-core=0.53.0`, usb drivers `0.92.0`), so two parallel phases both edit the same `version = "…"` lines — a guaranteed, semantically-meaningless conflict. And `AGENTS.md` is ~83 KB (~37 K tokens) loaded every session, 58 % of it a regression-gate table whose cells have become 200–600-word phase diaries. Both are cheap to fix and compound on every future phase.

## Learning Goals

- How to reconcile *claimed* vs *validated* status at scale using falsifiable evidence, and why an OS project's index/cross-cutting docs rot faster than its phase docs.
- Why a teaching OS that has lived on always-on CI gates needs an explicit **bare-metal validation strategy** when it moves into a hardware-only arc that QEMU cannot model.
- How a monorepo eliminates parallel-development version conflicts with a single `[workspace.package]` version, and why per-crate semver is pure bookkeeping for crates that never ship to crates.io.
- How to sequence a hardware bring-up arc by *dependency* (ACPI before I2C-HID; SMP robustness before multi-core GUI; Wi-Fi + power backends before the settings UI that drives them) rather than by feature wishlist.

## Feature Scope

### Track A — Backward audit + index repair (the trustworthy map)

- Walk every phase doc (1→97); reconcile its Status against a named passing gate, a recorded HW run, or a host test. Flip stale fields both directions (the Phase 54a `Planned`-but-closed case; the self-stale Phase 96 "stale `Planned`" example that the live README row already corrects).
- Produce/refresh `docs/appendix/audit-status/` with a per-phase **Validated** (gate/HW evidence cited) / **Claimed-unvalidated** (no current evidence) / **Regressed** (gate now fails) verdict, extending the 2026-05 audit (which stopped at 57e) and the pre-1.0 audit (which stopped at 75) up to **97**.
- **Repair the index layer** the docs-tree audit found rotted: regenerate `codebase-map.md`'s workspace/kernel/ports sections from `Cargo.toml` + the live tree (prefer a generated manifest over a hand-list), fix `docs/README.md` (~34 missing docs) and `docs/roadmap/tasks/README.md` (stale through P56, broken P51 link), correct the README delivery-gantt's pre-renumbering P78–81 labels, and correct/supersede `docs/appendix/file-backed-mmap.md` (contradicts Phase 95b).
- **Reconcile the 1.0 / versioning posture:** Phase 83 declared a "Release 1.0 Gate" (Complete) yet the kernel stayed `<1.0` and that gate names still-`Planned` deps (Phase 59 Validation Backlog, Phase 65 fat_server). Close/supersede or downgrade, and set an explicit version-cut policy for the new arc (pairs with Track C).
- **Decide CI posture for the forward arc**, which is the harder half: the entire next arc (ACPI, I2C-HID, AX201 Wi-Fi, bare-metal GUI, power, laptop audio) is **HW-only and un-CI-able**. Track A must deliver a **bare-metal validation strategy** — a per-HW-phase manual protocol generalized from `scripts/ure-vfio-validate.md`, a recorded-evidence convention (serial-capture / photo / on-device render assertion shipped over the `usb-logsink` / network sink), and a policy that HW-only phases carry **"Validated-on-HW (run N, date)"** rather than a bare "Complete."

### Track B — Forward re-charter: the GUI-workstation arc (Phases 99–110)

Charter the next arc as template-conformant design + task docs, sequenced by dependency. One-line milestone + the gating dependency for each (full design docs are the deliverable):

| Phase | Theme | Why it's here / gating dep |
|---|---|---|
| **99** | **SMP & Scheduler Robustness Hardening** | Retires the recurring lost-wakeup bug class by **consolidating + validating at `-smp 8`** the single-state-word block/wake model that already landed in Phase 57a (the per-site ad-hoc patches of 89/90b/95 are uneven and unvalidated above `-smp 4`), plus the kstack/`PROCESS_TABLE`-held-across-faults audit, the 4 GiB SMP panic-quiesce + OOM/race, the live ~11–15 % step-25 demand-fault NULL-deref CI flake, `copyfile→EFAULT`, and the 55c `net::remote` test encoder bug. **Prerequisite for multi-core bare-metal GUI** — the laptop is 8-core and cannot pin `-smp 1` like the toolchain gates do. CI-able (QEMU SMP). |
| **100** | **Bare-Metal GUI Session (Dell)** | Add `display_server`/`mouse_server`/`session_manager`/`greeter` to init's `BUILTIN_CONFIGS` (omitted today → laptop boots to a text console), add the **write-combining PAT attribute to the user framebuffer VMA** in `sys_framebuffer_mmap` (the false-premise fix), and drive the cursor with an **interim USB mouse** via the existing `usb-hid → mouse_server` inject path. Depends on 99. |
| **101** | **ACPI Platform Foundation** | AML interpreter (pragmatic subset) + namespace + `_HID`/`_CRS` device & interrupt-resource enumeration + SCI event handling. **The substrate I2C-HID and power both need** — the touchpad's I2C address and GpioInt come from ACPI `_CRS`. |
| **102** | **I2C-HID Touchpad (Intel LPSS)** | DesignWare I2C controller (`dwiic` ref) + I2C-HID transport + multitouch report parse (`imt` ref; Phase 92b Report-Protocol home) → `mouse_server` inject. **The real built-in pointer** (no PS/2 pointer on the laptop). Depends on 101. |
| **103** | **Laptop Power Management** | Battery/AC, backlight/brightness, thermal zones, lid-switch + power-button (SCI), P-states/cpufreq; S3/S0ix suspend-resume as a stretch. **Table-stakes for a daily-driver laptop.** Depends on 101. |
| **104** | **Wi-Fi: Intel AX201 / CNVi + Supplicant** | `iwx`-style AX201/CNVi driver (OpenBSD `iwx(4)` ref) → `RemoteNic`, **plus a running supplicant/connect daemon** (`wifi-core` today is only a config *parser*). The Dell's only built-in NIC (no Ethernet port). |
| **105** | **Native GUI Toolkit & Core Desktop Apps** | A minimal immediate-mode Rust widget toolkit on `desktop_client` (the central missing layer — every GUI app hand-rolls pixels today), a clipboard/data-transfer protocol, a screenshot tool, an image viewer, and a **settings/control panel** (network picker + brightness + battery + volume) — the user-facing consumer of 103/104. Depends on 100; settings panel sequenced after 103/104. |
| **106** | **USB Installer & NVMe Install** | The M1→M3 ladder: a combined GPT(ESP+ext2) USB image, a USB-ext2 root bootstrap in init, an NVMe root bootstrap (mirroring the AHCI path), and an on-device installer (raw image USB→NVMe copy first; GPT/ESP/on-device `mkfs.ext2` follow-on) + first-user setup. Depends on bare-metal NVMe root validated (Track A confirms). |
| **107** | **Networked & Signed Package Distribution** | GitHub Releases as the `.m3pkg` blob store + a tiny **ed25519-signed static `index.m3idx`** (gh-pages mirror); `pkg update`/fetch over the existing Phase 86c `curl`/mbedTLS (no new TLS in the installer — reuse the spawn seam); ed25519 index verify via `crypto-lib`; a `build-and-publish.yml` + `xtask repo-index` CI flow ($0 on a public repo). The `pkg` engine (solve/verify/extract/DB) is 100 % reused. |
| **108** | **HP OmniBook / AMD Strix Point Bring-up** | **MT7925 connac3 Wi-Fi** (the gating driver — device-ID already matches `is_mt792x`, but needs the MT7925 firmware blobs + connac3 MCU/WFDMA adaptation), **bare-metal AMD-Vi validation** (coded + host-tested, never run on real AMD silicon; fails graceful to identity-map), the **fam1Ah (Zen 5) microcode blob** (trivial), and the **AMD I2C-HID controller backend** (`AMDI0010` MMIO DesignWare I2C + `pinctrl-amd`/`AMDI0030` GPIO for the HID IRQ). Sequenced **after** the Dell line proves the stack; most paths (GOP FB, xHCI, NVMe, xAPIC, the Phase 96 boot-rescue) are bus-agnostic and carry over free. |
| **109** | **Bare-Metal Audio** | First **determine** the Dell codec path (legacy Intel HDA vs SoundWire + SOF DSP — modern Tiger Lake laptops often route audio over SoundWire, in which case the Phase 80 HDA driver may not bind), then HDA bare-metal validation **or** a new SoundWire+SOF driver. A scoping-risk the original charter missed. |
| **110** | **Real-Hardware Security Hardening** | Activate + **bare-metal-validate KPTI** (Phase 84 Track A scaffolding, never activated), add ASLR + stack canaries / CET shadow stacks, move password hashing to **argon2id**, and formally validate/record **Secure Boot on metal** (retiring the long-stale Phase 59 item). Real silicon storing real user data is exactly when these matter. |

### Track C — Versioning reform (specified; executes in a follow-on PR)

Adopt a **single unified workspace version**. Add `[workspace.package] version = "0.98.0"` + `edition = "2024"` to the root `Cargo.toml`; convert every first-party member's `[package]` to `version.workspace = true` / `edition.workspace = true`; leave the vendored `sunset-local` on its own version. After the reform there is exactly **one** version line in the tree, bumped only by a deliberate release step — phase branches touch **zero** version lines, so they cannot conflict on versions. The phase number lives entirely in `docs/roadmap/`, a `phase-NN` git tag, and commit messages. `env!("CARGO_PKG_VERSION")` then makes the boot banner / `uname` report the unified OS release version (the one intended behavior change: `uname` release `0.96.0 → 0.98.0`).

### Track D — Rules / `AGENTS.md` slimming (specified; executes in a follow-on PR)

Cut `AGENTS.md` from ~83 KB to ~28–30 KB (~63 %, ~37 K → ~13 K tokens/session) with **zero operational info lost**: replace the 48 KB regression-gate table with a lean `Gate | Env var | one-line purpose` table and move every full description verbatim into a new `docs/appendix/regression-gates.md` (exactly what the file's own "keep it small" policy prescribes); collapse the run-on capability-inventory bullets to one line per capability *class* (the 9 KB "Package management" saga → one line pointing at `docs/roadmap/`); fix the version-bump policy (rewrite for Track C) and the `v0.97.0`-header-vs-`0.96.0`-`Cargo.toml` drift; trim the stale ASCII architecture diagram and the duplicated doc-template rules; and make `.github/copilot-instructions.md` a thin pointer to `AGENTS.md` to stop drift.

## Important Components and How They Work

### The audit verdict matrix (`docs/appendix/audit-status/`)

Each Phase 1→97 row gets one of three verdicts with a cited pointer: **Validated** (gate name + last PASS count / HW run / host test), **Claimed-unvalidated** (Complete with no current evidence — the ~50 pre-63 rows, low risk where implicitly exercised downstream, flagged honestly), **Regressed** (a gate that now fails — none expected, but the matrix is where one would surface). The matrix is the input to Track A's README edits and to the version/1.0 reconciliation.

### The forward dependency graph

The arc is **not** a feature wishlist; it is a dependency DAG. `99 (SMP) → 100 (GUI, USB mouse)`; `101 (ACPI) → 102 (touchpad)` and `101 → 103 (power)`; `104 (Wi-Fi) + 103 (power) → 105 (settings panel)`; `106 (installer)` gated on bare-metal NVMe root; `108 (AMD)` after the Dell line. ACPI-before-I2C-HID and SMP-before-multi-core-GUI are the two sequencing traps the charter exists to avoid.

### The bare-metal validation strategy (Track A → reused by every 99–110 phase)

QEMU models none of the new hardware, so the always-on-gate quality story does not transfer. Track A defines a repeatable protocol (USB-passthrough where possible, AMT Serial-over-LAN pre-network, `usb-logsink` boot.log + network sink post-network, photo/on-device-render assertion for "the screen shows X") and an evidence convention so HW phases land as "Validated-on-HW (run N, date)" with a real pointer — not a bare "Complete" that recreates the audit debt this phase retires.

## How This Builds on Earlier Phases

- **Extends Phase 58 + the `audit-status/` corpus** from its 57e/75 cutoffs to Phase 97, and institutionalizes the *evidence* standard (a gate or recorded run behind every Status) rather than reconciling once.
- **Continues the Phase 96 bare-metal line** — 96 proved networking on the Dell and landed the bring-up workflow (`run --usb-passthrough`, SOL capture, log sink) the forward arc reuses; the WC framebuffer it added to the *kernel console* is finished for the *compositor* in Phase 100.
- **Reuses the Phase 85a `.m3pkg` substrate + the Phase 86c HTTPS/TLS stack** for Phase 107 (networked packaging adds only fetch + index-parse + ed25519-verify; the solve/verify/extract/DB engine is unchanged).
- **Picks up the AMD-Vi (Phase 55a/67), mt792x (Phase 81), HDA (Phase 80), and KPTI/Spectre (Phase 84)** work that was coded/host-tested but never validated on real silicon, and schedules that validation against the two physical laptops.

## Implementation Outline

1. **Track A — audit + index repair.** Build the Phase 1→97 verdict matrix (cite a gate / HW run / host test per row); batch the README Status edits + fix the stale 54a/96/gantt entries; regenerate `codebase-map.md` and fix the two README indexes + `file-backed-mmap.md`; reconcile the 1.0/version posture; write the bare-metal validation strategy doc.
2. **Track B — re-charter.** Author template-conformant design + task docs for Phases 99–110 from the dependency graph above; add their README rows + a "Next Arc (99→110)" section + gantt nodes; map every still-open deferred item and open handoff onto a phase or the accepted-deferred backlog.
3. **Track C — versioning spec.** Write the exact migration (root `[workspace.package]` block + the `version.workspace = true` mass-conversion of all members except `sunset-local` + the `AGENTS.md` policy rewrite); flag it for a follow-on PR that runs `cargo xtask check`.
4. **Track D — rules spec.** Write the `AGENTS.md` slimming plan + the new `docs/appendix/regression-gates.md` target structure; flag it for a follow-on PR.

## Acceptance Criteria

- Every Phase 1→97 README row Status is backed by a cited pointer (gate name + last result, recorded HW run, or host test), or is explicitly tagged **Claimed-unvalidated** in the verdict matrix — no bare "Complete" without disposition. The stale Phase 54a row and the self-stale Phase 96 "stale `Planned`" framing are corrected.
- `docs/appendix/audit-status/` carries a per-phase **Validated / Claimed-unvalidated / Regressed** matrix extended to Phase 97, plus the 1.0/version-gate reconciliation.
- The index layer is repaired: `codebase-map.md` lists all ~116 crates + the GUI/audio/driver/toolchain trees, `docs/README.md` and `tasks/README.md` are current, the delivery-gantt P78–81 labels are corrected, and `file-backed-mmap.md` no longer contradicts Phase 95b.
- Phases 99–110 each exist as a template-conformant design doc (with Milestone Goal + Acceptance Criteria) **and** a companion task doc, with README rows + a sequenced "next arc" section. Every still-open deferred item and the 7 open-unscheduled handoffs are either assigned to a chartered phase or recorded in the accepted-deferred backlog with an acceptance gate.
- A **bare-metal validation strategy** doc exists and is referenced by the HW-only phases (101/102/103/104/108/109/110), with the "Validated-on-HW (run N, date)" status convention.
- The **versioning reform** (Track C) and **`AGENTS.md` slimming** (Track D) are specified precisely enough to execute mechanically in a follow-on PR (exact files, the `[workspace.package]` block, the gate-table relocation target).

## Companion Task List

- [Phase 98 Task List](./tasks/98-roadmap-audit-and-recharter-tasks.md)

## How Real OS Implementations Differ

- Production projects run this continuously — CI dashboards, release-readiness reviews, RFC/roadmap processes, and a board-support-package (BSP) matrix per supported machine — rather than as a discrete phase. A teaching OS benefits from one explicit "stop, verify, and re-plan" beat at the end of a long roadmap.
- Real distributions never encode the phase/sprint number in artifact versions; they use semver (libraries) or a date/release train (the OS image). The Track C reform adopts the latter.
- Mature OSes treat ACPI, power management, and a Wi-Fi supplicant as foundational, not as a late "polish" arc; m3OS reaches them late because it prioritized the microkernel boundary and toolchains first — the re-charter makes that ordering deliberate rather than accidental.

## Deferred Until Later

The chartered phases (99–110) **execute as their own PRs** — this phase charters them. The two reforms (Track C versioning, Track D rules) **execute as follow-on PRs** — this phase specifies them. Beyond the chartered arc, these items are **consciously accepted-deferred backlog** (recorded so they are scheduled-by-decision, not lost), each with a future home or acceptance gate noted:

- **On-device `cargo` + proc-macros** (the Phase 95 Step-5 stretch) — acceptance is the existing `cargo-smoke` / `M3OS_CARGO_REGRESSION` gate; charter as a small toolchain-completion phase when the GUI arc frees capacity.
- **Networking depth** — TCP congestion control / reassembly / keepalive-prober, DNS caching / DNSSEC, TLS revocation, raw sockets / multicast / `SCM_RIGHTS`, the modern-NIC scaling ladder + more vendors/offloads, generic CDC-ECM live + RNDIS/ASIX, live UAS, USB4 fabric / USB-C-PD / DbC.
- **Kernel concurrency & MM maturity beyond Phase 99** — CFS/EEVDF fair scheduling, PI futexes, full kernel preemption (deferred indefinitely since 57e), lockdep/KASAN/loom, NUMA, OOM killer, swap, huge pages, a unified page cache.
- **Storage/FS depth** — AHCI NCQ/TRIM/SMART, ext3/4 journaling/xattr/ACL, the two-ext2-front-end unification beyond Phase 88.
- **Toolchain finish beyond cargo** — crates.io, `build.rs`/cc-crates, self-hosting rustc/clang/Go/LLVM, Python networking/pip/asyncio.
- **Broader security** — the wider Spectre family (Retbleed/SRSO/MDS/BHI), measured-boot/TPM beyond Phase 110's Secure-Boot validation, MAC/SELinux-class policy.
- **Dev-path known issue** — the intermittent PS/2 mouse stick-at-top-left (post-1.0-downgraded; likely real-HW-moot since the laptop pointer is I2C-HID, but it degrades the QEMU dev path); fix sketch recorded in the Phase 77 handoff.
