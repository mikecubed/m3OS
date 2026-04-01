# Phase 8 — Storage and VFS: Task List

**Status:** Complete
**Source Ref:** phase-8
**Depends on:** Phase 7 ✅
**Goal:** Introduce a VFS path router and a read-only filesystem backend so userspace clients can open and read files by path through IPC.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | File IPC contract and VFS server | — | ✅ Done |
| B | Filesystem backend and disk content | A | ✅ Done |
| C | Validation and documentation | A, B | ✅ Done |

---

## Track A — File IPC Contract and VFS Server

### A.1 — Define a file-oriented IPC contract for open and read

**File:** `kernel/src/fs/protocol.rs`
**Why it matters:** A stable IPC contract decouples file consumers from filesystem implementation details.

**Acceptance:**
- [x] Small IPC protocol defined for open and read operations
- [x] Contract is documented and used by VFS routing logic

---

### A.2 — Implement vfs_server as a path router

**File:** `kernel/src/fs/vfs.rs`
**Why it matters:** The VFS layer routes path requests to the correct backend, keeping filesystem-specific logic isolated.

**Acceptance:**
- [x] VFS routes file requests by path prefix to the appropriate backend
- [x] VFS is a router, not a monolithic filesystem implementation

---

## Track B — Filesystem Backend and Disk Content

### B.1 — Implement a read-only filesystem backend

**File:** `kernel/src/fs/fat32.rs`
**Symbol:** `Fat32Volume`, `mount_fat32`
**Why it matters:** A real filesystem backend lets the OS read files from the boot media rather than only embedded data.

**Acceptance:**
- [x] One read-only filesystem backend (FAT32) is functional
- [x] Files can be read from the mounted volume

---

### B.2 — Add sample files to the boot media

**Why it matters:** Demo files validate the end-to-end read path and provide content for shell commands.

**Acceptance:**
- [x] Sample files are present on the disk image for demos and testing

---

### B.3 — Keep write support out of first storage milestone

**Why it matters:** Deferring writes keeps the initial implementation simple and avoids crash-consistency concerns.

**Acceptance:**
- [x] No write, caching, or mutation support in this phase

---

## Track C — Validation and Documentation

### C.1 — Verify userspace can open and read a known file by path

**Why it matters:** End-to-end validation of the VFS + filesystem backend read path.

**Acceptance:**
- [x] A userspace client opens a known file by path and reads its contents correctly

---

### C.2 — Verify missing files and invalid paths return errors

**Why it matters:** Predictable error handling prevents silent failures and undefined behavior.

**Acceptance:**
- [x] Missing files and invalid paths produce predictable error codes

---

### C.3 — Verify VFS/filesystem ownership boundary is clear

**Why it matters:** Clean separation enables adding new filesystem backends without modifying the VFS router.

**Acceptance:**
- [x] VFS routing logic and filesystem-specific logic are in separate modules

---

### C.4 — Document file service protocol and VFS/backend split

**Why it matters:** Future phases adding writable filesystems need to understand the layering.

**Acceptance:**
- [x] File service protocol and VFS/backend architecture are documented

---

### C.5 — Document how sample files are packaged into the disk image

**Why it matters:** Contributors need to know how to add test content to the image.

**Acceptance:**
- [x] Disk image packaging process for sample files is documented

---

### C.6 — Note on mature filesystem features

**Why it matters:** Sets expectations for what a production OS adds (writable FS, caching, permissions, crash consistency).

**Acceptance:**
- [x] Short note explains how real OSes extend beyond read-only filesystems

---

## Documentation Notes

- Phase 8 added the first file access path, building on the IPC infrastructure from Phase 7.
- The VFS router and FAT32 backend are cleanly separated, allowing future backends (tmpfs, ext2) to slot in.
- Write support was explicitly deferred to a later phase.
