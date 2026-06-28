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

## Per-phase verdict matrix (Phases 1 → 97)

Every phase (including lettered sub-phases) carries one verdict with a cited pointer. The verdict words are defined in [Verdict scheme](#verdict-scheme) above; the additional honest states (**Planned**, **Deferred**, **Partial**, **Superseded**) are used where a phase is not "Complete" or was consciously re-scoped, rather than forcing one of the three core verdicts.

**Method per row.** *Validated* cites a named current gate (in `AGENTS.md`/`xtask`), a host test (`cargo xtask check`), or a recorded HW run that **directly exercises the phase's headline deliverable**. *Claimed-unvalidated* (CU) means "Complete" with no dedicated inline gate; where the deliverable is implicitly exercised by a cited downstream gate the row is tagged **CU (load-bearing)** — low risk, but the row itself proves nothing. This makes per-phase explicit what the [time-stratified banding](#headline-finding-the-evidence-convention-is-time-stratified) states in aggregate.

| Phase | Theme (short) | README status | Verdict | Evidence / pointer |
|---|---|---|---|---|
| 1 | Boot Foundation | Complete | CU (load-bearing) | No dedicated gate; every `smoke-test` boot exercises it |
| 2 | Memory Basics | Complete | CU (load-bearing) | Heap/frame alloc exercised by all of `smoke-test`/`check` |
| 3 | Interrupts | Complete | CU (load-bearing) | IDT/timer/IRQ exercised by every boot |
| 4 | Tasking | Complete | CU (load-bearing) | Scheduler base; exercised by `regression` SMP scenarios |
| 5 | Userspace Entry | Complete | CU (load-bearing) | ring-3 transition exercised by every userspace gate |
| 6 | IPC Core | Complete | CU (load-bearing) | All servers ride it; `session-smoke`/`regression` |
| 7 | Core Servers | Complete | CU (load-bearing) | init/console/kbd exercised by `smoke-test` |
| 8 | Storage and VFS | Complete | CU (load-bearing) | VFS exercised by every FS gate |
| 9 | Framebuffer and Shell | Complete | CU | Superseded by the GUI stack; no dedicated gate |
| 10 | Secure Boot *(opt)* | Complete | CU (HW-only) | `cargo xtask image --sign`; bare-metal validation (→ Phase 110) |
| 11 | Process Model | Complete | CU (load-bearing) | ELF exec exercised by every userspace gate |
| 12 | POSIX Compat | Complete | CU (load-bearing) | musl userspace exercised by git/python/clang/node gates |
| 13 | Writable FS | Complete | CU (load-bearing) | ext2 write exercised by `regression` (`storage-roundtrip`) |
| 14 | Shell and Tools | Complete | CU | pipes/redirection exercised by `smoke-test` |
| 15 | Hardware Discovery | Complete | CU (load-bearing) | ACPI/PCI/APIC exercised by every device gate |
| 16 | Network | Complete | CU (load-bearing) | TCP/IP exercised by every networking gate |
| 17 | Memory Reclamation | Complete | CU (load-bearing) | CoW fork exercised by `fork-test`/`regression` |
| 18 | Directory and VFS | Complete | CU (load-bearing) | `getdents64`/cwd exercised by every shell gate |
| 19 | Signal Handlers | Complete | CU | Prior audit flagged design-vs-task drift; downstream signal gates (`go-runtime-smoke` SIGURG, `claude-smoke` SIGINT) exercise it |
| 20 | Userspace Init and Shell | Complete | CU (load-bearing) | PID 1 init exercised by every boot |
| 21 | Ion Shell | Complete | CU | exercised by `smoke-test` post-login shell |
| 22 | TTY and Terminal Control | Complete | CU (load-bearing) | termios contract validated downstream by `termios-smoke` (69a) |
| 22b | ANSI Escape Sequences | Complete | CU | VT100 parser exercised by `tui-smoke`/`tui-app-smoke` |
| 23 | Socket API | Complete | CU (load-bearing) | BSD sockets exercised by every net gate |
| 24 | Persistent Storage | Complete | CU | virtio-blk exercised by `smoke-test`; FAT32 superseded by ext2 |
| 25 | SMP | Complete | CU (load-bearing) | multi-core exercised by `regression`/`smp-smoke` |
| 26 | Text Editor | Complete | CU | no dedicated gate |
| 27 | User Accounts | Complete | Validated | `regression` (`security-floor`) + `smoke-test` (uid) |
| 28 | ext2 Filesystem | Complete | CU (load-bearing) | ext2 root exercised by every storage gate |
| 29 | PTY Subsystem | Complete | CU | PTY exercised by `tui-app-smoke` (tmux) |
| 30 | Telnet Server | Complete | CU | prior audit: validation track deferred to manual; opt-in build flag |
| 31 | Compiler Bootstrap (TCC) | Complete | Validated | `smoke-test` (the TCC compile step) |
| 32 | Build Tools (make/ar) | Complete | CU | prior audit: validation track deferred to manual |
| 33 | Kernel Memory | Complete | CU | slab groundwork; closed out + measured in Phase 60 |
| 34 | Real-Time Clock | Complete | CU | CLOCK_REALTIME exercised by build-date floor (86a) |
| 35 | True SMP | Complete | CU (load-bearing) | per-core dispatch; load-balancing closed in Phase 61 |
| 36 | Expanded Memory | Complete | CU (load-bearing) | demand paging/mprotect exercised by `wx-violation`/mmap gates |
| 37 | I/O Multiplexing | Complete | Validated | `epoll-smoke` (Phase 77 verification) |
| 38 | Filesystem Enhancements | Complete | Validated | `coreutils-smoke` inode/hardlink battery; Phase 88 `clang-stress` |
| 39 | Unix Domain Sockets | Complete | Validated | `regression` (`log-pipeline` via `/dev/log`); `sendmsg-test` |
| 40 | Threading | Complete | CU (load-bearing) | futex/TLS/clone exercised by `node-smoke`/`go-runtime-smoke`/`smp-smoke` |
| 41 | Expanded Coreutils | Complete | CU | superseded by uutils (Phase 94 `coreutils-smoke`) |
| 42 | Crypto Primitives | Complete | Validated | `cargo xtask check` (`crypto-lib` host tests); `crypto-test` |
| 42b | Async Executor | Complete | CU | prior audit flagged `async-rt` deadlock risk; no dedicated gate |
| 43 | SSH | Complete | Validated | `ssh-e1000-banner-check` |
| 43a | Crash Diagnostics | Complete | CU | enriched fault handlers; exercised by fault-path gates |
| 43b | Kernel Trace Ring | Complete | CU | `sys_ktrace`/`ktrace`; auto-dump on crash |
| 43c | Regression & Stress | Complete | Validated | the `cargo xtask regression`/`stress` harness it delivered runs on every sweep |
| 44 | Rust Cross-Compilation | Complete | CU (load-bearing) | exercised by Phase 94 `coreutils-smoke` (Rust musl cross) |
| 45 | Ports System | Complete | CU (load-bearing) | exercised by `pkg-smoke` + every port build |
| 46 | System Services | Complete | CU | prior audit: unchecked boxes; service list exercised by `smoke-test` |
| 47 | DOOM | Complete | Validated | `doom-concurrent-smoke` + `doom-audio-smoke` (render + audio proof) |
| 48 | Security Foundation | Complete | Validated | `regression` (`security-floor`) |
| 49 | Architectural Declaration | Complete | CU | docs/arch boundary declaration; no runtime gate |
| 50 | IPC Completion | Complete | Validated | `page-grant-test` (Phase 74 grants) |
| 51 | Service Model Maturity | Complete (folded into 46) | Superseded | folded into Phase 46; no separate task doc (by design) |
| 52 | First Service Extractions | Complete (umbrella) | CU | umbrella for 52a–52d |
| 52a | Kernel Reliability Fixes | **Complete** | CU | exercised by `regression` SMP/IPC scenarios |
| 52b | Kernel Structural Hardening | **Complete** | CU | AddressSpace/UserBuffers exercised by `check` + `regression` |
| 52c | Kernel Architecture Evolution | **Complete** | CU | VMA tree/growable tables exercised by mmap + IPC gates |
| 52d | Kernel Completion & Alignment | Complete | CU | audit-backed closure phase; no dedicated gate |
| 53a | Kernel Memory Modernization | Complete | CU | magazine slab; `cargo xtask check` host tests |
| 53 | Headless Hardening | Complete | Validated | defines the always-on `smoke-test`+`regression` gate bundle |
| 54 | Deep Serverization | Complete | Validated | `udp-smoke` (UDP policy slice) + `regression`; `vfs_server` path |
| 54a | Post-Serverization Kernel Hygiene | ~~Planned~~ → **reconciled** | Validated (via Phase 66) | CLOEXEC/NONBLOCK plumbing landed + closed by **Phase 66** (`security-hygiene-closeout`); README row corrected by Track A.2 |
| 55 | Hardware Substrate | Complete | Validated | `device-smoke --device nvme` (NVMe) + `multi-nic-smoke` (e1000) |
| 55a | IOMMU Substrate | Complete | Validated | `cargo xtask check` (IOMMU host logic) + `--iommu` device-smoke; completed in Phase 67 |
| 55b | Ring-3 Driver Host | Complete | Validated | `nvme-crash-smoke`/`e1000-crash-smoke`/`max-restart-smoke` |
| 55c | Ring-3 Driver Correctness Closure | **Complete** | Validated | `ssh-e1000-banner-check` (bound-notification mux); `--iommu` device-smoke |
| 56 | Display and Input Architecture | Complete | Validated | `compositor-stress`/`less-render-probe`/`session-smoke` |
| 57 | Audio and Local Session | Complete | Validated | `audio-smoke`/`session-smoke`; real PCM in Phase 63 |
| 57a | Scheduler Block/Wake Rewrite | **Complete** | Validated (≤`-smp 4`) | `smp-smoke` + 57a soak; **`-smp 8` consolidation owned by Phase 99** |
| 57b | Preemption Foundation | **Complete** | CU | no-op refactor (no behavior change); unblocks 57d/e |
| 57c | Kernel Busy-Wait Conversion | **Complete** | CU | 57c busy-wait audit + validation soak |
| 57d | Voluntary Preemption | **Complete** | CU | IRQ-return user-mode preempt; no dedicated CI gate |
| 57e | Full Kernel Preemption | **Deferred (2026-05-07)** | Deferred (honest) | post-mortem cited; correctly hedged |
| 58 | Documentation Reconciliation | **Complete** | CU (docs phase) | the audit-status corpus is its artifact |
| 59 | Validation Backlog | ~~Planned~~ → **superseded** | Superseded | absorbed by the per-phase gate convention (63+) + the Phase 83 gate bundle; residual Secure-Boot-on-metal → **Phase 110** (Track A.4) |
| 60 | Slab Migration Closeout | **Complete** | CU | `cargo xtask check` (slab host tests); Phase 33 closeout |
| 61 | SMP Load Balancing Closeout | **Complete** | Validated | `regression` (`maybe_load_balance` + TLB-shootdown) |
| 62 | Phase 57a Pi-Lock Closeout | Complete (pending soak) | CU | Bug #9 audit zero-LEAK; guard-leak regression pinned |
| 63 | Audio Stack Implementation | **Complete** | Validated | `audio-smoke` + `doom-audio-smoke` (non-silent WAV) |
| 63a | DOOM Audio Wiring | **Complete** | Validated | `doom-audio-smoke` (non-silent WAV + re-arm) |
| 64 | Session Manager Lifecycle | **Complete** | Validated | `session-smoke` + `crash_stub` lifecycle tests |
| 65 | fat_server Implementation | ~~Planned~~ → **superseded** | Superseded | FAT32 writes are an explicit **1.0 non-goal** (`fat_server` stays ENOSYS); ext2 is the supported FS (Track A.4) |
| 66 | Security & Hygiene Closeout | **Complete** | Validated | `regression` (`security-floor`); closes Phase 54a |
| 67 | IOMMU Substrate Completion | **Complete** | Validated | `cargo xtask check` (IOMMU host tests) + `--iommu` device-smoke |
| 68 | Display Server Closeout | **Complete** | Validated | `compositor-stress`/`less-render-probe`/`tiling-smoke` |
| 69 | Terminal Contract Foundations | **Complete** | Validated | `tui-smoke` |
| 69a | Termios Raw Mode | **Complete** | Validated | `termios-smoke` |
| 69b | UTF-8 + Glyph Expansion | Complete | Validated | `tui-app-smoke` (UTF-8/box-drawing render) |
| 69c | TTF Font Loader | Complete | Validated | `tui-app-smoke`/`less-render-probe` |
| 69d | ncurses + TUI Apps | Complete | Validated | `tui-app-smoke` (less/htop/tmux) |
| 70 | DOOM In-GUI Surface | **Complete** | Validated | `doom-concurrent-smoke` |
| 71 | GUI Login Manager | **Complete** | Validated | `session-smoke` + `compositor-stress` (greeter render `Experimental`) |
| 72 | Compositor: Tiling + Workspaces | **Complete** | Validated | `tiling-smoke` |
| 73 | Compositor: Polish | **Complete** | Validated | `compositor-stress` (bar/launcher/notifyd/animations) |
| 74 | IPC Capability Grants + Bulk | **Complete** | Validated | `page-grant-test` |
| 75 | W^X Enforcement | **Complete** | Validated | `wx-violation` |
| 76 | Dynamic Linker (scaffolding) | **Complete** | Validated | `dynlink_smoke` |
| 76b | DT_NEEDED + Relocations | **Complete** | Validated | `dynlink-hello-smoke`/`dynlink-missing-smoke`/`dynlink-cycle-smoke` |
| 76c | dlopen | **Complete** | Validated | `dlopen-test-smoke` |
| 76d | PLT Lazy + GNU Hash + Versioning | **Complete** | Validated | `dynlink-hello-gnu-smoke`/`dynlink-hello-versioned-smoke` |
| 77 | Pre-1.0 Correctness + Cheap Security | Complete | Validated | `htop-render-probe` + `epoll-smoke` + `regression` |
| 78 | USB Host Foundation (umbrella) | Complete | Validated | `usb-smoke` + `xhci-bringup-smoke` + `xhci-enum-smoke` |
| 78a | xHCI Host Bring-Up | Complete | Validated | `xhci-bringup-smoke` |
| 78b | Enumeration + Hub | Complete | Validated | `xhci-enum-smoke` + `usb-hub-smoke` |
| 78c | HID + Release | Complete | Validated | `usb-smoke` (typed glyphs, screenshot) |
| 79 | Modern Intel/Realtek NIC | Complete | Validated | `multi-nic-smoke` (e1000e/igb arms); igc/r8169/RTL8125 bare-metal |
| 80 | Intel HDA Audio | Complete | Validated | `hda-smoke` (non-silent WAV) |
| 81 | Wi-Fi Reference (mt792x) | Driver-side complete; radio HW-only | Validated (host) + HW-only (radio) | `cargo xtask check` (`wifi-core`/`mt792x` host) + `wifi-smoke` skip-with-reason; radio via VFIO — honest hedge |
| 82 | AHCI / SATA Storage | Complete ✅ | Validated | `ahci-smoke` + `ahci-root-smoke` + `ahci-rw-smoke` + `ahci-persist-smoke` |
| 83 | Release 1.0 Gate | Complete ✅ | Validated | the Phase 83 gate bundle (`smoke-test`+`regression`+matrix); 59/65 deps dispositioned in Track A.4 |
| 84 | Spectre / KPTI / Retpoline | Spectre complete; KPTI deferred | Validated (Spectre) + deferred (KPTI) | `mitigations-status-smoke` + retpoline objdump gate; KPTI activation → Phase 110 — honest hedge |
| 85 | Cross-Compiled Toolchains (umbrella) | Complete | Validated | `pkg-smoke` (umbrella) |
| 85a | Package & Build-Cache Infra | Complete | Validated | `pkg-smoke` + `pkgcache-hit-check` |
| 85b | git (local) | Complete | Validated | `git-local-smoke` |
| 85c | Python | Complete | Validated | `python-smoke` |
| 85d | Clang / LLVM / LLD | Complete | Validated | `clang-smoke` (+ `clang-stress` stat-identity guard) |
| 86 | Networking and GitHub (umbrella) | ✅ Complete | Validated | `git-https-smoke` 36/36 + the 86x arms |
| 86a | Outbound Foundation | ✅ Complete | Validated | `tls-smoke`/`dns-smoke` PASS-not-SKIP (CSPRNG/resolver/CA) |
| 86b | SSH + git over SSH | 🟡 Implemented | Validated (core) + cred-gated | `git-ssh-smoke` + `connect-smoke`; live clone cred-gated — honest hedge |
| 86c | HTTPS/TLS + git smart-HTTP | ✅ Complete | Validated | `git-https-smoke` 36/36 (live cert-validate + reject arms) |
| 86d | Go-Runtime Gate | ✅ Complete | Validated | `go-runtime-smoke` 18/18 |
| 86e | GitHub CLI (`gh`) | 🟡 Implemented | Validated (core) + cred-gated | `gh-smoke` 16/16; auth arms `GH_TOKEN`-gated — honest hedge |
| 86f | Userspace SIMD / AES-NI | ✅ Complete | Validated | `userspace-simd-smoke` |
| 87 | VFS Bulk-I/O Throughput | 🟢 Landed | Validated | `vfs-bulkio-smoke` (~11× I/O reduction) |
| 88 | VFS `stat` Conformance + ext2 Consolidation | Complete | Validated | `clang-stress` (stat-identity guard) + `coreutils-smoke` inode battery |
| 89 | Node.js | 🟢 Landed | Validated | `node-smoke` (egress always-on) + `smp-smoke` |
| 90a | Memory Protection Keys + JIT V8 | ✅ Complete | Validated | `pku-smoke` + `node-jit-smoke` 20/20 (KVM/PKU) |
| 90b | Claude Code | 🟢 Landed | Validated | `claude-smoke` 27/27 (offline core always-on; live arms cred-gated) |
| 91 | IPv6 / DHCPv6 | 🟢 Landed | Validated | `ipv6-smoke` (always-on; SLAAC/DHCPv6 live behind `M3OS_IPV6_LIVE`) |
| 92 | USB Class Expansion (incl. 92a–92e) | Complete | Validated | `usb-smoke`/`usb-report-smoke`/`usb-storage-smoke`/`usb-hub-smoke`/`usb-mount-smoke`/`usb-multi-controller-smoke`; `usb-audio-smoke`; `usb-eth-smoke` skip-with-reason (no QEMU CDC-ECM) |
| 93 | Dynamic C Runtime (`libc.so`) | Complete | Validated | `dynamic-hello-smoke` (always-on) + `dynamic-python-smoke` (opt-in) |
| 94 | Rust-Cargo Ports & uutils | Complete | Validated | `coreutils-smoke` |
| 95 | Native Rust Toolchain (umbrella) | Host toolchain + install landed | Validated | `rustc-smoke` (host build + on-device `pkg install rust`); codegen in 95b |
| 95b | On-Device `rustc` Code Generation | ✅ Complete (milestone) | Validated | `rustc-smoke` `RUSTC_OK` under `M3OS_KVM=1` (multithreaded rust-lld) |
| 95c | VFS / Block-I/O Performance | **Partial** | Partial (honest) | `vfs-throughput-smoke` (A/C/E landed; B/D planned; F rejected) |
| 96 | Bare-Metal Networking (RTL8156 `ure`) | ✅ Complete | Validated-on-HW | `ure-smoke` + recorded bare-metal DHCP lease (`ip=192.168.1.221`) |
| 97 | `dlopen-test-smoke` → `DT_RELR` loader fix | ✅ Complete | Validated | `ldso_core` host tests + 232-iteration soak + `smoke-test` step 26 |

**Verdict tally (97 phases incl. lettered):** Validated ≈ 60 (all of 63→97 plus the gate-backed early phases 27/31/37/38/39/42/43/47/48/50/53/54/55/55a–c/56/57/61); Claimed-unvalidated ≈ 32 (the evidence-free early phases, most tagged *load-bearing*); special states 5 (57e Deferred; 95c Partial; 59 & 65 Superseded; 51 Superseded). **No Regressed gate** (a once-green gate now failing) — the lone live red is the unscheduled step-25 demand-fault flake, now owned by Phase 99.

## A.4 — 1.0 / versioning-posture reconciliation

The Phase 83 "Release 1.0 Gate" (`Complete ✅`, kernel `0.83.0`) still named two **`Planned`** dependencies — Phase 59 (Validation Backlog) and Phase 65 (fat_server) — a release-gate-atop-incomplete-deps inconsistency. Disposition:

| Dep | Old status | Disposition | Where recorded |
|---|---|---|---|
| **Phase 59 — Validation Backlog** | Planned | **Superseded.** Its "run every deferred manual QEMU test" purpose was institutionalized by the per-phase gate convention (Phase 63 onward) + the Phase 83 always-on gate bundle (`smoke-test`/`regression` + the env-gated matrix). The one residual item — **Secure-Boot-on-metal validation** — is rescheduled to **Phase 110**. | README row 59 → `Superseded`; Phase 110 doc carries the Secure-Boot item |
| **Phase 65 — fat_server** | Planned | **Superseded.** FAT32 *writes* are an explicit **1.0 non-goal** in [`1.0-release-gate.md`](../../release/1.0-release-gate.md) (`fat_server` stays a permanent ENOSYS stub); **ext2 is the supported on-disk filesystem**. The gate no longer depends on FAT32 write support. | README row 65 → `Superseded`; 1.0 gate Non-Goals |

With both deps re-dispositioned, the Phase 83 gate no longer sits atop `Planned` work; the README dependency-map edges `P59 → P83` / `P65 → P83` now point at `Superseded` nodes, and the gate's `Complete ✅` is consistent.

**Version-cut policy for the GUI-workstation arc** (pairs with Track C, [`versioning-reform.md`](../versioning-reform.md)): after the single-`[workspace.package]`-version reform, the unified version is an **OS release version**, not a per-phase number. Phase numbers live in `docs/roadmap/` + a `phase-NN` git tag + the commit message; **Cargo versions are not bumped per phase**. The version is bumped **only at a deliberate release step**. A SemVer **`1.0.0`** is cut only when a frozen public syscall/userspace ABI exists *and* the [1.0 gate](../../release/1.0-release-gate.md) support matrix is green on the reference machines — until then the OS stays `0.x` per the gate's [Versioning Posture](../../release/1.0-release-gate.md#versioning-posture). This policy is cross-referenced from the 1.0 gate doc.

## Bottom line

The roadmap is fundamentally honest — the audit found drift, not deception (the `52d`/`54a`/`58` remediation phases exist precisely because the project revisits over-claims). The corrections are: (1) cite the gates that already exist for the Validated late phases; (2) tag the pre-63 phases Claimed-unvalidated rather than bare-Complete; (3) fix the four stale rows (54a, the 98-charter's 96 example, the 83 1.0-gate, the marker legend); (4) **repair the index layer**, which is where the real rot is and where the next arc will be navigated from. None of the audit found a **Regressed** gate (a once-green gate now failing) — the one live red is the unscheduled step-25 demand-fault flake, now owned by Phase 99.
