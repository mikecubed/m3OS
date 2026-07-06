# Handoff — Phase 111: Remote Debugging (Source-Level Kernel + Userspace)

**Date:** 2026-07-06 (living doc — update on every session working this phase)
**Branch:** `feat/phase-111-kgdb-stub` (Track C.2–C.5) — open PR to `main`.
**State:** IN PROGRESS.
- **Track A (QEMU gdbstub + debug-info build)** ✅ merged — PR #311 (`1c2645d3`).
- **Track B (trap & debug-register substrate)** ✅ merged — PR #312 (`f34d7fc9`).
- **Track C.1 (RSP wire codec)** ✅ merged — PR #313 (`0edfa7a2`).
- **Track C.2–C.5 (in-kernel stub core)** ✅ **landed on `feat/phase-111-kgdb-stub`**
  (full-GPR `#BP`/`#DB` naked entry, polled COM2, RSP command loop, SMP all-stop
  + panic hook, `kgdb` feature + `kgdb-smoke` gate PASS). Details below
  ("What's landed"); the old blueprint is preserved under "Implementation notes".
- **Track D (ptrace + m3gdbserver)** — NOT started. The remaining increment;
  sketch below.

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

## Track D — ptrace + m3gdbserver (sketch, not started)

- Generate `SIGTRAP` (defined-but-unused since Phase 19) on ring-3 `int3` +
  single-step completion, delivered to the **tracer**.
- Convert the ring-3 `fault_kill_trampoline` path (`interrupts.rs`) to a
  **stop-and-notify** for a traced tracee (freeze + `wait` notify) instead of
  the SIGSEGV kill.
- `sys_ptrace(request,pid,addr,data)`: `TRACEME/ATTACH/DETACH/CONT/SINGLESTEP`,
  `PEEKTEXT/POKETEXT` (walk the tracee's VMA tree + page tables —
  `process/mod.rs` `find_vma`; B.3's sw-breakpoint patch gets its tracee variant
  here), `GETREGS/SETREGS` (`signal.rs` `SavedUserRegs` + trap frame),
  `GETSIGINFO`.
- `userspace/m3gdbserver` (native, four-place wiring) translating RSP↔ptrace
  over TCP/AF_UNIX (kernel is alive here — no polled transport needed; reuse the
  `gdb_rsp` codec — it's in kernel-core, also linkable from userspace? it's
  no_std, so yes if exposed). Gate `ptrace-smoke` (`M3OS_PTRACE_REGRESSION=1`).

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

1. **Track D** (ptrace + m3gdbserver) per the sketch above — the last Phase 111
   increment, fully QEMU-testable (kernel is alive → ordinary TCP/AF_UNIX
   transport, no polled link). Reuse the `gdb_rsp` codec (it is `no_std`, so it
   links from userspace too).
2. Phase 110 A/B.3/D + the Phase 111 Track C/D on-metal arms — operator-owned.

### Async-break follow-on (Track C.4, deferred)

The one Track C acceptance item not yet wired: GDB `0x03` (Ctrl-C) breaking a
*running* guest into the stub. The `gdb_rsp` reader already surfaces
`RspEvent::Interrupt`; what's missing is a poll of COM2 for `0x03` from the
idle/timer path (the stub is only ever entered from a trap today). Low risk,
additive — a good warm-up before Track D.
