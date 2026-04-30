//! In-memory filesystem (tmpfs) — re-exported from kernel-core with global state.
//!
//! The same tmpfs instance backs both `/tmp` and `/run` as distinct top-level
//! directories inside the shared tree:
//!
//! - `/tmp` (mode `1777`, sticky) — world-writable user scratch space.
//! - `/run` (mode `0755`) — root-owned runtime state (PID files, control
//!   sockets, per-service status). Matches the Linux convention where `/run`
//!   is tmpfs rather than persistent storage.
//!
//! Userspace paths like `/tmp/foo` and `/run/foo` are distinct inside the
//! tree (different parent directories), so they cannot collide. Permissions
//! are enforced per-node. `/run` is world-readable by design (matching the
//! Linux convention where non-root processes can `ls /run`) — confidentiality
//! of individual runtime-state files relies on their own mode / ownership,
//! not on hiding the directory itself. Files like `init.cmd` are created
//! with mode `0600` and owned by root so non-root openers are refused at
//! the file level.
#![allow(dead_code)]

use crate::task::scheduler::IrqSafeMutex;

#[allow(unused_imports)]
pub use kernel_core::fs::tmpfs::{MAX_FILE_SIZE, Tmpfs, TmpfsError, TmpfsStat};

/// Global tmpfs instance. Rooted at the tmpfs tree root; `/tmp` and `/run`
/// are created as top-level children by [`init`].
///
/// Phase 57b G.3 — IrqSafeMutex inherits Track F.1's preempt-discipline
/// (lock raises `preempt_count`, drop lowers it).  TMPFS is only acquired
/// from task context (init, syscall paths); no ISR ever reaches it.  Type
/// swap is a pure auto-deref change for callsites.
pub static TMPFS: IrqSafeMutex<Tmpfs> = IrqSafeMutex::new(Tmpfs::new());

/// Populate the tmpfs tree with the standard mount-point directories.
///
/// Must be called once at boot, before any task that opens files under
/// `/tmp` or `/run` runs.
pub fn init() {
    let mut fs = TMPFS.lock();
    // /tmp — mode 1777 (world-writable, sticky). Ignore AlreadyExists on
    // a warm-boot path where something preloaded the tree.
    let _ = fs.mkdir_with_meta("tmp", 0, 0, 0o1777);
    // /run — mode 0755 (root-writable, world-readable).
    let _ = fs.mkdir_with_meta("run", 0, 0, 0o755);
}
