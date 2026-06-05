---
status: OPEN — not started. Intermittent kernel panic at 4 GiB RAM whose panic
  banner is UNREADABLE because the panic path does not quiesce the other cores
  before printing. Reliable workaround today: run at the default 2 GiB.
branch: feat/phase-85c-python
repro-commit: f442172  # bug predates this; reproduces here. NOT caused by recent work.
date: 2026-06-05
component: kernel/smp + kernel/panic path (diagnosability); suspected kernel/scheduler
  or kernel/smp TLB-shootdown race that only manifests at >2 GiB. Secondary: the
  4 GiB instability documented in 2026-05-24-4gib-pci-hole-vga-mapping.md.
related:
  - docs/handoffs/2026-05-24-4gib-pci-hole-vga-mapping.md   # the 4 GiB SMP/TLB-shootdown saga (5 sessions); "FIXED via NMI shootdown" but this is a residual recurrence
  - docs/handoffs/2026-05-22-compositor-shm-leak-multi-term-oom.md  # the predecessor OOM-class handoff
  - kernel/src/lib.rs:1017            # handle_panic — the dispatcher that needs to halt APs first
  - kernel/src/main.rs:43             # #[panic_handler]
  - kernel/src/trace.rs:55            # dump_trace_rings (the trace-ring dump)
  - kernel/src/task/scheduler.rs      # stale-ready watchdog (~1959, ~2444, ~2596)
artifact: m3os-serial.log (project root, transient — capture a fresh one; it gets overwritten)
---

## TL;DR

A developer running `cargo xtask run-gui` with **`-m 4g` + `--kvm`** hit an
**intermittent** kernel panic during graphical boot (compositor / `display_server`
startup). It is the same 4 GiB-only crash class seen before; 2 GiB never reproduces
it. The blocking problem is **you cannot read the panic** — the panic banner comes
out as SMP-interleaved garbage because `handle_panic` prints + dumps while the other
three cores keep running and writing to the serial port.

**Fix #1 first (diagnosability, tractable): make the panic path quiesce the APs
(broadcast a halt NMI/IPI) and/or take the serial lock exclusively before printing,
so the next 4 GiB panic yields a clean `KERNEL PANIC at <file>:<line>` instead of
shredded bytes.** Only then is the underlying 4 GiB race chaseable.

## Symptom / evidence (from `m3os-serial.log`, 2026-06-05)

- **4 GiB RAM**: `[mm] buddy allocator: 1040316 free pages across 14 orders`
  (1,040,316 × 4 KiB ≈ 4 GiB). Framebuffer 1920×1080, SMP=4.
- Boot reaches **all services loaded incl. the graphical set** (`init: loaded
  service 'wallpaper'` / `'bar'` / `'notifyd'`), then crashes during service
  **startup** — last clean activity is `display_server: starting` +
  `[framebuffer_mmap] pid=7 mapped 2025 pages`.
- The log is **control-byte corrupted**: plain `grep` silently skips it (treats it
  as binary); you must use **`grep -a`**. Words are interleaved char-by-char across
  cores (e.g. `init: service 'xhci_driver exited pid=' exited normally21`), so the
  `KERNEL PANIC at …` banner and `info.message()` are unrecoverable from this log.
- Ends with a scheduler **`=== END TRACE RING DUMP ===`** (so a real panic ran —
  `dump_trace_rings()` is only called from `handle_panic`, `lib.rs:1034`), followed
  by `[WARN] [sched] stale-ready: pid=0 name=net core=3 stale~119 ms`. The last
  trace events (tick ~1010) show core 3 cycling `Dispatch`/`SwitchOut`/`BlockCurrent`
  between task_idx 2 and 16 (`BlockCurrent … scheduler.rs:3632` and
  `syscall/mod.rs:3931`). Whether the stale-ready is the cause or a post-panic
  symptom (APs not halted) is unknown — see "open questions".

## Why the panic is unreadable (root of the diagnosability problem)

`handle_panic` (`kernel/src/lib.rs:1017`) does, in order:
1. `serial::_panic_print("KERNEL PANIC at {file}:{line}")`
2. `serial::_panic_print("  {message}")`
3. `panic_diag::dump_crash_context()`
4. `trace::dump_trace_rings()`
5. `hlt_loop()`

It **never stops the other CPUs**. On a 4-core guest, while the panicking core
prints, cores 0–3 keep scheduling and logging to the same UART → every panic comes
out interleaved/garbled. This is also why two different 4 GiB crashes this session
(an OOM, see below; and this scheduler/SMP panic) both produced unreadable output.

## Two observed manifestations this session (relationship UNKNOWN)

1. **OOM panic** during `pkg install python` (21 MiB package, heavy heap-grow +
   VFS I/O): `kernel OOM: failed to allocate … after heap growth retry`, frame
   allocator down to `~4 MiB free / 4072 MiB total` — i.e. ~4 GiB was actually
   consumed. (`handle_alloc_error`, `lib.rs:1043`.)
2. **Scheduler / SMP panic** (this log) during graphical boot — no OOM markers,
   trace-ring dump + core-3 stale-ready.

Both at 4 GiB, both with corrupted output, both intermittent. They may share a root
cause (an SMP/timing fragility that scales with RAM — the 2026-05-24 handoff's
thesis was "scales with RAM") or be distinct (a memory leak vs a TLB/IF
scheduler race). Don't assume; the clean panic from Fix #1 will tell.

## Ruled out

- **My recent changes are NOT the cause.** The fw_cfg boot-mode work (f442172,
  `kernel/src/fwcfg.rs`) is a handful of BSP-only port reads early in boot, before
  APs come up, with no allocation; the console-EOF fix (`ec77e64`) is in the stdin
  read path. Neither touches SMP/scheduler/heap-grow-during-AP-boot. The 4 GiB crash
  class predates all of it (first seen before the fw_cfg work; the umbrella 4 GiB
  bug is documented from 2026-05-24).
- **Not display-backend specific** — reproduced under the QEMU VNC backend headless
  on the dev box's serial too; it's kernel-side.

## Reproduction

- `cargo xtask run-gui --fresh` (or `run`) with **`-m 4g --kvm`**, SMP=4. Boot a few
  times — it's intermittent (the user runs 4 GiB regularly and hits it occasionally).
  2 GiB (the default) does NOT reproduce.
- Always capture serial to a file and read with `grep -a` / `cat -v` (it's binary-
  corrupted). The interesting region is the last ~600 lines (the dump + the garbled
  banner just before the first `core=N … {…}` trace event).

## Next steps (in order)

1. **[Do this first] Quiesce APs + serialize the panic path.** In `handle_panic`
   (`kernel/src/lib.rs:1017`), before any `_panic_print`, broadcast a "stop" to the
   other cores so only the panicking core writes the UART. m3OS already has the IPI/
   NMI machinery from the 2026-05-24 work (`kernel/src/smp/ipi.rs` NMI send,
   `kernel/src/smp/tlb.rs` NMI shootdown) — reuse it to park APs in `hlt`. Mirror
   Linux's `panic_smp_self_stop` / `smp_send_stop`. Also consider a re-entrancy
   guard (an `AtomicBool` so a second core that panics during the dump doesn't
   re-corrupt). Acceptance: induce a panic at 4 GiB and read a clean single-stream
   `KERNEL PANIC at <file>:<line>` + message + trace dump.
2. **With a clean panic in hand, classify the crash.** Is it a fault (#PF/#GP) on a
   specific core, an `assert`/`unwrap` in the scheduler or compositor IPC path, or a
   hang-watchdog panic from the stale-ready detector? The `<file>:<line>` localizes
   it immediately.
3. **Then chase the 4 GiB race.** Cross-reference the 2026-05-24 handoff's
   "ruled-out-hypotheses" and "How to reproduce" sections — the prior fix was
   NMI-based TLB shootdown to dodge an IF=0 window during AP-boot heap-grows that
   "scales with RAM". A residual could be: a remaining IF=0 / lost-wakeup window, a
   frame-allocator/refcount-table sizing edge at >2 GiB, or memory that straddles
   the 4 GiB PCI hole (QEMU splits `-m 4g` above the 32-bit hole — the prior doc
   called this a "red herring", but re-test with a clean panic).

## Open questions

- Is the OOM manifestation (#1) a real leak (something consumes ~4 GiB) or the same
  scheduler stall preventing reclaim? A clean panic + `dump_crash_context` frame
  stats will disambiguate.
- Is `stale-ready core=3 net` the trigger (a genuine scheduler hang → watchdog
  panic) or a post-panic artifact (core 3 still running because APs aren't halted)?
  Fix #1 removes the ambiguity.

## Workaround (tell the user)

Run at the **default 2 GiB** (drop `-m 4g`). The persistent data disk — including an
installed `python` — is untouched, so nothing is lost; 2 GiB is the validated,
gate-tested configuration and does not reproduce this.
