# Memory Management

## Overview

Memory management is one of the first things the kernel must set up after boot.
It has three layers:

1. **Physical frame allocator** — tracks which 4 KiB physical pages are free
2. **Page table manager** — maps virtual addresses to physical frames, enforcing isolation
3. **Kernel heap allocator** — provides `alloc` support (`Vec`, `Box`, etc.) inside the kernel

---

## Physical Memory Layout (at boot)

The `bootloader` crate provides a memory map via `BootInfo::memory_regions`. The kernel
must use this to determine which physical frames are usable.

```mermaid
graph TD
    A["BootInfo::memory_regions\n(list of MemoryRegion)"] --> B{Region kind?}
    B -->|Usable| C["Add to free frame list"]
    B -->|Reserved / ACPI / etc.| D["Skip — do not touch"]
    C --> E["FrameAllocator\n(bitmap or free-list)"]
    E --> F["allocate_frame() → PhysFrame"]
    E --> G["deallocate_frame(PhysFrame)"]
```

---

## Physical Frame Allocator

### Phase 2 implementation: bump allocator

The Phase 2 frame allocator is a **bump allocator** — allocate-only, no free:

- Iterates `BootInfo::memory_regions` in order, skipping non-`Usable` regions
- Hands out 4 KiB-aligned frames one at a time by advancing a pointer
- Never returns a frame once allocated (no deallocation)

```rust
pub fn allocate_frame() -> Option<PhysFrame<Size4KiB>> {
    FRAME_ALLOCATOR.0.lock().allocate()
}
```

**Limitations:**
- Cannot reclaim frames — memory consumed during init is gone forever
- Not suitable for process termination or page-out (Phase 4+)
- A single freed frame from the middle of a usable region is unrecoverable

### Concepts: physical frames vs virtual pages vs kernel heap

| Concept | What it is | How the kernel uses it |
|---|---|---|
| **Physical frame** | A 4 KiB-aligned region of RAM, identified by its physical address | Tracked by the frame allocator; handed to `map_to` when creating mappings |
| **Virtual page** | A 4 KiB-aligned region of virtual address space | What the kernel and userspace programs actually use; backed by a physical frame via page tables |
| **Kernel heap** | A fixed virtual address range (`0xFFFF_8000_0000_0000`, 1 MiB) with physical frames mapped behind it | Where `Box`, `Vec`, `Arc`, `String` allocate their backing memory |

The frame allocator works in *physical* space. The page mapper works across the *physical↔virtual* boundary. The heap allocator works entirely in *virtual* space, carving up the already-mapped heap region.

### Future allocator evolution

Mature kernels replace the bump allocator in stages:

1. **Buddy allocator** — splits/merges power-of-two frame blocks; O(log n) alloc/free; easy to reclaim
2. **SLAB/SLUB allocator** — small-object caching on top of buddy; amortizes `kmalloc` overhead
3. **Huge pages** — 2 MiB or 1 GiB mappings; fewer TLB entries, better throughput for large buffers
4. **Demand paging / copy-on-write** — don't map physical frames until first access; enables fork() efficiency

For ostest, the bump allocator is sufficient through Phase 5 (userspace entry). Phase 6+ IPC page grants will require a proper frame reclaim path.

```
Physical Memory
┌──────────────────┐ 0x0000_0000
│ First 1 MiB      │  ← BIOS/UEFI reserved, mostly off-limits
├──────────────────┤ 0x0010_0000
│ Kernel image     │  ← loaded by bootloader
│ (code + data)    │
├──────────────────┤
│ Bootloader data  │  ← BootInfo, page tables set up by bootloader
├──────────────────┤
│                  │
│  Usable RAM      │  ← managed by frame allocator
│                  │
├──────────────────┤
│ MMIO / PCI       │  ← memory-mapped hardware registers
└──────────────────┘ top of RAM
```

---

## x86_64 Virtual Memory — 4-Level Paging

x86_64 uses a 4-level page table hierarchy. Each level is a 512-entry table of 64-bit
entries. A virtual address is split into 5 fields:

```
Virtual Address (48 bits used):
 ┌────────┬────────┬────────┬────────┬─────────────┐
 │  PML4  │  PDPT  │   PD   │   PT   │   Offset    │
 │ [47:39]│ [38:30]│ [29:21]│ [20:12]│   [11:0]    │
 │  9 bits│  9 bits│  9 bits│  9 bits│   12 bits   │
 └────────┴────────┴────────┴────────┴─────────────┘
      ↓         ↓        ↓        ↓
    PML4      PDPT      PD       PT
   (L4)      (L3)     (L2)     (L1)
```

```mermaid
graph LR
    CR3["CR3\n(physical addr\nof PML4)"] --> PML4["PML4\n512 entries"]
    PML4 -->|"entry[i]"| PDPT["PDPT\n512 entries"]
    PDPT -->|"entry[j]"| PD["Page Directory\n512 entries"]
    PD -->|"entry[k]"| PT["Page Table\n512 entries"]
    PT -->|"entry[l]"| PHYS["Physical Frame\n4 KiB"]

    style CR3 fill:#c0392b,color:#fff
    style PHYS fill:#27ae60,color:#fff
```

### Physical Memory Offset Mapping

The `bootloader` crate sets up an **offset mapping**: the entire physical memory is
mapped starting at a configurable virtual address (`physical_memory_offset`). This means
to access a physical address `P`, you just read from `physical_memory_offset + P`.

This avoids the complexity of recursive page tables and makes it easy to modify page
tables from the kernel:

```rust
let phys_addr = PhysAddr::new(0x1000);
let virt_addr = VirtAddr::new(physical_memory_offset + phys_addr.as_u64());
let page_table = unsafe { &mut *(virt_addr.as_mut_ptr::<PageTable>()) };
```

---

## Kernel Heap

Once paging is set up, the kernel allocates a heap region (e.g., 1 MiB at a fixed
virtual address) and initializes `linked_list_allocator` with it:

```rust
use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init_heap(mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    // Map HEAP_START..HEAP_START+HEAP_SIZE to physical frames
    // Then:
    unsafe {
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }
}
```

After this, `alloc` types (`Vec`, `Box`, `Arc`, `String`, etc.) work in the kernel.

---

## Address Space per Process

Each userspace process gets its own **PML4 table** (page table root). The kernel
pages are mapped into the top half of every address space (but with supervisor-only
permissions — ring 3 cannot access them).

```mermaid
graph LR
    subgraph P1["Process A"]
        PML4A["PML4-A"]
        UA["User pages A\n(code, stack, heap)"]
        KA["Kernel pages\n(shared, ring 0 only)"]
    end
    subgraph P2["Process B"]
        PML4B["PML4-B"]
        UB["User pages B"]
        KB["Kernel pages\n(same physical frames)"]
    end

    PML4A --> UA
    PML4A --> KA
    PML4B --> UB
    PML4B --> KB
    KA -. "same phys" .-> KB
```

---

## Key Crates

| Crate | Role |
|---|---|
| `x86_64` | `PhysAddr`, `VirtAddr`, `PageTable`, `Mapper`, `FrameAllocator` trait |
| `linked_list_allocator` | `#[global_allocator]` for kernel heap |
| `bootloader_api` | `BootInfo::memory_regions`, `physical_memory_offset` |

---

## Open Questions

- **Bitmap vs free-list** for the frame allocator — bitmap is simpler; free-list is faster at runtime
- **Heap size** — fixed 1–4 MiB initially; growable heap needed eventually
- **Copy-on-write fork** — not needed until we have process spawning from userspace; skip for now
