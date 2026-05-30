# Pre-1.0 Correctness, Cheap Security, and Network Polish (Phase 77)

**Aligned Roadmap Phase:** Phase 77
**Status:** Complete
**Source Ref:** phase-77
**Supersedes Legacy Doc:** new

## Overview

Phase 77 is a *release-gate cleanup* phase: a bundle of small, well-scoped fixes
the Phase 74a pre-1.0 audit promoted into "must-fix before 1.0." None of them is
a headline feature; together they close the gap between "boots and demos" and
"behaves correctly under the conditions a real user hits."

The lesson that ties the bundle together is **where bugs are cheapest to find**.
A release-gate phase is the *worst* place to discover that the SSH session hangs
on disconnect, or that a multi-threaded program deadlocks, or that the first
dropped TCP packet wedges a connection forever — those should have been caught
when the subsystem shipped. Phase 77 is the cost of having deferred them. It
also draws a useful line between two kinds of mitigation:

- **Cheap CR4 flips** — SMEP and SMAP are each a single control-register bit.
  Turning them on eliminates whole exploit classes (ring-0 executing or reading
  user pages) for almost no code and no measurable runtime cost. There is no
  reason not to have them on.
- **Expensive page-table reshapes** — KPTI (kernel page-table isolation, the
  Meltdown mitigation) requires maintaining *two* page tables per process and
  switching CR3 on every kernel entry/exit. That is a structural change with a
  real performance cost, and it is explicitly **not** in this phase (see
  "How This Phase Differs", below).

The other recurring lesson runs the opposite way: **one symptom can hide more
than one root cause, and a tidy single-cause story is seductive enough to stick
even after the evidence retires it.** The multi-threaded `pthread_join` hang and
the intermittent smoke-test flakiness *were* the same lost futex wakeup (Track
C). The SSH-disconnect freeze *looked* like the same bug — `sshd` reaps its
session via pthreads, so the lost-wake theory fit — and earlier handoffs were
flipped to RESOLVED on that basis. A deeper investigation (the 2026-05-29
handoff) overturned it: the disconnect freeze was a **separate userspace
async-runtime stall** — a transient missed I/O-readiness edge in the `async-rt`
executor — fixed by an idle-liveness backstop, *not* by the futex change. Two
different bugs wore one face.

## What This Doc Covers

Nine tracks, each a self-contained fix:

| Track | Fix |
|---|---|
| A | SSH-disconnect hang (an `async-rt` missed-edge stall) + a scheduler `on_cpu` cross-core deadlock + `sys_nanosleep` starvation |
| B | Enable + enforce SMEP and SMAP on every CPU |
| C | `PT_TLS` parsing → working multi-threaded thread-local storage |
| D | DNS resolver wiring + RFC 6298 TCP retransmission + 64-connection lift |
| E | Microcode loading (AMD container parse + per-CPU apply) |
| F | Verify `epoll_*` (already implemented) + a regression gate |
| G | Resolve 5 open handoffs + de-drift the Phase 50-56 deferral lists |
| H | `/proc` compatibility so `htop` / `ps` show real processes |
| I | Documentation + release: this learning doc + the `0.77.0` version bump |

## Why a release-gate phase is the wrong place to find these (60-second version)

Each of these bugs is invisible in the happy path:

- SSH works fine until you *disconnect* — then a missed PTY-hangup wake edge
  leaves the session's async relay parked forever.
- pthreads work in a one-thread toy until a *second* thread contends the
  thread-list lock (the lost futex wakeup).
- TCP is flawless on QEMU's lossless SLIRP LAN until the *first dropped packet*
  on the real internet.
- htop shows your own processes until an *unprivileged* user can't see the
  root-owned daemons.

The fix in each case is small. The cost is that they surfaced at the release
gate instead of in the phase that built the subsystem.

## Key Files

| File | What changed |
|---|---|
| `userspace/async-rt/src/executor.rs` | **The load-bearing SSH-disconnect fix:** `requeue_all` idle-liveness backstop — after several consecutive empty reactor polls, re-poll every parked task so a single missed I/O-readiness edge (a PTY-master `POLLHUP` that failed to wake the sshd relay) recovers instead of hanging forever |
| `sunset-local/src/{runner,channel}.rs` | clean logout: `server_session_exit` sends an exit-status/EOF/CLOSE so the client sees `exit 0`, not "closed by remote host" |
| `userspace/sshd/src/session.rs` | EOF-driven `cleanup` ordering (close PTY master first → shell EOF-exits → bounded reap) |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `do_clear_child_tid` wakes both private + non-private futex keys (the pthread/TLS lost-wake fix); futex-waiter dequeue on `FUTEX_WAIT` return; dead `sys_nanosleep` v1 branch removed |
| `kernel/src/process/mod.rs` | `make_fork_ctx_for_thread` preserves the clone child's caller-saved GPRs (incl. `r9`) |
| `kernel/src/task/scheduler.rs` | `wake_task_v2` defers the enqueue to the dispatch epilogue when the wakee is mid-switch-out (`on_cpu`) |
| `kernel/src/arch/x86_64/cpuid.rs`, `kernel/src/lib.rs`, `kernel/src/smp/boot.rs` | SMEP/SMAP probe + enable + per-CPU AC-clear; SFMASK clears `EFLAGS.AC` |
| `kernel/src/mm/elf.rs` | `PT_TLS` (7) parsed + logged |
| `kernel/src/net/tcp.rs`, `kernel-core/src/net/tcp.rs` | RFC 6298 `RttEstimator`, per-connection retransmit buffer + `tcp_tick`, `MAX_TCP_CONNECTIONS` 8→64 |
| `kernel/src/net/udp.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`, `userspace/net_server/src/main.rs` | unbound-UDP-send ephemeral source port (DNS resolver) |
| `kernel/src/arch/x86_64/microcode.rs`, `kernel-core/src/microcode.rs` | AMD microcode container parse + per-CPU apply |
| `kernel/src/fs/procfs.rs` | `/proc/<pid>/task/<tid>` per-thread subtree (htop's `scanMainThread`); `/proc/<pid>/status` gains `Tgid`/`VmRSS`/`VmData`/`VmStk` |

## Core Concepts

### The lost wakeup behind the pthread/TLS hang (Track C)

musl gives every joinable thread the *same* join word in one layout: it sets
`CLONE_CHILD_CLEARTID` to `&__thread_list_lock` so the kernel releases the
thread-list lock when the thread dies. Crucially, `__tl_lock` waits on that word
with a **non-private** futex. m3OS keys private futexes as `(0, addr)` and
non-private as `(cr3, addr)`. `do_clear_child_tid` woke only the private key —
so the lock-release wake was *lost*, and a thread blocked in `__tl_lock` while a
sibling exited never ran again. Its `pthread_join`er hung forever. The fix is
two lines: wake both keys. The companion bug — `make_fork_ctx_for_thread`
zeroing the clone child's `r9` (which musl's `__clone` uses as `call *%r9`) —
made every worker fault at `rip=0` before the join path was even reachable.
Together these make multi-threaded musl programs (and the 4-thread `tls-smoke`)
work.

### The SSH-disconnect freeze was a *different* bug (Track A)

It was tempting to fold the SSH-disconnect hang into the lost-wake story above
— `sshd` reaps its session with pthreads, so the same `__tl_lock` wake is on its
path, and earlier handoffs were marked RESOLVED on that theory. But the freeze
kept reproducing after the futex fix. The 2026-05-29 investigation pinned the
real cause in **userspace**, not the kernel: the `async-rt` executor is purely
edge-driven (a task only re-runs when a waker fires), so a single transient
*missed* I/O-readiness edge — a PTY-master `POLLHUP` that failed to wake the
sshd relay parked in `WaitWake` — stalled the whole session forever, with the
reactor spinning its 100 ms timeout finding nothing ready. The fix is an
**idle-liveness backstop** (`Executor::requeue_all` in
`userspace/async-rt/src/executor.rs`): after several consecutive empty reactor
polls, force every parked task to re-poll from its current await point, so a
level-triggered condition (`WaitWake` returns `Ready` once registered; the relay
then reaches its `waitpid(WNOHANG)` exit backstop) re-checks directly and a
permanent hang becomes a bounded (~hundreds-of-ms) recovery. The companion
`sunset-local` change makes the teardown send a proper exit-status/EOF/CLOSE so
the client logs a clean `exit 0` rather than "closed by remote host." The
scheduler `on_cpu` cross-core defer fix (`6f57fbc`) is a *real* fix for a
distinct cross-core deadlock class, but it is not what unwedged the disconnect.

> **Process note:** this is the cautionary half of "one symptom, two causes."
> The first three commits and an earlier draft of this doc attributed the
> SSH-disconnect freeze to the futex lost-wake; that attribution was wrong, and
> the in-tree `docs/handoffs/2026-05-29-ssh-disconnect-lost-wakeup.md` records
> how the evidence forced the correction. The futex fix was still necessary —
> just for Track C, not for this.

### Cheap CR4 security: SMEP and SMAP (Track B)

`CR4.SMEP` (bit 20) faults if ring 0 ever *fetches an instruction* from a user
page; `CR4.SMAP` (bit 21) faults if ring 0 *reads/writes* a user page while
`EFLAGS.AC == 0`. Two subtleties bit us: firmware leaves `AC = 1`, and
`without_interrupts` `popf`-restores it, so the kernel must explicitly `clac`
after enabling SMAP (and the `SYSCALL` `SFMASK` must clear `AC` on entry). m3OS's
`copy_from_user`/`copy_to_user` route through the physmap (a supervisor mapping
SMAP ignores), so they need no `stac`/`clac` — a debug self-test proves a raw
user-VA access from ring 0 faults.

### TLS, DNS, TCP retransmit, microcode

- **PT_TLS (C):** the `.tdata` template lives inside an already-mapped
  `PT_LOAD`, and musl rediscovers it through `AT_PHDR`/`AT_PHENT`/`AT_PHNUM`, so
  the loader only had to *recognise* the segment — the load-bearing proof is the
  4-thread `tls-smoke` test.
- **DNS (D):** musl's resolver `sendto`s on a never-bound UDP socket; m3OS
  returned `EINVAL`. Auto-assigning an ephemeral source port makes the
  kernel-level round-trip work (query out, reply in). A residual userspace
  poll/recvfrom delivery gap is documented.
- **TCP retransmit (D):** RFC 6298 SRTT/RTTVAR with a 1 s-min / 60 s-max RTO and
  exponential backoff. A per-connection `RetransmitQueue` buffers **every**
  outstanding segment (SYN, data, FIN); the single RFC 6298 timer times the
  oldest, `on_ack` prunes the cumulatively-acked prefix, and `service_rto`
  replays the earliest unacked segment. Send is flow-controlled — outstanding
  bytes are capped at `min(peer_window, 64 KiB)` and the send syscall blocks /
  `EAGAIN`s / `EPIPE`s on a full window. (The first cut buffered only one
  segment, leaving a multi-segment window — and a FIN behind in-flight data —
  unprotected.) The queue + estimator are host-tested in `kernel-core` because
  QEMU's lossless SLIRP cannot exercise packet loss.
- **Microcode (E):** the AMD container parser is host-tested pure logic; the
  apply writes `MSR_AMD64_PATCH_LOADER` only on an exact equivalence +
  strictly-newer-revision match, so QEMU and non-AMD CPUs are a clean,
  boot-safe skip.

### Verify-don't-reimplement (Track F) and listing processes (Track H)

`epoll_create1`/`epoll_ctl`/`epoll_wait` were already fully implemented; the
audit's "PARTIAL" was a source-search miss. F adds a regression gate, not code.
H's htop-zero-processes had two layers. An old per-user PID filter (already
fixed in Phase 72b) hid root-owned daemons from an unprivileged user — but the
*load-bearing* fix here is the `/proc/<pid>/task/<tid>` per-thread subtree:
htop's `scanMainThread` reads each process's main-thread stat from
`/proc/<pid>/task/<pid>/stat`, and m3OS never exposed that subtree, so
`readStatFile` failed for **every** process and the table rendered empty.
Track H adds the `task/<tid>/` subtree (`stat`/`statm`/`status`/`comm`/
`cmdline`/`maps`/`io`/`fd`) alongside the `/proc/<pid>/status` memory fields
(`VmRSS`/`VmData`/`VmStk`) htop/ps also read; `ps -e` (the same `/proc`
enumeration path) is the regression-protected proof.

## How This Phase Differs From Later Memory/Security Work

- **KPTI / Meltdown isolation, retpoline / IBRS (Spectre)** are **Phase 84**, not
  here. They require dual page tables and indirect-branch barriers — structural,
  measurable-cost changes. Phase 77's security is deliberately only the
  *free* CR4 bits (SMEP/SMAP) and the *firmware-updatable* microcode patch.
- **TCP congestion control beyond Reno-style retransmit** (CUBIC, BBR, SACK,
  window scaling) is **post-1.0**. Phase 77 adds only the RFC 6298 retransmission
  timer — enough to survive loss, not to be fast on a fat long pipe.
- **A virtio-input migration** is not a 1.0 requirement; the PS/2 stack shipped
  in Phase 56 covers graphical input (the 2026-05-04 handoff is closed as
  superseded).

## Related Roadmap Docs

- `docs/roadmap/77-pre-1-0-cleanup.md` — the phase design doc
- `docs/roadmap/tasks/77-pre-1-0-cleanup-tasks.md` — the per-track task list with
  acceptance status
- `docs/appendix/audit-status/74a-pre-1.0-audit.md` — the audit that promoted
  these into must-fix (epoll row updated by Track F)

## Known Follow-ups

- **DNS:** forward resolution works end to end — `getaddrinfo` over UDP
  resolves A records. (The earlier "userspace-delivery gap" was mis-attributed
  to `poll`/`recvfrom`: musl drains replies with `recvmsg`, not `recvfrom`, and
  `recvmsg` on AF_INET returned `EOPNOTSUPP`; closed by `sys_recvmsg_inet` in
  commit `8303990`.) Remaining follow-ups: the `dns-smoke` gate is
  intentionally **soft** — it accepts `DNS_SMOKE:SKIP` (no outbound DNS in a
  sandbox) as well as `:PASS`, so it proves the resolver path is wired and
  non-hanging but does **not** regression-protect a successful resolution; and
  there is no caching, no search-domain / `nsswitch.conf` handling, and no
  AAAA/IPv6 or DNSSEC.
- **TCP:** SACK / window scaling / modern congestion control; a real
  packet-loss test rig (tap + netem) to replace the host-tested estimator.
- **Microcode:** an Intel `0x79` path + a fetch-and-verify flow for the blob
  (currently the AMD fam19h container is committed directly).
