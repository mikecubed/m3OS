# Phase 111 - Remote Debugging (Source-Level Kernel + Userspace)

**Status:** In progress — **Track A (QEMU gdbstub) + Track B (trap/debug-register substrate) landed** (`cargo xtask debug`; `#DB`/`#BP` dispatch + single-step + `DR0`–`DR7` + `int3` patch, `debug-substrate-smoke` PASS). Track C **C.1 (RSP codec) landed** (`kernel_core::gdb_rsp`, host-tested); C.2–C.5 (stub + COM2 + all-stop + gate) and Track D (ptrace + m3gdbserver) planned.
**Source Ref:** phase-111
**Depends on:** Phase 3 (Interrupts/IDT) ✅, Phase 19 (Signal Handlers) ✅, Phase 25/35 (SMP + NMI-IPI) ✅, Phase 16/23 (TCP + Socket API) ✅, Phase 45 (Ports System) ✅
**Builds on:** Reuses the existing IDT/exception substrate (`kernel/src/arch/x86_64/interrupts.rs`), the user/kernel trap frames (`kernel/src/arch/x86_64/preempt_trap_frame.rs`), the signal-frame machinery (`kernel/src/signal.rs`), the NMI-IPI quiesce path used for TLB shootdown, and the host-side `addr2line` symbolication convention from the Phase 43a crash diagnostics. Turns the long-deferred "gdb stub" item (`docs/44-rust-cross-compilation.md`, `docs/95-native-rust-toolchain.md`) into real, source-level debugging for both ring 0 and ring 3.

## Milestone Goal

A developer can set a breakpoint in a **Rust function name**, hit it, walk the stack, and inspect variables — for both the kernel and a ring-3 process — using an unmodified upstream `gdb` on the host. Three escalating capabilities ship: (A) free source-level **kernel** debugging inside QEMU via QEMU's own gdbstub; (B) an **in-kernel** GDB stub (kgdb-style) that debugs the kernel over a wire so the same workflow works **on bare metal**; and (C) a `ptrace`-backed **userspace** debugger so the OS can debug its own ring-3 programs (Python, `rustc`, the GUI clients) the way Linux does with `gdbserver`.

## Why This Phase Exists

Today the only debugging tools are post-mortem: panic register dumps (`kernel/src/lib.rs` `handle_panic`), the per-core trace ring (`kernel/src/trace.rs`), and a manual offline `addr2line` workflow anchored on `double_fault_handler` (`kernel/src/arch/x86_64/interrupts.rs`). There is **no live debugging at all**:

- `#DB` (debug exception, vector 1) is **not registered** in the IDT (`interrupts.rs` IDT init), so single-step and hardware breakpoints are impossible.
- The `#BP` handler (`breakpoint_handler`, `interrupts.rs`) prints a frame and returns — `int3` is a no-op beyond logging.
- `DR0`–`DR7` are never touched anywhere in the tree; `RFLAGS.TF` single-step is never set.
- `SIGTRAP` (5) is **defined but never generated** (`kernel/src/process/mod.rs`); a faulting ring-3 process is summarily killed via `fault_kill_trampoline` → SIGSEGV (`interrupts.rs`), with no notion of a debugger attaching, stopping, or inspecting it.
- There is no `ptrace` syscall and no debugger-attach process model.

The bare-metal arc (Phases 96–110) is precisely the kind of work — drivers, IRQ routing, ACPI, SMP races — where bare-metal source-level debugging pays for itself many times over. QEMU's gdbstub helps in the emulator but is blind on real silicon; an in-kernel stub closes that gap. And now that the bulk of m3OS runs in ring 3 (Phases 85–95: Python, Clang, Node, `rustc`, the compositor clients), the inability to debug a userspace crash is the single largest developer-experience gap left.

## Learning Goals

- The GDB Remote Serial Protocol (RSP): packet framing, checksums, the minimal command set, and the x86_64 register-transfer ordering.
- Hardware debug facilities on x86_64: `#BP` vs `#DB`, `RFLAGS.TF` single-step, the `DR0`–`DR7` debug registers, and `DR6`/`DR7` status/control semantics.
- Software breakpoints via `int3` (0xCC) patching and the RIP-fixup contract a debugger expects.
- Why an all-stop kernel debugger must run with the rest of the machine **frozen**, and how to quiesce other cores via NMI-IPI without deadlocking.
- The `ptrace` attach/stop/wait model: tracer vs tracee, stop-and-notify instead of kill, and peek/poke across an address space the tracer does not share.
- Why a debugger is a deliberate hole in the security model (arbitrary memory peek/poke defeats W^X, PKU, and capability gating) and must be a build-time feature, off in production — like `telnet`, `panic-test`, and `trace`.

## Feature Scope

### Track A — QEMU gdbstub + debug-info build (the near-free win)

Wire QEMU's built-in gdbstub (`-s -S`) into `xtask` and ship a kernel build that preserves DWARF. This gives full source-level **kernel** debugging in the emulator with **zero kernel code**. Independently shippable and **flagged pull-forward**: it should be available to whoever is working the 101–110 bare-metal arc, regardless of where this doc sits in the numbering. Limitation: emulator-only, and GDB sees raw CPU state (it follows whatever is executing; the kernel's higher-half mapping is present in every address space so kernel symbols always resolve, but userspace requires the right CR3 to be live).

### Track B — Trap & debug-register substrate (shared groundwork)

The exception-level plumbing both the in-kernel stub (C) and the userspace debugger (D) consume:

- Register a real `#DB` handler (vector 1) and route `#BP` (vector 3) to a dispatchable sink instead of print-and-return.
- Single-step support: set/clear `RFLAGS.TF` on a target trap frame and field the resulting `#DB`.
- A safe wrapper over `DR0`–`DR7` for hardware breakpoints/watchpoints, with `DR6` hit-decode and `DR7` enable/len/rw encoding. Enforce ring-0-only access (a ring-3 `mov` to a debug register already `#GP`s; keep it that way).
- Software-breakpoint primitive: patch/restore the original byte at an address behind `int3`, with the RIP-`-1` fixup GDB expects.

### Track C — In-kernel GDB stub over polled COM2 (kgdb)

A `kgdb`-feature-gated GDB RSP stub that debugs the **kernel itself** over a wire, working on bare metal:

- RSP packet engine + the core command set (`?`, `g`/`G`, `m`/`M`, `c`/`s`, `Z0/z0` sw breakpoints, `Z1/z1` hw breakpoints, `qSupported`, `D`, `k`), mapping GDB's x86_64 register order onto the kernel trap frame.
- A **polled** COM2 (`0x2F8`) transport — deliberately *not* the live COM1 console and *not* the IRQ-driven TCP stack, because at a breakpoint the kernel is frozen and only synchronous register-polling works. QEMU routes COM2 to a TCP port so GDB attaches over `target remote`.
- **All-stop on SMP:** on stub entry, NMI-IPI the other APs into a parked spin-wait; release them on continue. Reuses the existing TLB-shootdown NMI path.
- Entry triggers: software breakpoint, hardware breakpoint, single-step, asynchronous break (GDB `Ctrl-C`/`0x03` on the polled link), and — high value on bare metal — a **panic hook that drops into the stub** instead of halting, for live post-mortem.

### Track D — `ptrace` + userspace gdbserver

Debug ring-3 programs the OS runs, the Linux way:

- Generate `SIGTRAP` on ring-3 `int3` and on single-step completion, delivered to the **tracer**, not the tracee.
- A "traced" process state: a debug trap (or, opt-in, a fatal signal) **stops** the tracee and notifies the tracer via `wait`, instead of the current kill-via-`fault_kill_trampoline`.
- A `sys_ptrace(request, pid, addr, data)` syscall covering the practical subset: `TRACEME`/`ATTACH`/`DETACH`, `CONT`, `SINGLESTEP`, `PEEKTEXT`/`POKETEXT` (read/write the tracee's memory through its own page tables / VMA tree), `GETREGS`/`SETREGS` (the tracee's `SavedUserRegs` + trap frame), `GETSIGINFO`.
- A small native `m3gdbserver` (or a ported upstream `gdbserver`) translating RSP ↔ `ptrace`, talking to host GDB over TCP or AF_UNIX (the kernel is alive here, so ordinary IRQ-driven networking works — no polled transport needed).
- Symbol retention: ports strip userspace ELFs (`xtask/src/port_build.rs`, Phase 85a relocation contract); keep unstripped copies host-side (and optionally a `-g` debuggable build variant) so GDB has DWARF.

## Important Components and How They Work

### Debug-info build profile (Tracks A/C)

The release profile (`Cargo.toml` `[profile.release]`) is `lto = true`, `panic = "abort"`, and implicit `debuginfo = 0` — no DWARF. A debug-enabled build (e.g. a `release` override with `debug = 2`, optionally `split-debuginfo` so the booted image stays lean) produces an unstripped **host-side** kernel ELF. GDB is pointed at that ELF; the stub/QEMU only ever moves raw addresses and registers, so **symbolication lives entirely on the GDB client** — neither QEMU's stub nor the in-kernel stub needs any symbol table in the guest. There is no KASLR, so kernel symbol addresses are fixed and `add-symbol-file` is unnecessary for the kernel image itself.

### `#DB`/`#BP` handlers and the debug-register wrapper (Track B)

A registered `#DB` handler decodes `DR6` to distinguish single-step (BS), a hardware breakpoint hit (B0–B3), and other debug events, then dispatches to whichever consumer is active (the kernel stub for a ring-0 trap, the `ptrace` stop path for a traced ring-3 trap). The `#BP` handler stops print-and-returning and instead dispatches; for an `int3` software breakpoint it presents RIP at the breakpoint address (the CPU leaves RIP *after* the 0xCC, so the handler decrements by one before the debugger sees it). A thin `DebugRegs` abstraction owns `DR0`–`DR3` (linear addresses), `DR7` (per-slot enable + length + read/write/exec condition), and `DR6` (sticky hit status, cleared after decode).

### In-kernel stub core + polled COM2 (Track C)

The stub is an all-stop loop: it owns the CPU from trap-entry until the developer continues. It reads RSP packets by **polling** the COM2 LSR (`0x2FD`) for RX-ready and data register (`0x2F8`) — no interrupts, because the rest of the kernel (including COM1's IRQ-driven feeder task, `kernel/src/lib.rs`) is frozen. COM1 stays the live console; COM2 is unconfigured today in both the kernel and the QEMU args (`xtask/src/main.rs` wires only `-serial stdio`), so it is free to claim and route to a host TCP port. On entry the stub NMI-IPIs the other cores into a parked loop and records their state for `info threads`; on `c`/`s` it releases them. The panic handler (`kernel/src/lib.rs`) gains an optional pre-halt hook that enters the stub so a bare-metal panic becomes an interactive session instead of a dead machine.

### `ptrace` stop/notify and cross-address-space peek/poke (Track D)

Today a ring-3 fault is redirected to `fault_kill_trampoline` and the thread group is torn down with exit code `-11` (SIGSEGV). Track D inserts a check: if the faulting/ trapping task is **traced**, convert the event into a *stop* — freeze the tracee, record the stop reason, and wake the tracer's `wait`. `PEEKTEXT`/`POKETEXT` read and write the tracee's memory by walking its VMA tree (`kernel/src/process/mod.rs` `find_vma`) and page tables — the same machinery the page-fault handler and `copyfile` already use — because the tracer does not share the tracee's address space. `GETREGS`/`SETREGS` marshal the tracee's saved registers (`kernel/src/signal.rs` `SavedUserRegs`, plus the trap frame). Single-step sets `RFLAGS.TF` on the tracee's frame and relies on Track B's `#DB` handler to deliver the resulting `SIGTRAP` to the tracer.

## How This Builds on Earlier Phases

- **Extends Phase 3** by registering the long-absent `#DB` handler and upgrading the `#BP` handler from a logging stub to a real dispatcher.
- **Extends Phase 19** by finally generating `SIGTRAP` and adding a stop-and-notify path alongside the existing fatal-signal delivery, instead of unconditionally killing on a trap.
- **Reuses the Phase 25/35 NMI-IPI** TLB-shootdown plumbing for the SMP all-stop quiesce.
- **Reuses the Phase 16/23 TCP + AF_UNIX** stack for the userspace gdbserver transport; deliberately **avoids** it for the kernel stub (frozen-kernel constraint) in favor of polled COM2.
- **Reuses the Phase 43a** host-side `addr2line` symbolication convention — generalized from panic-only to live debugging by shipping a DWARF-bearing host ELF.
- **Reuses the Phase 45 ports** infrastructure to (optionally) port `gdbserver`, and revisits the Phase 85a strip step to retain symbols.

## Implementation Outline

1. **Track A:** add a debug-info kernel build path and a `cargo xtask debug` subcommand that launches QEMU with `-s -S` and prints the `gdb -ex 'target remote :1234' <host-elf>` invocation; document the workflow.
2. **Track B:** register the `#DB` handler, route `#BP` to a dispatcher with the RIP fixup, add the `RFLAGS.TF` single-step helper and the `DebugRegs` (`DR0`–`DR7`) wrapper, and the `int3` patch/restore primitive. Host-test the `DR6`/`DR7` encode/decode in `kernel-core`.
3. **Track C:** implement the RSP packet codec (host-tested in `kernel-core`), the polled COM2 driver, the stub command dispatch, the NMI-IPI all-stop, and the panic→stub hook, all behind a `kgdb` cargo feature. Add COM2→TCP to the QEMU args.
4. **Track D:** add the traced-process state + stop/notify in the fault/trap paths, the `sys_ptrace` syscall surface, `wait` integration, and the `m3gdbserver` (native or ported). Retain userspace DWARF host-side.
5. Feature-gate B/C/D off in production images; document the security posture.

## Acceptance Criteria

- **Track A:** `cargo xtask debug` boots the kernel halted; host `gdb` connects to `:1234`, `break <rust_fn>` hits, `bt` shows named Rust frames with source lines, `p <local>` reads a variable, `c`/`si` work.
- **Track B:** host tests in `kernel-core` cover RSP framing checksums and `DR6`/`DR7` encode/decode; a QEMU smoke confirms `int3` from kernel context enters the dispatcher with RIP at the breakpoint address (not after it), and that an `RFLAGS.TF` step raises exactly one `#DB`.
- **Track C:** with COM2 routed to a host TCP port, `gdb` attaches to the running guest, sets a kernel breakpoint, hits it, and reads registers/memory; at `-smp 8` no other core advances while stopped (verified by a sentinel); a deliberate panic drops into the stub. CI: a serial/QMP-driven smoke that scripts a minimal GDB session and asserts on a known breakpoint hit, behind `M3OS_KGDB_REGRESSION=1`.
- **Track D:** a ring-3 test program launched under `m3gdbserver` is debuggable from host `gdb` over TCP — breakpoint in `main`, read a local, single-step, continue to clean exit; a traced process that faults **stops and reports** to the tracer instead of being killed. CI smoke behind `M3OS_PTRACE_REGRESSION=1`.
- The `kgdb`/`ptrace` features are **off** in the default/production image; an enabled debug image is clearly marked.

## Companion Task List

- [Phase 111 Task List](./tasks/111-remote-debugging-tasks.md)

## How Real OS Implementations Differ

- **Linux KGDB/KDB** uses an IRQ-or-polled serial (or `kgdboe` over UDP via a polling NIC `poll_controller` op) and a full architecture-specific register description; it also supports `kdb`, an in-kernel non-GDB monitor. m3OS Track C starts polled-serial-only and all-stop-only (no non-stop mode).
- **Linux `ptrace`** is vastly larger (`PTRACE_SYSCALL`, `PTRACE_O_*` options, seccomp/exec/clone events, `PTRACE_GETREGSET` with the full XSAVE area, group-stop semantics). Track D ships the practical breakpoint/step/peek/poke subset; FPU/XSAVE register access and syscall-tracing are deferred.
- **Production GDB stubs** advertise an XML target description (`qXfer:features:read`) so GDB learns the exact register layout; the first cut can hardcode the x86_64-64 layout GDB already knows.
- **JTAG / hardware debug ports** (and Intel's DCI/USB-DbC) give true halt-the-CPU debugging independent of the OS — strictly more powerful than a software stub for early-boot and SMM issues, but require hardware/cabling m3OS does not assume.
- Mature systems ship **separate debuginfo packages** and a symbol server; m3OS keeps the unstripped ELF host-side next to the build.

## Deferred Until Later

- Non-stop / all-stop-optional debugging and per-thread continue.
- FPU/SSE/AVX (XSAVE) register access over the stub and via `ptrace`.
- `PTRACE_SYSCALL` syscall-entry/exit tracing and a `strace`-equivalent.
- Watchpoint expression support beyond raw `DR`-backed address watchpoints.
- A `kgdboe`-style network transport for the kernel stub (needs a polling NIC path).
- An in-kernel non-GDB monitor (Linux `kdb` analog) and on-device symbolication.
- XML target-description (`qXfer`) negotiation.
- Reverse debugging / record-replay (QEMU `-rr` could provide a Track-A-only flavor).
