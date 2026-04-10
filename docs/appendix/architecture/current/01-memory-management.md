# Current Architecture: Memory Management

**Subsystem:** Page tables, copy_to_user/copy_from_user, TLB management, frame allocator, slab allocator, address space management
**Key source files:**
- `kernel/src/mm/paging.rs` — page table initialization and accessor
- `kernel/src/mm/mod.rs` — MM init, per-process page table create/free
- `kernel/src/mm/user_mem.rs` — copy_to_user, copy_from_user, demand fault helpers
- `kernel/src/mm/frame_allocator.rs` — intrusive free list + buddy allocator
- `kernel/src/mm/heap.rs` — kernel heap (linked_list_allocator)
- `kernel/src/mm/slab.rs` — kernel slab cache wrappers
- `kernel/src/smp/tlb.rs` — TLB shootdown protocol
- `kernel-core/src/buddy.rs` — buddy allocator algorithm
- `kernel-core/src/slab.rs` — slab cache algorithm
- `kernel/src/arch/x86_64/interrupts.rs` — page fault handler, CoW resolution, demand paging

## 1. Overview

m3OS uses x86_64 4-level paging (PML4 -> PDPT -> PD -> PT -> 4 KiB pages). The bootloader provides a physical memory offset mapping that makes all physical memory accessible at `PHYS_OFFSET + phys_addr`. The kernel uses this direct mapping for all kernel-to-user data transfers — `copy_to_user` never writes through user virtual addresses.

Address spaces are not first-class objects. A process's address space is identified by a single `PhysAddr` value (the PML4 physical frame) stored in `Process::page_table_root`. The hardware CR3 register determines which address space is active.

## 2. Address Space Layout

```
Virtual Address Space (48-bit, 256 TiB)
================================================

0x0000_0000_0000_0000  ┌──────────────────────┐
                       │  (unmapped null page) │  Guard: < 0x1000 rejected
0x0000_0000_0040_0000  ├──────────────────────┤
                       │  User code + data     │  ELF loader: USER_VADDR_MIN
                       │  (PT_LOAD segments)   │
                       ├──────────────────────┤
0x0000_0002_0000_0000  │  Heap (brk)          │  BRK_BASE, grows upward
                       │  ↓                    │
                       ├──────────────────────┤
0x0000_0020_0000_0000  │  Anonymous mmap       │  ANON_MMAP_BASE, grows upward
                       │  ↓                    │
                       ├──────────────────────┤
                       │                      │
                       │  (unmapped gap)       │
                       │                      │
0x0000_7FFF_FF00_0000  ├──────────────────────┤
                       │  ↑                    │
                       │  User stack           │  ELF_STACK_TOP, grows downward
                       │  (64 pages pre-mapped)│  Demand-paged within 8 MiB
0x0000_8000_0000_0000  ├══════════════════════┤  ← Kernel/user boundary
                       │  Kernel half          │  PML4 entries 256-511
                       │                      │
0xFFFF_8000_0000_0000  │  Kernel heap          │  HEAP_START, 8-64 MiB
                       │                      │
                       │  Physical offset map  │  All phys RAM at PHYS_OFFSET+
                       │                      │
                       │  Kernel binary        │  Bootloader-mapped
0xFFFF_FFFF_FFFF_FFFF  └──────────────────────┘
```

## 3. Data Structures

### 3.1 Global State

```rust
// kernel/src/mm/mod.rs
static PHYS_OFFSET: Once<u64>;        // Virtual base of physical memory mapping
static KERNEL_PML4_PHYS: Once<u64>;   // Physical address of kernel's original PML4
```

`KERNEL_PML4_PHYS` is captured at boot from CR3 before any process CR3 is loaded. It is the authoritative reference for building new process page tables — never `Cr3::read()`, which could return a process's CR3 during a syscall.

### 3.2 Per-Process Address Space Fields

```rust
// kernel/src/process/mod.rs (Process struct, line 518)
pub struct Process {
    pub page_table_root: Option<x86_64::PhysAddr>,  // PML4 physical address; None = no AS
    pub brk_current: u64,           // Current heap break (0 = uninitialized)
    pub mmap_next: u64,             // Next VA for anonymous mmap (0 = uninitialized)
    pub mappings: Vec<MemoryMapping>, // Tracked anonymous/file VMAs
    // ...
}

pub struct MemoryMapping {
    pub start: u64,   // Page-aligned start VA
    pub len: u64,     // Page-aligned length
    pub prot: u64,    // PROT_READ | PROT_WRITE | PROT_EXEC
    pub flags: u64,   // MAP_PRIVATE | MAP_ANONYMOUS + kernel flags
}
```

**There is no dedicated `AddressSpace` struct.** An address space is simply a `PhysAddr` pointing to a PML4 frame plus the `Vec<MemoryMapping>` for VMA tracking. No generation counter, no refcount, no per-CPU usage tracking.

### 3.3 Frame Allocator

```rust
// kernel/src/mm/frame_allocator.rs
struct FrameAllocator {
    head: u64,            // Physical addr of first free frame (intrusive list)
    free_count: usize,
    total_frames: usize,
    phys_offset: u64,
    max_frame_number: u64,
    buddy: Option<BuddyAllocator>,  // Replaces intrusive list after heap init
}

static FRAME_ALLOCATOR: Mutex<FrameAllocator>;

// Per-frame reference counting (for CoW)
static REFCOUNT_TABLE: Once<Vec<AtomicU16>>;  // Indexed by frame number
```

### 3.4 Buddy Allocator

```rust
// kernel-core/src/buddy.rs
pub const MAX_ORDER: usize = 9;  // Order 0 = 4 KiB, Order 9 = 2 MiB

pub struct BuddyAllocator {
    free_lists: [Vec<usize>; MAX_ORDER + 1],  // Per-order free list (Vec as stack)
    bitmaps:    [Vec<u64>;   MAX_ORDER + 1],  // Per-order bitmap (1 = free)
    free_counts:[usize;      MAX_ORDER + 1],
    total_pages: usize,
}
```

### 3.5 Slab Allocator

```rust
// kernel-core/src/slab.rs
struct Slab {
    base: usize,            // Base address of backing page
    free_bitmap: Vec<u64>,  // 1 = free slot, 0 = allocated
    free_count: usize,
    total_slots: usize,
}

pub struct SlabCache {
    object_size: usize,
    page_size: usize,        // Always 4096
    slots_per_slab: usize,   // page_size / object_size
    slabs: Vec<Slab>,
}

// kernel/src/mm/slab.rs — defined but NOT YET USED
pub struct KernelSlabCaches {
    task_cache:     Mutex<SlabCache>,  // 512-byte objects
    fd_cache:       Mutex<SlabCache>,  // 64-byte objects
    endpoint_cache: Mutex<SlabCache>,  // 128-byte objects
    pipe_cache:     Mutex<SlabCache>,  // 4096-byte objects
    socket_cache:   Mutex<SlabCache>,  // 256-byte objects
}
```

### 3.6 TLB Shootdown State

```rust
// kernel/src/smp/tlb.rs
static SHOOTDOWN_ADDR: AtomicU64;      // Single address to invalidate
static SHOOTDOWN_PENDING: AtomicU8;    // Cores that haven't ack'd yet
static SHOOTDOWN_LOCK: spin::Mutex<()>; // Serializes concurrent shootdowns
```

## 4. Algorithms

### 4.1 `copy_to_user` — The Bug Path

```mermaid
flowchart TD
    A["copy_to_user(dst_vaddr, src)"] --> B{Validate address range}
    B -->|"< 0x1000 or > user boundary"| FAIL[Return Err]
    B -->|Valid| C[For each 4 KiB page span]

    C --> D["Create mapper from current CR3"]
    D --> E{"is_user_writable(page_base)?"}

    E -->|Yes| F["translate_addr(page_base) → PhysAddr"]
    E -->|"Not present"| G["try_demand_fault_writable(page_base)"]
    E -->|"Present but read-only"| H{"is_cow_page?"}

    G -->|Success| C
    G -->|Fail| FAIL

    H -->|Yes| I["resolve_cow_fault(page_base)"]
    H -->|No| FAIL

    I -->|Success| F
    I -->|OOM| FAIL

    F --> J["frame_virt = PHYS_OFFSET + phys + page_offset"]
    J --> K["copy_nonoverlapping(src, frame_virt, len)"]
    K --> L{More pages?}
    L -->|Yes| C
    L -->|No| OK[Return Ok]

    style K fill:#ff6666,color:#000
    style J fill:#ff6666,color:#000
```

**Critical design choice:** The write at step K goes through the kernel's physical-offset direct mapping (`PHYS_OFFSET + phys`), NOT through the user virtual address `dst_vaddr`. This means:
- The kernel bypasses user PTE flags (WRITABLE, USER_ACCESSIBLE) for the actual write
- The kernel writes to the physical frame it resolved via `translate_addr`
- If the physical frame resolution is wrong (stale page table, wrong CR3), the data goes to the wrong physical page
- Userspace reads through its own TLB-cached virtual-to-physical mapping, which may point to a different physical frame

**This is the mechanism behind the `copy_to_user` bug:** the kernel and userspace can disagree on which physical frame backs a given user virtual address.

### 4.2 `get_mapper()` — CR3-Dependent Page Table Access

```mermaid
flowchart LR
    A["get_mapper()"] --> B["Read CR3 register"]
    B --> C["phys_addr = CR3 & PML4 mask"]
    C --> D["virt = PHYS_OFFSET + phys_addr"]
    D --> E["Cast to &mut PageTable"]
    E --> F["OffsetPageTable::new(pml4, PHYS_OFFSET)"]
```

**Design implication:** `get_mapper()` always reflects the **currently loaded CR3**. During a syscall, this is the calling process's page table. If a context switch occurs between `get_mapper()` and the subsequent `translate_addr()`, the mapper would be over a stale CR3. This is safe in practice because `switch_context` only runs at explicit yield/block points, not asynchronously. However, it means the mapper is implicitly coupled to the current CPU's CR3 state.

### 4.3 New Process Page Table Creation

```mermaid
flowchart TD
    A["new_process_page_table()"] --> B["Allocate fresh PML4 frame"]
    B --> C["Zero the PML4 (4096 bytes)"]
    C --> D["Read kernel PML4 from KERNEL_PML4_PHYS"]
    D --> E["Shallow-copy PML4 entries 1..511<br/>(kernel half: heap, phys offset, kernel binary)"]
    E --> F{"PML4[0] present in kernel?"}

    F -->|No| DONE[Return PML4 frame]
    F -->|Yes| G["Deep-copy PML4[0]"]

    G --> H["Allocate new PDPT frame, zero it"]
    H --> I[For each present PDPT entry]
    I --> J{Huge page?}
    J -->|Yes| K["Shallow-copy PDPT entry as-is"]
    J -->|No| L["Allocate new PD frame"]
    L --> M["Copy all 512 PD entries from kernel PD"]
    M --> N["Install new PD in new PDPT<br/>PRESENT|WRITABLE|USER_ACCESSIBLE"]
    N --> I
    K --> I

    I -->|Done| O["Install new PDPT in PML4[0]"]
    O --> DONE

    style G fill:#ffcc00,color:#000
    style L fill:#ffcc00,color:#000
```

**Why deep-copy PML4[0]?** User code is mapped into the lower half (PML4[0]), starting at `USER_VADDR_MIN = 0x400000`. Without a private PD, ELF mappings in one process would contaminate the shared kernel PD entries.

**Latent aliasing hazard:** The deep copy allocates new PDPT and PD frames but shallow-copies PD entries (which point to PT frames). If a low-half PT is later contaminated with user leaves, `free_process_page_table()` could free that shared PT frame. This was identified as a latent risk in the `copy_to_user` investigation but is a weak fit for the observed high-stack reproducer.

### 4.4 Fork: CoW Page Cloning

```mermaid
sequenceDiagram
    participant Parent as Parent Process
    participant Kernel as sys_fork
    participant Child as Child Page Table

    Kernel->>Child: new_process_page_table() → fresh PML4
    Kernel->>Kernel: mapper_for_frame(child_cr3) — no CR3 switch

    loop For each user page (PML4 0..255)
        Kernel->>Kernel: Read parent PTE flags
        alt Page is writable
            Kernel->>Child: Map same frame with PRESENT|USER|BIT_9 (CoW)
            Kernel->>Parent: Clear WRITABLE, set BIT_9 in parent PTE
            Kernel->>Kernel: refcount_inc(frame)
        else Page is read-only
            Kernel->>Child: Map same frame with same flags
            Kernel->>Kernel: refcount_inc(frame)
        end
    end

    Kernel->>Parent: CR3 reload (local TLB flush only)
    Note over Parent: NO SMP shootdown issued<br/>Other cores may have stale<br/>WRITABLE TLB entries for parent
```

**SMP gap:** The CR3 reload at the end of `cow_clone_user_pages` flushes only the local CPU's TLB. If another CPU had cached a WRITABLE TLB entry for a page that is now read-only (CoW-marked), that CPU could write through the stale entry without triggering a page fault. With the current cooperative scheduler, a single process shouldn't run on two CPUs simultaneously, but this is a fragile invariant.

### 4.5 CoW Fault Resolution

```mermaid
flowchart TD
    A["Page fault: write to<br/>PRESENT + BIT_9 + !WRITABLE page"] --> B["resolve_cow_fault(vaddr)"]
    B --> C["Create mapper, translate vaddr → old_frame"]
    C --> D["Read refcount of old_frame"]
    D --> E{refcount == 1?}

    E -->|"Yes (sole owner)"| F["Set WRITABLE, clear BIT_9 in PTE"]
    F --> G["invlpg(vaddr) — LOCAL flush only"]

    E -->|"No (shared)"| H["Allocate new frame"]
    H --> I["Copy 4096 bytes: old_frame → new_frame<br/>(via PHYS_OFFSET direct mapping)"]
    I --> J["Map new frame: PRESENT|WRITABLE|USER|NX"]
    J --> K["invlpg(vaddr) — LOCAL flush only"]
    K --> L["refcount_dec(old_frame)"]
    L --> M{"old refcount → 0?"}
    M -->|Yes| N["free_frame(old_frame)"]
    M -->|No| O[Done]

    G --> O
    N --> O

    style G fill:#ffcc00,color:#000
    style K fill:#ffcc00,color:#000
```

**SMP concern:** Only local `invlpg` is issued after CoW resolution. If threads share an address space (CLONE_VM), another CPU could still have a stale read-only TLB entry. In the current model, CLONE_THREAD processes share the same CR3, so a CoW resolution on one CPU should be visible to others only after a TLB miss.

### 4.6 TLB Shootdown Protocol

```mermaid
sequenceDiagram
    participant Core0 as Core 0 (initiator)
    participant Lock as SHOOTDOWN_LOCK
    participant Core1 as Core 1
    participant Core2 as Core 2
    participant CoreN as Core N

    Core0->>Lock: Acquire mutex
    Core0->>Core0: invlpg(addr) — local flush
    Core0->>Core0: SHOOTDOWN_ADDR = addr
    Core0->>Core0: SHOOTDOWN_PENDING = online_cores - 1
    Core0->>Core1: IPI vector 0xFD
    Core0->>Core2: IPI vector 0xFD
    Core0->>CoreN: IPI vector 0xFD

    par All cores handle IPI
        Core1->>Core1: invlpg(SHOOTDOWN_ADDR)
        Core1->>Core1: SHOOTDOWN_PENDING.fetch_sub(1)
        Core2->>Core2: invlpg(SHOOTDOWN_ADDR)
        Core2->>Core2: SHOOTDOWN_PENDING.fetch_sub(1)
        CoreN->>CoreN: invlpg(SHOOTDOWN_ADDR)
        CoreN->>CoreN: SHOOTDOWN_PENDING.fetch_sub(1)
    end

    Core0->>Core0: Spin until SHOOTDOWN_PENDING == 0
    Core0->>Lock: Release mutex
```

**Limitations:**
1. **Single address per shootdown** — `SHOOTDOWN_ADDR` is one `u64`. A `munmap(ptr, 1 GiB)` requires 262,144 sequential shootdowns.
2. **Global serialization** — `SHOOTDOWN_LOCK` prevents concurrent shootdowns from different cores.
3. **Broadcast to ALL cores** — uses `send_ipi_all_excluding_self`, not targeted IPIs. Even cores not running the affected address space are interrupted.

### 4.7 Which Operations Issue TLB Invalidation

| Operation | Flush Type | SMP Shootdown? | Notes |
|---|---|---|---|
| `munmap` | `tlb_shootdown(page)` per page | Yes | Correct but O(pages) |
| `mprotect` | `tlb_shootdown(page)` per page | Yes | Correct but O(pages) |
| `fork` CoW marking | CR3 reload (local) | **No** | Gap: other CPUs may have stale WRITABLE entries |
| CoW fault resolution | `invlpg` (local) | **No** | Safe if process runs on one CPU at a time |
| Demand page mapping | `invlpg` (local) | **No** | Safe: new mapping, no stale entry possible |
| `brk` growth | `flush.flush()` (local) | **No** | Safe: new mapping |
| ELF loading | `flush.ignore()` | **No** | Safe: mapper is not the active CR3 |
| `exec` old PT free | CR3 switch to new PT | N/A | Old PT is unreachable |
| Heap growth | `flush.flush()` (local) | **No** | Kernel-only mapping |

### 4.8 Frame Allocation: Two-Phase Design

```mermaid
flowchart TD
    subgraph "Phase 1: Pre-Heap (Intrusive Free List)"
        A["init(memory_regions)"] --> B["Skip regions < 1 MiB"]
        B --> C["For each usable frame: push_frame(phys)"]
        C --> D["Write (next_ptr, FREE_MAGIC) at PHYS_OFFSET + phys"]
        D --> E["Frame list: head → frame1 → frame2 → ..."]
    end

    subgraph "Phase 2: Post-Heap (Buddy Allocator)"
        F["init_buddy()"] --> G["Drain all frames from free list into Vec"]
        G --> H["Sort frames for better coalescing"]
        H --> I["Create BuddyAllocator(total_pfns)"]
        I --> J["For each frame: buddy.free(pfn, 0)"]
        J --> K["Buddies auto-coalesce: order 0→1→2→...→9"]
    end

    subgraph "Allocation"
        L["allocate_frame()"] --> M{Buddy available?}
        M -->|Yes| N["buddy.allocate(0) → PFN"]
        M -->|No| O["pop_frame() from free list"]
        N --> P["refcount_inc(phys) → count = 1"]
        O --> P
        P --> Q["Return PhysFrame"]
    end

    subgraph "Free"
        R["free_frame(phys)"] --> S{Refcount init'd?}
        S -->|Yes| T["refcount_dec(phys)"]
        T --> U{count > 0?}
        U -->|Yes| V[Return — frame still shared]
        U -->|No| W["buddy.free(pfn, 0)"]
        S -->|No| W
    end
```

### 4.9 Buddy Allocator Algorithm

```mermaid
flowchart TD
    subgraph "allocate(order)"
        A1["Search order..=MAX_ORDER for free_counts[o] > 0"] --> A2["Pop PFN from free_lists[source_order]"]
        A2 --> A3["Clear bitmap bit"]
        A3 --> A4{source_order > order?}
        A4 -->|Yes| A5["Split: push upper buddy<br/>pfn ^ (1 << intermediate_order)<br/>onto intermediate order's free list"]
        A5 --> A4
        A4 -->|No| A6["Return PFN"]
    end

    subgraph "free(pfn, order)"
        F1["Double-free check: is_free(order, pfn)?"] --> F2{order < MAX_ORDER?}
        F2 -->|Yes| F3["Compute buddy = pfn ^ (1 << order)"]
        F3 --> F4{Buddy free AND in bounds?}
        F4 -->|Yes| F5["remove_free(order, buddy) — O(n) linear scan"]
        F5 --> F6["Recurse: free(min(pfn, buddy), order + 1)"]
        F4 -->|No| F7["push_free(order, pfn), set bitmap bit"]
        F2 -->|No| F7
    end
```

**Performance note:** `remove_free` does a linear scan of the free list at the given order to find and remove the buddy. For heavy churn at order 0, this could become noticeable.

## 5. Data Flow: Page Fault to Frame Allocation

```mermaid
sequenceDiagram
    participant User as Userspace
    participant PF as Page Fault Handler
    participant MM as mm::demand_map_user_page
    participant FA as FrameAllocator
    participant PT as Page Table

    User->>PF: Access unmapped VA (CR2)
    PF->>PF: Check: ring 3? CoW? Stack region? VMA?

    alt CoW page (PRESENT + BIT_9 + write fault)
        PF->>PF: resolve_cow_fault(vaddr)
        PF->>FA: allocate_frame()
        FA-->>PF: new PhysFrame
        PF->>PF: Copy old frame → new frame (4096 bytes)
        PF->>PT: Map new frame WRITABLE, clear BIT_9
        PF->>PF: invlpg(vaddr) — LOCAL only
        PF->>FA: refcount_dec(old_frame)
    else Stack demand (within 8 MiB of ELF_STACK_TOP)
        PF->>MM: demand_map_user_page(addr, PROT_READ|PROT_WRITE)
        MM->>FA: allocate_frame()
        FA-->>MM: new PhysFrame
        MM->>MM: Zero frame via PHYS_OFFSET
        MM->>PT: Install PTE: PRESENT|WRITABLE|USER|NX
        MM->>MM: invlpg(vaddr) — LOCAL only
    else VMA demand (address in proc.mappings)
        PF->>PF: find_vma(addr) → prot
        PF->>MM: demand_map_user_page(addr, prot)
        Note over MM: Same as stack demand path
    else No mapping
        PF->>PF: Kill process (SIGSEGV equivalent)
    end

    PF-->>User: IRET back to faulting instruction
```

## 6. Known Issues

### 6.1 No AddressSpace Object (Critical)

**Evidence:** `kernel/src/process/mod.rs:518` — `page_table_root: Option<PhysAddr>` is the entire address space identity. No struct wraps it.

**Impact:** Cannot track which CPUs are using an address space, cannot implement targeted TLB shootdowns, cannot attach generation counters or debug metadata. The `copy_to_user` bug investigation needed this tracking but had to use ad-hoc CR3 logging.

### 6.2 Frames Not Zeroed on Free

**Evidence:** `kernel/src/mm/frame_allocator.rs` — `free_to_pool` pushes the frame to the buddy without zeroing. The `FREE_MAGIC` sentinel is written at offset 8 for double-free detection but the rest of the frame retains prior contents.

**Impact:** If a stale TLB entry maps a VA to a freed-and-reused frame, userspace sees the new tenant's data, not zeros. The `copy_to_user` bug doc specifically identified this as an "amplifier" — a stale mapping can observe prior tenant contents.

### 6.3 Linear VMA Lookup in Page Fault Handler

**Evidence:** `kernel/src/mm/user_mem.rs:211` — `find_vma(page_base)` scans `proc.mappings` (a `Vec<MemoryMapping>`) linearly.

**Impact:** O(n) in the page fault handler, which is on the critical path for every demand-paged access. Applications with many mappings (shared libraries, mmap'd files) will have slow page faults.

### 6.4 mmap VA Space Never Reclaimed

**Evidence:** `kernel/src/arch/x86_64/syscall/mod.rs` — `mmap_next` only advances upward. `munmap` frees the frames and VMAs but does not rewind `mmap_next`.

**Impact:** Over time, the mmap VA range exhausts the 128 TiB user space in one direction. Long-lived processes with many mmap/munmap cycles will eventually fail.

### 6.5 Single-Address TLB Shootdown for Bulk Operations

**Evidence:** `kernel/src/smp/tlb.rs` — `SHOOTDOWN_ADDR` is one `AtomicU64`. `munmap` calls `tlb_shootdown()` once per unmapped page.

**Impact:** A `munmap(ptr, N)` is O(N/4096) IPIs, each requiring lock acquisition, IPI round-trip, and spin-wait. For large unmaps this is a significant performance bottleneck.

### 6.6 Fork CoW Has No SMP Shootdown

**Evidence:** `kernel/src/arch/x86_64/syscall/mod.rs:3662` — `Cr3::write(current_cr3, cr3_flags)` is a local CR3 reload. No call to `tlb_shootdown()` or `send_ipi_all_excluding_self()`.

**Impact:** If the parent process's pages are cached as WRITABLE in another core's TLB (shouldn't happen with current cooperative scheduling, but could with true preemption or CLONE_VM threads), that core could write through a stale entry.

### 6.7 Slab Caches Defined But Not Used

**Evidence:** `kernel/src/mm/slab.rs` — `KernelSlabCaches` is initialized in `slab::init()` but no kernel code allocates from it. Comment: "infrastructure for future migration (Phase 33, Track C.4 — deferred)".

**Impact:** All kernel objects (tasks, FDs, endpoints, pipes, sockets) use the global `linked_list_allocator` heap, which has higher fragmentation and contention than purpose-built slab caches.

### 6.8 `remove_free` in Buddy Allocator is O(n)

**Evidence:** `kernel-core/src/buddy.rs` — `remove_free(order, buddy)` does a linear scan of `free_lists[order]` to find and remove the buddy PFN.

**Impact:** Under heavy allocation/free churn with many blocks at the same order, buddy coalescing becomes slow. A more efficient data structure (e.g., doubly-linked intrusive list per order, or a hash set) would make this O(1).

### 6.9 No Page Reclaim, Swap, or NUMA Awareness

**Evidence:** No swap partition support, no page eviction mechanism, no OOM killer. `allocate_frame()` returns `None` on exhaustion. Frame allocation is global (single `FRAME_ALLOCATOR` spinlock).

**Impact:** Memory-constrained workloads fail with OOM rather than evicting cold pages. NUMA hardware would allocate remote frames with equal probability to local frames.

## 7. Comparison Points for External Kernels

| Aspect | m3OS Current | What to Compare |
|---|---|---|
| Address space identity | Raw `PhysAddr` (CR3 value) | Redox `AddrSpaceWrapper`, Zircon VMAR/VMO, seL4 VSpace |
| User-copy mechanism | Direct-mapping write via `PHYS_OFFSET + phys` | Redox `stac/clac` + `rep movsb`, Zircon `user_copy`, seL4 no copy (IPC registers) |
| TLB shootdown | Single-address, global lock, broadcast IPI | Redox per-address-space `used_by` + `tlb_ack`, Zircon targeted shootdown |
| Frame allocator | Buddy (order 0-9) + per-frame `AtomicU16` refcount | Redox frame allocator, seL4 Untyped, Zircon PMM |
| VMA tracking | Linear `Vec<MemoryMapping>` | Linux `maple_tree`, Zircon VMAR tree |
| Page zeroing | Caller-side (demand mapper, ELF loader) | Linux `__GFP_ZERO`, Zircon zero-on-alloc VMOs |
