use kernel_core::slab::SlabCache;
use spin::Mutex;

/// Pre-configured slab caches for common kernel object sizes.
///
/// These caches are infrastructure for future migration of kernel object
/// allocations (Phase 33, Track C.4 — deferred).
#[allow(dead_code)]
pub struct KernelSlabCaches {
    /// 512-byte objects (e.g. task control blocks).
    pub task_cache: Mutex<SlabCache>,
    /// 64-byte objects (e.g. file descriptors).
    pub fd_cache: Mutex<SlabCache>,
    /// 128-byte objects (e.g. IPC endpoints).
    pub endpoint_cache: Mutex<SlabCache>,
    /// 4096-byte objects (e.g. pipe buffers).
    pub pipe_cache: Mutex<SlabCache>,
    /// 256-byte objects (e.g. socket structures).
    pub socket_cache: Mutex<SlabCache>,
}

static SLAB_CACHES: spin::Once<KernelSlabCaches> = spin::Once::new();

/// Initialize the kernel slab caches. Must be called after the heap is ready.
pub fn init() {
    SLAB_CACHES.call_once(|| KernelSlabCaches {
        task_cache: Mutex::new(SlabCache::new(512, 4096)),
        fd_cache: Mutex::new(SlabCache::new(64, 4096)),
        endpoint_cache: Mutex::new(SlabCache::new(128, 4096)),
        pipe_cache: Mutex::new(SlabCache::new(4096, 4096)),
        socket_cache: Mutex::new(SlabCache::new(256, 4096)),
    });
    log::info!("[mm] slab caches initialized");
}

/// Get a reference to the kernel slab caches.
#[allow(dead_code)]
pub fn caches() -> &'static KernelSlabCaches {
    SLAB_CACHES.get().expect("slab caches not initialized")
}

/// Summary of all slab cache statistics.
pub struct AllSlabStats {
    pub task: kernel_core::slab::SlabStats,
    pub fd: kernel_core::slab::SlabStats,
    pub endpoint: kernel_core::slab::SlabStats,
    pub pipe: kernel_core::slab::SlabStats,
    pub socket: kernel_core::slab::SlabStats,
}

/// Returns a snapshot of all slab cache statistics.
pub fn all_slab_stats() -> AllSlabStats {
    let c = caches();
    AllSlabStats {
        task: c.task_cache.lock().stats(),
        fd: c.fd_cache.lock().stats(),
        endpoint: c.endpoint_cache.lock().stats(),
        pipe: c.pipe_cache.lock().stats(),
        socket: c.socket_cache.lock().stats(),
    }
}
