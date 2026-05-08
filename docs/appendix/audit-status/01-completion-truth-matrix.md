# 01 — Completion Truth Matrix

For each phase, this document records **declared status** (from the README and the phase's design-doc header) vs. **actual evidence** (task-doc checkboxes, validation tracks, in-doc admissions, supporting appendix docs). Severity tags reflect the gap between claim and evidence — not the importance of the underlying feature.

Severity legend:

- ✅ **Clean** — declared and actual agree; deferrals are honest and assigned to a later phase
- 🟡 **Minor** — small gaps documented in-line; phase claim is largely accurate
- 🟠 **Medium** — meaningful acceptance items unverified, but the phase delivers most of its promise
- 🔴 **High** — substantial portions of the phase are unverified, deferred, or admitted not done in-doc
- 🛑 **Critical** — declared status materially contradicts evidence in the same doc set

Every cell is sourced from `findings/01-*.md` through `findings/06-*.md`. If a cell looks wrong, follow the citation in the right-hand column.

---

## Phases 01 – 16 (Foundation through Network)

| # | Phase | Declared | Evidence | Severity | Notes |
|---|---|---|---|---|---|
| 01 | Boot Foundation | Complete | All boxes checked | ✅ | — |
| 02 | Memory Basics | Complete | All boxes checked | ✅ | Frame reclaim deferred to 17 (transparent) |
| 03 | Interrupts | Complete | All boxes checked | ✅ | APIC deferred to 15 |
| 04 | Tasking | Complete | All boxes checked | ✅ | SMP deferred to 25 |
| 05 | Userspace Entry | Complete | All boxes checked | ✅ | — |
| 06 | IPC Core | Complete | All boxes checked | ✅ | Capability grants/timeouts deferred — see code finding below |
| 07 | Core Servers | Complete | Design doc's own status table marks `kbd_server` keyboard event flow as **Partial** ("scancode reading and client forwarding are Phase 8+") | 🟡 | Status field overstates; gap is transparently described |
| 08 | Storage and VFS | Complete | All boxes checked | ✅ | Write deferred to 13 |
| 09 | Framebuffer and Shell | Complete | All boxes checked | ✅ | — |
| 10 | Secure Boot | Complete | C.3 (real-hardware boot test) and D.3 (README update) explicitly `[ ]` deferred; "kernel boots on real hardware with Secure Boot enabled" acceptance criterion **not met** | 🟠 | Status overstates |
| 11 | Process Model | Complete | All boxes checked; Phase 12 Track A absorbed deferred items (transparent) | ✅ | — |
| 12 | POSIX Compat | Complete | All boxes checked; `getcwd`/`chdir` are stubs; `ioctl` is TIOCGWINSZ-only stub; only `MAP_ANONYMOUS` mmap | ✅ | Stubs documented |
| 13 | Writable FS | Complete | **No task doc exists** (README explicitly says "not yet created"); five acceptance criteria cannot be cross-checked | 🔴 | Doc gap |
| 14 | Shell and Tools | Complete | All boxes checked; pipe blocking is busy-wait yield-loop; `nanosleep` is yield-loop tick count; shell still kernel task; per-process cwd is global | 🟡 | Shortcuts transparent |
| 15 | Hardware Discovery | Complete | All boxes checked; legacy CF8/CFC PCI; no MSI/MSI-X; no MCFG | ✅ | — |
| 16 | Network | Complete | **Task doc has no checkboxes and no Status header** (planning-table format with 73 task IDs); TCP no retransmit, UDP checksum may be zero, single connection | 🔴 | Doc-format gap; no per-task evidence of completion |

## Phases 17 – 32 (Usability and Productivity)

| # | Phase | Declared | Evidence | Severity | Notes |
|---|---|---|---|---|---|
| 17 | Memory Reclamation | Complete | Table-format task doc; stress test deferred to "Phase 18+" | 🟡 | "Works under load" unverified |
| 18 | Directory and VFS | Complete | Table-format; all tracks Done | ✅ | — |
| 19 | Signal Handlers | **Complete** | **Task doc says all 6 tracks Not started** | 🛑 | Most severe single mismatch in the audit. Phases 21, 29, 30 list signal handling as a satisfied dependency. |
| 20 | Userspace Init and Shell | Complete | Table-format; all tracks Done | ✅ | — |
| 21 | Ion Shell | Complete | `phase-21-handoff.md` records 7 acceptance tasks (T028, T030–T034, T038–T044) deferred to Phase 22; ion is fallback, sh0 remained primary (milestone goal inverted) | 🟠 | Milestone inversion documented in handoff |
| 22 | TTY and Terminal Control | Complete | Phase 21 carry-forward resolved (ion `-c`, interactive mode). History persistence and tab completion **double-deferred without owner phase** | 🟡 | — |
| 22b | ANSI Escape Sequences | Complete | **No design doc exists**; entire Track F validation (7 items) deferred ("manual QEMU visual test"); P22b-T046 sh0 regression check deferred | 🔴 | Template violation + no acceptance baseline |
| 23 | Socket API | Complete | Table-format; all Done | ✅ | — |
| 24 | Persistent Storage | Complete | T034 (rename), T042 (mount builtin), T043 (reboot persistence — primary acceptance criterion), T044, T047, T048 all explicitly **deferred** | 🟠 | Milestone goal "files survive reboot" never validated even once |
| 25 | SMP | Complete | Track C "Done (BSP-only dispatch; per-core syscall deferred)"; Track E "munmap hook deferred" — TLB shootdown not wired; Track F "BSP-only mitigation"; 2 of 3 acceptance criteria unverified | 🔴 | TLB shootdown missing on munmap is a correctness hazard |
| 26 | Text Editor | Complete | All boxes checked including G.1–G.15 runtime QEMU validation | ✅ | Cleanest phase in this band |
| 27 | User Accounts | Complete | All boxes checked; entropy is TSC-seeded PRNG (not crypto-secure) — caveat documented | 🟡 | Entropy weakness inherited by Phase 43 |
| 28 | ext2 Filesystem | Complete | Track I acceptance hedged: `.m3os_permissions` FAT32 overlay "removed (or kept as fallback)" — workaround retained; triple-indirect blocks cap files at ~64 MB | 🟡 | Cleanup task accepted incomplete |
| 29 | PTY Subsystem | Complete | Track I (validation) tasks listed without per-item Status column — cannot confirm which acceptance tests ran | 🟡 | Doc format issue |
| 30 | Telnet Server | Complete | **Entire Track F validation (16 items) unchecked `[ ]`** with "manual QEMU test" annotations; G.3 (README update) "deferred until manual tests pass" | 🛑 | No recorded evidence telnetd was ever connected to |
| 31 | Compiler Bootstrap | Complete | **Entire Track D, E, F (~15 items) deferred**: TCC `--version`, `tcc -run hello.c`, multi-file compile, **all of Track E (TCC self-hosting)**, F.1 `cargo xtask check` passage all deferred | 🛑 | Central milestone "TCC compiles inside the OS" entirely unverified |
| 32 | Build Tools | Complete | **~20 items deferred** across B/C/D/E/F: `make --version`, simple Makefile, **POSIX shell features (for/if/$?/$())** in sh0 — substantial sh0 additions never tested | 🛑 | Same pattern as 31, broader scope |

## Phases 33 – 47 (Kernel Infrastructure through DOOM)

| # | Phase | Declared | Evidence | Severity | Notes |
|---|---|---|---|---|---|
| 33 | Kernel Memory | Complete | C.4 unchecked: **slab migration — the headline deliverable — never done.** Design doc admits: "most kernel allocations still flow through the global linked-list heap." A.4 (OOM stress test), D.4 (mmap/munmap loop), E.3 (heap coalescing test), G.2 (memory stress test) all unchecked | 🔴 | Phase scaffolding shipped without the migration |
| 34 | Real-Time Clock | Complete | E.2 (QEMU RTC accuracy test) deferred; verified by boot log only | 🟡 | — |
| 35 | True SMP Multitasking | Complete | E.1 unchecked (per-run-queue length counter); **E.2 unchecked: `maybe_load_balance()` hook commented out — SMP load balancing is inoperative**; G.2/G.3 unchecked (pipe/IPC wait-queue attachment); H.3 unchecked (child CPU times stubbed zero); design doc admits global `SCHEDULER` lock acquired on every dispatch — negates per-CPU queue benefit. **Design doc missing `Status:` and `Source Ref:` header fields** | 🛑 | The behaviour that distinguishes 35 from 25 is not running |
| 36 | Expanded Memory | Complete | All checked | ✅ | — |
| 37 | I/O Multiplexing | Complete | All checked; **timeouts have ~10ms granularity (no proper timer subsystem)** — affects all `poll`/`select`/`epoll` timeouts; no remediation cross-reference | 🟠 | Documented accuracy gap |
| 38 | Filesystem Enhancements | Complete | All checked | ✅ | — |
| 39 | Unix Domain Sockets | Complete | J.1 (3 acceptance items for AF_UNIX integration test) all unchecked. **Named sockets create regular file markers, not socket inodes — `stat()` returns wrong `st_mode` type bits** | 🟠 | Correctness gap for any program using `S_IFSOCK` test |
| 40 | Threading | Complete | All checked | ✅ | — |
| 41 | Expanded Coreutils | Complete | All checked; `bc` deferred to ports system (intentional) | ✅ | — |
| 42 | Crypto Primitives | Complete | All checked; **CSPRNG explicitly not cryptographically secure**; SSH (43) depends on it | 🟠 | No remediation cross-reference for CSPRNG hardening |
| 42b | Async Executor | Complete | **No design doc exists.** Task doc declares `Status: Complete` with **zero checked task checkboxes across all 7 tracks** | 🛑 | Status untrustworthy |
| 43 | SSH Server | Complete | E.5 (SIGWINCH) unchecked; **G.1 / G.2 / G.3 (all three end-to-end SSH tests — password, pubkey, traffic inspection) unchecked, labelled "Manual"**; pubkey format hex-Ed25519, **incompatible with standard `authorized_keys`** | 🔴 | No machine-verifiable record of SSH end-to-end correctness |
| 43a | Crash Diagnostics | Complete | All checked; clean phase | ✅ | — |
| 43b | Kernel Trace Ring | Complete | **Zero checked checkboxes across all 9 tracks** | 🛑 | Same pattern as 42b |
| 43c | Regression and Stress CI | Complete | **Zero checked checkboxes across all 11 tracks** | 🛑 | Same pattern; widest unchecked scope |
| 44 | Rust Cross-Compilation | Complete | All checked; `x86_64-m3os.json` is naming-only; musl crates not workspace members | ✅ | — |
| 45 | Ports System | Complete | All checked; `/var/run → /run` symlink deferred | ✅ | — |
| 46 | System Services | Complete | **Zero checked checkboxes across all 8 tracks (22 tasks)**: service manager, syslogd, crond, sys_reboot, shutdown/reboot all unverified in the task doc despite shipping per AGENTS.md | 🛑 | Largest unchecked surface in 33–47 |
| 47 | DOOM | Complete | **Zero checked checkboxes across all 6 tracks**: all three new kernel framebuffer/input syscalls (0x1002–0x1004), the entire doomgeneric platform layer, and xtask integration unverified | 🛑 | Phase 56 builds on this baseline |

## Phases 48 – 54a (Convergence and Serverization)

| # | Phase | Declared | Evidence | Severity | Notes |
|---|---|---|---|---|---|
| 48 | Security Foundation | Complete | All 9 tracks done. Pre-seeded image still ships **single-iteration** SHA-256 root password (only post-`passwd` users get `$sha256i$10000$`). B.2.2 large-SSH-output stress unverified | 🟡 | Two-format hash situation accepted by Phase 53 |
| 49 | Architectural Declaration | Complete | Keyboard matrix gap identified and resolved within the phase | ✅ | — |
| 50 | IPC Completion | Complete | Three in-kernel server loops (`console_server`, `fat_server`, `vfs_server`) still use raw-pointer kernel-task patterns at close — documented as planned migrations to 52/54 | ✅ | Honest residuals |
| 51 | Service Model Maturity | **In Progress** | **No task doc exists**; no per-criterion gate evidence. Phases 53 and 54 (Complete) implicitly assume 51's claims | 🛑 | Foundational phase silently bypassed by later closures |
| 52 | First Service Extractions | **In Progress** | Sub-phases 52a/b/c/d did the structural work; parent phase never formally closed | 🟠 | Tracking gap |
| 52a | Kernel Reliability Fixes | Complete | Fixed 2 of 4 listed bugs (other 2 already fixed pre-52a). `restore_caller_context` stop-gap **superseded by 52b before 52a's docs were finalised** — handoff documented in 52d. B.2.2 SSH output stress unverified | 🟡 | Aftermath transparent |
| 52b | Kernel Structural Hardening | Complete (with "partial" tagline) | `UserReturnState` shipped half-migrated: state still saved at block points (not syscall entry); `kernel_stack_top`/`fs_base` split between `Task`, `Process`, per-core scratch. Closed by 52d. **Task-doc acceptance items use unchecked `[ ]` boxes throughout** — only track-header asserts completion | 🟠 | Half-migration documented |
| 52c | Kernel Architecture Evolution | Complete (with "deferred closure" tagline) | Two acceptance criteria explicitly **annotated post-phase as overclaimed**: scheduler dispatch still acquires global lock; `stdin_feeder` still duplicated line discipline. 52d closed the keyboard duplication and re-deferred the scheduler hot path with rationale | 🟡 | Honest re-statement |
| 52d | Kernel Completion and Roadmap Alignment | Complete | The honest meta-audit phase. Documented and closed all five identified gaps in 52a/b/c. Re-deferred true per-core scheduling and growable notification pool with explicit rationale | ✅ | Model for how to close drift |
| 53a | Kernel Memory Modernization | Complete | Detailed acceptance criteria, explicit closure-sequence contract with 53 | ✅ | — |
| 53 | Headless Hardening | Complete | Depends on Phase 51 (still In Progress); shutdown/reboot verification manual-only; pre-seeded image uses old hash format (explicitly accepted) | 🟡 | — |
| 54 | Deep Serverization | Complete | One read-only rootfs slice + UDP policy extracted. Closure review surfaced two hygiene items → became 54a. **`fat_server` is a permanent ENOSYS stub — replies `-ENOSYS` to every request** (code-side finding) | 🟠 | Extracted service is non-functional |
| 54a | Post-Serverization Kernel Hygiene | **Planned** | Two items: CLOEXEC/NONBLOCK plumbing gap (silent drop on `open`/`openat`/`openat2`/`vfs_service_open` — security-relevant) and four `*_pub` layer-crossing wrappers. `AGENTS.md` version string stale at `v0.51.0` | 🟡 | Triggered by 54 closure review; not yet started |

## Phases 55 – 57e (Hardware Substrate, Display, Audio, Preemption)

| # | Phase | Declared (in design doc) | Evidence | Severity | Notes |
|---|---|---|---|---|---|
| 55 | Hardware Substrate | Complete | All 37 task-list acceptance items remain `[ ]` (bookkeeping only). Physical-hardware validation deferred for all NVMe/e1000 matrix entries. Ring-0 NVMe + e1000 placement is an acknowledged TCB widening | 🟡 | Functional gap absent; bookkeeping gap |
| 55a | IOMMU Substrate | **Planned** | 55c, 56, 57 all list 55a as `✅`; AGENTS.md treats IOMMU as operational. Design doc still contains "Known Open Bug — must close before Phase 58" section about VT-d MMIO `CTRL.RST` drops, **claimed closed by 55c R2 but the 55a doc was never updated**. AMD-Vi has no fault ISR (Track E TODO in code). VT-d scalable mode hardcoded false. Queued invalidation deferred. Multi-BDF AMD-Vi domains deferred | 🛑 | Roadmap status drift |
| 55b | Ring-3 Driver Host | **Planned** | 55c lists 55b as `✅`; AGENTS.md describes ring-3 NVMe + e1000 as operational. Adversarial review (Codex) found 3 issues incl. `is_authorized_driver_process` `if false` placeholder — all marked resolved. LOC-metric targets missed; classified as accounting misses | 🛑 | Roadmap status drift |
| 55c | Ring-3 Driver Correctness Closure | Complete | R1/R2/R3 closed. **Phase 55c isolation tests are 100% `todo!()` scaffolding** (cross-device MMIO denial, DMA denial, capability forge, post-crash invalidation — all `#[ignore]`d, all empty). EAGAIN over block I/O deferred. NVMe migration to bound-notification deferred. **Open follow-up: 3 RX-path tests in `kernel/src/net/remote.rs` use `encode_net_send` where they should use `encode_net_rx_notify`** — masked by an unrelated frame-allocator failure | 🟠 | Negative-path isolation has zero coverage |
| 56 | Display and Input Architecture | **Planned** (per design doc) | `56-phase-56-completion-gaps.md` says "all 9 closing boxes ticked"; kernel bumped to 0.56.0; 267 / 279 acceptance bullets ticked. **D-E4 `TODO(subscription-push)` markers in `userspace/display_server/src/control.rs` — server-initiated subscription push is unimplemented (events queue but never transmit)**. D-A0 L/R modifier chord differentiation deferred (wire-format change). D-F1a mouse_server dependency direction. Compositor has no damage tracking | 🟠 | Status drift + subscription push wire transmission absent |
| 57 | Audio and Local Session | Complete | Single-client AC'97 audio + `session_manager` boot sequence | 🟡 | Multi-client mixing deferred (intentional) |
| 57a | Scheduler Block/Wake Protocol Rewrite | **Planned** (design doc) / Complete (README) | AGENTS.md and 57b/c list 57a as `✅`; PR #129 (`4c72e34`) merged but design-doc Status not updated. **Code has 4 `TODO(57a-C/D): route through pi_lock + with_block_state` markers in `kernel/src/task/scheduler.rs:829, 3649, 3656, 3855` at sites that bypass the abstraction** | 🛑 | Status drift; pi_lock wiring missing at 4 callsites (validated 2026-05-08) |
| 57b | Preemption Foundation | **Complete pending soak (PR #132)** | PR #132 squash-merged as `f39ca13`; the "pending soak" qualifier is stale. No soak-result document exists. `preempt_count` discipline + IrqSafeMutex F.1 wiring landed and is now load-bearing for the SMP discipline hardening that survived the 57e deferral | 🟡 | Status field stale (validated 2026-05-08) |
| 57c | Kernel Busy-Wait Conversion | Complete | Independent of 57b; converts unbounded spins; annotates hardware-bounded spins. `preempt_disable` wrappers at annotated sites were deferred to 57e Track B — moot after 57e deferral | ✅ | Honest split |
| 57d | Voluntary Preemption | **Planned** (design doc) / Complete (README) | PR #134 (`797ed0c`) merged; README row reads `Complete`; design-doc Status still reads `Planned`. PREEMPT_VOLUNTARY is the m3OS production preemption model after 57e deferral | 🛑 | Status drift (added validation pass 2026-05-08) |
| 57e | Full Kernel Preemption | **Deferred (2026-05-07)** | PR #136 squash-merged as `ad7d9b2`. After 18 debugging sessions and 13 distinct bugs, real-hardware testing on `omarchy` confirmed timer-driven kernel-mode preemption could not be made lag-free without effectively reverting to voluntary mode. Post-mortem at `docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md`. SMP discipline hardening (preempt_count, IrqSafeMutex F.1, wake-bracket race-shape closures, `sys_waitpid` 1 s deadline backstop, init_task 50 ms reap-loop sleep) **retained** as preempt-model-independent. `check_and_preempt_kernel`, `preempt-full` Cargo flag, and kernel-mode preempt-frame plumbing **removed**. Future work: `cond_resched`-style explicit yield points if kernel-mode latency becomes a bottleneck | ✅ | Honest deferral with detailed post-mortem (validated 2026-05-08) |

---

## Cross-cutting patterns

1. **Universally-unchecked task docs with `Status: Complete` headers** — Phases 42b, 43b, 43c, 46, 47. Either the work was done and never recorded, or the work was not done. Either interpretation makes the task doc untrustworthy as a completion record.

2. **Wholesale validation-track deferrals** — Phases 22b, 30, 31, 32 each declare implementation Done while leaving entire validation tracks unchecked behind "manual QEMU test" annotations. The runtime behaviour of telnetd, TCC, `make`, and the ANSI parser was never actually verified.

3. **Status-field drift in the live phases** — 55a, 55b, 56, 57a, and 57d all say "Planned" in their design doc headers while every downstream phase and AGENTS.md treats them as completed dependencies. (Validation pass 2026-05-08: 57d added after PR #134 merged with no design-doc Status flip; 57b's "Complete pending soak (PR #132)" qualifier is now stale because PR #132 has merged.) This makes the README's status column unreliable as a source of truth.

4. **Headline deliverable left undone** — Phase 33 (slab migration), Phase 35 (`maybe_load_balance` commented out), Phase 21 (ion as primary shell). The supporting infrastructure shipped; the headline behaviour change did not.

5. **Acceptance criteria silently rephrased** — Phase 28 Track I "removed `.m3os_permissions` from FAT32 (or kept as fallback)" is the canonical example. The original goal was removal; the parenthetical hedge was added in lieu of doing the work.

6. **Foundational phases bypassed** — Phase 51 (Service Model Maturity, In Progress with no task doc) is implicitly assumed complete by Phase 53's gate bundle and Phase 54's deep-serverization closure. The remediation chain (52a → 52b → 52c → 52d) was disciplined; the service-model chain (51 → 52 → 53 → 54) was not.
