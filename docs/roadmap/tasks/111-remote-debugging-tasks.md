# Phase 111 — Remote Debugging (Source-Level Kernel + Userspace): Task List

**Status:** Planned
**Source Ref:** phase-111
**Depends on:** Phase 3 (Interrupts/IDT) ✅, Phase 19 (Signal Handlers) ✅, Phase 25/35 (SMP + NMI-IPI) ✅, Phase 16/23 (TCP + Socket API) ✅, Phase 45 (Ports System) ✅
**Goal:** Land source-level remote debugging in three escalating tiers — free QEMU-gdbstub kernel debugging (A), a bare-metal in-kernel GDB stub (C) on a shared trap/debug-register substrate (B), and a `ptrace`-backed userspace debugger (D) — with the invasive tiers feature-gated off in production.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | QEMU gdbstub wiring + debug-info kernel build | — | ✅ **Landed** — `kdebug` profile (DWARF) + `cargo xtask debug` (QEMU `-s -S` + auto gdb script); RSP round-trip validated |
| B | Trap & debug-register substrate (`#DB`/`#BP`, TF, `DR0`–`DR7`, `int3` patch) | — | ✅ **Landed** — `#DB` registered + `#BP` dispatcher (RIP-fixup), `RFLAGS.TF` single-step, `DebugRegs` (`DR0`–`DR7`), `int3` patch; `kernel_core::debug_regs` host-tested + `debug-substrate-smoke` PASS |
| C | In-kernel GDB stub (kgdb) over polled COM2 + SMP all-stop | B | ✅ **Landed** — C.1 RSP codec (`kernel_core::gdb_rsp`, host-tested) + C.2–C.5 (full-GPR `#BP`/`#DB` naked entry, polled COM2, RSP command loop, SMP all-stop + panic hook, `kgdb` feature + `kgdb-smoke` gate) |
| D | `ptrace` syscall + stop/notify + `m3gdbserver` | B | Planned |

Track A is standalone and **pull-forward** (usable by the in-flight 101–110 bare-metal arc). C and D both consume B and are otherwise independent; either may be split into its own sub-phase (111a/111b) if scoped separately during implementation.

---

## Track A — QEMU gdbstub + debug-info build

### A.1 — Debug-info kernel build path ✅

**File:** `Cargo.toml` (`[profile.kdebug]`), `xtask/src/main.rs` (`build_kernel_debug` / `build_kernel_binary`).

**Acceptance:**
- [x] `[profile.kdebug]` inherits `release` but sets `lto = false` (LTO inlines away small functions, so `break <fn>` and named backtraces stop resolving) + `debug = "full"`. `build_kernel_debug()` produces `target/x86_64-unknown-none/kdebug/kernel` — an unstripped ELF with full `.debug_*` DWARF (49 MiB vs 16 MiB release; the extra is all non-PT_LOAD, so the booted image's loaded footprint is identical). Demanglable Rust symbols present (`kernel::kernel_main_entry`, `kernel::handle_panic`).
- [x] The default/production build (`--release`) is untouched — `build_kernel()` and every gate still build stripped/lean.

### A.2 — `cargo xtask debug` subcommand ✅

**File:** `xtask/src/main.rs` (`cmd_debug`).

**Acceptance:**
- [x] `cargo xtask debug` builds the `kdebug` kernel and launches QEMU with `-s -S` (gdbstub on `tcp::1234`, vCPU frozen at reset). Accepts the same `--device`/`--fresh` flags as `run`.
- [x] Prints the exact attach invocation **and** writes an auto-generated `m3os-kernel.gdb` (`gdb -q -x <script>`) that does `target remote :1234` + `add-symbol-file … -o 0x10000000000`.
- [x] **Charter correction:** the kernel ELF is **ET_DYN (PIE)** and the `bootloader` crate relocates it by a **fixed 1 TiB offset** (`0x10000000000`) — verified deterministic. So `add-symbol-file <elf> -o 0x10000000000` **is** required (the charter's "no KASLR → add-symbol-file unnecessary" was wrong: no KASLR, but a non-zero PIE base). Without the offset `break kernel_main_entry` resolves to the un-relocated vaddr and never fires.
- [x] Round-trip validated headlessly (no host gdb) via a raw RSP client: `?`→`T05` (halted at reset), `Z1`/`Z0` breakpoint at `kernel_main_entry + 0x10000000000`, `c`→stop with `RIP == 0x10000ccf500` exactly. A real gdb `break kernel_main_entry ; continue ; bt` follows the identical path with source lines.

---

## Track B — Trap & debug-register substrate

### B.1 — Register the `#DB` handler and upgrade `#BP`

**File:** `kernel/src/arch/x86_64/interrupts.rs` (IDT + handlers), `kernel/src/arch/x86_64/debug.rs` (dispatch).

**Acceptance:**
- [x] `#DB` (vector 1) is registered — `debug_exception_handler` reads + clears `DR6` and decodes it (single-step BS vs hw-breakpoint B0–B3) via `kernel_core::debug_regs`, then dispatches (`debug::on_debug_exception`).
- [x] `#BP` (vector 3) dispatches instead of print-and-return — the handler computes the breakpoint address as `RIP-1` (past the `0xCC`) and calls `debug::on_breakpoint`. Proven by the self-test: `DEBUG_SELFTEST:bp-rip ok addr=…` (the recorded address equals the `int3` instruction's address).
- [x] Ring-0 vs ring-3 routing **seam** in place (`from_user` branch in `on_breakpoint`/`on_debug_exception`); with no consumer registered the safe default logs + resumes (past `int3`, clearing any stray `TF`), so production is undisturbed.

### B.2 — Single-step and `DR0`–`DR7` wrapper ✅

**Files:** new `kernel-core/src/debug_regs.rs` (pure logic, host-tested), `kernel/src/arch/x86_64/debug.rs` (HW wrapper + single-step). *(Deviation: the pure-logic codec lives in `kernel-core` — host-testable — with the HW `mov %dr` access in the arch module, rather than a single `arch/debug_regs.rs`.)*

**Acceptance:**
- [x] `set_single_step`/`clear_single_step` flip `RFLAGS.TF` on a trap frame; the self-test proves **exactly one** `#DB` per step (`DEBUG_SELFTEST:single-step ok count=1`).
- [x] `DebugRegs` arms/disarms `DR0`–`DR3` + `DR7` via `dr7_slot_bits`, and `read_and_clear_dr6` decodes + clears the sticky status; ring-3 `mov %dr` still `#GP`s (unchanged). `DEBUG_SELFTEST:dr7 ok arm+disarm`.
- [x] Host tests in `kernel-core::debug_regs` cover the `DR7` enable/R-W/LEN encoding and `DR6` BS/B0–B3/BD/BT decode (7 tests, in `cargo xtask check`).

### B.3 — Software-breakpoint patch primitive ✅

**File:** `kernel/src/arch/x86_64/debug.rs` (`insert_sw_breakpoint` / `remove_sw_breakpoint`).

**Acceptance:**
- [x] `insert_sw_breakpoint(addr) -> u8` saves the original byte, writes `0xCC`, and `mfence`s so the patching core refetches; `remove_sw_breakpoint(addr, orig)` restores it. `# Safety` docs the caller-owned save/restore lifecycle (double-insert would save `0xCC` as the "original").
- [x] Works against a kernel virtual address (Track C). The tracee-address variant (via the tracee's page tables) lands with Track D's `POKETEXT`.

---

## Track C — In-kernel GDB stub (kgdb) over polled COM2

### C.1 — RSP packet codec

**Files:**
- `kernel-core/src/` (new `gdb_rsp.rs` — host-testable)
- `kernel/src/debug/gdbstub.rs` (new)

**Symbol:** `checksum` / `encode_packet` / `PacketReader` (`kernel_core::gdb_rsp`)
**Landed as:** `kernel-core/src/gdb_rsp.rs` — host-tested (**11 tests**). ✅

**Acceptance:**
- [x] `encode_packet` frames `$payload#cc` with the correct mod-256 checksum; `PacketReader` decodes incrementally, surfacing `Packet`/`BadChecksum`/`Ack`/`Nak`/`Interrupt` events. **The RSP checksum covers the raw on-wire body** (incl. `*`+RLE count), not the expanded payload — a subtlety the tests pin.
- [x] Handles `+`/`-` ack/nak, the `0x03` async-interrupt byte, and run-length `*` expansion on decode; plus the hex helpers (`hex_encode`/`hex_decode`/`parse_hex_prefix`) the `g`/`m`/`M` commands use.
- [x] Host tests in `kernel-core` cover framing, checksum round-trips (incl. GDB's `?`→`3f` / `OK`→`9a` examples), RLE, and the hex codecs.

> **Note (Track C sub-phasing):** C.1 landed as the standalone, host-tested wire-format foundation. C.2–C.5 (the stub command loop + naked-entry register frame, the polled COM2 transport, the SMP all-stop + panic hook, and the `kgdb` feature + gate) are interdependent — COM2 has no consumer without the stub, and the stub needs a naked-asm exception entry to capture the full GPR set for `g`/`G` — so they land together as the next Track C increment.

### C.2 — Stub command dispatch + register mapping ✅

**File:** `kernel/src/debug/gdbstub.rs`
**Symbol:** stub command loop (`session` / `dispatch`)
**Why it matters:** The command set (`?`, `g`/`G`, `m`/`M`, `c`/`s`, `Z0/z0`, `Z1/z1`, `qSupported`, `D`, `k`) plus the x86_64 GDB register ordering is what turns raw traps into an interactive session.

**Landed as:** `kernel/src/debug/gdbstub.rs` — the all-stop RSP loop, driven by the host-tested `kernel_core::gdb_rsp` codec over polled COM2.

**Acceptance:**
- [x] Reads/writes all GPRs + RIP/EFLAGS + segment selectors in GDB's amd64 order, mapped onto the naked-entry `DebugTrapFrame` (full-GPR capture — an `extern "x86-interrupt"` handler could not see the GPRs, so `#BP`/`#DB` moved to `bp_entry`/`db_entry` naked stubs). FPU/XSAVE deferred per charter.
- [x] `m`/`M` read/write kernel memory (canonical-address guarded; `M` writes with `CR0.WP` cleared so kernel text patches); `Z0/z0` use B.3 `insert/remove_sw_breakpoint` with a planted-breakpoint table (RIP rewound to the bp address on a planted hit, left past a compiled-in `int3`); `Z1/z1` use B.2 `DebugRegs`.
- [x] `c`/`s` resume/single-step (`s` sets `RFLAGS.TF`); `D`/`k` remove planted breakpoints and resume; unsolicited stop reply sent after a `c`/`s` re-stop, `?`-driven on the initial attach.

### C.3 — Polled COM2 transport ✅

**Files:**
- `kernel/src/debug/com2.rs` (new)
- `xtask/src/main.rs` (QEMU args)

**Symbol:** `com2::{init, try_read_byte, write_byte, write_all}`
**Why it matters:** At a breakpoint the kernel is frozen, so the IRQ-driven COM1 feeder and the IRQ-driven TCP stack are both dead; the stub must **poll** a dedicated UART. COM2 (`0x2F8`) is unused today in both kernel and QEMU args.

**Acceptance:**
- [x] Polled RX/TX on COM2 via LSR (`0x2FD`, bit 0 RX-ready / bit 5 THR-empty) — IER=0, no interrupts, no allocation.
- [x] COM1 remains the live console (`-serial stdio`); COM2 is a **second** `-serial tcp:127.0.0.1:<port>,server,nowait` (order: first serial = COM1, second = COM2), routed to a host TCP port the gate connects to.
- [x] A raw GDB-RSP client attaches over the COM2 TCP port (a real `gdb` `target remote` follows the identical wire path; the dev machine has no host `gdb`, so the gate is a raw-RSP driver).

### C.4 — SMP all-stop quiesce + panic→stub hook ✅

**Files:**
- `kernel/src/smp/mod.rs` (`kgdb_stop_all_aps` / `kgdb_release_aps` / `kgdb_ack_and_wait`)
- `kernel/src/arch/x86_64/interrupts.rs` (`nmi_handler` kgdb branch, `total_timer_ticks` sentinel)
- `kernel/src/lib.rs` (`handle_panic` → `enter_from_panic`)

**Symbol:** stub-entry quiesce, panic hook
**Why it matters:** A correct all-stop debugger must freeze every other core (reusing the TLB-shootdown NMI-IPI), and a panic that drops into the stub turns a dead bare-metal machine into an interactive post-mortem.

**Acceptance:**
- [x] On stub entry the other APs park in the NMI handler (spin until released — a **releasable** variant of the panic-quiesce path, reusing `send_nmi_to_core`), and `kgdb_release_aps` frees them on `c`/`s`/`D`/`k`. The stub logs a `KGDB:release … ticks_before=X ticks_after=Y` sentinel; the gate asserts `X == Y` (no core advanced while stopped, since every AP's LAPIC timer is frozen in the park loop).
- [x] The panic handler enters the stub before halting (feature-gated) via `enter_from_panic` (a fresh `int3` so the stub gets a real `DebugTrapFrame`); the panic AP-quiesce has already parked the siblings, so the stub's own all-stop finds none online.
- [x] Asynchronous break (GDB `0x03`/Ctrl-C into a *running* guest): the BSP timer tick (`timer_handler_user`/`_kernel` → `kgdb_poll_async_break_*`) polls COM2 for a lone `0x03` and, when present, builds a `DebugTrapFrame` from the interrupted preempt frame and breaks into the stub at the interrupted RIP (register edits copied back so the naked stub's `iretq` applies them). Only the BSP polls (single COM2 reader), and only under the `kgdb` feature (a per-tick LSR read guarded by `async_break_pending()`; production timers are untouched). Proven by `kgdb-smoke`: after the breakpoint test it `z0`+`c`-runs the guest free, sends a bare `0x03`, and asserts the unsolicited stop reply + a valid RIP.

### C.5 — `kgdb` feature gate + CI smoke ✅

**Files:**
- `kernel/Cargo.toml` (feature)
- `xtask/src/main.rs` (`cmd_kgdb_smoke`)
- `.githooks/pre-push` (`M3OS_KGDB_REGRESSION` gate)

**Symbol:** `kgdb` cargo feature
**Why it matters:** Arbitrary kernel memory peek/poke defeats W^X/PKU/capabilities; the stub must be build-time opt-in and off in production, like `panic-test`/`trace`/telnet.

**Acceptance:**
- [x] `kgdb` feature off by default; production image excludes the stub (the whole `kernel/src/debug/` module and the `on_breakpoint`/`on_debug_exception` stub routing are `#[cfg(feature = "kgdb")]`).
- [x] `kgdb-smoke` (`M3OS_KGDB_REGRESSION=1`): boots with `kgdb`, waits for the `KGDB:waiting` breadcrumb on COM1, connects a raw-RSP client to COM2, sets `Z0` at `kgdb_probe_target` (`nm` vaddr + `0x10000000000`), continues, asserts the stop with `RIP` at that address, reads back `KGDB_PROBE_MAGIC` over `m`, then `z0`+`c`-runs the guest free and sends a bare `0x03` to assert the **async-break** stop. Also asserts the all-stop sentinel.

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
