# Findings: Top-level docs/*.md Legacy Docs (missed by 2026-05-07 audit)

**Validation pass:** 2026-05-08

---

## Phase-numbered legacy docs vs. roadmap

| Legacy doc | Phase | Has aligned template? | Status field | Roadmap status | Match? | Notes |
|---|---|---|---|---|---|---|
| `docs/01-boot.md` | 1 | Yes | Complete | Complete | Yes | — |
| `docs/02-memory.md` | 2 | Yes | Complete | Complete | Yes | — |
| `docs/03-interrupts.md` | 3 | Yes | Complete | Complete | Yes | — |
| `docs/04-tasking.md` | 4 | Yes | Complete | Complete | Yes | — |
| `docs/05-userspace-entry.md` | 5 | Yes | Complete | Complete | Yes | — |
| `docs/06-ipc.md` | 6 | Yes | Complete | Complete | Yes | `Supersedes Legacy Docs` self-references `docs/06-ipc.md` — the doc names itself; also references `docs/06-ipc-core.md` which does not exist in top-level docs/ |
| `docs/07-core-servers.md` | 7 | Yes | Complete | Complete | Yes | — |
| `docs/08-storage-and-vfs.md` | 8 | Yes | Complete | Complete | Yes | — |
| `docs/09-framebuffer-and-shell.md` | 9 | Yes | Complete | Complete | Yes | — |
| `docs/10-secure-boot.md` | 10 | Yes | Complete | Complete | Yes | — |
| `docs/11-elf-loader-and-process-model.md` | 11 | Yes | Complete | Complete | Yes | — |
| `docs/12-posix-compatibility-layer.md` | 12 | Yes | Complete | Complete | Yes | — |
| `docs/13-writable-filesystem.md` | 13 | Yes | Complete | Complete | Yes | — |
| `docs/14-shell-and-tools.md` | 14 | Yes | Complete | Complete | Yes | — |
| `docs/15-hardware-discovery.md` | 15 | Yes | Complete | Complete | Yes | — |
| `docs/16-network.md` | 16 | Yes | Complete | Complete | Yes | Stale: doc body says `net_task` is a "temporary kernel-mode implementation" with "long-term plan to move to userspace" — that plan completed in Phase 54 (Deep Serverization). The note survives in the doc unchanged. |
| `docs/17-memory-reclamation.md` | 17 | Yes | Complete | Complete | Yes | — |
| `docs/18-directory-vfs.md` | 18 | Yes | Complete | Complete | Yes | — |
| `docs/19-signal-handlers.md` | 19 | Yes | Complete | Complete | Yes | — |
| `docs/20-userspace-init.md` | 20 | Yes | Complete | Complete | Yes | — |
| `docs/21-ion-shell.md` | 21 | Yes | Complete | Complete | Yes | — |
| `docs/22-tty-terminal.md` | 22 | Yes | Complete | Complete | Yes | Stale: overview line says "PTY skeleton stubs that will support terminal multiplexers in a later phase" — PTY was completed in Phase 29. |
| `docs/22b-ansi-escape.md` | 22b | Yes | Complete | Complete | Yes | — |
| `docs/23-socket-api.md` | 23 | Yes | Complete | Complete | Yes | — |
| `docs/24-persistent-storage.md` | 24 | Yes | Complete | Complete | Yes | — |
| `docs/25-smp.md` | 25 | Yes | Complete | Complete | Yes | — |
| `docs/26-text-editor.md` | 26 | Yes | Complete | Complete | Yes | — |
| `docs/27-user-accounts.md` | 27 | Yes | Complete | Complete | Yes | — |
| `docs/28-ext2-filesystem.md` | 28 | Yes | Complete | Complete | Yes | — |
| `docs/29-pty-subsystem.md` | 29 | Yes | Complete | Complete | Yes | — |
| `docs/30-telnet-server.md` | 30 | Yes | Complete | Complete | Yes | — |
| `docs/31-compiler-bootstrap.md` | 31 | Yes | Complete | Complete | Yes | — |
| `docs/32-build-tools.md` | 32 | Yes | Complete | Complete | Yes | — |
| `docs/33-kernel-memory.md` | 33 | Yes | Complete | Complete | Yes | — |
| `docs/34-timekeeping.md` | 34 | Yes | Complete | Complete | Yes | — |
| *(no legacy doc)* | 35–43 | N/A | N/A | Complete | N/A | Phases 35–42 and 43 have roadmap docs but no top-level legacy doc; not a defect (these phases post-date the legacy doc era) |
| `docs/43a-crash-diagnostics.md` | 43a | Yes | Complete | Complete | Yes | — |
| `docs/43b-kernel-trace-ring.md` | 43b | Yes | Complete | Complete | Yes | — |
| `docs/43c-regression-stress-ci.md` | 43c | Yes | Complete | Complete | Yes | — |
| `docs/44-rust-cross-compilation.md` | 44 | Yes | Complete | Complete | Yes | — |
| `docs/45-ports-system.md` | 45 | Yes | Complete | Complete | Yes | — |
| `docs/46-system-services.md` | 46 | Yes | Complete | Complete | Yes | Cross-references Phase 51 correctly |
| `docs/47-doom.md` | 47 | Yes | Complete | Complete | Yes | — |
| `docs/48-security-foundation.md` | 48 | Yes | Complete | Complete | Yes | — |
| `docs/49-architectural-declaration.md` | 49 | Yes | Complete | Complete | Yes | — |
| `docs/50-ipc-completion.md` | 50 | Yes | Complete | Complete | Yes | — |
| `docs/51-service-model-maturity.md` | 51 | Yes | In Progress | In Progress | Yes | Consistent with roadmap; truth matrix (01-completion-truth-matrix.md) flags this phase as 🛑 (no task doc, silently bypassed). Legacy doc status is accurate but the underlying phase is a known gap. |
| `docs/52-first-service-extractions.md` | 52 | Yes | In Progress | In Progress | Yes | Consistent with roadmap; truth matrix flags as 🟠 (sub-phases 52a-d did the work; parent never closed). |
| `docs/52a-kernel-reliability-fixes.md` | 52a | Yes | Complete | Complete | Yes | — |
| `docs/52b-kernel-structural-hardening.md` | 52b | Yes | Complete | Complete | Yes | — |
| `docs/52c-kernel-architecture-evolution.md` | 52c | Yes | Complete | Complete | Yes | — |
| `docs/52d-kernel-completion-and-roadmap-alignment.md` | 52d | Yes | Complete | Complete | Yes | — |
| `docs/53-headless-hardening.md` | 53 | Yes | Complete | Complete | Yes | — |
| `docs/53a-kernel-memory-modernization.md` | 53a | Yes | Complete | Complete | Yes | — |
| `docs/54-deep-serverization.md` | 54 | Yes | Complete | Complete | Yes | — |
| `docs/55-hardware-substrate.md` | 55 | Yes | Complete | Complete | Yes | — |
| `docs/55a-iommu-substrate.md` | 55a | Yes | Complete | Complete | Yes | — |
| `docs/55b-ring-3-driver-host.md` | 55b | Yes | Complete | Complete | Yes | — |
| `docs/56-display-and-input-architecture.md` | 56 | Yes | **Planned** | **Complete** | **NO** | Status field says `Planned`; roadmap says `Complete`. Content body reads as a design/planning document — no "Supersedes" or update note. This is the single outright Status mismatch among all phase-numbered legacy docs. |
| `docs/57-audio-and-local-session.md` | 57 | Yes | Complete | Complete | Yes | — |
| *(no legacy doc)* | 54a, 55c, 57a–57e, 58–62 | N/A | N/A | Various | N/A | All post-55b phases lack a top-level legacy doc; roadmap docs exist under `docs/roadmap/`. |

---

## Top-level "future-phase" roadmap docs (post-1.0)

| Doc | Topic | Aligned with roadmap phases 59-62? | Notes |
|---|---|---|---|
| `docs/clang-llvm-roadmap.md` | Cross-compiled Clang/LLVM | Partially (Phase 59 covers cross-compiled toolchains including Clang) | No aligned-template header; baseline says "Today (Phase 32 complete)" — stale by 25 phases. References Phase 33 as "in progress". |
| `docs/claude-code-roadmap.md` | Running Claude Code on m3OS | Yes (Phase 62) | No aligned-template header; dependency graph references phases 33–46 as future/planned — all now Complete. |
| `docs/git-roadmap.md` | git native inside m3OS | Partially (Phase 60 covers git remotes/HTTPS as part of networking+GitHub) | No aligned-template header; baseline "Today (Phase 32)"; Phase 33 marked "in progress". |
| `docs/github-cli-roadmap.md` | `gh` CLI inside m3OS | Yes (Phase 60) | No aligned-template header; prerequisites listed as future are now Complete (Phase 37 epoll, Phase 39 AF_UNIX, Phase 42 crypto). |
| `docs/nodejs-roadmap.md` | Node.js inside m3OS | Yes (Phase 61) | No aligned-template header; baseline "Today (Phase 32)"; lists Phase 37 (epoll) as "planned" — Complete since Phase 37. |
| `docs/python-roadmap.md` | CPython inside m3OS | Yes (Phase 59, via cross-compiled toolchains) | No aligned-template header; baseline "Today (Phase 32)"; Phase 33 "in progress", Phase 37/38 "planned" — all Complete. |
| `docs/rust-crate-acceleration.md` | Rust crate strategy for roadmap phases | Informally covers phases 41–47 but no phase number assigned | No aligned-template header; references Phase 42/43/41 as future goals — all Complete. Does not conflict with phases 59-62 but is now fully superseded by completed phases. |

---

## Cross-cutting

### Legacy docs missing the aligned-template header

All **post-1.0 roadmap docs** lack the aligned-template header fields (`Aligned Roadmap Phase`, `Status`, `Source Ref`):

- `docs/clang-llvm-roadmap.md`
- `docs/claude-code-roadmap.md`
- `docs/git-roadmap.md`
- `docs/github-cli-roadmap.md`
- `docs/nodejs-roadmap.md`
- `docs/python-roadmap.md`
- `docs/rust-crate-acceleration.md`

All phase-numbered legacy docs (01 through 57) carry the full aligned-template header. No phase-numbered doc is missing the header.

### Stale claims in legacy docs (content predates roadmap completion)

1. **`docs/16-network.md`** — Body block-quote states: "This is a temporary kernel-mode implementation to get a working TCP/IP stack. The long-term plan is to move [the network stack to userspace]." Phase 54 (Deep Serverization, Complete) executed that plan. The note is now factually incorrect.

2. **`docs/22-tty-terminal.md`** — Overview states "PTY skeleton stubs that will support terminal multiplexers in a later phase." Phase 29 (PTY Subsystem, Complete) delivered the full PTY implementation. The word "stubs" is stale.

3. **`docs/56-display-and-input-architecture.md`** — `**Status:** Planned` while the roadmap marks Phase 56 as `Complete`. The document content reads as a forward-looking design spec, not a retrospective implementation record. This doc was likely never updated after Phase 56 closed.

4. **All seven post-1.0 roadmap docs** — Each uses "Today (Phase 32)" or "Phase 33 in progress" as its current-state baseline. The project is now at Phase 57+. All prerequisite phases (33–49) they list as future/planned/in-progress are Complete. The baselines are 20+ phases stale.

5. **`docs/rust-crate-acceleration.md`** — Treats Phases 41–47 as future planning targets. All are now Complete. The document is obsolete as a planning artifact, though its content is still accurate as a record of the strategy that was adopted.

### Legacy docs that contradict the audit's findings

The existing `docs/appendix/audit-status/01-completion-truth-matrix.md` is at the `audit-status/` level (not inside `findings/`). Cross-checking against it:

- **Phase 56** (`docs/56-display-and-input-architecture.md`, Status: Planned): Contradicts both the roadmap (Complete) and implicitly the truth matrix, which treats Phase 56 as closed. This is the only hard contradiction.
- **Phase 51** and **Phase 52** legacy docs correctly mirror the roadmap's `In Progress` status. The truth matrix flags both phases as problematic (🛑 and 🟠 respectively), but the legacy docs are not themselves contradictory — they reflect the same In Progress state.
- **`docs/06-ipc.md`**: The `Supersedes Legacy Docs` field lists `docs/06-ipc.md` (self-reference) and `docs/06-ipc-core.md`. The self-reference is a copy-paste error; `docs/06-ipc-core.md` does not exist in the top-level docs directory — it may be a phantom reference to a file that was renamed or never created.

### Legacy docs with no roadmap counterpart

Every phase-numbered legacy doc has a corresponding roadmap phase in `docs/roadmap/README.md`. There are no orphaned legacy docs.

The converse gap is significant: **Phases 35–43 and 54a, 55c, 57a–57e, 58–62** have roadmap docs but no top-level legacy docs. This is expected: the legacy doc convention was retired after Phase 34, and later phases use the roadmap template directly. No action required, but the `docs/README.md` documentation index table only lists docs through Phase 7 — it is heavily incomplete.
