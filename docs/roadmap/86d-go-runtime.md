# Phase 86d - Go-Runtime Gate

**Status:** Done ✅ — `go-runtime-smoke` PASSES (GO_HELLO_OK + GO_GOROUTINE_OK + GO_HTTP_OK); kernel at `0.86.3`. See "As-built notes" below for the kernel capabilities the real Go 1.24 binary required beyond the originally-scoped substrate.
**Source Ref:** phase-86d
**Depends on:** Phase 86a (Outbound Foundation — `getrandom` CSPRNG + `AT_RANDOM`), Phase 37 (I/O Multiplexing) ✅, Phase 40 (Threading) ✅, Phase 36 (Expanded Memory) ✅, Phase 45 (Ports System) ✅, Phase 85 (Cross-Compiled Toolchains) ✅
**Builds on:** Sub-phase **86d** of the [Phase 86 umbrella](./86-networking-and-github.md); it consumes 86a's CSPRNG/`AT_RANDOM` foundation and clears three concrete kernel gaps so a static (`CGO_ENABLED=0`) Go binary runs, then ships `ports/lang/go` the same way Phase 85 shipped CPython/Clang. It does **not** depend on 86c (HTTPS/TLS) for the plaintext gate.
**Primary Components:** `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_linux_mmap`, `sys_mprotect`, `sys_epoll_ctl`, `sys_epoll_wait`, `fd_poll_events`, signal/`tgkill`/`sched_yield`/`madvise` dispatch), `kernel/src/process/mod.rs` (signal table — `SIGURG`), `kernel/src/arch/x86_64/interrupts.rs` (timer ISR / `check_pending_signals` decision), `ports/lang/go/Portfile` + `xtask/src/port_build.rs` (`build_go`), `xtask/src/main.rs` (`cmd_go_runtime_smoke`, `BUNDLE_ONLY_PORTS`), `kernel/Cargo.toml`

## Milestone Goal

A static Go binary runs end-to-end inside m3OS: it starts the Go runtime (whose Ms are backed by real OS threads created via `clone(CLONE_THREAD)`), runs a `LockOSThread` goroutine that completes a channel rendezvous, and performs a **plaintext HTTP GET** over the in-kernel TCP stack. The capability is delivered by clearing two **hard** kernel blockers (`mmap` `MAP_FIXED` + `PROT_NONE` arena reservations; edge-triggered `EPOLLET` + `EPOLLRDHUP`) and one **soft** blocker (`SIGURG`-based async preemption), then packaging `ports/lang/go` as a `.m3pkg`. The kernel bumps to **0.86.3**. HTTPS-over-Go is deliberately deferred so this gate validates the *runtime* without waiting on 86c.

## Why This Phase Exists

Running *any* Go program is a non-trivial kernel bring-up because the Go runtime is a managed runtime that stresses a young kernel in ways a C program never does. It reserves its heap arenas as `PROT_NONE` regions and later commits them in place at a fixed address; it drives its network poller edge-triggered; and it preempts goroutines and stops the world for GC by signalling itself with `SIGURG`. m3OS's `getrandom`/`AT_RANDOM` were only made trustworthy in 86a (Go's `randinit` XORs `AT_RANDOM`, then calls `getrandom` with `GRND_NONBLOCK`), but three kernel mechanisms are still missing or wrong:

- `sys_linux_mmap` discards the address hint and masks `MAP_FIXED`, so Go's reserve-then-commit-same-address contract throws on the very first arena.
- `epoll` is level-triggered only — the `EPOLLET` flag is silently ignored — so Go's netpoll busy-loops or hangs, and `EPOLLRDHUP` (half-close) is absent.
- `SIGURG` is undefined, `tgkill` does not exist, and signals are delivered only at syscall-return, so Go's `doSigPreempt` preemption and GC stop-the-world have no delivery path.

`gh` (Phase 86e) is a Go binary, so clearing these blockers and proving the runtime on a small static binary is the prerequisite that de-risks bundling the much heavier `gh` artifact. Splitting the plaintext gate out from HTTPS keeps this a pure-runtime validation: Go carries its **own** `crypto/tls`, so HTTPS-over-Go rides 86c's CA bundle and lands in 86e, not here.

## Learning Goals

- How a managed runtime's allocator uses `PROT_NONE` reservations + `MAP_FIXED` in-place commit, and why overwriting/splitting a reservation VMA must interact correctly with demand-paging, CoW, and TLB-shootdown.
- The difference between level-triggered and edge-triggered `epoll`, why edge state must be **per-interest** (not per-fd, since one fd can sit in multiple epoll sets), and how `EPOLLRDHUP` reports peer half-close.
- How Go preempts goroutines and stops the world for GC via `tgkill(SIGURG)` → `doSigPreempt`, and the trade-off between an IRQ-return signal-delivery path and `asyncpreemptoff` + `tgkill`-for-STW-only.
- How the existing runtime substrate (`clone(CLONE_THREAD)`, `futex`, `arch_prctl ARCH_SET_FS`, `gettid`, `/proc/self/exe`, `clock_gettime`, `sched_getaffinity`) makes this a targeted bring-up rather than greenfield.

## Feature Scope

### `mmap` `MAP_FIXED` + `PROT_NONE` reservations (hard blocker 1)

`sys_linux_mmap` must honor `MAP_FIXED | MAP_ANONYMOUS` at the **exact** requested address and record `PROT_NONE` reservations as VMAs that map no frames. Go's allocator (`runtime/mem_linux.go`) reserves a 64 MB arena `PROT_NONE` `MAP_ANON` near `~0xc000000000` via `sysReserveOS`, then commits it `PROT_RW` `MAP_FIXED` at the **same** address via `sysMapOS`; if the returned address differs, the runtime throws. Today `sys_linux_mmap` (`kernel/src/arch/x86_64/syscall/mod.rs:9800`) drops the address hint, masks the request to `MAP_PRIVATE | MAP_ANONYMOUS`, and allocates at `ANON_MMAP_BASE` (`0x20_0000_0000`, `mod.rs:5654`), so the first commit lands at the wrong address and Go aborts. The `MAP_FIXED` overwrite/split of an existing reservation VMA must go through `with_shared_mm_mut` (`mod.rs:9839`) without corrupting neighbor VMAs, and the `PROT_NONE` guard semantics (`sys_mprotect` clears `PRESENT` and marks a guard page at `mod.rs:10407`) must continue to `SIGSEGV` on access.

### Edge-triggered `epoll` — `EPOLLET` + `EPOLLRDHUP` (hard blocker 2)

`epoll` must support edge-triggered interest and half-close notification. Go's netpoll (`runtime/netpoll_epoll.go`) registers each fd with `EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET`. m3OS's `sys_epoll_ctl` (`mod.rs:19475`) ignores `EPOLLET`/`EPOLLONESHOT`, and `fd_poll_events` (`mod.rs:18548`) reports level-triggered readiness only (`IN`/`OUT`/`ERR`/`HUP`). Edge state must be **per-interest** — the same fd can be registered in multiple epoll instances, so the "last reported readiness" must be tracked per epoll-entry, not on the fd — and `EPOLLRDHUP` must fire on peer half-close. The 12-byte `epoll_event` wire layout must stay unchanged, and the existing readiness consumers must be unaffected — the `poll()`-based ones (`async-rt`, `sshd`) and the level-triggered `epoll` path (the `epoll-smoke` gate), since `fd_poll_events` is shared across `poll`/`select`/`epoll` (so the new `POLLRDHUP` bit must surface only when the caller requested it).

### Signals / `SIGURG` / `tgkill` / `sched_yield` / `madvise` (soft blocker)

Add the syscalls and signal Go uses for preemption: `SIGURG` (signal 23), `tgkill` (syscall 234, = `tkill`-by-tid), `sched_yield` (24), and `madvise` (28, a safe no-op). Go's sysmon and GC use `tgkill(tid, SIGURG)` → `doSigPreempt`, which rewrites the target's PC to `asyncPreempt`, to preempt compute-bound goroutines and to stop the world for GC. m3OS delivers signals only at syscall-return (`check_pending_signals`, `mod.rs:2330`); the timer ISR (`timer_handler_user`, `interrupts.rs:1587`) never calls it. Adding signal delivery to the timer-IRQ-return path would build a signal frame in a context that today only ever builds one at syscall-return — a destabilization risk that this sub-phase must explicitly decide. The two viable paths are (a) an **IRQ-return delivery path** (true async preemption of compute-bound goroutines) or (b) **`asyncpreemptoff`** in the Go runtime + `tgkill`-for-STW-only (simpler, but compute-bound goroutines won't preempt). The decision is captured as a written note in the design/task docs. `sys_rt_sigaction` (`mod.rs:3314`) must accept `SIGURG` even though it currently ignores signals 32–63; the futex single-thread fast-path (`mod.rs:15305`, inside `sys_futex` at `mod.rs:15256`) must be confirmed not to misfire if Go futex-sleeps before its first `newosproc` thread exists.

#### As-built decision (86d): async preemption ENABLED, delivered at syscall-return (no IRQ-return path)

**Chosen: signal-based preemption at syscall-return, with the runtime's async preemption left ON.** The probe runs with Go's **default** `GODEBUG` — `asyncpreemptoff` is **not** set (not on the command line, not in `go.mod`, no `//go:debug` directive), so the runtime's async preemption is **enabled**. The kernel's timer-IRQ-return path (`timer_handler_user`, `interrupts.rs:1587`) is left **unchanged** — it does scheduling/preemption but never calls `check_pending_signals` — so the kernel delivers signals only at **syscall-return** (`check_pending_signals`, `mod.rs:2330`). `SIGURG`(23)/`tgkill`(234) are wired so the syscalls don't `ENOSYS` and `rt_sigaction` installs Go's real `SA_SIGINFO` handler; the dispatcher now hands that handler valid `siginfo` (RSI) and `ucontext` (RDX) pointers so `doSigPreempt` can read the interrupted RIP at `ucontext+0xa8` (= frame+176 = `OFF_MCONTEXT` 48 + `MC_RIP` 128) and rewrite the goroutine PC to `asyncPreempt` — **exactly the SA_SIGINFO/ucontext fix this sub-phase ships**, which would be dead code under `asyncpreemptoff=1` (Go only calls `doSigPreempt` when `asyncpreemptoff==0`). A `tgkill(tid, SIGURG)` aimed at a thread blocked in `futex`/`epoll_pwait`/`recv` wakes it (an installed `Handler` makes the wait interruptible), so the preempt lands **opportunistically at the next syscall boundary**, and GC stop-the-world reaches its safepoints cooperatively.

**Rationale + the one limitation.** Building a signal frame from interrupt context (the IRQ-return path) is a genuine destabilization risk: the existing signal machinery only ever constructs a frame at syscall-return, and doing it from a timer ISR would have to reconstruct the interrupted user `RIP`/`RSP`/register state from the preempt trap frame and contend with nested-interrupt/SMAP/`preempt_count` invariants. For this gate — a goroutine **channel rendezvous** + a **plaintext HTTP GET**, both of which block on the kernel frequently (futex/channel, epoll/network, `read`) — syscall-return delivery covers every preemption Go actually needs. The **only** case it does *not* cover is a pure compute-bound goroutine that runs with **no syscall between safepoints**: the kernel never reaches `check_pending_signals` for it, so its `SIGURG` is never delivered and it is not async-preempted. Closing that one gap requires the **timer-IRQ-return signal-delivery path**, which is deferred (recorded under "Deferred Until Later").

**Futex single-thread fast-path — confirmed safe.** The fast path (`sys_futex` `FUTEX_WAIT`) fires only when `thread_group.is_none()`, i.e. before Go's first `clone(CLONE_THREAD)`. In that pre-thread window Go does not perform a blocking `FUTEX_WAIT`: its futex mutexes (`runtime/lock_futex.go`) are uncontended single-threaded (the `cas` to acquire succeeds, so `futexsleep` is never reached), and notes (`notesleep`) are not slept on before the first M is spun up. The first thread Go creates is `sysmon`, very early in `runtime.main`; from that `clone` onward `thread_group` is `Some`, so every subsequent futex wait takes the real blocking path and the zero-and-return fast path never touches Go's lock words. The fast path is therefore left **unchanged** (changing it risks regressing musl's `__lock`, which depends on the zero-and-return behavior), and the Track D `go-runtime-smoke` empirically confirms Go boots without futex-lock corruption.

### Go port + split smoke + version

Add `ports/lang/go` (Go 1.24+, `GOTOOLCHAIN=local`, `CGO_ENABLED=0`) as a `build_go` port routed through the standard `BUNDLE_ONLY_PORTS` pattern (`xtask/src/main.rs:17541`), build it as a fully static binary (same class as static CPython/Clang — m3OS's custom `ld-musl` has no real `libc.so`), and gate it with a plaintext `cmd_go_runtime_smoke` that asserts the goroutine rendezvous and a plaintext HTTP GET over the in-kernel TCP stack — without 86c. Bump `kernel/Cargo.toml` to `0.86.3`.

## Important Components and How They Work

### `sys_linux_mmap` / `sys_mprotect` — arena reservation and commit

The reservation path records a VMA with no committed frames at the requested address (`PROT_NONE`, guard semantics via `sys_mprotect`'s `BIT_10` guard mark at `mod.rs:10407`). The commit path, when `MAP_FIXED` is set and the address falls within an existing reservation, must overwrite/split that VMA in place and return **exactly** the requested address — never relocate to `ANON_MMAP_BASE`. All VMA mutation flows through `with_shared_mm_mut` so it is consistent with demand-paging/CoW and TLB-shootdown. `~0xc000000000` must be placeable within `USER_SPACE_END` (`0x0000_8000_0000_0000`, `mod.rs:9837`).

### `sys_epoll_ctl` / `fd_poll_events` — edge state

`sys_epoll_ctl` stores `EPOLLET` (and tolerates `EPOLLONESHOT`) on the epoll entry. `sys_epoll_wait` (`mod.rs:19572`) consults `fd_poll_events` for current readiness and, for ET entries, compares it against the per-entry last-reported readiness, only emitting an event on a not-ready→ready transition. `EPOLLRDHUP` is derived from the half-close state of the fd. Level-triggered entries keep their existing behavior. `sys_epoll_create1` (`mod.rs:19432`) is unchanged.

### Signal table + dispatch — `SIGURG`, `tgkill`, `sched_yield`, `madvise`

`SIGURG = 23` is added to the signal constants near `SIGHUP..SIGWINCH` (`kernel/src/process/mod.rs:607`, with `SIGWINCH = 28` at `mod.rs:623`) with a default disposition (ignore) so an un-handled `SIGURG` is harmless. `tgkill` reuses the `tkill` machinery (`TKILL = 200`, dispatched at `mod.rs:1843`; `tgkill` = 234, currently missing) targeting a tid; `sched_yield` (24) yields; `madvise` (28) returns success without doing anything. The chosen preempt-delivery path (`check_pending_signals` from the IRQ return vs `asyncpreemptoff`) is wired per the written decision.

### `build_go` + `cmd_go_runtime_smoke`

`build_go` follows the AGENTS.md port rules and the static-binary class of `build_git`/`build_python`; the Go bootstrap is host-side (the Go toolchain cross-compiles itself for the m3OS target). `cmd_go_runtime_smoke` mirrors the `cmd_git_local_smoke` template (`xtask/src/main.rs:13584`): build the `.m3pkg`, bundle it, boot m3OS, `pkg install go`, then run a bundled static Go program over serial.

## How This Builds on Earlier Phases

- Consumes **86a**'s CSPRNG-backed `getrandom` (`GRND_NONBLOCK`) and `AT_RANDOM`, which Go's `randinit` (`runtime/rand.go`) relies on for its random bootstrap.
- Reuses the existing runtime substrate from earlier phases: `sys_clone` (`CLONE_THREAD` const at `mod.rs:14553`), `futex` (`mod.rs:15256`), `sys_linux_set_tid_address` (`mod.rs:14488`) + `do_clear_child_tid` (`mod.rs:2581`), `arch_prctl ARCH_SET_FS` (`mod.rs:14196`), `gettid` (`mod.rs:2558`), `clock_gettime` (`mod.rs:15199`), `sched_getaffinity` (`mod.rs:1864`, for `GOMAXPROCS`), and `/proc/self/exe` (`kernel/src/fs/procfs.rs:88`, for `os.Executable`).
- Reuses the Phase 85 `.m3pkg` substrate + offline installer and the `BUNDLE_ONLY_PORTS` bundling path, exactly as `git`/`python` did.
- Reuses the Phase 77 outbound TCP `connect` (`sys_connect` → `tcp::connect`) for the plaintext HTTP GET.

## Implementation Outline

1. Honor `MAP_FIXED | MAP_ANON` exact-address commit and `PROT_NONE` reservations in `sys_linux_mmap`; ensure in-place overwrite/split through `with_shared_mm_mut` preserves neighbor VMAs and guard semantics.
2. Add per-interest `EPOLLET` edge state in `sys_epoll_ctl`/`sys_epoll_wait` + `EPOLLRDHUP` half-close in `fd_poll_events`, preserving the 12-byte layout, the level-triggered `epoll` path, and the shared `poll()`/`select()` readiness consumers (`async-rt`, `sshd`).
3. Add `SIGURG`(23)/`tgkill`(234)/`sched_yield`(24)/`madvise`(28); write the preempt-delivery decision (IRQ-path vs `asyncpreemptoff`); confirm the futex single-thread fast-path bootstrap hazard.
4. Add `ports/lang/go/Portfile` + `build_go` (1.24+, `GOTOOLCHAIN=local`, `CGO_ENABLED=0`, static), seal into `.m3pkg`, bundle, `pkg install go`.
5. Add `cmd_go_runtime_smoke` (goroutine rendezvous + plaintext HTTP GET) + the `M3OS_GO_REGRESSION` gate.
6. Bump `kernel/Cargo.toml` `0.86.2` → `0.86.3`.

## Acceptance Criteria

- An `mmap` `PROT_NONE` `MAP_ANON` records a VMA with no committed frames; a subsequent `MAP_FIXED` `PROT_RW` at the same address returns **exactly** that address and commits in place without corrupting neighbor VMAs; `~0xc000000000` is placeable within `USER_SPACE_END`; `PROT_NONE` guard pages still `SIGSEGV` on access.
- An `EPOLLET` interest reports only on a not-ready→ready transition (per-interest edge state): a still-readable fd is **not** re-reported until it is drained then refilled; `EPOLLRDHUP` fires on peer half-close; the 12-byte `epoll_event` layout is unchanged; readiness is preserved for the `poll()`-based consumers (`async-rt`, `sshd`) and the level-triggered `epoll` path (the new `POLLRDHUP` surfaces only when requested).
- `SIGURG`(23) is deliverable; `tgkill`(234), `sched_yield`(24), and `madvise`(28, no-op) all dispatch without `ENOSYS`; a written decision records the chosen preempt-delivery path (the as-built: async preemption left **enabled**, `SIGURG` delivered at **syscall-return**, the timer-IRQ-return path deferred) and its one limitation (a syscall-free compute-bound goroutine won't async-preempt); the futex single-thread fast-path is confirmed not to misfire on a pre-`newosproc` futex sleep.
- A static Go binary runs end-to-end: a `LockOSThread` goroutine completes a channel rendezvous (on a runtime whose Ms are backed by `clone(CLONE_THREAD)` OS threads), and a plaintext HTTP GET over the in-kernel TCP stack succeeds — with **no** 86c dependency.
- `go` installs via `pkg install go` from a bundled `.m3pkg`; `os.Executable` resolves via `/proc/self/exe`; `GOMAXPROCS` derives from `sched_getaffinity`; the `cmd_go_runtime_smoke` gate is wired as `M3OS_GO_REGRESSION=1` with a long `--timeout`.
- `kernel/Cargo.toml` reads `0.86.3`; `cargo xtask check` is clean; the boot banner reports `0.86.3`.

## Companion Task List

- [Phase 86d Task List](./tasks/86d-go-runtime-tasks.md)

## How Real OS Implementations Differ

- **Go arena management** (`runtime/mem_linux.go`): `sysReserveOS` reserves with `PROT_NONE` `MAP_ANON`, `sysMapOS` commits `PROT_RW` `MAP_FIXED` at the same address, 64 MB at a time near `~0xc000000000`; throwing if the address moves is the contract m3OS must satisfy.
- **Go netpoll** (`runtime/netpoll_epoll.go`): registers `EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET` with the 12-byte `epoll_event` layout; Linux gives Go a real edge-triggered poller with half-close.
- **Go preemption** (sysmon + GC stop-the-world): `tgkill(SIGURG)` → `doSigPreempt` rewrites the target PC to `asyncPreempt`; Linux delivers `SIGURG` asynchronously from the kernel, whereas m3OS delivers it at **syscall-return** (opportunistic — it wakes a thread blocked in a syscall and preempts at that boundary). The true-async IRQ-return path that would also preempt a syscall-free compute loop is deferred.
- **Go random bootstrap** (`runtime/rand.go`): `randinit` XORs `AT_RANDOM` then calls `getrandom` with `GRND_NONBLOCK`; Linux supplies a full CSPRNG, which m3OS only got in 86a.
- **Static Go** on a full Linux is uncommon (most distros ship dynamic `cgo`-enabled Go); m3OS pins `GOTOOLCHAIN=local` + `CGO_ENABLED=0` + `-trimpath -ldflags=-s -w` because the custom `ld-musl` has no real `libc.so`.

## As-built notes (bring-up beyond the originally-scoped substrate)

Running a real static **Go 1.24.6** binary surfaced three kernel capabilities the plan's "runtime substrate already exists" list did **not** anticipate; all three were added and are exercised by the passing gate:

1. **`epoll_pwait`(281)** — Go's netpoll (`runtime/netpoll_epoll.go`) issues `epoll_pwait`, not `epoll_wait`. Dispatched to `sys_epoll_wait` (Go passes a nil sigmask).
2. **SA_SIGINFO `ucontext`** — the signal dispatcher (`enter_signal_handler`) now passes the System V handler args `rsi`=`&siginfo` and `rdx`=`&ucontext`. Go's `SIGURG` handler (`doSigPreempt`) reads `ucontext->uc_mcontext.gregs[REG_RIP]` at `ucontext+0xa8`; without a valid `rdx` it faulted at `0xa8`. `restore_sigframe` already restores a handler's RIP rewrite, so Go's async preemption works at syscall-return delivery points. (This fix is needed **precisely because async preemption is enabled** — Go reaches `doSigPreempt` only when `asyncpreemptoff==0`; it would be unreachable under `asyncpreemptoff=1`. See the "As-built decision (86d)" above.)
3. **`eventfd2`(290)** — Go 1.21+'s cross-thread M-wakeup primitive (`netpollBreak`); new `crate::eventfd` object (8-byte counter + `WaitQueue`; counter state machine extracted to host-tested `kernel_core::eventfd`), wired through the full fd surface (read/write/poll/epoll-wake) and **every** lifecycle path that the sibling refcounted backends use: `dup`/`dup2`/`fcntl(F_DUPFD)`, `fork` (`add_fd_refs`), `execve` close-on-exec, process-exit teardown, `SCM_RIGHTS` pass, `fstat`/`fstatfs`/`lseek`/`ftruncate`, and procfs `fd_target`.

**Run configuration.** `go-runtime-smoke` runs the guest **single-core** (`-smp 1`): Go is still exercised across multiple OS threads (the runtime spins up `sysmon` and other Ms via `clone(CLONE_THREAD)`), but a single core avoids cross-core SMP futex/IPC races that otherwise made the heavy Go-load + slow-VFS pipeline intermittently deadlock — a tracked multi-core robustness follow-up, independent of the runtime correctness this gate proves. The capability proven needs real OS *threads* (clones) — which the runtime creates — not a second core. The host HTTP server is reached via a SLIRP **`guestfwd`** rule (`10.0.2.100:80` → host loopback).

## Deferred Until Later

- **HTTPS-over-Go** — Go's own `crypto/tls` needs 86c's CA bundle; deferred until after 86c and exercised in 86e (`gh`).
- **Multi-core (`-smp > 1`) Go runs** — the single-core gate proves runtime correctness; eliminating the cross-core SMP futex/IPC deadlock observed under heavy Go load is a tracked scheduler-robustness follow-up.
- **True async preemption of compute-bound goroutines** — async preemption is *enabled*, but `SIGURG` is delivered only at **syscall-return**, not via an IRQ-return path, so a tight compute loop that issues no syscall between safepoints won't async-preempt (it has no syscall boundary at which the pending `SIGURG` can be delivered). Adding the timer-IRQ-return signal-frame path would close this.
- **Self-hosting the Go toolchain** inside m3OS (building Go *on* m3OS) — Phase 86 umbrella deferral.
- The `gh` binary itself — Phase 86e.
