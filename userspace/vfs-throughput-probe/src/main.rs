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
//! VFS_THRPUT:inkernel_root_reads_delta=<d>   (Phase C1 — must be ~0)
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

use syscall_lib::{CLOCK_MONOTONIC, STDERR_FILENO, STDOUT_FILENO, heap::BrkAllocator};

/// Read a monotonic-clock timestamp as nanoseconds since boot. Used to bracket
/// each I/O phase so per-operation latency can be derived (Phase 95c latency
/// investigation: block-op COUNTS alone can't tell a tick-latency wake from a
/// cheap directed hand-off — wall time per block-op does).
fn now_ns() -> u64 {
    let (s, ns) = syscall_lib::clock_gettime(CLOCK_MONOTONIC);
    if s < 0 {
        return 0;
    }
    (s as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(ns as u64)
}

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
    // Ensure the writable parent dir exists on a FRESH data disk (the gate
    // recreates the disk for reproducible deltas). Done up front so both the
    // small-write IPC phase and the bulk phases can open files under it.
    let _ = syscall_lib::mkdir(b"/usr\0", 0o755);
    let _ = syscall_lib::mkdir(b"/usr/local\0", 0o755);

    // ---- 0. IPC ROUND-TRIP ISOLATION (Phase 95c latency probe). ----
    // Each `write()` to a vfs_server-backed fd is one app↔vfs_server IPC
    // round-trip. SMALL_WRITES single-byte appends to a fresh file all land in
    // the SAME first data block (cached in vfs_server's Ext2State after the
    // first), so device I/O is ~nil and the measured per-op time is dominated by
    // the IPC rendezvous + the scheduler's wake of each peer. If that per-op time
    // is ~one timer tick, the bottleneck is tick-latency wakes (no directed
    // hand-off); if it's a few µs, the round-trip is cheap and the bulk wall is
    // elsewhere (device ops / metadata / buffer faulting).
    measure_ipc_rtt();

    // ---- 1. Snapshot blkstats BEFORE the bulk write. ----
    let (_rc_before, wc_before, ik_before) = match read_blkstats() {
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
    // Bracket the loop with the monotonic clock to derive write throughput and
    // per-block-op latency.
    let write_t0 = now_ns();
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
    let write_ns = now_ns().saturating_sub(write_t0);

    // ---- 3. Snapshot blkstats AFTER the write. ----
    let (rc_after_write, wc_after_write, _ik_after_write) = match read_blkstats() {
        Ok(v) => v,
        Err(e) => {
            error(e);
            cleanup();
            return 1;
        }
    };
    let write_delta = wc_after_write.saturating_sub(wc_before);
    print_kv("VFS_THRPUT:write_calls_delta=", write_delta);
    report_phase("write", PAYLOAD_BYTES as u64, write_ns, write_delta);

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
    let read_t0 = now_ns();
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
    let read_ns = now_ns().saturating_sub(read_t0);
    let _ = syscall_lib::close(fd_r);

    // Byte count must match the written payload.
    if read_total != PAYLOAD_BYTES {
        verify_ok = false;
    }

    // ---- 5. Snapshot blkstats AFTER the read. ----
    let (rc_after_read, _wc_after_read, ik_after_read) = match read_blkstats() {
        Ok(v) => v,
        Err(e) => {
            error(e);
            cleanup();
            return 1;
        }
    };
    let read_delta = rc_after_read.saturating_sub(rc_after_write);
    print_kv("VFS_THRPUT:read_calls_delta=", read_delta);
    report_phase("read", PAYLOAD_BYTES as u64, read_ns, read_delta);

    // Engine-unification Phase C1: the in-kernel root ext2 engine must have
    // served ~0 reads across the whole write+read+verify — every root read now
    // routes to vfs_server (Phases A+B). A non-zero delta means a root read
    // reached the second engine, the exact regression this guard catches.
    let inkernel_delta = ik_after_read.saturating_sub(ik_before);
    print_kv("VFS_THRPUT:inkernel_root_reads_delta=", inkernel_delta);

    // ---- 5b. Many-files-into-one-directory phase (Phase 95c latency probe). ----
    // Runs AFTER the bulk read snapshot so it cannot perturb the gate's asserted
    // write/read block-op deltas. Exposes super-linear directory-insertion cost.
    measure_manyfiles();

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

/// Read `/proc/blkstats` and return `(read_calls, write_calls,
/// inkernel_root_reads)`.
///
/// The file contains lines like:
/// ```text
/// read_calls 12345
/// write_calls 678
/// inkernel_root_reads 0
/// ```
/// We scan for the exact key prefix used by `cmd_vfs_bulkio_smoke`. The third
/// counter (engine-unification Phase C1) is the blocks the in-kernel root ext2
/// engine served; its steady-state delta across the probe must be ~0 (every
/// root read routes to `vfs_server`).
fn read_blkstats() -> Result<(u64, u64, u64), &'static str> {
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
    // Default to 0 so an older kernel without the line still parses (the delta
    // is then trivially 0 — the assertion degrades to a no-op, never a spurious
    // failure).
    let mut inkernel_root_reads: u64 = 0;

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
        } else if let Some(rest) = line.strip_prefix("inkernel_root_reads ")
            && let Ok(v) = rest.trim().parse::<u64>()
        {
            inkernel_root_reads = v;
        }
    }

    match (read_calls, write_calls) {
        (Some(r), Some(w)) => Ok((r, w, inkernel_root_reads)),
        _ => Err("/proc/blkstats missing read_calls or write_calls"),
    }
}

// ---------------------------------------------------------------------------
// Output helpers
// ---------------------------------------------------------------------------

/// Number of single-byte appends used to isolate the app↔vfs_server IPC
/// round-trip latency. All land in the SAME first data block, so after the
/// first they hit vfs_server's in-memory cache — the measured per-op time is
/// the IPC rendezvous + scheduler wake, with device I/O factored out.
const IPC_RTT_OPS: u64 = 512;

const IPC_RTT_PATH: &[u8] = b"/usr/local/vfsthr-rtt.bin\0";

/// Measure and report the per-op app↔vfs_server IPC round-trip latency.
///
/// This is the discriminating measurement for the Phase 95c latency hunt: if
/// the per-op time is ~one timer tick the sync IPC rendezvous itself is
/// tick-latency-bound (the woken peer is set Ready and waits for a tick rather
/// than being run via a directed hand-off); if it is a few µs the rendezvous is
/// cheap and the bulk-I/O wall must live in the device-IRQ wake or metadata.
fn measure_ipc_rtt() {
    let flags = syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC;
    let fd = syscall_lib::open(IPC_RTT_PATH, flags, 0o644);
    if (fd as i64) < 0 {
        emit("VFS_THRPUT:ipc_rtt=skip(open)\n");
        return;
    }
    let fd = fd as i32;
    let one = [0u8; 1];
    let t0 = now_ns();
    let mut done = 0u64;
    while done < IPC_RTT_OPS {
        // Rewind to offset 0 each iteration so every write stays in block 0
        // (cached) — isolates the round-trip, not block allocation.
        let _ = syscall_lib::lseek(fd, 0, syscall_lib::SEEK_SET);
        let n = syscall_lib::write(fd, &one);
        if n <= 0 {
            break;
        }
        done += 1;
    }
    let elapsed = now_ns().saturating_sub(t0);
    let _ = syscall_lib::close(fd);
    let _ = syscall_lib::unlink(IPC_RTT_PATH);

    print_kv("VFS_THRPUT:ipc_rtt_ops=", done);
    print_kv("VFS_THRPUT:ipc_rtt_total_us=", elapsed / 1000);
    // Per-op latency in microseconds: total_ns / ops / 1000. `checked_div`
    // skips the line entirely when no op completed.
    if let Some(ns_per_op) = elapsed.checked_div(done) {
        print_kv("VFS_THRPUT:ipc_rtt_us_per_op=", ns_per_op / 1000);
    }
}

/// Files created in the "many files into one growing directory" phase, in
/// batches, so per-batch timing exposes O(N²) directory-insertion degradation
/// (the prime suspect for the slow `pkg install`: a few huge files write at
/// full bandwidth, but ~1500 small files into growing dirs can dominate if each
/// `open(O_CREAT)` scans the whole directory for a free dirent slot).
// Kept modest (300 files) so this diagnostic phase stays light under TCG (the
// CI backend, ~10-50x slower than the KVM runs these numbers were tuned on) —
// enough batches to expose super-linear per-create growth without dominating
// the gate's wall-clock.
const MF_BATCHES: u64 = 5;
const MF_PER_BATCH: u64 = 60;
const MF_FILE_BYTES: usize = 16 * 1024;
const MF_DIR: &[u8] = b"/usr/local/mf\0";

/// Create `MF_BATCHES * MF_PER_BATCH` files (each `MF_FILE_BYTES`) in ONE
/// directory, timing each batch. If later batches are dramatically slower than
/// the first, directory insertion is super-linear in the directory's entry
/// count — i.e. the install bottleneck is metadata (dirent/inode) work, not data
/// bandwidth. Reports the first- and last-batch wall times plus the overall
/// files/sec so the shape is unambiguous.
fn measure_manyfiles() {
    let _ = syscall_lib::mkdir(MF_DIR, 0o755);
    let payload = alloc::vec![0u8; MF_FILE_BYTES];
    let flags = syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC;

    let mut first_batch_ms = 0u64;
    let mut last_batch_ms = 0u64;
    // Per-batch block-op deltas for the FIRST and LAST batch — distinguishes
    // device-I/O growth (deltas grow with N ⇒ super-linear allocation / cache
    // thrash) from pure-CPU growth (wall grows but deltas flat ⇒ in-memory scan).
    let mut first_batch_rd = 0u64;
    let mut first_batch_wr = 0u64;
    let mut last_batch_rd = 0u64;
    let mut last_batch_wr = 0u64;
    let total_t0 = now_ns();
    let mut created = 0u64;

    for batch in 0..MF_BATCHES {
        let (rc0, wc0, _) = read_blkstats().unwrap_or((0, 0, 0));
        let b0 = now_ns();
        for i in 0..MF_PER_BATCH {
            let n = batch * MF_PER_BATCH + i;
            // Path: /usr/local/mf/f<n> (NUL-terminated).
            let mut path = Vec::with_capacity(32);
            path.extend_from_slice(b"/usr/local/mf/f");
            // decimal n
            let mut tmp = [0u8; 20];
            let mut len = 0;
            let mut v = n;
            if v == 0 {
                tmp[0] = b'0';
                len = 1;
            } else {
                while v > 0 {
                    tmp[len] = b'0' + (v % 10) as u8;
                    v /= 10;
                    len += 1;
                }
            }
            for k in 0..len {
                path.push(tmp[len - 1 - k]);
            }
            path.push(0);
            let fd = syscall_lib::open(&path, flags, 0o644);
            if (fd as i64) < 0 {
                continue;
            }
            let fd = fd as i32;
            let _ = syscall_lib::write(fd, &payload);
            let _ = syscall_lib::close(fd);
            created += 1;
        }
        let batch_ms = now_ns().saturating_sub(b0) / 1_000_000;
        let (rc1, wc1, _) = read_blkstats().unwrap_or((rc0, wc0, 0));
        if batch == 0 {
            first_batch_ms = batch_ms;
            first_batch_rd = rc1.saturating_sub(rc0);
            first_batch_wr = wc1.saturating_sub(wc0);
        }
        if batch == MF_BATCHES - 1 {
            last_batch_ms = batch_ms;
            last_batch_rd = rc1.saturating_sub(rc0);
            last_batch_wr = wc1.saturating_sub(wc0);
        }
    }
    let total_ns = now_ns().saturating_sub(total_t0);
    let total_ms = total_ns / 1_000_000;

    print_kv("VFS_THRPUT:manyfiles_count=", created);
    print_kv("VFS_THRPUT:manyfiles_total_ms=", total_ms);
    print_kv("VFS_THRPUT:manyfiles_first_batch_ms=", first_batch_ms);
    print_kv("VFS_THRPUT:manyfiles_last_batch_ms=", last_batch_ms);
    print_kv("VFS_THRPUT:manyfiles_first_batch_rd=", first_batch_rd);
    print_kv("VFS_THRPUT:manyfiles_first_batch_wr=", first_batch_wr);
    print_kv("VFS_THRPUT:manyfiles_last_batch_rd=", last_batch_rd);
    print_kv("VFS_THRPUT:manyfiles_last_batch_wr=", last_batch_wr);
    if let Some(per_sec) = (created * 1000).checked_div(total_ms) {
        print_kv("VFS_THRPUT:manyfiles_per_sec=", per_sec);
    }
    if let Some(ns_per_file) = total_ns.checked_div(created) {
        print_kv("VFS_THRPUT:manyfiles_us_per_file=", ns_per_file / 1000);
    }
}

/// Emit throughput (KB/s) + per-block-op latency (µs) for a bulk phase.
/// `bytes` = payload size, `elapsed_ns` = wall time, `blockops` = the
/// `/proc/blkstats` call-count delta for the phase.
fn report_phase(name: &str, bytes: u64, elapsed_ns: u64, blockops: u64) {
    let ms = elapsed_ns / 1_000_000;
    emit("VFS_THRPUT:");
    emit(name);
    emit("_ms=");
    syscall_lib::write_u64(STDOUT_FILENO, ms);
    emit("\n");
    // KB/s = (bytes/1024) * 1e9 / elapsed_ns.
    if let Some(kbps) = (bytes / 1024)
        .wrapping_mul(1_000_000_000)
        .checked_div(elapsed_ns)
    {
        emit("VFS_THRPUT:");
        emit(name);
        emit("_kbps=");
        syscall_lib::write_u64(STDOUT_FILENO, kbps);
        emit("\n");
    }
    // Per-block-op latency in µs = elapsed_ns / blockops / 1000.
    if let Some(ns_per_op) = elapsed_ns.checked_div(blockops) {
        let us = ns_per_op / 1000;
        emit("VFS_THRPUT:");
        emit(name);
        emit("_us_per_blockop=");
        syscall_lib::write_u64(STDOUT_FILENO, us);
        emit("\n");
    }
}

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
