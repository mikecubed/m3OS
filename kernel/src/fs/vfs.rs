//! VFS routing layer — Phase 8 (`vfs_server` handler logic).
//!
//! # Phase 8 behaviour
//!
//! In Phase 8 the VFS enforces the routing boundary at the IPC level:
//! `vfs_server_task` (in `main.rs`) receives file requests from clients over
//! the `"vfs"` endpoint and forwards them to `fat_server` over the `"fat"`
//! endpoint using [`crate::ipc::endpoint::call_msg`].  The two-hop IPC chain
//! (`client → vfs_server → fat_server`) is the ownership boundary between
//! path dispatch and file-data access validated by P8-T008.
//!
//! # Phase 9+ plans
//!
//! When the project gains multiple filesystem backends, this module will own a
//! mount table that maps path prefixes to backend endpoint IDs.  For example:
//!
//! - `/`     → ramdisk / initrd backend
//! - `/tmp`  → tmpfs backend
//! - `/home` → ext2 / FAT backend over a block device
//!
//! `vfs_server_task` will call a `route(path) -> EndpointId` function here to
//! select the backend before forwarding each `FILE_OPEN` request.  `FILE_READ`
//! and `FILE_CLOSE` will use the fd-to-backend mapping cached at open time.
//!
//! # Why keep a separate `vfs` module in Phase 8?
//!
//! Even without routing code, the module establishes the naming and ownership
//! conventions that Phase 9+ will fill in:
//!
//! 1. **Routing boundary** — `vfs_server_task` owns path dispatch; `fat_server`
//!    owns file data.  Clients only ever hold a `"vfs"` endpoint capability;
//!    they never hold `"fat"` directly.  The IPC chain enforces this boundary.
//!
//! 2. **Zero-cost refactor** — Phase 9 mount-table logic slots in here without
//!    touching `ramdisk.rs`, `fat_server_task`, or any client.
