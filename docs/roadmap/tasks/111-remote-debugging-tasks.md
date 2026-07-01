# Phase 111 — Remote Debugging (Source-Level Kernel + Userspace): Task List

**Status:** Planned
**Source Ref:** phase-111
**Depends on:** Phase 3 (Interrupts/IDT) ✅, Phase 19 (Signal Handlers) ✅, Phase 25/35 (SMP + NMI-IPI) ✅, Phase 16/23 (TCP + Socket API) ✅, Phase 45 (Ports System) ✅
**Goal:** Land source-level remote debugging in three escalating tiers — free QEMU-gdbstub kernel debugging (A), a bare-metal in-kernel GDB stub (C) on a shared trap/debug-register substrate (B), and a `ptrace`-backed userspace debugger (D) — with the invasive tiers feature-gated off in production.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | QEMU gdbstub wiring + debug-info kernel build | — | Planned |
| B | Trap & debug-register substrate (`#DB`/`#BP`, TF, `DR0`–`DR7`, `int3` patch) | — | Planned |
| C | In-kernel GDB stub (kgdb) over polled COM2 + SMP all-stop | B | Planned |
| D | `ptrace` syscall + stop/notify + `m3gdbserver` | B | Planned |

Track A is standalone and **pull-forward** (usable by the in-flight 101–110 bare-metal arc). C and D both consume B and are otherwise independent; either may be split into its own sub-phase (111a/111b) if scoped separately during implementation.

---

## Track A — QEMU gdbstub + debug-info build

### A.1 — Debug-info kernel build path

**File:** `Cargo.toml` (`[profile.release]` / new override)
**Symbol:** kernel build profile
**Why it matters:** The release profile is `lto = true` + implicit `debuginfo = 0` — there is no DWARF for GDB to resolve names against. A debug-info build is the prerequisite for every other track's symbolication.

**Acceptance:**
- [ ] A debug-info kernel build emits an unstripped host-side ELF with DWARF (`debug = 2`, optionally `split-debuginfo` to keep the booted image lean).
- [ ] The default/production image build is unchanged (still stripped/lean).

### A.2 — `cargo xtask debug` subcommand

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_debug` (new), QEMU arg builder
**Why it matters:** QEMU already exposes the guest CPU to GDB; the only missing piece is launching with `-s -S` and telling the developer how to attach. Zero kernel code for full in-emulator kernel debugging.

**Acceptance:**
- [ ] `cargo xtask debug` launches QEMU with `-s -S` (gdbstub on :1234, CPU halted at reset).
- [ ] It prints the exact `gdb -ex 'target remote :1234' <host-elf>` invocation (or writes a `.gdbinit`).
- [ ] Documented developer workflow: `break <rust_fn>` → `c` → `bt` shows named Rust frames with source.

---

## Track B — Trap & debug-register substrate

### B.1 — Register the `#DB` handler and upgrade `#BP`

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** IDT init (vector 1), `breakpoint_handler` (vector 3)
**Why it matters:** Vector 1 is currently **unregistered** and `breakpoint_handler` only prints-and-returns — so single-step, hardware breakpoints, and real `int3` breakpoints are all impossible today.

**Acceptance:**
- [ ] `#DB` (vector 1) has a handler that decodes `DR6` (single-step BS vs hw-breakpoint B0–B3) and dispatches to the active consumer.
- [ ] `#BP` (vector 3) dispatches instead of returning; for a software breakpoint it presents RIP at the breakpoint address (decrement past the 0xCC).
- [ ] A ring-0 trap routes to the kernel stub; a traced ring-3 trap routes to the `ptrace` stop path (seam in place even before C/D land).

### B.2 — Single-step and `DR0`–`DR7` wrapper

**File:** `kernel/src/arch/x86_64/` (new `debug_regs.rs`), `kernel/src/arch/x86_64/preempt_trap_frame.rs`
**Symbol:** `set_single_step` / `DebugRegs`
**Why it matters:** `RFLAGS.TF` and `DR0`–`DR7` are never touched anywhere in the tree; they are the hardware basis for stepping and watchpoints.

**Acceptance:**
- [ ] Set/clear `RFLAGS.TF` on a given trap frame; exactly one `#DB` results per step.
- [ ] `DebugRegs` encodes/decodes `DR7` (enable + len + rw) and `DR6` (sticky hit, cleared after read) for 4 slots; ring-3 access still `#GP`s.
- [ ] Host tests in `kernel-core` cover the `DR6`/`DR7` bit encoding.

### B.3 — Software-breakpoint patch primitive

**File:** `kernel/src/arch/x86_64/` (debug module)
**Symbol:** `insert_sw_breakpoint` / `remove_sw_breakpoint`
**Why it matters:** GDB `Z0`/`z0` breakpoints are implemented by writing `0xCC` and restoring the saved byte; the RIP-fixup contract must be exact or the debugged program corrupts.

**Acceptance:**
- [ ] Save original byte, write `0xCC`, restore on removal; idempotent and safe across the same address twice.
- [ ] Works against both a kernel address (Track C) and a tracee address via its page tables (Track D).

---

## Track C — In-kernel GDB stub (kgdb) over polled COM2

### C.1 — RSP packet codec

**Files:**
- `kernel-core/src/` (new `gdb_rsp.rs` — host-testable)
- `kernel/src/debug/gdbstub.rs` (new)

**Symbol:** `RspPacket` encode/decode
**Why it matters:** The Remote Serial Protocol framing (`$...#cc`, checksum, ack/nak) is the wire format every GDB client speaks; isolating it in `kernel-core` makes it host-testable without QEMU.

**Acceptance:**
- [ ] Encode/decode `$payload#checksum` with correct mod-256 checksum and ack/nak handling.
- [ ] Host tests in `kernel-core` cover framing, escaping, and checksum round-trips.

### C.2 — Stub command dispatch + register mapping

**File:** `kernel/src/debug/gdbstub.rs`
**Symbol:** stub command loop
**Why it matters:** The command set (`?`, `g`/`G`, `m`/`M`, `c`/`s`, `Z0/z0`, `Z1/z1`, `qSupported`, `D`, `k`) plus the x86_64 GDB register ordering is what turns raw traps into an interactive session.

**Acceptance:**
- [ ] Reads/writes all GPRs + RIP/RFLAGS in GDB's x86_64 order, mapped onto the kernel trap frame.
- [ ] `m`/`M` read/write kernel memory; `Z0/z0` use B.3; `Z1/z1` use B.2 debug registers.
- [ ] `c`/`s` resume/single-step; `D`/`k` detach/halt cleanly.

### C.3 — Polled COM2 transport

**Files:**
- `kernel/src/serial.rs` (or a new `kernel/src/debug/com2.rs`)
- `xtask/src/main.rs` (QEMU args)

**Symbol:** `Com2Polled`
**Why it matters:** At a breakpoint the kernel is frozen, so the IRQ-driven COM1 feeder and the IRQ-driven TCP stack are both dead; the stub must **poll** a dedicated UART. COM2 (`0x2F8`) is unused today in both kernel and QEMU args.

**Acceptance:**
- [ ] Polled RX/TX on COM2 via LSR (`0x2FD`) — no interrupts, no allocation.
- [ ] COM1 remains the live console (`-serial stdio`); COM2 added and routed to a host TCP port.
- [ ] Host `gdb` attaches over `target remote` to the COM2 TCP port.

### C.4 — SMP all-stop quiesce + panic→stub hook

**Files:**
- `kernel/src/arch/x86_64/interrupts.rs` (NMI-IPI path)
- `kernel/src/lib.rs` (`handle_panic`)

**Symbol:** stub-entry quiesce, panic hook
**Why it matters:** A correct all-stop debugger must freeze every other core (reusing the TLB-shootdown NMI-IPI), and a panic that drops into the stub turns a dead bare-metal machine into an interactive post-mortem.

**Acceptance:**
- [ ] On stub entry, other APs park in an NMI spin-wait; released on continue. At `-smp 8`, a sentinel confirms no other core advances while stopped.
- [ ] The panic handler optionally enters the stub before halting (feature-gated).
- [ ] Asynchronous break: GDB `0x03` (Ctrl-C) on the polled link interrupts a running guest into the stub.

### C.5 — `kgdb` feature gate + CI smoke

**Files:**
- `kernel/Cargo.toml` (feature)
- `xtask/src/main.rs` (`kgdb-smoke` step)

**Symbol:** `kgdb` cargo feature
**Why it matters:** Arbitrary kernel memory peek/poke defeats W^X/PKU/capabilities; the stub must be build-time opt-in and off in production, like `panic-test`/`trace`/telnet.

**Acceptance:**
- [ ] `kgdb` feature off by default; production image excludes the stub.
- [ ] A serial/QMP-driven smoke scripts a minimal GDB session and asserts a known kernel breakpoint hit, behind `M3OS_KGDB_REGRESSION=1`.

---

## Track D — `ptrace` + userspace gdbserver

### D.1 — Traced-process state + stop/notify

**Files:**
- `kernel/src/arch/x86_64/interrupts.rs` (`fault_kill_trampoline` path)
- `kernel/src/process/mod.rs` (process state, `SIGTRAP`)

**Symbol:** traced-stop conversion
**Why it matters:** Today a ring-3 trap/fault → `fault_kill_trampoline` → SIGSEGV kill (exit `-11`); `SIGTRAP` (5) is defined but never generated. A debugger needs the trap to **stop and notify**, not kill.

**Acceptance:**
- [ ] `SIGTRAP` is generated on ring-3 `int3` and on single-step completion, delivered to the **tracer**.
- [ ] A traced tracee that hits a debug trap (or, opt-in, a fatal signal) stops instead of being torn down.
- [ ] The tracer learns of the stop via `wait`/`waitpid` with a ptrace-stop status encoding.

### D.2 — `sys_ptrace` syscall surface

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_ptrace`
**Why it matters:** The attach/inspect/resume primitives a gdbserver maps RSP onto; peek/poke must cross into the tracee's address space, which the tracer does not share.

**Acceptance:**
- [ ] `TRACEME`/`ATTACH`/`DETACH`, `CONT`, `SINGLESTEP` implemented.
- [ ] `PEEKTEXT`/`POKETEXT` read/write tracee memory via its VMA tree (`find_vma`) + page tables.
- [ ] `GETREGS`/`SETREGS` marshal the tracee's `SavedUserRegs` + trap frame; `GETSIGINFO` returns the stop reason.

### D.3 — `m3gdbserver` (native or ported)

**Files:**
- `userspace/m3gdbserver/` (new) **or** `ports/devel/gdbserver/Portfile`
- `xtask/src/main.rs` (`bins` array, if native) / `xtask/src/port_build.rs` (if ported)

**Symbol:** RSP↔ptrace translator
**Why it matters:** Host GDB speaks RSP; the on-device server translates that to `sys_ptrace`. The kernel is alive during userspace debugging, so ordinary TCP/AF_UNIX transport works — no polled link needed.

**Acceptance:**
- [ ] A ring-3 test program launched under `m3gdbserver` is debuggable from host `gdb` over TCP: breakpoint in `main`, read a local, single-step, continue to clean exit.
- [ ] (If native) wired through all four userspace-binary registration points; (if ported) routed through the shared musl-toolchain plumbing.

### D.4 — Userspace symbol retention + CI smoke

**Files:**
- `xtask/src/port_build.rs` (Phase 85a strip step)
- `xtask/src/main.rs` (`ptrace-smoke` step)

**Symbol:** debuggable build variant
**Why it matters:** Ports strip ELFs (Phase 85a relocation contract), leaving no DWARF; GDB needs symbols, retained host-side.

**Acceptance:**
- [ ] Unstripped host-side copies (or a `-g` variant) retained for debuggable targets; the on-device/stripped artifacts are unchanged.
- [ ] A smoke scripts a gdb/`m3gdbserver` session over the in-kernel TCP stack asserting a known userspace stop, behind `M3OS_PTRACE_REGRESSION=1`.

---

## Documentation Notes

- This phase registers the `#DB` handler absent since Phase 3 and finally generates the `SIGTRAP` defined-but-unused since Phase 19 — call out both as substrate corrections, not just new features.
- Track C deliberately **does not** reuse the in-kernel TCP stack (Phase 16/23) for transport — document the frozen-kernel reason so a future contributor doesn't "optimize" COM2 into TCP.
- The `kgdb` and `ptrace` features widen the kernel's attack surface; document the production-off posture next to the existing `panic-test`/`trace`/telnet precedent.
- Prefer exact files/symbols; the `interrupts.rs` line numbers drift, so reference symbols (`breakpoint_handler`, `fault_kill_trampoline`, IDT init) over line numbers.
- Add the two `M3OS_*_REGRESSION` gates to the `AGENTS.md` regression-gate table and `docs/appendix/regression-gates.md` when C.5/D.4 land.
