//! Phase 95c Track E.1 — `vfs-throughput-probe`.
//!
//! Measures the VFS read+write throughput and IPC call count by writing a
//! deterministic byte-pattern payload to a writable ext2 path
//! (`/usr/local/vfsthr-probe.bin`) and reading it back, bracketing both
//! operations with `/proc/blkstats` snapshots.
//!
//! Output (one sentinel per line to stdout):
//!
//! ```text
//! VFS_THRPUT:bytes=<N>
//! VFS_THRPUT:write_calls_delta=<d>
//! VFS_THRPUT:read_calls_delta=<d>
//! VFS_THRPUT:verify=ok        (or :verify=FAIL)
//! VFS_THRPUT:done
//! ```
//!
//! Errors print `VFS_THRPUT:error=<reason>` and exit non-zero.
//! The probe file is unlinked at the end (best-effort).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::Layout;

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, heap::BrkAllocator};

// ---------------------------------------------------------------------------
// Global allocator
// ---------------------------------------------------------------------------

#[global_allocator]
static A: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDERR_FILENO, "vfs-throughput-probe: alloc error\n");
    syscall_lib::exit(99)
}

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// Default payload size: 8 MiB — large enough to produce a meaningful
/// block-request delta while staying well within available disk space.
const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// I/O chunk size: 256 KiB — matches the `pkg` installer's `READ_CHUNK` so
/// the probe exercises the same IPC-coalescing path.
const CHUNK: usize = 256 * 1024;

/// Target path on the writable ext2 data disk.  `/usr/local/` is writable
/// (the installer extracts there); we use a distinctive name so the probe is
/// never confused with a real package artifact.
const PROBE_PATH: &[u8] = b"/usr/local/vfsthr-probe.bin\0";

/// `/proc/blkstats` — the kernel's per-boot block-request counters.
const BLKSTATS_PATH: &[u8] = b"/proc/blkstats\0";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    // ---- 1. Snapshot blkstats BEFORE the write. ----
    let (rc_before, wc_before) = match read_blkstats() {
        Ok(v) => v,
        Err(e) => {
            error(e);
            return 1;
        }
    };

    // ---- 2. Write the payload. ----
    let n_bytes = PAYLOAD_BYTES as u64;
    print_kv("VFS_THRPUT:bytes=", n_bytes);

    let flags = syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC;
    let fd = syscall_lib::open(PROBE_PATH, flags, 0o644);
    if (fd as i64) < 0 {
        error("open probe file for write failed");
        cleanup();
        return 1;
    }
    let fd = fd as i32;

    // Write PAYLOAD_BYTES of a simple repeating pattern in CHUNK-sized blocks.
    let mut written_total = 0usize;
    let chunk = alloc::vec![0u8; CHUNK]; // zero pattern — deterministic
    while written_total < PAYLOAD_BYTES {
        let remaining = PAYLOAD_BYTES - written_total;
        let slice = &chunk[..remaining.min(CHUNK)];
        let n = syscall_lib::write(fd, slice);
        if n <= 0 {
            let _ = syscall_lib::close(fd);
            error("write failed");
            cleanup();
            return 1;
        }
        written_total += n as usize;
    }
    let _ = syscall_lib::close(fd);

    // ---- 3. Snapshot blkstats AFTER the write. ----
    let (rc_after_write, wc_after_write) = match read_blkstats() {
        Ok(v) => v,
        Err(e) => {
            error(e);
            cleanup();
            return 1;
        }
    };
    let write_delta = wc_after_write.saturating_sub(wc_before);
    print_kv("VFS_THRPUT:write_calls_delta=", write_delta);

    // ---- 4. Read back and verify. ----
    let fd_r = syscall_lib::open(PROBE_PATH, syscall_lib::O_RDONLY, 0);
    if (fd_r as i64) < 0 {
        error("open probe file for read failed");
        cleanup();
        return 1;
    }
    let fd_r = fd_r as i32;

    let mut read_total = 0usize;
    let mut verify_ok = true;
    let mut rbuf = alloc::vec![0u8; CHUNK];
    loop {
        let n = syscall_lib::read(fd_r, &mut rbuf);
        if n < 0 {
            verify_ok = false;
            break;
        }
        if n == 0 {
            break;
        }
        // Verify byte pattern: all bytes must be zero (the write pattern).
        for b in &rbuf[..n as usize] {
            if *b != 0u8 {
                verify_ok = false;
                break;
            }
        }
        read_total += n as usize;
    }
    let _ = syscall_lib::close(fd_r);

    // Byte count must match the written payload.
    if read_total != PAYLOAD_BYTES {
        verify_ok = false;
    }

    // ---- 5. Snapshot blkstats AFTER the read. ----
    let (rc_after_read, _wc_after_read) = match read_blkstats() {
        Ok(v) => v,
        Err(e) => {
            error(e);
            cleanup();
            return 1;
        }
    };
    let read_delta = rc_after_read.saturating_sub(rc_after_write);
    print_kv("VFS_THRPUT:read_calls_delta=", read_delta);

    // ---- 6. Emit verify sentinel. ----
    if verify_ok {
        emit("VFS_THRPUT:verify=ok\n");
    } else {
        emit("VFS_THRPUT:verify=FAIL\n");
    }

    // ---- 7. Cleanup and done. ----
    cleanup();
    emit("VFS_THRPUT:done\n");

    if verify_ok { 0 } else { 1 }
}

// ---------------------------------------------------------------------------
// /proc/blkstats parser — returns (read_calls, write_calls)
// ---------------------------------------------------------------------------

/// Read `/proc/blkstats` and return `(read_calls, write_calls)`.
///
/// The file contains lines like:
/// ```text
/// read_calls 12345
/// write_calls 678
/// ```
/// We scan for the exact key prefix used by `cmd_vfs_bulkio_smoke`.
fn read_blkstats() -> Result<(u64, u64), &'static str> {
    let fd = syscall_lib::open(BLKSTATS_PATH, syscall_lib::O_RDONLY, 0);
    if (fd as i64) < 0 {
        return Err("open /proc/blkstats failed");
    }
    let fd = fd as i32;

    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 512];
    loop {
        let n = syscall_lib::read(fd, &mut chunk);
        if n < 0 {
            let _ = syscall_lib::close(fd);
            return Err("read /proc/blkstats failed");
        }
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
    }
    let _ = syscall_lib::close(fd);

    let text = core::str::from_utf8(&buf).map_err(|_| "/proc/blkstats is not UTF-8")?;

    let mut read_calls: Option<u64> = None;
    let mut write_calls: Option<u64> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("read_calls ") {
            if let Ok(v) = rest.trim().parse::<u64>() {
                read_calls = Some(v);
            }
        } else if let Some(rest) = line.strip_prefix("write_calls ") {
            if let Ok(v) = rest.trim().parse::<u64>() {
                write_calls = Some(v);
            }
        }
    }

    match (read_calls, write_calls) {
        (Some(r), Some(w)) => Ok((r, w)),
        _ => Err("/proc/blkstats missing read_calls or write_calls"),
    }
}

// ---------------------------------------------------------------------------
// Output helpers
// ---------------------------------------------------------------------------

fn emit(s: &str) {
    syscall_lib::write_str(STDOUT_FILENO, s);
}

fn error(reason: &str) {
    emit("VFS_THRPUT:error=");
    emit(reason);
    emit("\n");
}

fn print_kv(key: &str, val: u64) {
    emit(key);
    syscall_lib::write_u64(STDOUT_FILENO, val);
    emit("\n");
}

fn cleanup() {
    // Best-effort: ignore errors (path may not exist on early failure).
    let _ = syscall_lib::unlink(PROBE_PATH);
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDERR_FILENO, "vfs-throughput-probe: PANIC\n");
    syscall_lib::exit(101)
}
