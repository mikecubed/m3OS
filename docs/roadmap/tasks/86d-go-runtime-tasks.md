# Phase 86d — Go-Runtime Gate: Task List

**Status:** In Progress
**Source Ref:** phase-86d
**Depends on:** Phase 86a (Outbound Foundation — `getrandom` CSPRNG + `AT_RANDOM`), Phase 37 (I/O Multiplexing) ✅, Phase 40 (Threading) ✅, Phase 36 (Expanded Memory) ✅, Phase 45 (Ports System) ✅, Phase 85 (Cross-Compiled Toolchains) ✅
**Goal:** Clear the three kernel blockers that stop a static (`CGO_ENABLED=0`) Go binary from running — `mmap` `MAP_FIXED` + `PROT_NONE` arena reservations (hard), edge-triggered `EPOLLET` + `EPOLLRDHUP` (hard), and `SIGURG`-based async preemption via `tgkill` (soft) — then ship `ports/lang/go` as a `.m3pkg` and prove the runtime with a goroutine rendezvous + plaintext HTTP GET over the in-kernel TCP stack, all without 86c. Bump the kernel to `0.86.3`.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirrors the [Phase 92 task-list style](./92-vfs-bulk-io-tasks.md).)

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `mmap` `MAP_FIXED` exact-address commit + `PROT_NONE` reservations (hard blocker 1) | 86a | In Progress |
| B | Edge-triggered `epoll` — `EPOLLET` per-interest edge state + `EPOLLRDHUP` (hard blocker 2) | — | In Progress |
| C | Signals — `SIGURG`/`tgkill`/`sched_yield`/`madvise` + preempt-delivery decision (soft blocker) | A, B | Planned |
| D | `ports/lang/go` + split plaintext smoke gate + version bump | A, B, C | Planned |

> **Execution note (parallel-impl).** Tracks A and B are independent (disjoint regions of `mod.rs`, ~10k lines apart) and run as parallel implementer tracks (concurrency cap 2). Track C depends on A+B and Track D on all, so they run serially after. Integration, all `cargo xtask check`/QEMU validation, and the Go smoke gate are owned by the coordinator. Branch: `feat/86d-go-runtime`.

---

## Track A — `mmap` `MAP_FIXED` (hard blocker 1)

### A.1 — Honor `MAP_FIXED | MAP_ANON` exact address + `PROT_NONE` reservations

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_linux_mmap` (`mod.rs:9800`; reservation/commit VMA mutation via `with_shared_mm_mut` at `mod.rs:9839`; `ANON_MMAP_BASE` `mod.rs:5654`; `USER_SPACE_END` `mod.rs:9837`; `sys_mprotect` `PROT_NONE`-clears-`PRESENT` guard mark at `mod.rs:10407`)
**Why it matters:** Go's allocator (`runtime/mem_linux.go`) reserves an arena `PROT_NONE` `MAP_ANON` then commits it `PROT_RW` `MAP_FIXED` at the **same** address, throwing if the returned address differs; today `sys_linux_mmap` discards the address hint and masks `MAP_FIXED`, so the first arena commit lands at `ANON_MMAP_BASE` and Go aborts. The `MAP_FIXED` overwrite/split of the reservation VMA must interact correctly with demand-paging/CoW/TLB-shootdown — a bug corrupts **other** VMAs.

**Acceptance:**
- [ ] An `mmap` `PROT_NONE` `MAP_ANON` records a VMA mapping **no** committed frames at the requested address.
- [ ] A subsequent `MAP_FIXED` `PROT_RW` `MAP_ANON` at the **same** address returns **exactly** that address (not relocated to `ANON_MMAP_BASE`) and commits in place, overwriting/splitting the reservation VMA via `with_shared_mm_mut` without corrupting neighbor VMAs.
- [ ] An address near `~0xc000000000` is placeable within `USER_SPACE_END` (`0x0000_8000_0000_0000`).
- [ ] `PROT_NONE` guard pages still `SIGSEGV` on access (the guard mark at `mod.rs:10407` is preserved through the reserve→commit→`mprotect` sequence).
- [ ] A `kernel-core`/QEMU regression exercises reserve-then-commit-same-address and asserts a neighboring VMA's bytes are unchanged after the in-place commit.

---

## Track B — Edge-triggered `epoll` (hard blocker 2)

### B.1 — Add `EPOLLET` per-interest edge state + `EPOLLRDHUP`

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_epoll_ctl` (`mod.rs:19475`, ignores `EPOLLET`/`EPOLLONESHOT` today), `sys_epoll_wait` (`mod.rs:19572`), `fd_poll_events` (`mod.rs:18548`, level-triggered `IN`/`OUT`/`ERR`/`HUP`); `sys_epoll_create1` (`mod.rs:19432`) unchanged
**Why it matters:** Go's netpoll (`runtime/netpoll_epoll.go`) registers each fd `EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET`; m3OS is level-triggered only, so Go busy-loops or hangs. Edge state must be **per-interest** (not per-fd — the same fd can be registered in multiple epoll instances) plus `EPOLLRDHUP` half-close.

**Acceptance:**
- [ ] `sys_epoll_ctl` stores `EPOLLET` (and tolerates `EPOLLONESHOT`) on the epoll **entry**, tracking last-reported readiness per entry, not on the fd.
- [ ] An `EPOLLET` interest reports an event **only** on a not-ready→ready transition: a still-readable fd is **not** re-reported on a subsequent `epoll_wait` until it is drained then refilled.
- [ ] `EPOLLRDHUP` fires on peer half-close (derived from the fd's half-close state).
- [ ] The 12-byte `epoll_event` wire layout is unchanged.
- [ ] Level-triggered behavior is preserved for the existing `async-rt` and `sshd` consumers (no regression in their smoke gates).

---

## Track C — Signals / `SIGURG` / `tgkill` / `sched_yield` (soft blocker)

### C.1 — Add `SIGURG`/`tgkill`/`sched_yield`/`madvise` and decide the preempt-delivery path

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (signal/syscall dispatch — `TKILL = 200` (const at `mod.rs:1359`, dispatched at `mod.rs:1843`), `tgkill`(234) missing; `sys_rt_sigaction` `mod.rs:3314` ignores 32–63; `sched_getaffinity` `mod.rs:1864`; `check_pending_signals` `mod.rs:2330`; `sys_futex` `mod.rs:15256` single-thread fast-path `mod.rs:15305`)
- `kernel/src/process/mod.rs` (signal constants — `SIGHUP` `mod.rs:607` .. `SIGWINCH = 28` `mod.rs:623`; `SIGURG = 23` missing)
- `kernel/src/arch/x86_64/interrupts.rs` (timer ISR `timer_handler_user` `interrupts.rs:1587` never calls `check_pending_signals`)

**Symbol:** `SIGURG`, `tgkill`, `sched_yield`, `madvise`, `check_pending_signals`
**Why it matters:** Go uses `tgkill(tid, SIGURG)` + `doSigPreempt` for goroutine preemption and GC stop-the-world; m3OS delivers signals only at syscall-return. Adding `check_pending_signals` to the timer-IRQ-return path would build a signal frame in a context that today only ever builds one at syscall-return — a destabilization risk that must be decided deliberately, not stumbled into.

**Acceptance:**
- [ ] `SIGURG`(23) is added to the signal constants (default disposition ignore) and is deliverable; `sys_rt_sigaction` accepts it.
- [ ] `tgkill`(234) dispatches as `tkill`-by-tid (reusing the `TKILL` machinery), `sched_yield`(24) yields, and `madvise`(28) returns success as a no-op — none returns `ENOSYS`.
- [ ] A written decision in the docs picks **IRQ-return delivery path** vs **`asyncpreemptoff` + `tgkill`-for-STW-only**; the rationale (destabilization risk vs preemption coverage) is recorded.
- [ ] If the IRQ-path is chosen: a smoke confirms a compute-bound goroutine is preempted (e.g. a tight loop yields to another goroutine within a bounded interval). If `asyncpreemptoff` is chosen: the limitation is documented (compute-bound goroutines won't async-preempt; GC stop-the-world still works via `tgkill`).
- [ ] The futex single-thread fast-path (`mod.rs:15305`) is confirmed **not** to misfire if Go futex-sleeps before its first `newosproc` thread is created (no spurious zero-and-return that would corrupt Go's runtime locks).

---

## Track D — Go port + split smoke + version

### D.1 — `ports/lang/go` (1.24+, `GOTOOLCHAIN=local`, `CGO_ENABLED=0`) + plaintext smoke

**Files:**
- `ports/lang/go/Portfile` (new)
- `xtask/src/port_build.rs` (new `build_go`, registered in `PORTS` + the `port_build` `match name` dispatch)
- `xtask/src/main.rs` (`cmd_go_runtime_smoke` modeled on `cmd_git_local_smoke` `main.rs:13584`; bundle via `BUNDLE_ONLY_PORTS` `main.rs:17541`)
- `AGENTS.md` + `.githooks/pre-push` (opt-in `M3OS_GO_REGRESSION=1` gate row)

**Symbol:** `build_go`, `cmd_go_runtime_smoke`
**Why it matters:** static Go is the same class as static CPython/Clang (no `libc.so`); splitting the gate lets the plaintext smoke validate the **runtime** without waiting on 86c (Go carries its own `crypto/tls`).

**Acceptance:**
- [ ] `ports/lang/go/Portfile` pins Go 1.24+ (SHA-256), and `build_go` produces a fully **static** Go (`GOTOOLCHAIN=local`, `CGO_ENABLED=0`, `-trimpath -ldflags=-s -w`), sealed into a `target/pkgcache/<key>.m3pkg` (a warm second build is a pkgcache hit, zero compiler invocations).
- [ ] The `go` `.m3pkg` is bundled on the data disk via `BUNDLE_ONLY_PORTS` and `pkg install go` lays it into `/usr`.
- [ ] Inside m3OS: a static Go program prints `GO_HELLO_OK`, then a goroutine scheduled on a second OS thread (via `clone(CLONE_THREAD)`) completes a channel rendezvous printing `GO_GOROUTINE_OK`.
- [ ] A plaintext HTTP GET over the in-kernel TCP stack (Phase 77 `sys_connect` → `tcp::connect`) succeeds, printing `GO_HTTP_OK` — with **no** 86c/TLS dependency.
- [ ] `os.Executable` resolves via `/proc/self/exe` (`procfs.rs:88`); `GOMAXPROCS` derives from `sched_getaffinity` (`mod.rs:1864`).
- [ ] The gate is wired as `cargo xtask go-runtime-smoke` and as an opt-in pre-push regression (`M3OS_GO_REGRESSION=1`) in both `AGENTS.md` and `.githooks/pre-push`, with a long `--timeout` (clang-gate class — the cold install + slow ring-3 VFS take many minutes).
- [ ] HTTPS-over-Go is **not** exercised here; the doc records it as deferred until after 86c (rides 86e).

### D.2 — Bump kernel crate `0.86.2` → `0.86.3`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.86.3"` (currently `0.85.3` at `kernel/Cargo.toml:3`)
**Why it matters:** the 86d cut is the fourth Phase 86 sub-phase (`0.86.0` → `0.86.5`); the version bump is how the sub-phase's landing is recorded in the boot banner and `uname`.

**Acceptance:**
- [ ] `kernel/Cargo.toml` line 3 reads `version = "0.86.3"` (+ `Cargo.lock` updated).
- [ ] `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + host tests + retpoline gate).
- [ ] The boot banner / `uname` reports `0.86.3`.

---

## Documentation Notes

- **Split-gate rationale.** This sub-phase deliberately validates the Go **runtime** on plaintext only; HTTPS-over-Go rides 86c's CA bundle and is exercised in [86e](./86e-github-cli-tasks.md), not here. Keep the design doc's "Deferred Until Later" aligned.
- **Hazards to call out in the as-built notes.** (1) The `MAP_FIXED` VMA-overwrite/split must not corrupt neighbor VMAs (Track A) — this is the GC-arena hazard. (2) The `SIGURG` IRQ-path destabilization risk (Track C) — record which path was chosen and why. (3) The futex single-thread fast-path bootstrap hazard (Track C). (4) `EPOLLET` edge state must be **per-interest**, not per-fd (Track B).
- **Most of the runtime substrate already exists** — `sys_clone` (`CLONE_THREAD` const at `mod.rs:14553`), `futex` (`mod.rs:15256`), `sys_linux_set_tid_address` (`mod.rs:14488`) + `do_clear_child_tid` (`mod.rs:2581`), `arch_prctl ARCH_SET_FS` (`mod.rs:14196`), `gettid` (`mod.rs:2558`), `clock_gettime` (`mod.rs:15199`), `sched_getaffinity` (`mod.rs:1864`), `/proc/self/exe` (`procfs.rs:88`) — so this is targeted bring-up, not greenfield. Reference these exact symbols, not "the threading code".
- **Prefer exact targets.** Reference `sys_linux_mmap`, `fd_poll_events`, `sys_epoll_ctl`, `check_pending_signals`, and the `SIGURG`/`tgkill` constants explicitly, not "the mmap/epoll/signal paths".
- **Cross-links.** This list is companion to [86d-go-runtime.md](../86d-go-runtime.md) and a child of the [Phase 86 umbrella](../86-networking-and-github.md); the `gh` consumer is [86e](./86e-github-cli-tasks.md).
