# Handoff — Phase 111: Remote Debugging (Source-Level Kernel + Userspace)

**Date:** 2026-07-06 (living doc — update on every session working this phase)
**Branch:** `feat/phase-111-ptrace` (Track D.1–D.2), stacked on
`feat/phase-111-kgdb-stub` (Track C.2–C.5, PR #314 open) — both open PRs to `main`.
**State:** IN PROGRESS.
- **Track A (QEMU gdbstub + debug-info build)** ✅ merged — PR #311 (`1c2645d3`).
- **Track B (trap & debug-register substrate)** ✅ merged — PR #312 (`f34d7fc9`).
- **Track C.1 (RSP wire codec)** ✅ merged — PR #313 (`0edfa7a2`).
- **Track C.2–C.5 (in-kernel stub core)** ✅ **landed on `feat/phase-111-kgdb-stub`**
  (PR #314; full-GPR `#BP`/`#DB` naked entry, polled COM2, RSP command loop, SMP
  all-stop + panic hook, `kgdb` feature + `kgdb-smoke` gate PASS). Details below
  ("What's landed"); the old blueprint is preserved under "Implementation notes".
- **Track D.1 + D.2 (ptrace substrate + `sys_ptrace`)** ✅ **landed on
  `feat/phase-111-ptrace`** (traced-process stop/notify, `SIGTRAP` on ring-3
  `int3`, cross-address-space peek/poke, `ptrace-smoke` PASS). See "Track D" below.
- **Track D.3 + D.4 (`m3gdbserver` + symbol retention)** — NOT started.

**Charter:** `docs/roadmap/111-remote-debugging.md`
**Tasks:** `docs/roadmap/tasks/111-remote-debugging-tasks.md`

> **No host `gdb` on the dev machine.** Every validation this phase used a
> hand-rolled **raw GDB-RSP client** (Python) driving QEMU's stub / the future
> in-kernel stub directly. Keep this in mind: the C.5 gate must be a raw-RSP
> driver, not a `gdb` invocation. Reference probe:
> `scratchpad/rsp_probe.py` pattern from the Track A session (connect, `?`,
> `Z1,addr,1`, `c`, read `g`, compare RIP).

---

## What's landed (and the load-bearing details)

### Track A — `cargo xtask debug` (PR #311)

- `[profile.kdebug]` in the root `Cargo.toml` inherits `release` but sets
  `lto = false` (LTO inlines small fns away, so `break <fn>` / named
  backtraces stop resolving) + `debug = "full"`. `build_kernel_debug()` /
  `build_kernel_binary(debug_info)` in `xtask/src/main.rs` produce a
  DWARF-bearing kernel ELF under `target/x86_64-unknown-none/kdebug/kernel`
  (49 MiB vs 16 MiB release — all non-`PT_LOAD`, booted image unchanged).
- `cargo xtask debug` launches QEMU `-s -S` (gdbstub on `tcp::1234`, halted at
  reset) and writes an auto `m3os-kernel.gdb` (`gdb -q -x <script>`).
- **CHARTER CORRECTION (important, reused by C):** the kernel ELF is **ET_DYN
  (PIE)** and the `bootloader` crate relocates it by a **fixed 1 TiB offset
  `0x10000000000`** (verified deterministic). The charter's "no KASLR →
  `add-symbol-file` unnecessary" was wrong — no KASLR, but a non-zero PIE base.
  The generated gdb script does `add-symbol-file <elf> -o 0x10000000000`.
  **Any kernel-symbol ↔ runtime-address mapping (including the C.5 gate's
  breakpoint addresses) MUST add `0x10000000000` to the `nm`/ELF vaddr.**

### Track B — trap & debug-register substrate (PR #312)

The seam C.2 plugs into. All in `kernel/src/arch/x86_64/debug.rs` +
`kernel-core/src/debug_regs.rs` + `interrupts.rs`:

- **`#DB` (vector 1)** registered (`debug_exception_handler`): reads + clears
  `DR6`, decodes it (`kernel_core::debug_regs::dr6_decode`), dispatches to
  `debug::on_debug_exception(status, rip, frame, from_user)`.
- **`#BP` (vector 3)** upgraded to a dispatcher (`breakpoint_handler`):
  computes the breakpoint address as **`RIP-1`** (past the `0xCC`) and calls
  `debug::on_breakpoint(bp_addr, frame, from_user)`.
- **`from_user` routing seam**: `on_breakpoint`/`on_debug_exception` branch on
  ring-0 vs ring-3. With no consumer registered, the safe default logs + resumes
  (clearing stray `TF`), so production is undisturbed. **C.2's kernel stub
  registers as the ring-0 consumer here; D's ptrace stop as the ring-3 one.**
- `set_single_step`/`clear_single_step` flip `RFLAGS.TF` on the trap frame
  (`frame.as_mut().update(|f| f.cpu_flags |= …)`).
- `DebugRegs::arm/disarm/status` program `DR0`–`DR3` + `DR7` via the host-tested
  `debug_regs` codec; ring-3 `mov %dr` still `#GP`s.
- `insert_sw_breakpoint(addr) -> orig` / `remove_sw_breakpoint(addr, orig)` —
  save byte, write `0xCC`, `mfence`, restore. **Caller owns the save/restore
  lifecycle** (double-insert saves `0xCC` as the "original").
- Self-test behind the `debug-substrate-test` feature (`run_boot_self_test`,
  `DEBUG_SELFTEST:` sentinels), gate `debug-substrate-smoke`
  (`M3OS_DEBUG_SUBSTRATE_REGRESSION`). PASSES.

### Track C.1 — RSP wire codec (PR #313)

`kernel-core/src/gdb_rsp.rs`, host-tested (11 tests), the layer the stub drives:

- `checksum()` (mod-256), `encode_packet()` (`$payload#cc`), and an incremental
  `PacketReader::feed(byte) -> Option<RspEvent>` (`Packet(len)` / `BadChecksum`
  / `Ack` / `Nak` / `Interrupt`). `payload()` returns the decoded body.
- **SUBTLETY the tests pin:** the RSP checksum covers the **raw on-wire body**
  (including `*` + the RLE repeat count), NOT the RLE-expanded payload. The
  reader tracks `raw_sum` separately.
- Hex helpers: `hex_encode` / `hex_decode` / `parse_hex_prefix` (big-endian, for
  `m addr,len` / `g` / `M`).

### Track C.2–C.5 — in-kernel `kgdb` stub (`feat/phase-111-kgdb-stub`)

The interactive stub, all behind the `kgdb` cargo feature (off by default;
production excludes it — same posture as `panic-test`/`trace`/telnet).

- **Full-GPR `#BP`/`#DB` naked entry** (`interrupts.rs` `bp_entry`/`db_entry`
  + `debug.rs` `DebugTrapFrame`): the old `extern "x86-interrupt"` handlers
  could not see the interrupted GPRs, which `g`/`G` need. The stubs push all 15
  GPRs (r15→rax, so rax=`gprs[0]`) then reuse the CPU's 5-field iretq frame —
  **one layout for both rings** (64-bit mode pushes `SS:RSP` unconditionally),
  `cs&3` splits ring in Rust. `offset_of` asserts pin it. `debug-substrate-smoke`
  still PASSES (naked entry validated).
- **Polled COM2** (`debug/com2.rs`): `0x2F8`, 115200 8N1, IER=0. `try_read_byte`
  polls LSR bit 0, `write_byte` spins on bit 5. No IRQ, no alloc — the machine
  is frozen when the stub runs. QEMU wires COM2 as a **second** `-serial
  tcp:…,server,nowait` (first=COM1 stdio, second=COM2).
- **Stub command loop** (`debug/gdbstub.rs`): all-stop RSP loop over the
  `gdb_rsp` codec. `?`, `qSupported` (`PacketSize=400`), `g`/`G` (amd64 order,
  LE hex, FPU deferred), `m`/`M` (canonical-guarded; `M` clears `CR0.WP` to
  patch kernel text), `Z0/z0` (planted-bp table; RIP rewound to the bp on a
  planted hit, left past a compiled-in `int3`), `Z1/z1` (DR0), `c`/`s`/`D`/`k`,
  `H`/`q*` stubs. Unsolicited stop reply after `c`/`s`; `?`-driven on attach.
- **SMP all-stop** (`smp/mod.rs` `kgdb_stop_all_aps`/`kgdb_release_aps`/
  `kgdb_ack_and_wait` + `nmi_handler` branch): a **releasable** clone of the
  panic-quiesce NMI path — parked APs spin in the NMI handler until the owner
  clears `KGDB_STOP`, then `iretq` back (panic-stop parks forever; this
  returns). Sentinel: `interrupts::total_timer_ticks()` sampled before/after,
  logged on `KGDB:release … ticks_before=X ticks_after=Y` (equal = no advance).
- **Panic hook** (`lib.rs handle_panic` → `gdbstub::enter_from_panic`, a fresh
  `int3` so the stub gets a real frame): a bare-metal panic drops into the stub
  after the banner/dump instead of a dead halt.
- **Entry + gate**: `kgdb_break()` (wait-for-debugger `int3` early in boot after
  `boot_aps`) + `kgdb_probe_target()` (`#[inline(never)]`, `#[no_mangle]` —
  the deterministic breakpoint symbol) + `KGDB_PROBE_MAGIC` (`#[used]` static
  the `m` read verifies). `cargo xtask kgdb-smoke` (`M3OS_KGDB_REGRESSION=1`) is
  the raw-RSP driver: `nm kgdb_probe_target + 0x10000000000` → `Z0` → `c` →
  assert stop + RIP + `m` magic + all-stop sentinel. **PASSES (~7 s).**

---

## Implementation notes — Track C.2–C.5 blueprint (as-built, kept for reference)

The plan below is what was implemented above; retained for the design rationale.

### 1. Full-GPR exception entry (prerequisite for `g`/`G`)

GDB's `g` packet needs all 16 GPRs + RIP + RFLAGS + segments, but the current
`extern "x86-interrupt"` `#BP`/`#DB` handlers only expose `InterruptStackFrame`
(RIP/CS/RFLAGS/RSP/SS). **Rework `#BP` and `#DB` to naked-asm entry stubs** that
push all GPRs into a frame, modelled on the existing preempt entry stubs:
- Pattern: `kernel/src/arch/x86_64/interrupts.rs` `timer_entry` / naked-asm
  stubs + `kernel/src/arch/x86_64/preempt_trap_frame.rs`
  (`PreemptTrapFrameKernel` = `gprs[15]` [rax,rbx,rcx,rdx,rsi,rdi,rbp,r8..r15]
  then the CPU iretq frame). Reuse this layout (or a `KgdbRegs` clone).
- `#BP`/`#DB` fire in ring-0 during kernel debugging AND ring-3 for a traced
  process (D). The stub cares about ring-0; keep the `from_user` split.
- **Risk is contained**: `int3`/`#DB` never fire in production (no debugger), so
  a bug in these stubs only affects the debug path. Still, get the GPR push
  order + iretq-frame offsets exact (there are `const _: () = assert!(offset_of…)`
  guards in `preempt_trap_frame.rs` — add equivalents for the kgdb frame).

### 2. Polled COM2 driver (C.3) — `kernel/src/debug/com2.rs`

- COM2 base `0x2F8`: DATA `0x2F8`, IER `0x2F9`, FCR `0x2FA`, LCR `0x2FB`,
  MCR `0x2FC`, **LSR `0x2FD`** (bit 0 = RX ready, bit 5 = THR empty).
- Init (115200 8N1, **interrupts OFF** — the stub polls): IER=0x00; LCR=0x80
  (DLAB); DLL=0x01, DLM=0x00 (divisor 1); LCR=0x03; FCR=0xC7; MCR=0x03.
- `try_read_byte() -> Option<u8>` (poll LSR bit 0), `write_byte(u8)` (spin on
  LSR bit 5 then write DATA). Raw `x86_64::instructions::port::Port`. No alloc,
  no interrupts — the rest of the kernel is FROZEN when the stub runs.
- **COM1 stays the live console** (`-serial stdio`); COM2 is unused today.
- QEMU args: add a **second** `-serial` routing COM2 to a host TCP port, e.g.
  `-serial tcp:127.0.0.1:<port>,server,nowait` (the gate connects to it).
  Order matters: the first `-serial` is COM1, the second is COM2.

### 3. Stub command loop + register mapping (C.2) — `kernel/src/debug/gdbstub.rs`

An all-stop loop: on entry it owns the CPU until `c`/`s`/`D`/`k`. Reads RSP via
`com2::try_read_byte()` → `gdb_rsp::PacketReader`, replies via
`com2::write_byte()` + `gdb_rsp::encode_packet`. Command set:
- `?` → `S05` (or `T05`) stop reply.
- `qSupported` → `PacketSize=1000` (and nothing else needed for the first cut;
  no XML target description — hardcode the amd64 layout gdb already knows).
- `g` → all registers in **GDB amd64 order**, little-endian hex:
  `rax,rbx,rcx,rdx,rsi,rdi,rbp,rsp, r8..r15` (16×u64), `rip` (u64),
  `eflags` (u32), `cs,ss,ds,es,fs,gs` (u32 each). FPU/XSAVE **deferred** (charter).
  Map from the naked-entry `KgdbRegs`. `G` → write them back.
- `m addr,len` → hex-dump kernel memory (bounds-check!); `M addr,len:hex` → write.
- `c` / `s` → resume / set `TF` then resume (leave the loop; the next `#DB`/`#BP`
  re-enters). `Z0,addr,kind` / `z0,…` → `insert/remove_sw_breakpoint` (B.3).
  `Z1/z1` → `DebugRegs::arm/disarm` (B.2).
- `D` (detach) / `k` (kill) → leave the loop / halt.
- ACK discipline: send `+` on a good packet, `-` on `BadChecksum`.

**Entry:** a `kgdb_break()` / "wait for debugger" call early in boot (behind the
`kgdb` feature) does an `int3`, entering the stub so the gate can attach while
the machine is frozen at a known point. `on_breakpoint`/`on_debug_exception`
(Track B seam) route ring-0 traps into the stub loop when the `kgdb` consumer is
registered.

### 4. SMP all-stop + panic hook (C.4)

- On stub entry, NMI-IPI the other APs into a parked spin-wait; release on
  `c`/`s`. **Reuse the TLB-shootdown NMI path** (`kernel/src/smp/ipi.rs`
  `send_nmi`, `kernel/src/smp/tlb.rs`; the NMI has its own IST stack —
  `gdt::NMI_IST_INDEX`). A sentinel proves no other core advances while stopped.
- `kernel/src/lib.rs` `handle_panic` gains an optional pre-halt hook that enters
  the stub (feature-gated) — a bare-metal panic becomes an interactive session.
- Async break: poll COM2 for `0x03` from a timer tick (or the idle loop) to
  break a *running* guest into the stub. (Can be a follow-on within C.4.)

### 5. `kgdb` feature + gate (C.5)

- `kgdb` cargo feature in `kernel/Cargo.toml`, **off by default** (arbitrary
  kernel peek/poke defeats W^X/PKU/capabilities — same posture as `panic-test`/
  `trace`/telnet). Production image excludes the stub.
- Gate `kgdb-smoke` (`M3OS_KGDB_REGRESSION=1`): build with `kgdb`, boot (the
  wait-for-debugger `int3` freezes it), route COM2→TCP, then a **raw-RSP Python
  driver** connects, `?`→stop, sets `Z0` at a known kernel fn
  (**`nm` addr + 0x10000000000**), `c`, asserts the stop + reads `g` (RIP at the
  breakpoint) + `m` a known value. Model the driver on the Track A `rsp_probe.py`.

---

## Track D — ptrace + m3gdbserver

**D.1 + D.2 landed on `feat/phase-111-ptrace`** (stacked on the Track C branch —
it reuses the Track C full-GPR `DebugTrapFrame` naked entry for the ring-3 path).
`ptrace-smoke` PASSES. D.3 (`m3gdbserver`) + D.4 (symbol retention) remain.

### As-built (D.1 + D.2)

- **`kernel/src/process/ptrace.rs`** (new): the `Ptrace` per-process state
  (`traced`/`tracer_pid`/`stopped`/`stop_reported`/`stop_sig`/`resume`/`regs`),
  the ring-3 trap consumers (`on_user_breakpoint`/`on_user_debug`), the
  stop/park loop (`enter_stop_and_wait`), the `sys_ptrace` request handlers, and
  cross-address-space `peek`/`poke` (via `mm::mapper_for_frame` over the tracee's
  CR3 → physmap; POKE bypasses RO text so it can plant an `int3`).
- **The stop mechanism = the `fault_kill_trampoline` redirect.** A traced
  tracee's `#BP`/`#DB` snapshots its `DebugTrapFrame` into `Process.ptrace.regs`
  and rewrites the trap frame so the naked-stub `iretq` lands in
  `syscall::ptrace_stop_trampoline` (kernel CS, IF clear, current kernel RSP) —
  a blockable ring-0 continuation. It notifies the tracer
  (`wake_child_waiters` + SIGCHLD), busy-yields until the tracer sets
  `ptrace.resume`, then resumes to userspace via `restore_and_enter_userspace`
  (applying any `SETREGS` edits; TF set for `SINGLESTEP`). **Do not** try to
  block directly inside the `#BP` handler — the redirect keeps blocking out of
  exception context, exactly like the proven `#PF`→kill path.
- **`SavedUserRegs` is the GETREGS/SETREGS shape** (18 × u64, `#[repr(C)]`,
  now `Default`). `regs_from_frame` maps `DebugTrapFrame.gprs` (15, no rsp) +
  `rip/rsp/rflags` onto it.
- **`#BP` gate → DPL 3 under `ptrace`** (`interrupts.rs` IDT) so a ring-3 `int3`
  delivers `#BP` not `#GP`. RIP is reported **past** the `int3` (Linux
  semantics — the tracer rewinds it for planted breakpoints; rewinding in-kernel
  would loop a compiled-in `int3` on CONT). Production keeps DPL 0.
- **`waitpid`** (`syscall/mod.rs`) reports a ptrace-stopped child as
  `WIFSTOPPED`/`SIGTRAP` (one-shot, ignoring `WUNTRACED`), before the generic
  signal-stop scan.
- **Gate**: `/bin/ptrace-test` (`userspace/ptrace-test`, native, fork+TRACEME
  tracer/tracee) + `cargo xtask ptrace-smoke` (`M3OS_PTRACE_REGRESSION=1`).
  Validates stop/notify + GETREGS/SETREGS + PEEK/POKETEXT + CONT with **no**
  m3gdbserver — a `SETREGS`'d `rbx` flows into the tracee's exit code.

### D.3 / D.4 remaining

- `userspace/m3gdbserver` (native, four-place wiring per the codebase map —
  sshd template + `kernel-core` for `gdb_rsp`): fork+TRACEME+`execve` a tracee,
  translate GDB RSP ↔ `sys_ptrace` over TCP/AF_UNIX (kernel is alive → ordinary
  IRQ-driven transport, no polled link). The `gdb_rsp` codec is `no_std`, so it
  links from userspace. Register-order marshalling matches the `g`/`G` amd64
  layout the kgdb stub already implements.
- **exec-stop**: for the `execve` model, `execve` of a `TRACEME`'d child should
  emit a SIGTRAP ptrace-stop so the tracer gains control before the new program
  runs (Linux). Not yet wired — the native D.1/D.2 test hits `int3` in the same
  binary (no exec), so this is a D.3 prerequisite.
- **`ATTACH` / `GETSIGINFO`** deferred (the fork+TRACEME model needs neither).
- D.4: retain unstripped userspace ELFs host-side for DWARF; the current
  `ptrace-smoke` is the native-tracer variant, the `m3gdbserver` RSP variant
  rides D.3.

---

## Gotchas learned this phase (don't re-discover)

- **PIE 1 TiB offset.** Kernel runtime addr = ELF vaddr + `0x10000000000`. Every
  gdb/RSP breakpoint at a kernel symbol needs this offset (Track A + the C.5
  gate). Deterministic across boots (verified).
- **No host gdb.** Validate stubs with a raw-RSP client. QEMU's own gdbstub and
  the future in-kernel stub speak the same protocol; the `gdb_rsp` codec is the
  same wire format. `Z1` (hardware breakpoint) is more robust than `Z0` for a
  not-yet-loaded/PIE address in a raw probe.
- **RSP checksum is over the raw on-wire body**, not the RLE-expanded payload.
- **Inlining bites raw breakpoints.** `break kernel_main` didn't fire because
  `kernel_main` (a thin wrapper) inlines into `_start` at opt-level 3; break at a
  large non-inlined fn like `kernel_main_entry`. Real gdb handles inline sites
  via DWARF; a raw addr probe does not.
- **`extern "x86-interrupt"` handlers can't see the interrupted GPRs** — need a
  naked entry stub (see the preempt stub pattern) for `g`/`G`.
- **Merge hiccup:** `gh pr merge … --admin` prints
  `git: 'remote-https' is not a git command` + a fast-forward warning from its
  internal pull, but the **squash merge still succeeds server-side**. Just
  `git checkout main && git pull && git remote prune origin` after.

---

## Also this session — Phase 110 (Real-Hardware Security)

Landed alongside the Phase 111 work (context, not this phase):
- **Track C (argon2id)** ✅ PR #309 (`04169091`) — RFC 9106 argon2id + BLAKE2b,
  `verify_password` fallback + login re-hash, `argon2-smoke`.
- **Track B.1/B.2 (ASLR + stack canaries)** ✅ PR #310 (`8611d185`) —
  per-`execve` CSPRNG stack/mmap/PIE jitter, `-Z stack-protector=strong` +
  `syscall_lib::stack_protector`, gates `aslr-smoke` + `stack-smash-smoke`.
- Remaining Phase 110: Track A (KPTI), B.3 (CET), D (Secure Boot) — all
  **bare-metal-validation-gated** (QEMU models no speculation/CET/Secure-Boot),
  so they need the Dell. See `docs/handoffs/next-dell-session.md`.

## Next actions (suggested order)

1. **Track D.3** (`m3gdbserver`) + **D.4** (userspace symbol retention) per the
   "Track D" section above — the last Phase 111 increment, fully QEMU-testable
   (kernel is alive → ordinary TCP/AF_UNIX transport, no polled link). Reuse the
   `gdb_rsp` codec (it is `no_std`, so it links from userspace too). First wire
   the `execve`→SIGTRAP-stop so a fork+TRACEME+exec tracee stops before the new
   program runs.
2. Phase 110 A/B.3/D + the Phase 111 Track C/D on-metal arms — operator-owned.

### Async-break (Track C.4) — ✅ landed

GDB `0x03` (Ctrl-C) now breaks a *running* guest into the stub. The **BSP timer
tick** (`timer_handler_user`/`_kernel` → `kgdb_poll_async_break_*` in
`interrupts.rs`) polls COM2 (`com2::rx_pending()` guard → `try_read_byte`); on a
lone `0x03` it builds a `DebugTrapFrame` from the interrupted preempt frame and
calls `gdbstub::poll_async_break`, which sends the stop reply GDB is waiting for
and serves the session at the interrupted RIP. Register edits (`G`) are copied
back into the live preempt frame so the naked stub's `iretq` applies them.
Only the BSP polls (single COM2 reader); the per-tick cost (one LSR read) is
`#[cfg(feature = "kgdb")]` so production timers are untouched. Proven by
`kgdb-smoke` (after the breakpoint test: `z0` + `c`-run-free + bare `0x03` →
assert the async stop + a valid RIP).

**Gotcha (cost me a link error):** when inserting the two `#[cfg(kgdb)]` helper
`fn`s just above `timer_handler_user`, the `#[unsafe(no_mangle)]` that belonged
to `timer_handler_user` re-attached to the first helper — so the default build
linked (helpers `cfg`-out, `no_mangle` falls back onto `timer_handler_user`) but
the `kgdb` build failed with `undefined symbol: timer_handler_user` (the naked
`timer_entry` asm calls it by that raw name). Clippy doesn't link, so it passed;
always do a full `--bin kernel` link build with the feature. Fix: the helpers
live *below* the timer handlers now.
