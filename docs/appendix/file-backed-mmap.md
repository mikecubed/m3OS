# File-Backed mmap — Design and Implementation Plan

**Type:** Appendix — kernel feature design  
**Status:** Planned  
**Depends on:** Phase 33 (buddy allocator, demand paging, mprotect/munmap) ✅  
**Related:** Phase 47 (DOOM port) — identified WAD file mmap as root cause of zone pressure

---

## Background

m3OS currently supports only anonymous `mmap` (`MAP_ANONYMOUS`). Any call to
`mmap(fd, ...)` with a file descriptor returns `-EINVAL`. This means programs
that rely on file-backed memory mappings — most notably DOOM (WAD loading),
but also shared libraries, memory-mapped databases, and language runtimes —
cannot use the OS-provided mmap fast path and must fall back to `read()`-based
I/O combined with heap allocation.

### Impact on DOOM (the motivating case)

DOOM's WAD loader in chocolate-doom/doomgeneric calls `mmap` on the opened WAD
file to get a zero-copy pointer into the file data. When this succeeds,
`lump->wad_file->mapped != NULL` and every lump access returns a pointer
directly into the mapped region — **no zone allocation at all**.

When mmap returns `MAP_FAILED` (as it does on m3OS today), every lump read
instead calls `Z_Malloc` + `W_ReadLump`, feeding all 4 MB of WAD data through
the 6 MB DOOM zone allocator. This causes excessive `PU_CACHE` eviction,
contributes to null pointer crashes in status-bar rendering, and degrades
overall game performance.

The fix is not DOOM-specific — file-backed mmap is a POSIX primitive that
many programs depend on.

---

## What File-Backed mmap Is

```
mmap(NULL, length, PROT_READ, MAP_PRIVATE, fd, offset)
```

This call asks the OS to:

1. Find virtual address space in the calling process.
2. Map `length` bytes of file `fd` starting at `offset` into that range.
3. Return the virtual address to the caller.

From then on the process can read (and optionally write) the file's data via
normal load/store instructions. The OS backs those virtual pages with the
file's data, fetching them lazily on page fault or eagerly at map time.

`MAP_PRIVATE` means writes create a private copy (copy-on-write); the
underlying file is not modified.

`MAP_SHARED` means writes propagate back to the file and are visible to other
processes mapping the same file.

---

## Design

### Two implementation strategies

#### Strategy A — Eager loading (simpler, implement first)

When `mmap(fd, len, prot, MAP_PRIVATE, offset)` is called:

1. Resolve `fd` → VFS file handle → inode.
2. Allocate `ceil(len / 4096)` physical frames.
3. Read the file bytes `[offset, offset+len)` into those frames.
4. Map the frames into the process's page table at a fresh virtual address
   (using the same `mmap_next` bump allocator used for anonymous mappings).
5. Return the virtual address.

On `munmap`, unmap the pages and return the frames to the frame allocator.

This is the easiest path to correctness. It does not require page-fault
handling changes and works within the existing mm subsystem. The cost is that
all mapped file bytes are loaded into RAM upfront, which is fine for small
files (WAD files, executables) but wasteful for large sparse files.

#### Strategy B — Demand paging (proper, implement second)

Instead of loading all pages eagerly, record a `VmaRegion` for the mapping and
handle page faults:

1. `mmap` just creates a `VmaRegion` entry in the process's address space
   descriptor. No frames are allocated yet.
2. On page fault: the fault handler looks up the faulting address in the
   process's VMA list, finds the backing file + offset, allocates one frame,
   reads the page from disk, maps it, and resumes the process.
3. `munmap` removes the VMA and frees all loaded frames.

This is how Linux and every production OS implements mmap. It enables
memory-mapped executables (the foundation for a dynamic linker), shared
anonymous memory, and copy-on-write fork optimisation.

**For the immediate DOOM fix, Strategy A is sufficient.** Strategy B is the
right long-term design.

---

## Kernel Changes Required

### 1. VFS / file handle layer

The kernel needs a way to read a byte range from an open fd into a caller-
supplied buffer at `mmap` time (eager) or into a single physical frame at
fault time (demand). This likely already exists via `sys_linux_read` /
`sys_linux_pread`; the mmap path just needs to call the same logic internally.

Key requirement: given `(fd, offset, length)` in kernel space, produce bytes
without going through userspace syscall machinery.

### 2. `sys_linux_mmap` extension

Current signature (kernel-internal):

```rust
fn sys_linux_mmap(addr_hint: u64, len: u64, prot: u64) -> u64
```

The `flags` register is already read from `per_core_syscall_arg3()`. Add a
fourth argument: `fd` from the syscall `arg3` register (r10), and `offset`
from `arg5` (r9).

Remove the early return on `MAP_ANONYMOUS == 0` and instead branch:

```
if MAP_ANONYMOUS set  →  existing anonymous path
else                  →  new file-backed path
```

### 3. Process address space descriptor

Each `Process` already has `mmap_next` (the bump pointer) and a `mappings`
`Vec<MemoryMapping>`. Extend `MemoryMapping` to carry optional backing-file
info:

```rust
pub struct MemoryMapping {
    pub start: u64,
    pub len:   u64,
    pub prot:  u64,
    pub flags: u64,
    // New fields for file-backed mappings:
    pub backing: Option<FileBacking>,
}

pub struct FileBacking {
    pub inode_key: InodeKey,   // stable key into the VFS inode table
    pub file_offset: u64,      // byte offset within the file
}
```

`InodeKey` must remain valid after the fd is closed (POSIX allows this). The
VFS inode reference count must be incremented on mmap and decremented on
munmap.

### 4. Frame allocation and page mapping

Strategy A: allocate frames with `alloc_frames(n)`, call VFS read into each
frame (converting physical frame address to kernel virtual address via
`phys_offset`), then call `map_user_frames` as the framebuffer mmap already
does.

Strategy B: on page fault in `handle_page_fault`, check the process VMA list
before killing the process. If the fault address falls within a file-backed
VMA, service the fault.

### 5. munmap changes

`sys_linux_munmap` already exists. For file-backed pages:

- `MAP_PRIVATE` dirty pages are simply discarded (no writeback).
- `MAP_SHARED` dirty pages must be written back to the file before freeing
  the frame (not required for the DOOM use case but needed for correctness).
- Decrement the inode reference count.

---

## Acceptance Criteria

- `mmap(fd, len, PROT_READ, MAP_PRIVATE, 0)` on a regular file returns a
  valid pointer and the mapped bytes match the file contents.
- DOOM's WAD loader sees `lump->wad_file->mapped != NULL` and uses the mapped
  region directly; no `Z_Malloc` calls are made for lump data.
- `munmap` on a file-backed region frees all allocated frames without leaking.
- Existing anonymous mmap paths are unaffected (no regression).
- `mmap` followed by `close(fd)` still works (mapping outlives the fd).

---

## What This Enables Beyond DOOM

| Use case | Why it needs file-backed mmap |
|---|---|
| Dynamic linker (`ld.so`) | Maps ELF segments directly from the shared object file |
| Memory-mapped databases (SQLite) | Maps database file for random access without buffering |
| Large file processing | Programs like `grep`, `sort`, `wc` can mmap instead of read-loop |
| Copy-on-write `fork` | Shared file mappings propagate correctly across fork |
| Executable loading | Static ELF loader can be simplified to mmap + jump |

---

## Implementation Order

1. **Strategy A (eager)** — implement first, unblocks DOOM and all
   `MAP_PRIVATE | PROT_READ` use cases.
2. **`MAP_SHARED` writeback** — needed for mmap-based IPC and databases.
3. **Strategy B (demand paging)** — enables dynamic linking and efficient
   large-file access; requires page-fault handler VMA lookup.
4. **`MAP_SHARED` cross-process visibility** — requires a kernel page cache
   and reverse mapping table so multiple processes see coherent data.

---

## Files to Touch

| File | Change |
|---|---|
| `kernel/src/arch/x86_64/syscall.rs` | Extend `sys_linux_mmap` to handle non-anonymous flags; read `fd` and `offset` args |
| `kernel/src/process/mod.rs` | Add `FileBacking` to `MemoryMapping`; add inode ref-counting on map/unmap |
| `kernel/src/mm/user_space.rs` | No change needed for Strategy A; page-fault VMA lookup for Strategy B |
| `kernel/src/fs/` (VFS layer) | Expose a kernel-internal `read_at(inode, offset, buf)` function |
| `userspace/doom/dg_m3os.c` | No change needed — DOOM already uses mmap correctly; it will just work |
