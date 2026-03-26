//! In-memory filesystem (tmpfs) — re-exported from kernel-core with global state.
#![allow(dead_code)]

use spin::Mutex;

#[allow(unused_imports)]
pub use kernel_core::fs::tmpfs::{Tmpfs, TmpfsError, TmpfsStat, MAX_FILE_SIZE};

/// Global tmpfs instance mounted at `/tmp`.
pub static TMPFS: Mutex<Tmpfs> = Mutex::new(Tmpfs::new());
