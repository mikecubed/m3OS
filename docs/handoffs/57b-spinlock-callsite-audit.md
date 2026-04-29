# Phase 57b — Spinlock Callsite Audit (Track A.1)

**Status:** Complete
**Source Ref:** phase-57b-track-A.1
**Depends on:** Phase 57a
**Builds on:** `kernel/src/task/scheduler.rs::IrqSafeMutex`, the post-57a lock hierarchy
**Audit method:** Both scans run on `feat/57b-track-a` worktree.

```
rg -n 'spin::Mutex|spin::RwLock|use spin::Mutex|use spin::RwLock|IrqSafeMutex|BlockingMutex|Lazy<Mutex|Arc<Mutex' kernel/src kernel-core/src
rg -n '\.lock\(|\.try_lock\(|\.read\(|\.write\(' kernel/src kernel-core/src
```

The first scan returned 100 lines of declarations and import statements.  The second scan returned 607 raw rows for `.lock`/`.try_lock` and 166 raw rows for `.read`/`.write` — the vast majority of the latter are file/buffer reads (not RwLock acquisitions); the only kernel `spin::RwLock` callsites that matter are the four in `kernel/src/iommu/registry.rs`.

This audit is structured **per lock declaration** (the durable artefact future reviewers consult): each lock has a uniform classification across its acquisition sites because IRQ exposure is determined by the lock, not by the callsite.  The acquisition-site count column shows how many `.lock()` / `.read()` / `.write()` callsites the lock has across `kernel/` and `kernel-core/`.  Per-callsite line numbers are listed in the appendix at the end of each Track G section so a reviewer can jump directly from a Track G PR to the callsite list it must migrate.

---

## Classification Legend

- **already-irqsafe**: the lock is already an `IrqSafeMutex` (or wraps one — `SchedulerGuard`, `Task::pi_lock`).  Inherits Track F's `preempt_disable` integration automatically.  No per-callsite migration required.
- **convert-to-irqsafe**: a plain `spin::Mutex` whose every callsite runs in task context.  Track G converts it to `IrqSafeMutex`; preempt-discipline is then inherited from Track F.
- **explicit-preempt-and-cli**: a plain `spin::Mutex` whose contents are mutated from an ISR (or whose acquire path runs without IF being known to be 0).  Cannot be naively converted to `IrqSafeMutex` if any task-context callsite expects to keep IRQs enabled across the section; in practice every IRQ-shared lock in this kernel must run with IF=0 inside the critical section, so the migration shape is `without_interrupts(|| { let _g = LOCK.lock(); ... })` plus an explicit `preempt_disable` / `preempt_enable` pair around the whole region.  Both are required: `preempt_disable` does *not* substitute for IRQ masking on a same-core ISR-shared lock.
- **host-test-only**: the lock is declared in `kernel-core/` and is exercised by pure-logic tests on the host (`cargo test -p kernel-core`).  When `kernel-core` is consumed by the kernel build, the lock becomes kernel-context — but only if instantiated in kernel code.  The lock still inherits Track F via the kernel-side wrapper that holds it; the host-side path uses a no-op preempt stub.

---

## Audit Table

Columns: `file:line | symbol | lock kind | current wrapping pattern | context | acquisitions | classification | Track G owner`.

| file:line | symbol | lock kind | current wrapping pattern | context | acquisitions | classification | Track G owner |
|---|---|---|---|---|---|---|---|
| `kernel/src/task/scheduler.rs:272` | `SCHEDULER_INNER` | `IrqSafeMutex<Scheduler>` | naked `IrqSafeMutex` (acquire via `scheduler_lock()`) | task + IRQ-shared (taken from IRQ paths via `wake_task` / `signal_reschedule`) | ~21 inside `scheduler.rs` | already-irqsafe | G.6 (touchpoint only — no migration) |
| `kernel/src/task/mod.rs:300` | `Task::pi_lock` | `IrqSafeMutex<TaskBlockState>` | naked `IrqSafeMutex` | task (PI inheritance + block/wake) | ~5 across scheduler.rs + 1 in `task/mod.rs` | already-irqsafe | G.6 (touchpoint only) |
| `kernel/src/smp/mod.rs:194` | `PerCoreData::run_queue` | `spin::Mutex<VecDeque<usize>>` | callers wrap in `interrupts::without_interrupts` (per `kernel/src/arch/x86_64/interrupts.rs` doc-block) | IRQ-shared (taken from IRQ-context `signal_reschedule` and ISR-driven dispatch) | 12 (in `scheduler.rs`) | explicit-preempt-and-cli | G.8 (smp/arch) |
| `kernel/src/smp/tlb.rs:30` | `SHOOTDOWN_LOCK` | `spin::Mutex<()>` | bare `.lock()`; the function callers run from task context but TLB shootdown is invoked from `tlb_shootdown_ipi_handler` (IPI ISR) | IRQ-shared | 2 | explicit-preempt-and-cli | G.8 (smp/arch) |
| `kernel/src/stdin.rs:54` | `STDIN` | `Mutex<StdinState>` | bare `.lock()`; written by tty input which runs in task context, never from a same-core ISR (PS/2 ISR routes through `RAW_INPUT_ROUTER`, not STDIN) | task | 4 | convert-to-irqsafe | G.7 (misc) |
| `kernel/src/ipc/endpoint.rs:56` | `ENDPOINTS` | `Lazy<Mutex<EndpointRegistry>>` | bare `.lock()` in task context; explicit doc-comment requires hooks fire after lock release | task | 17 (across endpoint.rs / cleanup.rs / mod.rs / main.rs) | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/ipc/notification.rs:225` | `WAITERS` | `Mutex<[Option<TaskId>; MAX_NOTIFS]>` | bare `.lock()` in task context | task | 1 | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/ipc/notification.rs:232` | `ALLOCATED` | `Mutex<[bool; MAX_NOTIFS]>` | bare `.lock()` in task context | task | 2 | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/ipc/registry.rs:13` | `REGISTRY` | `Lazy<Mutex<Registry>>` | bare `.lock()` in task context | task | 8 | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/pty.rs:12` | `PTY_TABLE` | `Mutex<[Option<PtyPairState>; MAX_PTYS]>` | bare `.lock()` in task context | task | 6 | convert-to-irqsafe | G.7 (misc — pty/serial) |
| `kernel/src/syscall/device_host.rs:229` | `DEVICE_HOST_REGISTRY` | `Mutex<DeviceHostRegistry>` | bare `.lock()` — module doc-comment says "narrow `spin::Mutex` — no lock is held across IPC" | task | ~12 within device_host.rs | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/syscall/device_host.rs:404` | `MMIO_REGISTRY` | `Mutex<MmioRegistry>` | bare `.lock()` in task context | task | ~5 within device_host.rs | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/syscall/device_host.rs:439` | `IRQ_BINDING_REGISTRY` | `Mutex<IrqBindingRegistryCore>` | bare `.lock()` from task context (binding install/uninstall via syscall path); IRQ delivery side reads via separate dispatch table | task | 4 | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/syscall/device_host.rs:1903` | `DMA_REGISTRY` | `Mutex<DmaRegistry>` | bare `.lock()` in task context | task | ~6 within device_host.rs | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/syscall/device_host.rs:1908` | `IDENTITY_FALLBACK_LOGGED` | `Mutex<Vec<DeviceCapKey>>` | bare `.lock()` in task context (one-shot logging) | task | ~2 | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/blk/virtio_blk.rs:414` | `DRIVER` (virtio-blk) | `Mutex<Option<VirtioBlkDriver>>` | task-context callers wrap in `interrupts::without_interrupts(`); ISR side `virtio_blk_irq_handler` (`kernel/src/blk/virtio_blk.rs:449`) takes the same lock — explicit doc-comment at decl-site (line 437) | IRQ-shared (ISR: `virtio_blk_irq_handler`) | 12 | explicit-preempt-and-cli | G.1 (blk) |
| `kernel/src/blk/virtio_blk.rs:422` | `REQUEST_LOCK` | `Mutex<()>` | bare `.lock()` in task context only | task | 2 | convert-to-irqsafe | G.1 (blk) |
| `kernel/src/blk/remote.rs:39` | `REMOTE_BLOCK` | `Lazy<Mutex<RemoteBlockInner>>` | task-context only — userspace ring-3 driver facade; no kernel ISR | task | 14 | convert-to-irqsafe | G.1 (blk) |
| `kernel/src/tty.rs:30` | `TTY0` | `Mutex<TtyState>` | task-context only; serial input flows in via task path, framebuffer console writes from task | task | ~6 across tty/main/syscall | convert-to-irqsafe | G.7 (misc — pty/serial/tty) |
| `kernel/src/process/mod.rs:994` | `PROCESS_TABLE` | `Mutex<ProcessTable>` | bare `.lock()`; taken from page-fault exception handler (`terminate_current_process_segv`) — exception is task-context (CPL3 fault → kernel exception entry, not external IRQ) | task (incl. exceptions) | ~26 inside process/mod.rs + many more from syscall/scheduler | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/process/mod.rs:765` | `Process::shared_fd_table` | `Option<Arc<Mutex<[Option<FdEntry>; MAX_FDS]>>>` | bare `.lock()`; shared between `clone()`-with-`CLONE_FILES` parent + child | task | ~6 inside process/mod.rs | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/process/mod.rs:768` | `Process::shared_signal_actions` | `Option<Arc<Mutex<[SignalAction; 32]>>>` | bare `.lock()`; shared between `clone()`-with-`CLONE_SIGHAND` parent + child | task | ~3 inside process/mod.rs | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/process/futex.rs:47` | `FUTEX_TABLE` | `Lazy<Mutex<BTreeMap<FutexKey, Vec<FutexWaiter>>>>` | bare `.lock()` in task context; futex-wake from signal delivery is also task context | task | ~6 inside futex.rs | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/fs/fat32.rs:30` | `FAT32_PERMISSIONS` | `Mutex<BTreeMap<String, Fat32FileMeta>>` | bare `.lock()` in task context | task | 5 | convert-to-irqsafe | G.3 (fs) |
| `kernel/src/fs/fat32.rs:190` | `FAT32_VOLUME` | `Mutex<Option<Fat32Volume>>` | bare `.lock()` in task context | task | 6 | convert-to-irqsafe | G.3 (fs) |
| `kernel/src/fs/ext2.rs:59` | `EXT2_VOLUME` | `Mutex<Option<Ext2Volume>>` | bare `.lock()` in task context | task | 5 inside ext2.rs + ~10 from syscall | convert-to-irqsafe | G.3 (fs) |
| `kernel/src/fs/ext2.rs:?` | `Ext2Volume::block_cache` | `Mutex<...>` (member of Ext2Volume) | bare `.lock()` in task context | task | 5 inside ext2.rs | convert-to-irqsafe | G.3 (fs) |
| `kernel/src/fs/tmpfs.rs:28` | `TMPFS` | `Mutex<Tmpfs>` | bare `.lock()` in task context | task | 1 inside tmpfs.rs (consumers in syscall) | convert-to-irqsafe | G.3 (fs) |
| `kernel/src/fb/mod.rs:807` | `CONSOLE` | `Mutex<Option<FbConsole>>` | bare `.lock()`; never taken in ISR — panic path also runs at task context with IF off | task (panic-context) | 11 | convert-to-irqsafe | G.7 (misc — fb) |
| `kernel/src/arch/x86_64/ps2.rs:108` | `MOUSE_DECODER` | `Mutex<Ps2MouseDecoder>` | bare `.lock()`; declaration doc-comment notes the lock is "one-shot" but `keyboard_handler` ISR also reaches `ps2.rs::feed_byte` → `MOUSE_DECODER.lock()` | IRQ-shared (ISR: `keyboard_handler` / `mouse_handler` in `kernel/src/arch/x86_64/interrupts.rs`) | 3 | explicit-preempt-and-cli | G.8 (smp/arch — ps2 lives under arch/) |
| `kernel/src/net/virtio_net.rs:451` | `DRIVER` (virtio-net) | `Mutex<Option<VirtioNetDriver>>` | task callers wrap in `interrupts::without_interrupts`; ISR `virtio_net_irq_handler` (`kernel/src/net/virtio_net.rs:542`) takes the same lock — explicit doc at decl-site (line 529) | IRQ-shared (ISR: `virtio_net_irq_handler`) | 8 | explicit-preempt-and-cli | G.2 (net) |
| `kernel/src/net/tcp.rs:304` | `TCP_CONNS` | `Mutex<TcpConnections>` | bare `.lock()`; explicit doc at line 113 documents the post-release-and-flush pattern; never touched from ISR (NIC ISR routes through wake-queues) | task | 14 | convert-to-irqsafe | G.2 (net) |
| `kernel/src/net/arp.rs:79` | `ARP_CACHE` | `Mutex<ArpCache>` | bare `.lock()` in task context (handle_packet runs in net polling task) | task | 3 | convert-to-irqsafe | G.2 (net) |
| `kernel/src/net/udp.rs:12` | `UDP_BINDINGS` | `Mutex<UdpBindings>` | bare `.lock()` in task context | task | 5 | convert-to-irqsafe | G.2 (net) |
| `kernel/src/net/unix.rs:120` | `UNIX_SOCKET_TABLE` | `Mutex<UnixSocketTable>` | bare `.lock()` in task context | task | ~7 | convert-to-irqsafe | G.2 (net) |
| `kernel/src/net/unix.rs:249` | `UNIX_PATH_MAP` | `Mutex<BTreeMap<String, usize>>` | bare `.lock()` in task context | task | 2 | convert-to-irqsafe | G.2 (net) |
| `kernel/src/net/mod.rs:205` | `SOCKET_TABLE` | `Mutex<SocketTable>` | bare `.lock()` in task context (high-level socket dispatch) | task | 9 | convert-to-irqsafe | G.2 (net) |
| `kernel/src/net/remote.rs:85` | `REMOTE_NIC` | `Mutex<Option<NicEntry>>` | bare `.lock()` in task context — userspace NIC facade | task | 13 | convert-to-irqsafe | G.2 (net) |
| `kernel/src/serial.rs:12` | `SERIAL1` | `Mutex<Option<SerialPort>>` | bare `.lock()`; `serial_handler` ISR (`kernel/src/arch/x86_64/interrupts.rs:1127`) only EOIs the PIC and reads scancode-style data — it does NOT take SERIAL1; SERIAL1 is task-context only (kernel logs, panic) | task | 5 | convert-to-irqsafe | G.7 (misc — serial) |
| `kernel/src/serial.rs:13` | `DMESG_RING` | `Mutex<LogRing<DMESG_RING_SIZE>>` | bare `.lock()`; written from same path as SERIAL1 | task (panic/log-context) | 2 | convert-to-irqsafe | G.7 (misc — serial) |
| `kernel/src/pipe.rs:21` | `PIPE_TABLE` | `Mutex<Vec<Option<Pipe>>>` | bare `.lock()` in task context | task | 13 | convert-to-irqsafe | G.7 (misc — pipe) |
| `kernel/src/pipe.rs:26` | `PIPE_WAITQUEUES` | `Mutex<Vec<Option<WaitQueue>>>` | bare `.lock()` in task context | task | 5 | convert-to-irqsafe | G.7 (misc — pipe) |
| `kernel/src/pci/mod.rs:557` | `PCI_DEVICE_REGISTRY` | `Mutex<PciDeviceRegistry>` | bare `.lock()` in task context (enumeration / device-host registry) | task | 3 | convert-to-irqsafe | G.7 (misc — pci) |
| `kernel/src/pci/mod.rs:793` | `PCI_DEVICES` | `Mutex<PciDeviceList>` | bare `.lock()` in task context | task | 4 | convert-to-irqsafe | G.7 (misc — pci) |
| `kernel/src/pci/mod.rs:1291` | `MSI_POOL` | `Mutex<kpci::MsiVectorAllocator>` | bare `.lock()` in task context (MSI vector alloc) | task | 1 | convert-to-irqsafe | G.7 (misc — pci) |
| `kernel/src/pci/mod.rs:1610` | `DRIVER_REGISTRY` | `Mutex<DriverRegistry>` | bare `.lock()` in task context | task | 2 | convert-to-irqsafe | G.7 (misc — pci) |
| `kernel/src/mm/heap.rs:36` (member) | `Heap::state` | `Mutex<HeapState>` | bare `.lock()` from `KernelHeap` global allocator; never re-entered from ISR (allocator is not allowed in ISR) | task | 4 inside heap.rs | convert-to-irqsafe | G.4 (mm) |
| `kernel/src/mm/heap.rs:468` | `ALLOCATOR_RECLAIM_LOCK` | `Mutex<()>` | bare `.lock()` task-context only | task | 1 | convert-to-irqsafe | G.4 (mm) |
| `kernel/src/mm/heap.rs:678` | `GROW_HEAP_LOCK` | `Mutex<()>` | bare `.lock()` task-context only | task | 1 | convert-to-irqsafe | G.4 (mm) |
| `kernel/src/mm/slab.rs:?` (member) | `SlabCache::slab` | `Mutex<...>` | bare `.lock()` task-context | task | ~10 inside slab.rs + 1 in main.rs | convert-to-irqsafe | G.4 (mm) |
| `kernel/src/mm/slab.rs:349` | `SLAB_RECLAIM_LOCK` | `Mutex<()>` | bare `.lock()` task-context only | task | 1 | convert-to-irqsafe | G.4 (mm) |
| `kernel/src/mm/frame_allocator.rs:?` (`FRAME_ALLOCATOR.0`) | `FrameAllocator` | `Mutex<...>` | bare `.lock()`; allocation path taken from page-fault handler (CoW path), which is exception (task) context | task (exceptions) | 12 | convert-to-irqsafe | G.4 (mm) |
| `kernel/src/mm/frame_allocator.rs:789` | `CACHE_DRAIN_LOCK` | `Mutex<()>` | bare `.lock()` task-context | task | 1 | convert-to-irqsafe | G.4 (mm) |
| `kernel/src/mm/mod.rs:35` | `AddressSpace::page_table_lock` | `Mutex<()>` | bare `.lock()` task-context (per-AddressSpace) | task | 1 | convert-to-irqsafe | G.4 (mm) |
| `kernel/src/iommu/intel.rs:597` | `UNIT_SLOTS` | `spin::Mutex<[Option<usize>; MAX_FAULT_UNITS]>` | bare `.lock()`; `dispatch_fault_irq` (called from MSI fault ISR) acquires the lock to snapshot slot pointers | IRQ-shared (ISR: VT-d fault MSI handler — `dispatch_fault_irq`) | 2 | explicit-preempt-and-cli | G.5 (iommu) |
| `kernel/src/iommu/registry.rs:174` | `REGISTRY` (iommu) | `Mutex<Vec<RegisteredUnit>>` | bare `.lock()` task-context (registry lookups during device map/unmap) | task | 7 | convert-to-irqsafe | G.5 (iommu) |
| `kernel/src/iommu/registry.rs:183` | `REGISTERED` | `spin::RwLock<bool>` | bare `.read()` / `.write()` in task context (state-flag read on every map; written once during init) | task | 1 read, 1 write | convert-to-irqsafe (RwLock variant) | G.5 (iommu) |
| `kernel/src/iommu/registry.rs:187` | `TRANSLATING` | `spin::RwLock<bool>` | bare `.read()` / `.write()` in task context | task | 1 read, 1 write | convert-to-irqsafe (RwLock variant) | G.5 (iommu) |
| `kernel/src/iommu/amd.rs:919` | `UNITS` (amd-vi) | `Mutex<Vec<AmdViUnit>>` | bare `.lock()` task-context — fault dispatch path is currently a Track E TODO (no ISR handler installed today, see line 695 comment) | task | 0 (no current acquisitions outside amd.rs internals) | convert-to-irqsafe | G.5 (iommu) |
| `kernel/src/arch/x86_64/interrupts.rs:756` | `PICS` | `spin::Mutex<pic8259::ChainedPics>` | bare `.lock()`; **every callsite is itself in ISR context** (`PICS.lock().notify_end_of_interrupt(...)` at line 815, 992, 1063, 1134) — there is no task-context EOI path | IRQ-only (ISR-only) | 5 | explicit-preempt-and-cli (callsite already ISR — `preempt_disable` becomes the IRQ-context counter increment, IF is already 0) | G.8 (smp/arch) |
| `kernel/src/arch/x86_64/interrupts.rs:855` | `RAW_INPUT_ROUTER` | `spin::Mutex<ScancodeRouter>` | task callers wrap in `interrupts::without_interrupts` (per the comment at line 873); ISR `keyboard_handler` (line 920) takes the same lock | IRQ-shared (ISR: `keyboard_handler`, `mouse_handler`) | 4 | explicit-preempt-and-cli | G.8 (smp/arch — interrupts) |
| `kernel/src/arch/x86_64/interrupts.rs:1190` | `DEVICE_IRQ_TABLE` | `spin::Mutex<[Option<DeviceIrqEntry>; ...]>` | bare `.lock()`; install/uninstall path from task context, dispatch reads from `device_irq_stub_*` ISR | IRQ-shared (ISR: `device_irq_stub_*`) | 3 | explicit-preempt-and-cli | G.8 (smp/arch — interrupts) |
| `kernel/src/arch/x86_64/syscall/mod.rs:95` | `MOUNT_OP_LOCK` | `spin::Mutex<()>` | bare `.lock()` in task context | task | 2 | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/arch/x86_64/syscall/mod.rs:11980` | `ThreadGroup::members` | `spin::Mutex<Vec<...>>` | bare `.lock()` in task context (per-process member list) | task | ~9 inside syscall/mod.rs | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/arch/x86_64/syscall/mod.rs:12003`, `12024` | `Process::shared_fd_table` / `shared_signal_actions` instantiation site | `Arc<spin::Mutex<...>>` | constructor only; field acquisitions counted under `process/mod.rs:765/768` rows above | task | (constructors, no acquisitions here) | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/arch/x86_64/syscall/mod.rs:15213` | `EPOLL_TABLE` | `spin::Mutex<[Option<EpollInstance>; MAX_EPOLL_INSTANCES]>` | bare `.lock()` in task context | task | 7 | convert-to-irqsafe | G.6 (process/ipc/syscall) |
| `kernel/src/task/wait_queue.rs:?` | `WaitQueue::waiters` | `Mutex<VecDeque<WaitEntry>>` | bare `.lock()` in task context (block/wake API) | task | 7 | convert-to-irqsafe | G.7 (misc — task/wait_queue) |
| `kernel/src/ipc/cleanup.rs:42, 115` | (consumers of `ENDPOINTS`) | `Mutex<EndpointRegistry>` | bare `.lock()` task context | task | 2 (already counted above) | convert-to-irqsafe | G.6 (already counted) |
| `kernel-core/src/magazine.rs:?` | `MagazineDepot::full` / `MagazineDepot::empty` | `spin::Mutex<...>` | bare `.lock()`; kernel-side wrapper holds it; host-side tests call directly | task (kernel-build) / host-test-only (host-build) | 14 inside magazine.rs (incl. tests) | host-test-only on host build / convert-to-irqsafe via wrapper on kernel build | G.9 (kernel-core) |
| `kernel-core/src/device_host/registry_logic.rs` | (no lock declared in kernel-core; doc-comment references that the kernel wrapper holds a `spin::Mutex`) | n/a | n/a | host-test-only | 0 | host-test-only | G.9 (kernel-core) |

### Notes on counts

- Acquisition counts above are **per-lock totals across all files that acquire that lock**, not per-file totals.  Where a single lock is acquired from multiple files (e.g., `PROCESS_TABLE` from `process/mod.rs`, `arch/x86_64/syscall/mod.rs`, `arch/x86_64/interrupts.rs`, `task/scheduler.rs`, `fs/procfs.rs`, `main.rs`), the count is the sum.
- File `kernel/src/main.rs` is a consumer (7 `.lock()` calls); each acquires a lock declared elsewhere — accounted for under the source declaration.
- `kernel/src/ipc/mod.rs` similarly is a consumer (2 `ENDPOINTS.lock()` calls).
- `kernel/src/fs/procfs.rs` is a consumer of `PROCESS_TABLE` (3 `.lock()` calls).
- `kernel/src/task/scheduler.rs` is a consumer of `PROCESS_TABLE` (4 `.lock()` calls — at lines 991, 2575, 2631, 2788).

### Acquisition site appendix (per Track G owner)

Lines below are the union of `.lock()` / `.try_lock()` / RwLock `.read()` / `.write()` callsites grouped by the Track G PR that owns them.  Use this with `git blame` to drive each Track G migration.

**G.1 (blk):** `kernel/src/blk/virtio_blk.rs:451, 466, 492, 513, 531, 573, 634, 827`; `kernel/src/blk/virtio_blk.rs:449` (ISR `virtio_blk_irq_handler` body — also takes `DRIVER`); `kernel/src/blk/remote.rs` (14 callsites).

**G.2 (net):** `kernel/src/net/virtio_net.rs:501, 513, 550, 588, 653, 888`; `kernel/src/net/virtio_net.rs:542` (ISR `virtio_net_irq_handler`); `kernel/src/net/tcp.rs:308, 321, 332, 343, 354, 367, 379, 390, 398, 408, 433, 466`; `kernel/src/net/arp.rs:83, 105, 133`; `kernel/src/net/udp.rs:16, 21, 32, 37, 58`; `kernel/src/net/unix.rs:124, 139, 180, 191, 200, 214, 254, 264`; `kernel/src/net/mod.rs:205` and ~9 SOCKET_TABLE callsites; `kernel/src/net/remote.rs` (13 callsites).

**G.3 (fs):** `kernel/src/fs/fat32.rs:34, 46, 53, 67, 94, 126, 142, 814, 823, 827, 828`; `kernel/src/fs/ext2.rs:135, 154, 180, 201, 211, 1499, 1506, 1510, 1516, 1530`; `kernel/src/fs/tmpfs.rs:35`; `kernel/src/fs/procfs.rs:154, 213, 327` (consumers of PROCESS_TABLE — but FS owns the lock-flow audit for procfs).

**G.4 (mm):** `kernel/src/mm/heap.rs:253, 257, 261, 265, 484, 776`; `kernel/src/mm/slab.rs:368, 400, 420, 478, 602, 712, 729, 745, 790, 791, 792, 793, 794, 803, 826, 852, 862, 875, 885`; `kernel/src/mm/frame_allocator.rs:470, 493, 502, 561, 570, 593, 614, 737, 744, 752, 804, 842, 1020`; `kernel/src/mm/mod.rs:76`.

**G.5 (iommu):** `kernel/src/iommu/intel.rs:537, 904`; `kernel/src/iommu/registry.rs:200, 201, 202, 226, 234, 240, 254, 267, 288, 304, 317`.

**G.6 (process/ipc/syscall):** `kernel/src/process/mod.rs:88, 276, 372, 450, 845, 855, 865, 887, 897, 912, 933, 945, 954, 1054, 1062, 1072, 1147, 1208, 1273, 1288, 1312, 1392, 1449, 1651, 1674` (~26 callsites); `kernel/src/process/futex.rs` (6 callsites); `kernel/src/ipc/endpoint.rs:280, 306, 443, 542, 607, 618, 642, 692, 763, 795, 861, 980, 1007` (13); `kernel/src/ipc/cleanup.rs:45, 115`; `kernel/src/ipc/registry.rs` (8); `kernel/src/ipc/notification.rs:241, 261` (2 ALLOCATED + 1 WAITERS); `kernel/src/ipc/mod.rs:453, 461`; `kernel/src/syscall/device_host.rs` (~30 across DEVICE_HOST_REGISTRY/MMIO_REGISTRY/IRQ_BINDING_REGISTRY/DMA_REGISTRY/IDENTITY_FALLBACK_LOGGED); `kernel/src/arch/x86_64/syscall/mod.rs:11158, 11239` (MOUNT_OP_LOCK), `15230, 15241, 15250, 15269, 15352, 15430` (EPOLL_TABLE), `2128, 2237, 2311, 2353, 2440, 2453, 3859, 11961` (members); plus the PROCESS_TABLE acquisitions at `kernel/src/arch/x86_64/syscall/mod.rs:161, 170, 179, 217, 914, 1392, 1626, 1828, 1837, 1889, 1941`.

**G.7 (misc — pty/serial/tty/pipe/fb/pci/stdin):** `kernel/src/pty.rs:54, 63, 89, 97, 109, 141`; `kernel/src/serial.rs:18, 23, 31, 32, 96`; `kernel/src/tty.rs` (~6 across tty/main); `kernel/src/pipe.rs:30, 42, 43, 63, 68, 77, 88, 98, 111, 119, 129, 154, 177, 193, 202, 218`; `kernel/src/fb/mod.rs:867, 879, 894, 901, 917, 936, 950, 962, 978, 988, 1030`; `kernel/src/pci/mod.rs:494, 611, 802, 807, 872, 954, 1299, 1615, 1640, 1653`; `kernel/src/stdin.rs:64, 77, 85, 96`; `kernel/src/task/wait_queue.rs:49, 74, 82, 87, 99, 114, 120`.

**G.8 (smp/arch):** `kernel/src/smp/tlb.rs:78, 115`; `kernel/src/task/scheduler.rs:569, 663, 677, 771, 820, 2956, 2975, 3093` (`run_queue.lock()`); `kernel/src/arch/x86_64/interrupts.rs:117, 127, 767, 815, 881, 899, 946, 992, 1063, 1134, 1206, 1228, 1242`; `kernel/src/arch/x86_64/ps2.rs:156, 325, 345`.

**G.9 (kernel-core):** `kernel-core/src/magazine.rs:130, 133, 148, 151, 167, 175, 183, 188, 269, 288, 322, 325, 351, 352`; `kernel-core/src/device_host/registry_logic.rs` (no acquisitions in kernel-core; doc-only references to the kernel-side wrapper).

---

## Summary of classifications

- **already-irqsafe (no work):** 2 locks — `SCHEDULER_INNER`, `Task::pi_lock`.
- **convert-to-irqsafe:** 51 locks — the bulk of kernel state; each becomes `IrqSafeMutex` and inherits Track F's preempt-discipline.
- **explicit-preempt-and-cli:** 9 locks — IRQ-shared `spin::Mutex` callsites that ABI-bind to handlers and cannot trivially migrate to `IrqSafeMutex`: `PerCoreData::run_queue`, `SHOOTDOWN_LOCK`, `virtio_blk::DRIVER`, `virtio_net::DRIVER`, `MOUSE_DECODER`, `UNIT_SLOTS` (vt-d), `PICS`, `RAW_INPUT_ROUTER`, `DEVICE_IRQ_TABLE`.  Each row in the table cites the ISR that takes the same lock.
- **host-test-only:** 2 locks — `kernel-core::magazine::MagazineDepot::{full, empty}` (kernel-build path migrates with the wrapper; host-build path stays unchanged).

Total: 64 unique lock declarations classified.  ~607 `.lock()` / `.try_lock()` callsites all owned by exactly one Track G subtask via the table above.

---

## ISR cross-references for IRQ-shared classification

Required citation per A.1 acceptance: every `explicit-preempt-and-cli` row above lists the ISR that takes the same lock.  For convenience, here is the ISR table:

| Lock | ISR(s) |
|---|---|
| `PerCoreData::run_queue` | `signal_reschedule` (IRQ-context wake-side) and `reschedule_ipi_handler` (`kernel/src/arch/x86_64/interrupts.rs:1085`) |
| `SHOOTDOWN_LOCK` | `tlb_shootdown_ipi_handler` (`kernel/src/arch/x86_64/interrupts.rs:1107`) |
| `virtio_blk::DRIVER` | `virtio_blk_irq_handler` (`kernel/src/blk/virtio_blk.rs:449`) |
| `virtio_net::DRIVER` | `virtio_net_irq_handler` (`kernel/src/net/virtio_net.rs:542`) |
| `MOUSE_DECODER` | `mouse_handler` (`kernel/src/arch/x86_64/interrupts.rs:1011`) reaches `ps2.rs::feed_byte` |
| `UNIT_SLOTS` (vt-d) | `dispatch_fault_irq` (`kernel/src/iommu/intel.rs:533`), wired into MSI fault handler via `install_msi_irq` |
| `PICS` | every `extern "x86-interrupt" fn` in `kernel/src/arch/x86_64/interrupts.rs` that EOIs in PIC mode |
| `RAW_INPUT_ROUTER` | `keyboard_handler` (`kernel/src/arch/x86_64/interrupts.rs:920`) and `mouse_handler` (line 1011) |
| `DEVICE_IRQ_TABLE` | `device_irq_stub_0..15` (`kernel/src/arch/x86_64/interrupts.rs:1273..`) — read in ISR to dispatch |

---

## How this audit is consumed

- **Track F** (the `IrqSafeMutex` wiring change) gives every "already-irqsafe" and (after the Track G migration) every "convert-to-irqsafe" lock preempt-discipline for free.  Track F lands once; the per-callsite migration is nine `convert-to-irqsafe` PRs (G.1–G.9 minus G.8 split).
- **Track G PRs** consume the appendix line-number lists directly.  Each PR opens with "see `docs/handoffs/57b-spinlock-callsite-audit.md` row N", does its conversions, and runs the existing `cargo xtask test` plus the targeted regression test the Track G subtask requires.
- **Future audits** can re-run the two scans at the top of this file and diff against the row count here to detect newly-introduced locks that must be classified before the next phase merges.
