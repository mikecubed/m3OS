//! Kernel-side path metadata (stat) cache — Phase 89.
//!
//! ## Why this exists
//!
//! Node's `require()` resolution does tens of thousands of `stat` / path-walk
//! operations during startup — overwhelmingly *repeated* lookups of the same
//! prefix directories (`/usr/lib/node_modules`, …) and *negative* probes for
//! non-existent module candidates (`./x.js` when the answer is `./x/index.js`).
//! Each one otherwise crosses the ring-0↔ring-3 IPC boundary to the
//! single-threaded `vfs_server` (`VFS_STAT_PATH`, ~80–200 µs each), which is the
//! Phase-89 `npm install` bottleneck: the metadata-op storm serialises through
//! one ring-3 process.
//!
//! This caches the [`vfs_service_stat_path`](crate::arch::x86_64::syscall)
//! result — positive *and* negative (`ENOENT`) — keyed by absolute path, so a
//! repeated stat or path-walk component hits RAM and never crosses the IPC
//! boundary. Because the per-component `path_node_nofollow` walk *also* funnels
//! through `vfs_service_stat_path`, one cache covers both the user-visible
//! `stat`/`getdents` path and the resolution prefix-walk — the higher-leverage
//! win, since every module resolution re-walks the same prefix directories.
//!
//! ## Scope & security
//!
//! The cache sits ONLY on the user-visible stat path. The DAC-enforcement path
//! (`path_metadata`) deliberately stays on kernel-verified ext2 metadata and is
//! never cached here: a compromised or misbehaving `vfs_server` must not be able
//! to spoof uid/gid/mode for an access check by seeding this cache. The worst a
//! lying `vfs_server` can do through this cache is return wrong *user-visible*
//! `stat` output, which it could already do — enforcement is unaffected.
//!
//! ## Coherence model — a global epoch
//!
//! Every ext2-mutating operation bumps [`EPOCH`]; a cache line is valid only
//! while its stamped epoch equals the current one. A bump therefore invalidates
//! the *entire* cache in O(1) and — crucially — makes a stale read structurally
//! impossible as long as *every* mutation bumps. Correctness does NOT depend on
//! per-path invalidation precision or key normalisation: a mismatched key is
//! merely a miss, never a stale hit. The single invariant to uphold is *"every
//! ext2 mutation calls [`bump`]"*, which the call sites guarantee:
//!
//! * vfs-routed mutations (the common path for normal processes: write / create
//!   / unlink / rmdir / rename / link / truncate) bump via
//!   [`crate::fs::ext2::invalidate_cache`], which the syscall layer already
//!   calls after every routed mutation;
//! * direct-engine `chmod` / `chown` / `utimensat` (which never route to
//!   `vfs_server`) bump explicitly at their syscall sites;
//! * as a backstop, `blk::write_sectors` bumps on *any* kernel block write (the
//!   kernel ext2 engine funnels all of its writes through it), so a direct-engine
//!   fallback mutation (boot window) or any future kernel-side mutation path that
//!   forgets to bump is still caught when it reaches the disk.
//!
//! The epoch is captured *before* the IPC fetch and re-checked implicitly on the
//! next lookup, so a mutation racing the fetch can never install a line that is
//! then served as fresh (see [`store`]).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::arch::x86_64::syscall::VfsPathStat;

/// `ENOENT` as a negative errno in `u64` form — the only error result worth
/// caching (a stable negative). Transient errors (`EIO`, `EINVAL`, …) are never
/// cached, so a hiccup in the `vfs_server` transport can't be remembered.
const ENOENT_U64: u64 = (-2_i64) as u64;

/// Global coherence epoch. Bumped on every ext2-mutating operation; a cache
/// line is valid only while `line.epoch == EPOCH`.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// Observability counters (per boot), surfaced at `/proc/metacache`. A healthy
/// stat-heavy workload (npm) shows `hits` ≫ `misses`; each hit is one
/// ring-0↔ring-3 `VFS_STAT_PATH` IPC that never happened.
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static BUMPS: AtomicU64 = AtomicU64::new(0);

/// Upper bound on cached paths. Node walks a few thousand distinct paths during
/// startup; this comfortably holds the hot working set. On overflow the cache is
/// cleared wholesale (cheap, rare) rather than paying per-entry LRU bookkeeping
/// on every lookup.
const MAX_ENTRIES: usize = 16384;

struct CacheLine {
    epoch: u64,
    /// `Ok` = the path exists with this stat; `Err(ENOENT)` = a cached negative.
    result: Result<VfsPathStat, u64>,
}

static CACHE: Mutex<BTreeMap<String, CacheLine>> = Mutex::new(BTreeMap::new());

/// Invalidate the entire cache in O(1) by advancing the epoch. Lock-free — only
/// touches the atomic, never the map lock — so it is safe to call from the block
/// layer or any context without risk of lock contention or deadlock.
#[inline]
pub(crate) fn bump() {
    EPOCH.fetch_add(1, Ordering::Release);
    BUMPS.fetch_add(1, Ordering::Relaxed);
}

/// Snapshot the per-boot counters as `(hits, misses, bumps, live_entries)` for
/// `/proc/metacache`.
pub(crate) fn stats() -> (u64, u64, u64, u64) {
    let entries = CACHE.lock().len() as u64;
    (
        HITS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
        BUMPS.load(Ordering::Relaxed),
        entries,
    )
}

/// The current epoch. Capture this *before* an IPC fetch and pass it to
/// [`store`]; if a mutation bumps the epoch while the fetch is in flight, the
/// stored line is stamped with the now-stale epoch and is dropped on its next
/// lookup, so the possibly-stale fetched value is never served as fresh.
#[inline]
pub(crate) fn epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}

/// Look up `path`. Returns the cached result iff a line exists for `path` AND
/// its stamped epoch equals `cur_epoch` (i.e. no ext2 mutation has happened
/// since it was stored). A clone is returned so the lock is released before the
/// caller touches the value.
pub(crate) fn lookup(path: &str, cur_epoch: u64) -> Option<Result<VfsPathStat, u64>> {
    let cache = CACHE.lock();
    match cache.get(path) {
        Some(line) if line.epoch == cur_epoch => {
            HITS.fetch_add(1, Ordering::Relaxed);
            Some(line.result.clone())
        }
        _ => {
            MISSES.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

/// Store `result` for `path`, stamped with `at_epoch` (the epoch captured before
/// the fetch). Only positive results and stable `ENOENT` negatives are cached;
/// transient errors are dropped so they can't be remembered. If the epoch has
/// advanced since `at_epoch` — a mutation raced the fetch — the line is stored
/// stale and simply misses on the next lookup, never serving the racy value.
pub(crate) fn store(path: &str, at_epoch: u64, result: Result<VfsPathStat, u64>) {
    // Never cache transient errors — only positives and the stable ENOENT
    // negative (the npm negative-resolution win).
    if let Err(errno) = result
        && errno != ENOENT_U64
    {
        return;
    }
    let mut cache = CACHE.lock();
    // Bounded: drop the whole working set on overflow rather than evicting
    // per-entry. Node's working set fits comfortably under the cap, so this is
    // a rare self-healing reset, not a steady-state cost.
    if cache.len() >= MAX_ENTRIES && !cache.contains_key(path) {
        cache.clear();
    }
    cache.insert(
        path.to_string(),
        CacheLine {
            epoch: at_epoch,
            result,
        },
    );
}
