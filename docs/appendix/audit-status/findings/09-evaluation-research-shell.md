# Findings: docs/evaluation, docs/research, docs/shell (missed by 2026-05-07 audit)

**Validation pass:** 2026-05-08

---

## Per-doc summary

### `docs/evaluation/README.md`

**What it documents:** Executive overview of the m3OS evaluation pass, scoped through Phase 47 / v0.47.0. Links to all evaluation sub-docs.

**Date / recency:** Explicitly scoped to "Phase 47 / v0.47.0" as the shipped base. No calendar date.

**Status claims:** Phases 1–47 treated as the shipped base. Calls out security P0 failures, missing GUI, and incomplete microkernel enforcement as the three dominant gaps. Cites Phase 46 as having delivered a real service baseline.

**Strategic positioning:** Frames m3OS as "serious reference-quality OS, not yet a desktop or safely network-exposed multi-user OS." References evaluation/roadmap/ as a release overlay (not a replacement) on top of `docs/roadmap/`.

**Relevant to audit:** Confirms the R01 security foundation gap (setuid/setgid, entropy, telnet, default creds) that the audit already captured. Consistent with the audit's findings on Phase 46 and Phase 47. No new gaps surfaced; serves as an accurate cross-check frame.

---

### `docs/evaluation/current-state.md`

**What it documents:** Architecture reality check and subsystem maturity snapshot as of Phase 53 / post-53a. Describes the kernel/userspace split gap, the "partial serverization" state, and the Phase 53 closure contract.

**Date / recency:** References Phase 53 closure contract and Phase 53a allocator changes. Effectively dated to the Phase 53a/54 window (likely early 2026).

**Status claims:**
- Phase 46 service model: ships real PID 1 supervision, syslogd, crond.
- Phase 47 DOOM: ships a graphical proof.
- Phase 53: names exact gate bundle (`cargo xtask check`, `kernel-core` unit + loom tests, `smoke-test --timeout 300`, `regression --timeout 90`).
- Phase 54 extracts `fat_server`, `vfs_server`, `net_server` (UDP only).
- States Phase 53 is not complete until the gate bundle passes on a post-53a image.

**Strategic positioning:** Explicitly separates "supported headless/reference workflow" from GUI, broad hardware, large ecosystems. Defines the Stage 1 usability ceiling before Stage 2 (desktop substrate).

**Items relevant to audit:**
- Confirms the `fat_server` partial extraction (audit Red Flag #14: `fat_server` is a permanent ENOSYS stub). The text here says "fat_server … are now real supervised services" — inconsistent with code evidence. This doc's claim is ahead of the runtime reality the audit found.
- Phase 53 gate bundle description here matches the formal gate spec in `docs/roadmap/53-headless-hardening.md`.
- "Phase 46/51 supervised crash recovery" claim: Phase 51 has no task doc (audit Red Flag #6). This doc treats Phase 51 as shipped capability, which the audit flagged as unverified.

---

### `docs/evaluation/gui-strategy.md`

**What it documents:** Options and recommended path for a GUI stack, updated through Phase 57.

**Date / recency:** Has explicit "Status update (Phase 56)" and "Status update (Phase 57)" sections, placing it post-Phase 57.

**Status claims:**
- Phase 56 four Goal-A contract points: declared delivered (layout module, keybind grab hook, surface roles, control socket).
- Phase 57: `audio_server`, `session_manager`, `term`, recovery-to-text-mode, multi-client audio policy (single-client EBUSY) — all declared done.
- Names Phase 56b / 57b / 57c as pending additive GUI UX work.

**Items relevant to audit:**
- The "Phase 57 multi-client audio policy — second client receives -EBUSY deterministically" claim is contradicted by the post-phase-57 evaluation (`01-phase-57-progress.md`), which documents that `Ac97Backend::submit_frames` does accounting only and no PCM is consumed. This doc's Phase 57 completion language is ahead of the runtime reality.
- Subscription delivery "pull/queue-driven" vs. "event-push" gap: matches audit Red Flag #15 (subscription events queued but not transmitted).
- Phase 56 display-server follow-throughs (zero-copy buffer transport deferred, exclusive Layer handling incomplete) are consistent with audit Red Flag #15.

---

### `docs/evaluation/rust-os-comparison.md`

**What it documents:** Positioning of m3OS relative to Redox, Tock, Hubris, Theseus, blog_os.

**Date / recency:** References Phase 46 headless baseline as current. No calendar date. Pre-Phase 55 in tone ("no real GUI yet") but the intro quadrant chart is from an earlier snapshot.

**Status claims:** Describes m3OS as "far beyond tutorial kernels, but still pre-desktop and pre-broad-hardware compared with Redox." Microkernel enforcement comparison: Redox enforces, m3OS does not yet.

**Strategic positioning:** Recommends positioning as "serious reference-quality microkernel-style OS in Rust." Warns against claiming secure general-purpose OS or Redox-class desktop too early.

**Items relevant to audit:** No new gaps. Consistent with audit findings. Provides comparative framing but no phase-status claims.

---

### `docs/evaluation/microkernel-path.md`

**What it documents:** Current microkernel deficiencies and a five-stage migration path (Stages 0–5).

**Date / recency:** References Stage 0 (Phase 49 — Complete), Stage 1 (Phase 50 — Complete v0.50.0), Stage 2 (Phase 52 — In Progress). Dated to the Phase 52 active-work window.

**Status claims:**
- Stage 0 (Phase 49): Complete. Deliverable table fully populated with source file refs.
- Stage 1 (Phase 50): Complete. Full deliverable table.
- Stage 2 (Phase 52): In Progress. Console extraction, keyboard extraction, stdin feeder listed as "In Progress"; boundary measurements "TBD".
- Stages 3–5 (storage, networking, kernel narrowing): not started, no phase assigned.

**Items relevant to audit:**
- Stage 2 In Progress status is consistent with the audit's finding that Phase 52's extractions are partial.
- `fat_server` ENOSYS gap is not surfaced here but is implied by "service extraction still selective."
- IPC shared-address-space shortcuts still present: confirms the audit's finding that the microkernel boundary is incomplete.
- Stages 3–5 have no owner phases — this is a genuine roadmap gap not surfaced in the main `docs/roadmap/` audit, though the audit noted serverization was selective.

---

### `docs/evaluation/hardware-driver-strategy.md`

**What it documents:** Donor strategy for real-hardware drivers (specs first, Redox second, BSD third, Linux as reference only).

**Date / recency:** Contains an explicit "Phase 55 update (kernel v0.55.0)" note saying the doc's "current reality" rows describe pre-Phase-55 state. Effectively dated to or after Phase 55.

**Status claims:**
- Pre-Phase-55 state: legacy PCI port I/O, VirtIO-only block/net. Accurately marked historical.
- Phase 55 landed: PCIe MCFG, MSI/MSI-X, NVMe, Intel e1000 — consistent with Phase 55 design doc.
- No Redox code was imported for Phase 55 drivers (specs + VirtIO behavioral reference only).

**Items relevant to audit:** No contradictions found. Consistent with audit findings on Phase 55 completion.

---

### `docs/evaluation/security-review.md`

**What it documents:** Security posture analysis, P0 gaps, P1 remaining work, comparison to Redox.

**Date / recency:** Has explicit "Phase 48 closed the most severe security gaps" language. Dated to post-Phase-48.

**Status claims:**
- P0 items (setuid/setgid, entropy, telnet default, default creds, password hashing): all five resolved by Phase 48.
- Remaining P1 items: SSH daemon hardening (no privilege separation, no rate limiting), auth-file update atomicity, secret hygiene (`.gitignore` gaps), filesystem semantics (sticky-bit, home-directory behavior).

**Items relevant to audit:**
- **NEW GAP (not in the main audit):** Auth-file rewrites are not atomic. `passwd` and `adduser` do simple file rewrites without temp+rename or file locking. This is a data integrity risk under concurrent access or crash mid-write. The security-review.md calls this out explicitly; the main audit does not surface it.
- **NEW GAP:** `.gitignore` is missing generic secret/key patterns. Specific key files used by the Secure Boot workflow may not be covered.
- **NEW GAP:** `/tmp` semantics: sticky-bit not enforced; temporary paths used as long-lived home directories. This is a local privilege escalation surface on a multi-user system.
- SSH hardening gap (no privilege separation for sshd): confirmed by audit's Phase 43 findings (audit Red Flag #12). Security-review adds the "no rate limiting, no brute-force controls, no key re-exchange" specifics that the audit did not surface.
- Microkernel TCB → security gap linkage is clearly articulated: filesystem, networking, and terminal policy remaining in ring 0 means their bugs are kernel bugs with kernel blast radius. Consistent with audit findings.

---

### `docs/evaluation/usability-roadmap.md`

**What it documents:** Four usability stages (current QEMU system → bounded headless → local desktop → broader platform) with gate definitions and readiness criteria.

**Date / recency:** Contains "Phase 56 update" and "Phase 57 update" sections, placing it post-Phase 57.

**Status claims:**
- Phase 56: display server substrate complete (four contract points delivered).
- Phase 57: `audio_server`, `session_manager`, `term`, recovery-to-text-mode, multi-client audio policy — all declared complete.
- Stage 1 is anchored to Phase 53 gate bundle passing on a post-53a image.
- Stage 2 is said to be "in scope for evaluation" after Phase 57.

**Items relevant to audit:**
- The Stage 2 "in scope for evaluation" claim contradicts the post-phase-57 evaluation finding that the real-hardware GUI gate still fails due to cooperative-scheduling starvation. This doc presents Stage 2 as achieved; the research docs say it is not.
- Phase 57 audio completion language is ahead of runtime: `audio_server` Ac97Backend submits frames for accounting only (no PCM to hardware). See `01-phase-57-progress.md`.
- Stage 2 readiness criterion 4 ("a crash in one GUI app does not take down the whole session") is not yet met per the display server subscription delivery gap (Red Flag #15).

---

### `docs/evaluation/redox-driver-porting.md`

**What it documents:** Deep analysis of Redox driver architecture, class-by-class portability, technical blockers, and a shim feasibility assessment.

**Date / recency:** Has explicit "Phase 55 update (kernel v0.55.0)" noting that the two "high feasibility" targets (NVMe, e1000) were built without porting any Redox code.

**Status claims:** Pre-Phase-55 analysis sections are correctly labeled historical. Phase 55 outcome is accurately described.

**Items relevant to audit:** No contradictions. Confirms the audit's Phase 55 findings. Useful detail on USB/xHCI as a deferred blocker (explicitly no phase assigned for USB HID).

---

### `docs/evaluation/roadmap/README.md` (evaluation/roadmap overview)

**What it documents:** Release-oriented overlay grouping official roadmap phases into R01–R10 release phases. Does not replace `docs/roadmap/`.

**Date / recency:** References "Completed Phases 44-47" as the starting base, and Phases 55–57 covering R08/R09. Effectively current through Phase 57.

**Status claims:**
- R01 (Security Foundation): Proposed — meaning P0 is resolved but R01 is not fully closed.
- R02 (Architectural Declaration): Complete (Phase 49).
- R03 (IPC Completion): Complete (v0.50.0).
- R04 (Service Model): In Progress.
- R05 (First Service Extractions): In Progress (Phase 52).
- R06 (Hardening and Operational Polish): In Progress.
- R07 (Deep Serverization): Complete.
- R08 (Hardware Substrate): Phase 55 non-display complete; 55a/55b still planned.
- R09 (Display and Input Architecture): In Progress (Phase 56 substrate + Phase 57 audio/session/term done; 56b/57b/57c pending).
- R10 (Release 1.0): Proposed.

**Items relevant to audit:**
- R07 "Complete" for Deep Serverization is inconsistent with the `fat_server` ENOSYS stub (audit Red Flag #14). This overlay labels Phase 54/R07 as Complete, but the FAT server is non-functional.
- R04 "In Progress" is consistent with Phase 51 having no task doc (audit Red Flag #6).
- R05 "In Progress" matches audit findings on Phase 52.
- R08 listing 55a and 55b as "still planned" matches the audit's Red Flag #7 (design docs say Planned while downstream treats them as done).

---

### `docs/evaluation/roadmap/R01-security-foundation.md`

**What it documents:** Release phase for P0 security fixes (setuid/setgid, entropy, telnet default, default creds, password hashing).

**Status:** Proposed. Acceptance criteria listed.

**Items relevant to audit:** R01 "Proposed" status implies the P0 issues are not yet fully resolved per this overlay's definition. The main security-review.md says Phase 48 closed these items. This is likely a doc-update lag: R01 was defined before Phase 48 closed the items. Consistent with Phase 48 being marked Complete.

---

### `docs/evaluation/roadmap/R02-architectural-declaration.md`

**Status:** Complete (Phase 49). Acceptance criteria all satisfied with source file references. Consistent with the audit.

---

### `docs/evaluation/roadmap/R03-ipc-completion.md`

**Status:** Complete (v0.50.0). Full deliverable table populated. Consistent with audit.

**Additional detail:** Notes that typed IPC IDLs and code-generated bindings are deferred to a later phase (not yet assigned). This is a gap the audit did not surface explicitly.

---

### `docs/evaluation/roadmap/R04-service-model.md`

**Status:** In Progress. Phase 46 baseline and Phase 51 maturity work described. Consistent with audit Red Flag #6 (Phase 51 has no task doc).

---

### `docs/evaluation/roadmap/R05-first-service-extractions.md`

**Status:** In Progress (Phase 52). Implementation Progress section notes display ownership deferred to Phase 56 (resolved). Consistent with audit.

---

### `docs/evaluation/roadmap/R06-hardening-and-operational-polish.md`

**Status:** In Progress. Acceptance criteria reference Phase 53 gate bundle and note Phase 53 cannot be marked complete until post-53a gates pass.

**Items relevant to audit:** Explicitly defers outbound HTTPS/TLS, DNS, git, GitHub tooling to post-1.0 (Phases 59–62). This is a concrete scope boundary the audit did not surface explicitly as a confirmed resolution.

---

### `docs/evaluation/roadmap/R07-deep-serverization.md`

**Status:** Complete.

**Items relevant to audit:** R07 "Complete" conflicts with `fat_server` being an ENOSYS stub (audit Red Flag #14). The acceptance criterion "at least one real storage path runs through userspace-owned block/filesystem services" may be technically satisfied by `vfs_server` + `net_server` even if `fat_server` is a stub, but the doc does not qualify this. This is a meaningful ambiguity.

---

### `docs/evaluation/roadmap/R08-hardware-substrate.md`

**Status:** "Phase 55 Complete — non-display workstreams landed; Phase 55a (IOMMU), Phase 55b (ring-3 driver host), and Phase 56 (display / input) still planned."

**Items relevant to audit:** Accurately reflects the partial Phase 55 state. Phase 55a and 55b listed as planned, consistent with audit Red Flag #7 (design docs say Planned but downstream treats them as done). The Phase 55 Completion Status section here is detailed and accurate. Notes that Phase 55a must land before Phase 55b (IOMMU-gated DMA required for real isolation).

---

### `docs/evaluation/roadmap/R09-display-and-input-architecture.md`

**Status:** In Progress (Phase 56 substrate + Phase 57 audio/session/term complete; Phase 56b / 57b / 57c additive UX work pending).

**Items relevant to audit:**
- Lists Phase 57 audio, session, terminal as "Done" — contradicted by the post-phase-57 evaluation (Ac97Backend is a production stub; session_manager does not own the full lifecycle).
- "Multi-client audio policy — single-client per YAGNI rule; a second OpenStream returns -EBUSY deterministically. Done." — this claim is ahead of the runtime per `01-phase-57-progress.md`, which documents that `submit_frames` does accounting only with no PCM consumed.
- Subscription delivery gap (Red Flag #15) is acknowledged: "subscription delivery is still a pull/queue-driven control-socket model rather than a richer event-push design … deferred."

---

### `docs/evaluation/roadmap/R10-release-1-0-and-beyond.md`

**Status:** Proposed.

**Items relevant to audit:** Explicitly states that the 1.0 release promise requires a "support matrix with explicit non-goals." Lists post-1.0 work (larger toolchains, Node.js, AI-agent workflows, wider hardware, richer desktop polish) as 1.x scope. Consistent with the audit's pre-1.0 blocker analysis.

---

### `docs/research/README.md`

**What it documents:** Index for three allocator research docs, stating they inform Phase 53a.

**Items relevant to audit:** Phase 53a (Kernel Memory Modernization) is not in the main audit's findings directory (the audit covered through Phase 57e). This is expected — Phase 53a is a real phase that exists in the roadmap.

---

### `docs/research/allocator-theory.md`

**What it documents:** Foundational theory on slab allocators, magazine layer, buddy systems, per-CPU design, false sharing, interrupt safety, and debugging techniques.

**Status:** Draft.

**Items relevant to audit:** No phase-status claims. Confirms the per-CPU cooperative scheduling observation: "No kernel preemption (cooperative scheduling). Per-CPU access via APIC ID is safe without explicit preemption disable." This is consistent with the scheduler starvation gap (audit Red Flag #7 / 57b-57d scope) but applies to the allocator domain, not the scheduler preemption problem.

---

### `docs/research/m3os-allocator-analysis.md`

**What it documents:** Deep analysis of the current m3OS four-layer memory subsystem. Catalogs 27 deficiencies (D-01 through D-27). Proposes a five-phase migration plan.

**Status:** Draft. Branch: `research/memory-allocator`.

**Key deficiency catalog:**
- D-01: No interrupt-safe allocation — deadlock risk.
- D-03: `remove_free` is O(n) in buddy — coalescing degrades under load.
- D-08: Slab caches defined but not integrated — all kernel objects still go through the O(n) heap.
- D-10: Heap never returns memory to buddy — no reclamation.

**Items relevant to audit:**
- **NEW GAP:** Slab caches (task_cache, fd_cache, endpoint_cache, pipe_cache, socket_cache) are defined in `kernel-core/src/slab.rs` and `kernel/src/mm/slab.rs` but are not integrated — all kernel objects still route through the O(n) first-fit linked-list heap. Phase 33 (Kernel Memory) created the slab infrastructure; Phase 53a (Kernel Memory Modernization) added per-CPU page caches and magazine-based slab improvements. The allocator analysis pre-dates Phase 53a, so some deficiencies may be partially closed. However, the D-08 "slab caches in use: 0" claim about the five named caches is consistent with the audit's Phase 33 Red Flag #4 (slab migration headline deliverable was deferred). If Phase 53a closed D-08, the research doc needs updating and the Phase 33 deferred status needs re-checking.
- D-01 (no interrupt-safe allocation path) is a correctness hazard the audit did not independently surface. This is a NEW gap to document.
- D-03 (O(n) remove_free in buddy) — coalescing degrades under load. Phase 53a may have addressed this; unclear from doc alone.

---

### `docs/research/memory-allocator-survey.md`

**What it documents:** Cross-system survey of Linux SLUB/buddy/PCP, FreeBSD UMA, Redox, Theseus, jemalloc, mimalloc, tcmalloc. Feature matrices and design recommendations for m3OS.

**Status:** Draft.

**Items relevant to audit:** No phase-status claims. Background research informing Phase 53a. No contradictions.

---

### `docs/research/post-phase-57 evaluation/README.md`

**What it documents:** Post-Phase-57 evaluation index. Dated **2026-04-29**. Baseline: `main` at `4c72e34` (Phase 57a scheduler rewrite, merged 2026-04-29).

**Status claims:**
- Phase 57 has the "right shape" for a local graphical system but is "architecture-complete, scheduler-rewritten, and smoke-connected" — not fully functional.
- The real-hardware GUI gate still fails due to cooperative-scheduling starvation.
- Several Phase 57 components contain production stubs or validation shortcuts.
- Phase 57a v2 block/wake protocol is in-tree; improves foundation but does not close the starvation problem.

**Items relevant to audit:** Directly relevant to audit Red Flags #7 (55a/55b/56/57a/57d status drift) and #16 (pi_lock wiring). Confirms that the "Phase 57 Complete" language in the official roadmap and evaluation docs is ahead of the runtime.

---

### `docs/research/post-phase-57 evaluation/01-phase-57-progress.md`

**What it documents:** Detailed Phase 57 progress ledger. Dated **2026-04-29**. Grades Phase 57 on architecture (High), runtime completeness (Medium-low), validation strength (Low-medium), usable GUI readiness (Low).

**Status claims — specific production stubs:**
1. `Ac97Backend::init` does not allocate or program BDL or PCM ring.
2. `submit_frames` accepts bytes for accounting only.
3. `drain` returns immediately.
4. `handle_irq` returns `IrqEvent::None`.
5. `audio-smoke` in xtask checks that `audio_server.conf` loads; does not run `audio-demo` or assert frame consumption.
6. `session_manager` `start(name)` returns Ack unconditionally without issuing start; `stop` logs F.4 future work; `restart` returns Ack without doing work.
7. Phase 57a fixes lost-wake class but not cooperative-scheduling starvation (timer IRQ return goes back to interrupted user code).

**Items relevant to audit:**
- **CONFIRMS** and extends audit Red Flag #7 for Phase 57: the official roadmap/README marks Phase 57 as Complete; this doc (dated 2026-04-29) shows the audio backend, session manager lifecycle, and GUI reliability gate are production stubs.
- **NEW DETAIL not in audit:** `audio-smoke` in xtask does not assert PCM frame consumption progress. The smoke gate is shallower than implied by Phase 57 acceptance criteria.
- **NEW DETAIL:** `session_manager`'s `start/stop/restart` are not real lifecycle owners — they return Ack unconditionally. Phase 57 acceptance criteria for session management are not satisfied.
- Scheduler starvation = Phase 57b/57c/57d scope, captured in the audit. This doc confirms the same diagnosis independently.

---

### `docs/research/post-phase-57 evaluation/02-usable-gui-omarchy-hyprland-roadmap.md`

**What it documents:** Roadmap for a native m3OS tiling compositor (Omarchy/Hyprland workflow style) without porting Hyprland itself. Dated **2026-04-29**.

**Status claims:** Phase 57a merged and improves scheduler foundation. Real-hardware GUI gate still fails. Tiling layout, workspaces, bar, launcher, notification daemon, lockscreen, configurable keybinds — all remain unbuilt.

**Strategic positioning:** Recommends building a native `m3wm` compositor policy layer inside or alongside `display_server`, implementing master-stack, dwindle/BSP, and floating layouts, then adding bar/launcher/notification/lockscreen as Phase 56 Layer-role clients.

**Items relevant to audit:** Confirms no tiling UX exists yet. Phase 56b / 57b / 57c scope boundary consistent with R09 evaluation doc.

---

### `docs/research/post-phase-57 evaluation/03-real-applications-browser-roadmap.md`

**What it documents:** Gap analysis for real application support, especially browsers. Dated **2026-04-29**.

**Status claims:** Real browser requires: dynamic linker or all-static strategy, C++ runtime, POSIX process model details, threading, epoll/eventfd/timerfd/inotify, /dev//proc//sys/runtime dirs, Wayland or X11 client protocol, hardware-accelerated or software-rendered graphics, DNS/TLS/TCP stability, font stack, browser sandbox policy. None of these are present.

**Strategic positioning:** Application classes ranked: native m3OS apps → TUI apps in `term` → simple Wayland clients via `wl_shm` adapter → toolkit GUI apps → browsers. Browsers are described as a "long-term multi-phase program."

**Items relevant to audit:**
- **NEW GAPS surfaced:**
  - Dynamic linker / shared library loading: not in any current phase. The audit flagged `all-static` distribution discipline as an open question; this doc confirms dynamic linking is absent and unplanned.
  - DNS resolver with config: not yet implemented per Phase 60 scope. The audit referenced Phase 60 but this doc makes the browser-level dependency explicit.
  - Hardware-accelerated graphics path: unplanned. Not a 1.0 concern but relevant to "broader developer platform."
  - Browser sandbox policy: not in any current phase.

---

### `docs/research/post-phase-57 evaluation/04-tui-and-neovim-roadmap.md`

**What it documents:** Gap analysis for TUI applications and Neovim specifically. Dated **2026-04-29**.

**Status claims:** Phase 57a improves blocking-wait reliability; cooperative-scheduling starvation remains. Terminal compatibility gaps: raw/cbreak termios modes, alternate screen buffer, 256-color/truecolor, UTF-8 decoding + Nerd Font glyphs, resize/SIGWINCH, bracketed paste, mouse reporting, scrollback/clipboard, terminfo entry, keyboard protocol clarity.

**Strategic positioning:** Recommends host-cross static Neovim build (musl, bundled deps, no JIT if needed) as the first Neovim milestone. Needs: CMake/Ninja, libuv, LuaJIT or PUC Lua, tree-sitter, runtime files.

**Items relevant to audit:**
- **NEW GAPS:**
  - `term` lacks a published `TERM=m3os` terminfo entry. No tests assert the terminal implements its escape sequences. This is a concrete gap the audit did not surface.
  - `term` alternate screen buffer missing: full-screen TUIs will clobber shell scrollback.
  - 256-color and truecolor support missing from `term`.
  - `SIGWINCH` / resize handling gap.
  - Mouse reporting missing from `term`.

---

### `docs/shell/brush-integration-analysis.md`

**What it documents:** Feasibility analysis of integrating the `brush` Rust shell into m3OS. Dated **2026-03-26**.

**Date / recency:** Dated 2026-03-26. References Phases 1–16 as complete, Phases 17–23+ as future work. This is a snapshot from very early in the Phase 17–23 window, before musl (Phase 12 is called "not built" here — yet the audit finds Phase 12 is now Complete). The doc is therefore significantly stale relative to the current codebase.

**Status claims at doc date:**
- Phase 12 (musl): "designed, not built" — now stale; Phase 12 is Complete.
- Threading (`clone(CLONE_VM)` + epoll + eventfd + timerfd): "not in roadmap" — threads are now part of Phases 40+ (threading primitives, epoll via Phase 37 io multiplexing).

**Items relevant to audit:**
- The "threading not in roadmap" claim is stale: the audit finds Phase 40 (threading primitives) is Complete and epoll exists from Phase 37. However, full Tokio-compatible threading (pthreads semantics, shared address-space `clone`) may still not be implemented — the doc's concern about full brush binary remains valid even if the specific roadmap claim is stale.
- The brush-parser extraction recommendation (use `brush-parser` without tokio, write custom executor) is still architecturally sound advice.
- **Staleness note:** This doc should not be used as a current state-of-the-art reference for Phase 12 or threading status.

---

### `docs/shell/alternative-shells.md`

**What it documents:** Evaluation of ion shell (from Redox OS) as an earlier-integrable alternative to full brush. Companion to brush-integration-analysis.md.

**Date / recency:** No explicit date. References Phase 20 as the target integration point. Written in the Phase 17–22 planning window. Stale relative to current state — the audit finds Phase 21 is Complete (ion integration happened but with sh0/ion inversion — see audit Red Flag #13).

**Status claims:** Recommends ion as best shell alternative (no tokio, no threads, Redox-tested). Maps ion's nix feature requirements to m3OS phase readiness.

**Items relevant to audit:**
- Audit Red Flag #13 (Phase 21 milestone-goal inversion: sh0 became primary, ion became fallback) is the outcome of this recommendation. The doc predicted ion as the winner; ion was integrated but relegated to fallback. The `phase-21-handoff.md` doc in the audit captures the inversion. Phase 22 is marked Complete, suggesting the inversion was eventually undone, but Phase 21's status field never reflected it.
- The `sigaction` requirement for ion script mode (nix::signal) maps to Phase 19, which the audit flagged as Red Flag #1 (task doc all six tracks "Not started" despite design doc saying Complete). If Phase 19 signals genuinely work, ion script mode is feasible; if Phase 19 is incomplete, ion script mode may also be incomplete.
- `termios` (tcgetattr/tcsetattr) for ion interactive mode maps to Phase 22 — marked Complete in the audit.

---

## Cross-cutting findings

### State-snapshot docs and their staleness vs. README

| Doc | Date | Phase claim | Current state | Discrepancy |
|---|---|---|---|---|
| `evaluation/README.md` | Scoped to v0.47.0 | Phases 1–47 are shipped base | Phases 1–57e are now shipped | Significantly behind; treats Phase 47 as the frontier; ~10 phases stale |
| `evaluation/current-state.md` | Phase 53/53a window | Phase 54 extractions (fat_server, vfs_server, net_server) complete | `fat_server` is ENOSYS stub (Red Flag #14) | Overclaims Phase 54 FAT extraction |
| `evaluation/gui-strategy.md` | Post-Phase 57 | Phase 57 audio, session, terminal: done | Ac97Backend is accounting stub; session_manager lifecycle incomplete | Phase 57 production stubs not reflected |
| `evaluation/usability-roadmap.md` | Post-Phase 57 | Stage 2 (desktop substrate) in scope after Phase 57 | Real-hardware GUI gate fails (starvation); audio stub | Overclaims Stage 2 readiness |
| `evaluation/roadmap/R07-deep-serverization.md` | Post-Phase 54 | Complete | `fat_server` is ENOSYS stub | Overclaims Phase 54 FAT storage extraction |
| `evaluation/roadmap/R08-hardware-substrate.md` | Post-Phase 55 | Phase 55a/55b "still planned" | README/AGENTS treats them as done (Red Flag #7) | Accurately conservative; consistent with audit |
| `evaluation/roadmap/R09-display-and-input-architecture.md` | Post-Phase 57 | Phase 57 audio/session/terminal: Done | Production stubs; GUI gate fails | Overclaims Phase 57 completion |
| `research/post-phase-57 evaluation/README.md` | **2026-04-29** | Phase 57 architecture-complete but not product-ready | Consistent with current codebase | Most accurate recent snapshot |
| `research/post-phase-57 evaluation/01-phase-57-progress.md` | **2026-04-29** | Grades runtime completeness Medium-low; GUI readiness Low | Consistent with audit code evidence | Most accurate; directly confirms Red Flags #7/#16 |
| `docs/shell/brush-integration-analysis.md` | **2026-03-26** | Phase 12 "not built"; threading "not in roadmap" | Phase 12 Complete; Phase 40 threading exists | Significantly stale; ~15+ phases behind |

---

### NEW gaps these docs identify that the audit missed

1. **Auth-file write non-atomicity** (`security-review.md`): `passwd` and `adduser` do simple file rewrites without temp+rename or locking. Risk: torn shadow file on crash or concurrent write. No owner phase assigned.

2. **`.gitignore` secret hygiene** (`security-review.md`): Missing generic secret/key file patterns. Specific key files may not be excluded. No owner phase.

3. **`/tmp` sticky-bit + home-directory semantics** (`security-review.md`): Sticky-bit not enforced on `/tmp`; temporary paths used as long-lived home directories. Local privilege escalation surface on a multi-user system. No owner phase.

4. **No interrupt-safe allocation path** (`m3os-allocator-analysis.md`, D-01): `spin::Mutex` in the frame allocator does not disable interrupts. An interrupt handler that allocates while the frame allocator lock is held would deadlock. Currently mitigated by convention ("no allocation in interrupt handlers") — not enforced. No owner phase.

5. **Slab caches defined but not integrated** (`m3os-allocator-analysis.md`, D-08): Five named slab caches (task, fd, endpoint, pipe, socket) route kernel objects through the O(n) linked-list heap instead. Phase 33 created the infrastructure; audit Red Flag #4 already flags the migration deferral, but this doc provides the specific deficiency ID and class listing.

6. **`term` has no published terminfo entry** (`04-tui-and-neovim-roadmap.md`): No `TERM=m3os` terminfo entry exists. No tests assert the terminal implements its advertised escape sequences. TUI applications that query terminfo will break or fall back to a generic terminal profile with unpredictable behavior.

7. **`term` missing alternate screen buffer, 256-color, truecolor, SIGWINCH, mouse reporting** (`04-tui-and-neovim-roadmap.md`): Full-screen TUI apps (nvim, tmux, btop) require these. None are listed as present in the post-57 evaluation. No owner phase for these gaps.

8. **Typed IPC IDLs / code-generated bindings deferred** (`R03-ipc-completion.md`): Phase 50 standardizes the raw message and capability-grant contract but explicitly defers typed IDLs and code-generated bindings to a later unassigned phase.

9. **`audio-smoke` does not assert PCM frame consumption** (`01-phase-57-progress.md`): The xtask audio smoke gate checks that `audio_server.conf` loads, not that audio frames reach hardware. The smoke gate is shallower than Phase 57 acceptance criteria imply.

10. **`session_manager` start/stop/restart are stub Ack returns** (`01-phase-57-progress.md`): Session manager does not own the lifecycle. `start(name)` returns Ack unconditionally; `stop` logs future F.4 work; `restart` returns Ack without restarting. Phase 57 acceptance criteria for session management are unsatisfied.

11. **Dynamic linker / shared library loading**: Not in any current phase. The all-static distribution discipline is an open question; `03-real-applications-browser-roadmap.md` makes the consequence explicit: no realistic path to toolkit GUI apps or browsers without dynamic linking or a disciplined all-static strategy for large dependency graphs.

---

### Strategic positioning docs that imply alternative roadmap structures

`docs/evaluation/roadmap/` (R01–R10) is an explicit **release-oriented overlay** on the official `docs/roadmap/`. The README for this directory says clearly it does not replace the official phases; it groups them by release purpose.

Key divergence from official roadmap:
- The R-series assigns release readiness status (Proposed / In Progress / Complete) to groupings of official phases. Where R-series says "Complete" (R07 — Deep Serverization) but the official phases underneath it contain production stubs (`fat_server`), the R-series status is an overstatement.
- R-series identifies a 1.0 definition: "narrowly scoped, well-documented OS in Rust that enforces its microkernel direction, is materially safer to operate than the current tree, boots in QEMU and on a small reference hardware set, and has a coherent headless service model." The optional local-system extension (Phase 56/57) is called out as a choice, not a requirement, for 1.0. This is a tighter and more honest 1.0 framing than the official roadmap alone provides.
- Phases 59–62 are explicitly marked as post-1.0 growth in the R-series. The official roadmap has them as named phases without a clear 1.0 / post-1.0 boundary statement.

---

### Items these docs raise that confirm or contradict audit red flags

| Audit Red Flag | Evaluation/Research finding | Direction |
|---|---|---|
| **#1 (Phase 19 task doc all Not started)** | `alternative-shells.md` maps ion's `sigaction` requirement to Phase 19, treating signals as a future Phase 19 deliverable. Stale (Phase 19 design doc says Complete). | Confirms the unresolved Phase 19 task/design-doc contradiction. |
| **#4 (Phase 33 slab migration deferred)** | `m3os-allocator-analysis.md` D-08: slab caches in use = 0. Confirms slab infrastructure exists but kernel objects still route through linked-list heap. | Confirms Red Flag #4. |
| **#6 (Phase 51 no task doc)** | `R04-service-model.md` status "In Progress"; references Phase 51 as a maturity step without resolving its missing task doc. | Confirms Red Flag #6 indirectly. |
| **#7 (55a/55b/56/57a/57d status drift)** | `01-phase-57-progress.md` (2026-04-29) explicitly grades Phase 57 runtime completeness as Medium-low and GUI readiness as Low, contradicting official Complete status. | **Strongest external confirmation of Red Flag #7's Phase 57 dimension.** |
| **#12 (Phase 43 SSH tests unchecked, pubkey format incompatible)** | `security-review.md` identifies no rate limiting, no privilege separation, no key re-exchange for sshd — additional SSH hardening gaps the audit's Red Flag #12 did not enumerate. | **Extends Red Flag #12 with specific missing hardening items.** |
| **#13 (Phase 21 sh0/ion inversion)** | `alternative-shells.md` recommended ion as primary; outcome was inverted. | Confirms the context behind Red Flag #13. |
| **#14 (`fat_server` ENOSYS stub)** | `current-state.md` and `R07-deep-serverization.md` both claim Phase 54 FAT extraction is complete. | **Contradicts Red Flag #14's code evidence.** These evaluation docs are ahead of the runtime. |
| **#15 (Phase 56 subscription push not transmitted)** | `gui-strategy.md` and `R09` acknowledge subscription delivery is "still a pull/queue-driven model rather than a richer event-push design." | **Confirms Red Flag #15.** Both evaluation docs acknowledge this explicitly. |
| **#16 (Phase 57a pi_lock wiring missing; 57b stale qualifier)** | `01-phase-57-progress.md` confirms Phase 57a runtime gaps. Phase 57b stale "pending soak" qualifier confirmed by the 2026-04-29 evaluation. | Confirms Red Flag #16's 57a dimension. |
