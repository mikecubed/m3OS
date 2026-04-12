# External Sources and References

All external claims in the architecture documents are traceable to these sources. Each entry includes what was verified and where it was used.

## m3OS Internal Sources

All m3OS code references are relative to commit `1d7a49c` (HEAD of `feat/phase-52`).

| Document | Source File | Lines | What It Documents |
|---|---|---|---|
| Bug analysis | `docs/appendix/copy-to-user-reliability-bug.md` | Full | copy_to_user intermittent failure |
| Bug analysis | `docs/appendix/sshd-hang-analysis.md` | Full | SSHD post-authentication hang |
| Comparison | `docs/appendix/redox-copy-to-user-comparison.md` | Full | Redox architecture lessons |
| MM | `kernel/src/mm/paging.rs` | 15-67 | Page table init, get_mapper() |
| MM | `kernel/src/mm/mod.rs` | 71-370 | MM init, new/free_process_page_table |
| MM | `kernel/src/mm/user_mem.rs` | 35-236 | copy_to_user, copy_from_user, demand fault |
| MM | `kernel/src/mm/frame_allocator.rs` | Full | Frame allocator, refcounts |
| MM | `kernel/src/mm/heap.rs` | Full | Kernel heap management |
| MM | `kernel/src/mm/slab.rs` | Full | Kernel slab caches (unused) |
| MM | `kernel-core/src/buddy.rs` | Full | Buddy allocator algorithm |
| MM | `kernel-core/src/slab.rs` | Full | Slab cache algorithm |
| Process | `kernel/src/process/mod.rs` | 75-1466 | Process struct, fork, thread group |
| Process | `kernel/src/task/mod.rs` | 100-308 | Task struct, switch_context, init_stack |
| Process | `kernel/src/task/scheduler.rs` | 75-1158 | Scheduler, pick_next, load balance |
| Process | `kernel/src/task/wait_queue.rs` | Full | WaitQueue primitive |
| Syscall | `kernel/src/arch/x86_64/syscall/mod.rs` | 789-911 | syscall_entry assembly |
| Syscall | `kernel/src/arch/x86_64/syscall/mod.rs` | 3449-3483 | restore_caller_context |
| Syscall | `kernel/src/arch/x86_64/syscall/mod.rs` | 2927-3303 | sys_fork, sys_execve |
| Syscall | `kernel/src/arch/x86_64/syscall/mod.rs` | 9930-10688 | clone_thread, futex |
| Syscall | `kernel/src/arch/x86_64/syscall/mod.rs` | 12704-13497 | poll, select, epoll |
| SMP | `kernel/src/smp/mod.rs` | 62-534 | PerCoreData, BSP/AP init |
| SMP | `kernel/src/smp/boot.rs` | Full | AP boot trampoline |
| SMP | `kernel/src/smp/ipi.rs` | Full | IPI vectors and handlers |
| SMP | `kernel/src/smp/tlb.rs` | Full | TLB shootdown protocol |
| IPC | `kernel/src/ipc/endpoint.rs` | 43-439 | Endpoint, rendezvous IPC |
| IPC | `kernel/src/ipc/notification.rs` | 60-367 | Notification objects |
| IPC | `kernel/src/ipc/mod.rs` | Full | IPC dispatch, bulk, registry |
| IPC | `kernel-core/src/ipc/capability.rs` | Full | CapabilityTable |
| IPC | `kernel-core/src/ipc/registry.rs` | Full | Service registry |
| TTY | `kernel/src/tty.rs` | Full | TTY0 global state |
| TTY | `kernel-core/src/tty.rs` | Full | Termios, EditBuffer |
| PTY | `kernel/src/pty.rs` | Full | PTY table, lifecycle |
| PTY | `kernel-core/src/pty.rs` | Full | PtyPairState, ring buffers |
| Terminal | `kernel/src/stdin.rs` | Full | Kernel stdin buffer |
| Terminal | `kernel/src/serial.rs` | Full | UART driver, serial RX |
| Terminal | `kernel/src/fb/mod.rs` | Full | Framebuffer console |
| Terminal | `kernel/src/main.rs` | 538-700 | serial_stdin_feeder_task |
| Terminal | `userspace/stdin_feeder/src/main.rs` | Full | Keyboard line discipline |
| Terminal | `userspace/kbd_server/src/main.rs` | Full | Keyboard IPC server |
| Async | `userspace/sshd/src/session.rs` | Full | SSH session architecture |
| Async | `userspace/async-rt/src/executor.rs` | Full | Async executor |
| Async | `userspace/async-rt/src/reactor.rs` | Full | I/O reactor |
| Async | `sunset-local/src/channel.rs` | 840-845 | wake_write() bug |

## Redox OS Sources

All Redox findings are verified from the GitHub mirror of the official GitLab kernel repository (`https://github.com/redox-os/kernel`, mirror of `https://gitlab.redox-os.org/redox-os/kernel`) and the analysis in `docs/appendix/redox-copy-to-user-comparison.md`.

| Claim | Source | Used In |
|---|---|---|
| `AddrSpaceWrapper` with `used_by: LogicalCpuSet` and `tlb_ack: AtomicU32` | `src/context/memory.rs` — `https://github.com/redox-os/kernel/blob/master/src/context/memory.rs` | next/01 (AddressSpace), next/04 (targeted TLB) |
| `UserSlice<const READ: bool, const WRITE: bool>` const-generic wrappers | `src/syscall/usercopy.rs` — `https://github.com/redox-os/kernel/blob/master/src/syscall/usercopy.rs` | next/02 (UserBuffer wrappers) |
| Low-level copy: `#[unsafe(naked)]` with `stac/clac` + `rep movsb`, runtime-patched via `alternative!` macro | `src/arch/x86_64/mod.rs` — `https://github.com/redox-os/kernel/blob/master/src/arch/x86_64/mod.rs` | current/01 (comparison) |
| `ProcessorControlRegion.user_rsp_tmp` is nanosecond-lifetime scratch only; syscall return value written to kernel stack frame (`(*stack).scratch.rax = ret`) | `src/arch/x86_64/interrupt/syscall.rs` — `https://github.com/redox-os/kernel/blob/master/src/arch/x86_64/interrupt/syscall.rs`; `src/arch/x86_shared/gdt.rs` | next/02 (task-owned state) |
| `arch::Context` stores `{rsp, rbx, r12-r15, rbp, fsbase, gsbase, rflags}` — callee-saved only | `src/context/arch/x86_64.rs` — `https://github.com/redox-os/kernel/blob/master/src/context/arch/x86_64.rs` | next/02 (context switch comparison) |
| `switch_arch_hook()` in `PercpuBlock` explicitly removes CPU from old `used_by`, issues SeqCst fence, then adds to new | `src/percpu.rs` — `https://github.com/redox-os/kernel/blob/master/src/percpu.rs` | next/04 (targeted TLB) |
| TLB shootdown uses per-AddrSpace `tlb_ack` counter with `wants_tlb_shootdown` per-CPU AtomicBool | `src/percpu.rs`, `src/context/memory.rs` | next/01, next/04 |
| `CaptureGuard` for page borrowing: unaligned head/tail copied to kernel `BorrowedHtBuf`, aligned middle pages grant-mapped with `is_pinned_userscheme_borrow = true` | `src/scheme/user.rs` — `https://github.com/redox-os/kernel/blob/master/src/scheme/user.rs` | next/01 (future bulk IPC) |
| Buddy allocator with 11 orders (`ORDER_COUNT = 11`), `PageInfo` with atomic refcount | `src/memory/mod.rs` — `https://github.com/redox-os/kernel/blob/master/src/memory/mod.rs` | current/01 (frame allocator comparison) |
| Global `CONTEXT_SWITCH_LOCK` AtomicBool serializes all switches | `src/context/switch.rs` — `https://github.com/redox-os/kernel/blob/master/src/context/switch.rs` | next/04 (scheduler comparison) |
| Pragmatic microkernel mediation via scheme routing | Redox documentation `doc.redox-os.org/book/`; LWN: `https://lwn.net/Articles/979524/` | next/README (design principles) |

### Redox Documentation URLs
- Kernel source (GitHub mirror): `https://github.com/redox-os/kernel`
- Kernel source (GitLab canonical): `https://gitlab.redox-os.org/redox-os/kernel`
- `https://www.redox-os.org/`
- `https://doc.redox-os.org/book/microkernels.html`
- `https://doc.redox-os.org/book/schemes.html`
- LWN: `https://lwn.net/Articles/979524/`
- DWRR scheduler: `https://www.phoronix.com/news/Redox-OS-New-CPU-Sched`

## seL4 Sources

All seL4 findings verified from official documentation (`docs.sel4.systems`) and source code on GitHub (`github.com/seL4/seL4`).

| Claim | Source | Used In |
|---|---|---|
| TCB contains all task state; no per-core mutable scratch for return values | `https://raw.githubusercontent.com/seL4/seL4/master/include/object/structures.h` (tcb_t struct); `https://docs.sel4.systems/Tutorials/threads.html` | next/02 (comparison) |
| No `copy_to_user`; IPC buffer validated via `lookupIPCBuffer()` capability chain | `https://raw.githubusercontent.com/seL4/seL4/master/src/kernel/thread.c`; `https://docs.sel4.systems/Tutorials/ipc.html` | current/01 (comparison) |
| Fast-path IPC: first 4 message words in hardware registers (rdi, rsi, rdx, rcx on x86_64) | `libsel4/sel4_arch_include/x86_64/sel4/sel4_arch/constants.h` (`seL4_FastMessageRegisters = 4`) | next/03 (IPC comparison) |
| Fast-path conditions: no extra caps, EPState_Recv, same domain, valid VSpace, priority check | `https://raw.githubusercontent.com/seL4/seL4/master/src/fastpath/fastpath.c` | next/03 (IPC comparison) |
| TLB bitmap embedded in unused PML4 entries: `tlb_bitmap_set/unset/get` per-VSpace per-CPU | `https://raw.githubusercontent.com/seL4/seL4/master/include/arch/x86/arch/kernel/tlb_bitmap.h` | next/04 (TLB comparison) |
| SMP uses CLH queue Big Kernel Lock (fair FIFO spinlock) | `https://raw.githubusercontent.com/seL4/seL4/master/include/smp/lock.h` | next/04 (scheduler comparison) |
| Notification: word-sized bitfield, 3-state (Idle/Waiting/Active), badge-OR accumulation | `https://raw.githubusercontent.com/seL4/seL4/master/src/object/notification.c`; `https://docs.sel4.systems/Tutorials/notifications.html` | next/03 (comparison) |
| `seL4_ReplyRecv` atomic reply+recv (single syscall) | `https://docs.sel4.systems/projects/sel4/api-doc.html`; `https://docs.sel4.systems/Tutorials/ipc.html` | next/03 (atomic reply_recv) |
| CNode/CSpace tree with MDB derivation tracking | `https://raw.githubusercontent.com/seL4/seL4/master/src/object/cnode.c`; `https://docs.sel4.systems/Tutorials/capabilities.html` | next/03 (comparison) |
| MCS scheduler: budget/period, passive servers, scheduling context donation | `https://docs.sel4.systems/Tutorials/mcs.html`; `include/object/structures.h` (sched_context_t) | next/04 (preemption) |
| Untyped memory: watermark allocation, no kernel heap, retype creates typed objects | `https://docs.sel4.systems/Tutorials/untyped.html` | current/01 (comparison) |
| Classic scheduler: 256-priority two-level bitmap, O(1) selection via clzl() | `https://raw.githubusercontent.com/seL4/seL4/master/src/kernel/thread.c`; `include/kernel/thread.h` | next/04 (scheduler comparison) |
| PCID support: `CONFIG_SUPPORT_PCID`, `INVPCID_TYPE_ADDR`, CR3 bit 63 for TLB preservation | `https://raw.githubusercontent.com/seL4/seL4/master/include/arch/x86/arch/64/mode/machine.h` | next/04 (TLB comparison) |
| Verified configurations: C-level functional correctness on ARM, ARM_HYP, AArch64, RISC-V64, x64 | `https://docs.sel4.systems/projects/sel4/verified-configurations.html` | general reference |

### seL4 Documentation URLs
- seL4 Reference Manual: `https://sel4.systems/Info/Docs/seL4-manual-latest.pdf`
- seL4 Tutorials: `https://docs.sel4.systems/Tutorials/`
- seL4 API: `https://docs.sel4.systems/projects/sel4/api-doc.html`
- seL4 source: `https://github.com/seL4/seL4`
- seL4 FAQ: `https://sel4.systems/About/FAQ.html`
- Lyons et al., "Scheduling-Context Capabilities," EuroSys 2018

## Zircon (Fuchsia) Sources

All Zircon findings verified from official Fuchsia documentation (`fuchsia.dev`) and Zircon source code on `fuchsia.googlesource.com`.

| Claim | Source | Used In |
|---|---|---|
| `VmAspace` inherits from `fbl::RefCounted<VmAspace>`, independently ref-counted first-class object | `https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/zircon/kernel/vm/include/vm/vm_aspace.h` | next/01 (AddressSpace comparison) |
| `VmAspace.active_cpus_` bitmap for targeted TLB shootdown; `mp_sync_exec()` for IPI dispatch | `https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/zircon/kernel/arch/x86/mmu.cc` | next/04 (TLB comparison) |
| PCID optimization: per-PCID `INVPCID` instruction, `active_cpus_` dirty bits defer flushes | `zircon/kernel/arch/x86/mmu.cc` | next/04 (TLB comparison) |
| ASLR: 36-bit max entropy, CSPRNG-seeded per-aspace PRNG (`aslr_prng_`), `ZX_VM_COMPACT` mode | `zircon/kernel/vm/vm_aspace.cc` | next/01 (AddressSpace comparison) |
| VMO: `VmObjectPaged` with COW clones, `GetPage()` for page faults, user pager support | `https://fuchsia.dev/fuchsia-src/reference/kernel_objects/vm_object`; `zircon/kernel/vm/include/vm/vm_object_paged.h` | current/01 (comparison) |
| VMAR hierarchy: non-overlapping child allocation, permission inheritance, `ZX_VM_CAN_MAP_SPECIFIC` | `https://fuchsia.dev/fuchsia-src/reference/kernel_objects/vm_address_region` | next/01 (VMA tree) |
| Handle: XOR-obfuscated arena index, `handle_table_id_` KOID check, `BrwLockPi` protected | `https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/zircon/kernel/object/handle_table.cc`; `zircon/kernel/object/include/object/handle.h` | next/03 (comparison) |
| Handle rights: immutable per-handle bitmask, narrowable only via `zx_handle_duplicate()` | `https://fuchsia.dev/fuchsia-src/concepts/kernel/handles` | next/03 (comparison) |
| Channel: datagram, bidirectional, 65KB/64 handles max, atomic handle transfer under `channel_lock_` | `https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_write`; `zircon/kernel/object/channel_dispatcher.cc` | next/03 (IPC comparison) |
| `zx_channel_call()`: synchronous request-response via `MessageWaiter` with txid matching | `zircon/kernel/object/channel_dispatcher.cc` | next/03 (IPC comparison) |
| Hybrid fair+deadline scheduler: `critical_deadline_run_queue_`, `deadline_run_queue_`, `fair_run_queue_` | `https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/zircon/kernel/kernel/scheduler.cc` | next/04 (scheduler comparison) |
| Fair scheduler: WAVL tree ordered by virtual finish time, augmented with min-finish-time | `zircon/kernel/kernel/scheduler.cc` | next/04 (scheduler comparison) |
| Work stealing: `StealWork()` with `CpuSearchSet` ordered by cache affinity | `zircon/kernel/kernel/scheduler.cc` | next/04 (work-stealing comparison) |
| Port: event aggregation, `zx_object_wait_async()` observer registration, edge/level triggered | `https://fuchsia.dev/fuchsia-src/reference/kernel_objects/port` | next/06 (future event port) |
| Futex: thread-owned, priority inheritance via waiter priorities, 6 syscall operations | `https://fuchsia.dev/fuchsia-src/reference/kernel_objects/futex` | current/03 (comparison) |
| `x86_percpu` via GS segment: `current_thread`, `saved_user_sp`, `last_user_aspace`, `blocking_disallowed` | `https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/zircon/kernel/arch/x86/include/arch/x86/mp.h` | next/02 (per-CPU comparison) |
| Dispatcher pattern: `DECLARE_DISPTAG` + `DownCastDispatcher<T>()` for type-safe syscall routing | `zircon/kernel/object/include/object/dispatcher.h` | general architecture reference |
| KOIDs: unique 64-bit IDs, never reused during system lifetime | `zircon/kernel/object/dispatcher.cc` | general architecture reference |
| Job hierarchy: recursive kill, policy enforcement, critical process designation | `https://fuchsia.dev/fuchsia-src/reference/kernel_objects/job` | general architecture reference |

### Zircon Documentation URLs
- Kernel concepts: `https://fuchsia.dev/fuchsia-src/concepts/kernel/concepts`
- Fair scheduler: `https://fuchsia.dev/fuchsia-src/concepts/kernel/fair_scheduler`
- Scheduling: `https://fuchsia.dev/fuchsia-src/concepts/kernel/kernel_scheduling`
- Zircon source: `https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/zircon/`

## MINIX3 Sources

All MINIX3 findings verified from the MINIX3 Wiki, GitHub source repository, and published academic papers.

| Claim | Source | Used In |
|---|---|---|
| 64-byte fixed messages (4 header + 56 payload union, 100+ named structures, `_ASSERT_MSG_SIZE()` verified) | `https://github.com/Stichting-MINIX-Research-Foundation/minix/blob/master/minix/include/minix/ipc.h`; `https://wiki.minix3.org/doku.php?id=developersguide:messagepassing` | next/03 (IPC comparison) |
| Synchronous rendezvous: `send`/`receive`/`sendrec` with `p_caller_q` linked-list queueing | `https://github.com/Stichting-MINIX-Research-Foundation/minix/blob/master/minix/kernel/proc.c`; `https://wiki.minix3.org/doku.php?id=developersguide:messagepassing` | next/03 (IPC comparison) |
| Async `NOTIFY` primitive: non-blocking, per-recipient bitmap, payload generated on reception | `https://wiki.minix3.org/doku.php?id=developersguide:messagepassing` | next/03 (notification comparison) |
| `deadlock()` function traverses `p_sendto_e`/`p_getfrom_e` chain; returns ELOCKED on cycle | `https://github.com/Stichting-MINIX-Research-Foundation/minix/blob/master/minix/kernel/proc.c` | general reference |
| Endpoint generation numbers: `endpoint = (generation * GENERATION_SIZE) + slot`; stale references rejected | `https://github.com/jncraton/minix3/blob/master/include/minix/endpoint.h` | general reference |
| Grant types: Direct, Indirect (chain depth 5, `MAX_INDIRECT_DEPTH`), Magic (VFS-only) | `https://github.com/Stichting-MINIX-Research-Foundation/minix/blob/master/minix/kernel/system/do_safecopy.c`; `https://wiki.minix3.org/doku.php?id=developersguide:memorygrants` | current/01 (comparison) |
| `sys_vircopy` for privileged cross-AS copy; `SYS_SAFECOPYFROM`/`SYS_SAFECOPYTO` for grant-validated copy | `https://github.com/jncraton/minix3/blob/master/lib/syslib/sys_vircopy.c`; `https://wiki.minix3.org/doku.php?id=developersguide:kernelapi` | current/01 (comparison) |
| VM server: userspace page fault handling via `RTS_PAGEFAULT` flag; `vir_region`/`phys_region`/`phys_block` hierarchy | `https://wiki.minix3.org/doku.php?id=developersguide:vminternals`; `https://github.com/jncraton/minix3/blob/master/servers/vm/pagefaults.c` | current/01 (comparison) |
| RS pings services periodically; auto-restarts on crash/timeout; SEF library for service lifecycle | `https://wiki.minix3.org/doku.php?id=www:documentation:reliability` | general reliability reference |
| 2.4 million fault injection: system survived all driver crashes | Herder et al., ACM SIGOPS OSR 2006, DOI: `10.1145/1151374.1151391`; verified via `https://research.vu.nl/en/publications/minix-3-a-highly-reliable-self-repairing-operating-system/` | general reliability reference |
| Live update: quiescence + slot swap + state transfer (identity/LLVM Magic) + hot rollback | `https://wiki.minix3.org/doku.php?id=developersguide:liveupdate` | general reliability reference |
| 16 priority queues; I/O-bound → head of queue; CPU-bound → tail; priority demotion/promotion/rebalancing | `https://minixnitc.github.io/scheduling.html` | next/04 (scheduler comparison) |
| Userspace SCHED server: kernel sends quantum-expired messages, SCHED decides priority/quantum via `sys_schedule()` | `https://wiki.minix3.org/doku.php?id=developersguide:userspacescheduling` | next/04 (scheduler comparison) |
| `RTS_*` flags: process runnable only when `p_rts_flags == 0`; each set bit is a suspension reason | `https://github.com/Stichting-MINIX-Research-Foundation/minix/blob/master/minix/kernel/proc.h` | general reference |
| VFS deadlock resolver: dedicated worker thread handles jobs from system processes when all normal threads blocked | `https://wiki.minix3.org/doku.php?id=developersguide:vfsinternals` | general reference |

### MINIX3 Documentation URLs
- MINIX3 Wiki: `https://wiki.minix3.org/`
- MINIX3 source: `https://github.com/Stichting-MINIX-Research-Foundation/minix`
- Herder et al., "MINIX 3: A Highly Reliable, Self-Repairing Operating System," ACM SIGOPS OSR, Vol. 40, No. 3, 2006, DOI: `10.1145/1151374.1151391`
- MINIX3 IPC: `https://minixnitc.github.io/ipc.html`
- MINIX3 Scheduling: `https://minixnitc.github.io/scheduling.html`

## Linux Kernel Sources (for comparison)

| Claim | Source | Used In |
|---|---|---|
| Maple tree VMA structure (v6.1+) | Linux commit `d4af56c5c7c6`; `include/linux/maple_tree.h` | next/01 (VMA tree comparison) |
| `__GFP_ZERO` for page zeroing | Linux `include/linux/gfp.h` | next/01 (frame zeroing comparison) |
| N_TTY line discipline | Linux `drivers/tty/n_tty.c` | next/05 (unified ldisc comparison) |
| Signal disposition reset on exec | POSIX.1-2017 Section 2.4.1; Linux `fs/exec.c` (`flush_signal_handlers`) | next/02 (exec signal fix) |
