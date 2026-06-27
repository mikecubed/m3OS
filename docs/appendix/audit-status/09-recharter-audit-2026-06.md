# Phase 98 Re-Charter Audit — Evidence Reconciliation (Phases 1 → 97)

**Audit date:** 2026-06-27
**Scope:** All roadmap phases 1 → 97 (including lettered sub-phases). Extends the original completion audit (`README.md` + `01`–`08`, cutoff Phase 57e, 2026-05-08) and the pre-1.0 audit (`74a-pre-1.0-audit.md`, cutoff Phase 75, 2026-05-26) up to the current head (Phase 97, kernel `v0.97.0`).
**Method:** Evidence reconciliation against three falsifiable sources — a named passing **gate** (in `AGENTS.md` / `xtask`), a recorded **hardware run**, or a **host test** — cross-checked against the `AGENTS.md` capability inventory and the live tree. This is the Phase 98 Track A deliverable; it feeds the README Status corrections (Track A.2) and the 1.0/version reconciliation (Track A.4).

## Verdict scheme

| Verdict | Meaning |
|---|---|
| **Validated** | Status backed by a cited gate (with last PASS/HW result), a recorded HW run, or a host test. |
| **Claimed-unvalidated** | Marked "Complete" with no current inline evidence. Not an allegation of breakage — most are implicitly exercised by everything built on top — but the table itself proves nothing. |
| **Regressed / stale** | The Status field materially contradicts current evidence (a closure claim elsewhere, a live row already corrected, or a gate-atop-incomplete-deps inconsistency). |

## Headline finding: the evidence convention is time-stratified

The single most important pattern: **evidence-citation discipline flips at ~Phase 63.**

- **Phases ~1–62** — marked "Complete" with **no inline falsifiable evidence**, only a terse Primary Outcome. These predate the gate-citation convention. ~50 rows. **Verdict: Claimed-unvalidated**, low risk where load-bearing (P12 musl userspace, P16 TCP/IP, P25/35 SMP, P28 ext2 are exercised by dozens of later gates), but **the table cites nothing**. A few carry strong claims worth a real pointer: P31 (TCC compiler bootstrap), P43c (the regression/stress/CI phase — ironically cites no gate), P47 (DOOM — no render proof inline).
- **Phases ~63 → 97** — nearly every "Complete/Landed" row names a concrete gate and often a PASS count or HW run. **Verdict: Validated.** Representative citations: `audio-smoke`/`doom-audio-smoke` (63), `tui-app-smoke`/`htop-render-probe` (69), `tiling-smoke` (72), `wx-violation` (75), USB `xhci-*`/`usb-*-smoke` (78/92), `multi-nic-smoke` (79), `hda-smoke` (80), `ahci-*-smoke` (82), `mitigations-status-smoke` (84), `git-https-smoke` 36/36 (86c), `go-runtime-smoke` 18/18 (86d), `userspace-simd-smoke` (86f), `vfs-bulkio-smoke`/`clang-stress` (87/88), `node-smoke`+`smp-smoke` (89), `pku-smoke`+`node-jit-smoke` 20/20 (90a), `claude-smoke` 27/27 (90b), `ipv6-smoke` (91), `dynamic-hello-smoke` (93), `coreutils-smoke` (94), `rustc-smoke`/`RUSTC_OK` (95b), `vfs-throughput-smoke` (95c), `ure-smoke` + bare-metal DHCP lease (96), `ldso_core` host tests + 232-iteration soak (97).

Several Validated phases simply **fail to cite the gate that exists** for them — they read evidence-free though evidence exists: P80 (`hda-smoke`), P82 (`ahci-smoke`), P86f (`userspace-simd-smoke`), P88 (`clang-stress` as the stat-identity regression guard), P72 (`tiling-smoke`), P93 (`dynamic-hello-smoke`). Track A.2 adds the citations.

## Regressed / stale rows (Track A.2 corrections)

| Row | Problem | Disposition |
|---|---|---|
| **Phase 54a** | README Status reads `Planned`, but Phase 66's row claims to *close* "+ Phase 54a" (the CLOEXEC/NONBLOCK plumbing) and Phase 66 is Complete. | Reconcile 54a to its true state (the plumbing appears delivered) with a cited pointer. |
| **Phase 96 (in the 98 charter)** | The Phase 98 stub's motivating example — "Phase 96's stale `Planned` while HW-validated" — is itself stale: the live Phase 96 row already reads `✅ Complete`. | Re-frame the charter's motivating example (done in the rewritten 98 design doc). |
| **Phase 83 "Release 1.0 Gate"** | Marked `Complete ✅`, kernel stayed `<1.0`, and the gate names still-`Planned` deps **Phase 59** (Validation Backlog) and **Phase 65** (fat_server). A release-gate-atop-incomplete-deps inconsistency. | Track A.4: close/supersede 59/65 or downgrade the gate; set an explicit version-cut policy (pairs with Track C). |
| **Status markers** | The table mixes `Complete`, `**Complete**`, `Complete ✅`, `✅ Complete`, `🟢 Landed`, `🟡 Implemented` with no legend. | Track A.2: add a legend. |
| **Honest hedges (no action)** | P57e `Deferred` (post-mortem cited — accurate); P86b/P86e `Implemented` (live arms cred-gated/SKIP — accurate); P81 Wi-Fi (driver-complete, radio HW-only — accurate); P95c `Partial` (accurate). | Keep — these are correctly hedged. |

## Index-layer rot (Track A.3) — the worst decay is NOT in phase docs

Phase design↔task pairing is essentially complete (every Phase 1–97 has both; the "missing" 51/78/85/86/98 task docs are all explained — 51 merged into 46, 78/85/86 are umbrellas, 98 is this phase). The decay is in the **indexes that claim completeness**:

- **`docs/appendix/codebase-map.md`** — the designated index, the single largest gap. Frozen at ~Phase 45–55: lists ~40 of ~116 workspace members; omits the entire GUI stack (`display_server`/`term`/`greeter`/compositor clients), the audio servers (`audio_server`/`hda`/`ac97`), the `userspace/drivers/*` tree, `pkg`/`m3ctl`/`wifi-core`, every kernel module added since (`epoll.rs`/`eventfd.rs`/`flock.rs`/`mitigations.rs`/`timerfd.rs`/`trace.rs`/`iommu/`), and every toolchain port (ncurses/git/python/clang/go/node/rust/musl).
- **`docs/README.md`** — Documentation Index omits ~34 existing top-level learning docs (the 43b–46, 55a, 58, 60–82 band).
- **`docs/roadmap/tasks/README.md`** — stale through Phase 56: a broken link to the removed P51 task doc, P13 marked "not yet created" though it exists, and a "57+ deferred" claim contradicted by 40+ existing task docs.
- **`docs/roadmap/README.md` gantt** — the bottom delivery gantt still labels P78–81 with their pre-renumbering themes.
- **`docs/appendix/file-backed-mmap.md`** — still says demand-paged mmap is unimplemented, though Phase 95b landed `MAP_LAZY_FILE`.

## Open-unscheduled follow-ups (Track B.2 scheduling)

Of 28 handoffs + 6 appendix analysis docs, the **vast majority are RESOLVED** (closed in Phases 55c/63/64/69d/73/77/92/95/96/97 or specific PRs). **Seven carry genuinely open, unscheduled work** — all scheduled by the re-charter:

| # | Open item | Scheduled into |
|---|---|---|
| 1 | Lost-wakeup bug class → consolidate + validate (at `-smp 8`) the single-state-word block/wake model that landed in Phase 57a (scheduler-design-comparison); the per-site patches of 89/90b/95 are uneven and unvalidated above `-smp 4` | **Phase 99** |
| 2 | Track-D kstack-overflow origin audit + `SCHEDULER`/`PROCESS_TABLE`-held-across-faults audit | **Phase 99** |
| 3 | 4 GiB SMP panic-path AP-quiesce diagnosability + residual 4 GiB OOM/race | **Phase 99** |
| 4 | The live ~11–15 % step-25 demand-fault NULL-deref CI flake (distinct from the *resolved* Phase 97 dlopen `DT_RELR` issue the original charter mis-cited) | **Phase 99** |
| 5 | `claude` `copyfile → EFAULT` kernel bug | **Phase 99** (kernel-correctness track) |
| 6 | 55c `net::remote` RX-path unit test using the wrong header encoder (`kernel/src/net/remote.rs:941`, still Open) | **Phase 99** |
| 7 | USB-kbd text mode (`stdin_feeder` to drain `usb-hid` `KBD_EVENT_PULL`) + the `usb-hid`/`usbhub` CPU-hog busy-poll | **Phase 100** (input polish) |

Items 1–6 are all in the recurring SMP/scheduler lost-wakeup + fault-handling family, which is why Phase 99 (SMP & Scheduler Robustness Hardening) is the foundation of the next arc. Two more handoffs (ext2 DAC write-back; virtio-input migration) are Open-but-deliberately-optional, not bugs.

## Deferred-item disposition (Track B.2)

~55 distinct still-open deferral themes across the 97 phase docs. The large majority of early-phase (1–50) generic-OS deferrals were **already delivered** by later phases (AF_UNIX→39, AF_INET6→91, TLS→86c, AES-NI→86f, dynamic linking→76, `EPOLLET`/`eventfd2`→86d, hardlinks/`st_ino`→88, atomic `pwrite64`→88, chmod/chown→89, `ctypes`/`dlopen`→93). The remaining themes map onto the chartered arc (GUI/pointer→100/102, power/ACPI→101/103, Wi-Fi→104, apps→105, installer→106, packaging→107, AMD→108, audio→109, security→110) or the Phase 98 accepted-deferred backlog (on-device cargo, networking depth, ext4 journaling, NUMA/swap, crates.io, broader Spectre family, USB4 fabric, the PS/2-mouse dev-path bug).

## Bottom line

The roadmap is fundamentally honest — the audit found drift, not deception (the `52d`/`54a`/`58` remediation phases exist precisely because the project revisits over-claims). The corrections are: (1) cite the gates that already exist for the Validated late phases; (2) tag the pre-63 phases Claimed-unvalidated rather than bare-Complete; (3) fix the four stale rows (54a, the 98-charter's 96 example, the 83 1.0-gate, the marker legend); (4) **repair the index layer**, which is where the real rot is and where the next arc will be navigated from. None of the audit found a **Regressed** gate (a once-green gate now failing) — the one live red is the unscheduled step-25 demand-fault flake, now owned by Phase 99.
