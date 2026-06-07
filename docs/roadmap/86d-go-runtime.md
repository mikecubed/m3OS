# Phase 86d - Go-Runtime Gate

**Status:** Planned
**Source Ref:** phase-86d
**Depends on:** Phase 86a (Outbound Foundation — `getrandom` CSPRNG + `AT_RANDOM`), Phase 37 (I/O Multiplexing) ✅, Phase 40 (Threading) ✅, Phase 36 (Expanded Memory) ✅, Phase 45 (Ports System) ✅, Phase 85 (Cross-Compiled Toolchains) ✅
**Builds on:** Sub-phase **86d** of the [Phase 86 umbrella](./86-networking-and-github.md); it consumes 86a's CSPRNG/`AT_RANDOM` foundation and clears three concrete kernel gaps so a static (`CGO_ENABLED=0`) Go binary runs, then ships `ports/lang/go` the same way Phase 85 shipped CPython/Clang. It does **not** depend on 86c (HTTPS/TLS) for the plaintext gate.
**Primary Components:** `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_linux_mmap`, `sys_mprotect`, `sys_epoll_ctl`, `sys_epoll_wait`, `fd_poll_events`, signal/`tgkill`/`sched_yield`/`madvise` dispatch), `kernel/src/process/mod.rs` (signal table — `SIGURG`), `kernel/src/arch/x86_64/interrupts.rs` (timer ISR / `check_pending_signals` decision), `ports/lang/go/Portfile` + `xtask/src/port_build.rs` (`build_go`), `xtask/src/main.rs` (`cmd_go_runtime_smoke`, `BUNDLE_ONLY_PORTS`), `kernel/Cargo.toml`

## Milestone Goal

A static Go binary runs end-to-end inside m3OS: it starts the Go runtime, schedules a goroutine onto a second OS thread that completes a channel rendezvous, and performs a **plaintext HTTP GET** over the in-kernel TCP stack. The capability is delivered by clearing two **hard** kernel blockers (`mmap` `MAP_FIXED` + `PROT_NONE` arena reservations; edge-triggered `EPOLLET` + `EPOLLRDHUP`) and one **soft** blocker (`SIGURG`-based async preemption), then packaging `ports/lang/go` as a `.m3pkg`. The kernel bumps to **0.86.3**. HTTPS-over-Go is deliberately deferred so this gate validates the *runtime* without waiting on 86c.

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

`epoll` must support edge-triggered interest and half-close notification. Go's netpoll (`runtime/netpoll_epoll.go`) registers each fd with `EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET`. m3OS's `sys_epoll_ctl` (`mod.rs:19475`) ignores `EPOLLET`/`EPOLLONESHOT`, and `fd_poll_events` (`mod.rs:18548`) reports level-triggered readiness only (`IN`/`OUT`/`ERR`/`HUP`). Edge state must be **per-interest** — the same fd can be registered in multiple epoll instances, so the "last reported readiness" must be tracked per epoll-entry, not on the fd — and `EPOLLRDHUP` must fire on peer half-close. The 12-byte `epoll_event` wire layout must stay unchanged, and the existing level-triggered consumers (`async-rt`, `sshd`) must be unaffected.

### Signals / `SIGURG` / `tgkill` / `sched_yield` / `madvise` (soft blocker)

Add the syscalls and signal Go uses for preemption: `SIGURG` (signal 23), `tgkill` (syscall 234, = `tkill`-by-tid), `sched_yield` (24), and `madvise` (28, a safe no-op). Go's sysmon and GC use `tgkill(tid, SIGURG)` → `doSigPreempt`, which rewrites the target's PC to `asyncPreempt`, to preempt compute-bound goroutines and to stop the world for GC. m3OS delivers signals only at syscall-return (`check_pending_signals`, `mod.rs:2330`); the timer ISR (`timer_handler_user`, `interrupts.rs:1587`) never calls it. Adding signal delivery to the timer-IRQ-return path would build a signal frame in a context that today only ever builds one at syscall-return — a destabilization risk that this sub-phase must explicitly decide. The two viable paths are (a) an **IRQ-return delivery path** (true async preemption of compute-bound goroutines) or (b) **`asyncpreemptoff`** in the Go runtime + `tgkill`-for-STW-only (simpler, but compute-bound goroutines won't preempt). The decision is captured as a written note in the design/task docs. `sys_rt_sigaction` (`mod.rs:3314`) must accept `SIGURG` even though it currently ignores signals 32–63; the futex single-thread fast-path (`mod.rs:15305`, inside `sys_futex` at `mod.rs:15256`) must be confirmed not to misfire if Go futex-sleeps before its first `newosproc` thread exists.

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
2. Add per-interest `EPOLLET` edge state in `sys_epoll_ctl`/`sys_epoll_wait` + `EPOLLRDHUP` half-close in `fd_poll_events`, preserving the 12-byte layout and level-triggered consumers.
3. Add `SIGURG`(23)/`tgkill`(234)/`sched_yield`(24)/`madvise`(28); write the preempt-delivery decision (IRQ-path vs `asyncpreemptoff`); confirm the futex single-thread fast-path bootstrap hazard.
4. Add `ports/lang/go/Portfile` + `build_go` (1.24+, `GOTOOLCHAIN=local`, `CGO_ENABLED=0`, static), seal into `.m3pkg`, bundle, `pkg install go`.
5. Add `cmd_go_runtime_smoke` (goroutine rendezvous + plaintext HTTP GET) + the `M3OS_GO_REGRESSION` gate.
6. Bump `kernel/Cargo.toml` `0.86.2` → `0.86.3`.

## Acceptance Criteria

- An `mmap` `PROT_NONE` `MAP_ANON` records a VMA with no committed frames; a subsequent `MAP_FIXED` `PROT_RW` at the same address returns **exactly** that address and commits in place without corrupting neighbor VMAs; `~0xc000000000` is placeable within `USER_SPACE_END`; `PROT_NONE` guard pages still `SIGSEGV` on access.
- An `EPOLLET` interest reports only on a not-ready→ready transition (per-interest edge state): a still-readable fd is **not** re-reported until it is drained then refilled; `EPOLLRDHUP` fires on peer half-close; the 12-byte `epoll_event` layout is unchanged; level-triggered behavior is preserved for `async-rt`/`sshd`.
- `SIGURG`(23) is deliverable; `tgkill`(234), `sched_yield`(24), and `madvise`(28, no-op) all dispatch without `ENOSYS`; a written decision selects IRQ-path vs `asyncpreemptoff`+`tgkill`-for-STW, and the chosen path's behavior/limitation is documented; the futex single-thread fast-path is confirmed not to misfire on a pre-`newosproc` futex sleep.
- A static Go binary runs end-to-end: a goroutine scheduled on a second OS thread completes a channel rendezvous, and a plaintext HTTP GET over the in-kernel TCP stack succeeds — with **no** 86c dependency.
- `go` installs via `pkg install go` from a bundled `.m3pkg`; `os.Executable` resolves via `/proc/self/exe`; `GOMAXPROCS` derives from `sched_getaffinity`; the `cmd_go_runtime_smoke` gate is wired as `M3OS_GO_REGRESSION=1` with a long `--timeout`.
- `kernel/Cargo.toml` reads `0.86.3`; `cargo xtask check` is clean; the boot banner reports `0.86.3`.

## Companion Task List

- [Phase 86d Task List](./tasks/86d-go-runtime-tasks.md)

## How Real OS Implementations Differ

- **Go arena management** (`runtime/mem_linux.go`): `sysReserveOS` reserves with `PROT_NONE` `MAP_ANON`, `sysMapOS` commits `PROT_RW` `MAP_FIXED` at the same address, 64 MB at a time near `~0xc000000000`; throwing if the address moves is the contract m3OS must satisfy.
- **Go netpoll** (`runtime/netpoll_epoll.go`): registers `EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET` with the 12-byte `epoll_event` layout; Linux gives Go a real edge-triggered poller with half-close.
- **Go preemption** (sysmon + GC stop-the-world): `tgkill(SIGURG)` → `doSigPreempt` rewrites the target PC to `asyncPreempt`; Linux delivers `SIGURG` asynchronously from the kernel, which m3OS can approximate via an IRQ-return path or sidestep with `asyncpreemptoff`.
- **Go random bootstrap** (`runtime/rand.go`): `randinit` XORs `AT_RANDOM` then calls `getrandom` with `GRND_NONBLOCK`; Linux supplies a full CSPRNG, which m3OS only got in 86a.
- **Static Go** on a full Linux is uncommon (most distros ship dynamic `cgo`-enabled Go); m3OS pins `GOTOOLCHAIN=local` + `CGO_ENABLED=0` + `-trimpath -ldflags=-s -w` because the custom `ld-musl` has no real `libc.so`.

## Deferred Until Later

- **HTTPS-over-Go** — Go's own `crypto/tls` needs 86c's CA bundle; deferred until after 86c and exercised in 86e (`gh`).
- **True async preemption of compute-bound goroutines**, if the `asyncpreemptoff` path is chosen instead of the IRQ-return delivery path.
- **Non-blocking `connect`** semantics — Go expects a non-blocking dial, but m3OS's `sys_connect` is a 3 s synchronous cap; reconciling the two is out of scope for the plaintext gate.
- **Self-hosting the Go toolchain** inside m3OS (building Go *on* m3OS) — Phase 86 umbrella deferral.
- The `gh` binary itself — Phase 86e.
