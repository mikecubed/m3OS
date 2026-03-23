# Phase 8 Tasks - Storage and VFS

**Depends on:** Phase 7

```mermaid
flowchart LR
    A["file IPC contract"] --> B["vfs_server"]
    B --> C["filesystem backend"]
    C --> D["disk image content"]
    D --> E["read path validation"]
    E --> F["docs"]
```

## Implementation Tasks

- [x] P8-T001 Define a small file-oriented IPC contract for open and read operations.
- [ ] P8-T002 Implement `vfs_server` as a path router rather than a monolithic filesystem.
- [ ] P8-T003 Implement one read-only filesystem backend, such as a small FAT-based service.
- [x] P8-T004 Add sample files to the boot media or image content used for demos.
- [ ] P8-T005 Keep write support, caching, and mutation out of the first storage milestone.

<!-- Phase 8 implementation started 2026-03-23 on branch phase-8-storage-and-vfs -->

## Validation Tasks

- [ ] P8-T006 Verify a userspace client can open a known file by path and read its contents.
- [ ] P8-T007 Verify missing files and invalid paths return predictable errors.
- [ ] P8-T008 Verify the ownership boundary between VFS routing and filesystem-specific logic stays clear.

## Documentation Tasks

- [ ] P8-T009 Document the first file service protocol and the split between VFS and the filesystem backend.
- [ ] P8-T010 Document how sample files are packaged into the disk image for demos and tests.
- [ ] P8-T011 Add a short note explaining how mature OSes add writable filesystems, caching, permissions, and crash-consistency features later.
