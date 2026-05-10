//! Static kernel-stack pool.
//!
//! Replaces the previous `Box<[u8]>` heap allocation for [`crate::task::Task`]'s
//! kernel stack with a fixed-size pool living in `.bss`. The structural
//! guarantee is that a stack's backing memory is *only* ever used as a kernel
//! stack — it never aliases with any other heap allocation. This eliminates the
//! AP-core saved-frame corruption tracked in
//! `docs/handoffs/ap-core-gpf-saved-rsp-stack-corruption.md`, where a freshly-
//! formatted log message landed at the same physmap address that a kernel
//! stack later resolved through.
//!
//! ## Sizing
//!
//! [`MAX_KERNEL_STACKS`] covers every consumer that previously allocated a
//! 16-32 KiB stack from the kernel heap (Task kernel-mode stacks, process
//! syscall stacks, AP per-core syscall + double-fault stacks). See the
//! constant's docstring for the breakdown.
//!
//! ## Lock-free claim
//!
//! Slot ownership is tracked by a parallel array of [`AtomicBool`]s. Allocation
//! does a linear `compare_exchange` scan; on success, the caller has exclusive
//! mutable access to the slot's bytes for the lifetime of the returned
//! [`KernelStack`] guard. Deallocation zeros the slot and releases the bit.
//!
//! Allocation is rare (per task spawn, not per dispatch), so the O(N) scan is
//! not on any hot path.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

/// Size of one kernel stack in bytes (32 KiB). Matches the prior
/// `Box<[u8; KERNEL_STACK_SIZE]>` allocation.
pub const KERNEL_STACK_SIZE: usize = 4096 * 8;

/// Maximum number of concurrent kernel stacks.
///
/// Sized to cover every consumer that historically allocated a 16-32 KiB
/// stack from the kernel heap:
///
/// - One [`crate::task::Task`] kernel-mode stack per task (up to `MAX_TASKS`).
/// - One process-level syscall stack per userspace task — leaked, see
///   `kernel/src/process/mod.rs::alloc_kernel_stack` (up to `MAX_TASKS`).
/// - Two AP per-core stacks (syscall + double-fault IST) per non-BSP core —
///   leaked by `kernel/src/smp/mod.rs::init_ap_per_core` (up to
///   `2 × (MAX_CORES - 1)`).
///
/// The BSP's syscall and double-fault stacks live in `.bss` directly (see
/// `kernel/src/arch/x86_64/gdt.rs`), so they do not consume pool slots.
///
/// Pool footprint = `MAX_KERNEL_STACKS × 32 KiB` in `.bss`, zero-initialized
/// by the loader. The slots themselves are 32 KiB even when the consumer
/// "needed" only 16 KiB or 20 KiB; the overhead is acceptable for the
/// structural isolation guarantee.
pub const MAX_KERNEL_STACKS: usize = 2 * crate::task::MAX_TASKS + 2 * (crate::smp::MAX_CORES - 1);

#[repr(C, align(16))]
struct StackSlot {
    bytes: UnsafeCell<[u8; KERNEL_STACK_SIZE]>,
}

// SAFETY: each slot's `UnsafeCell` is exclusively owned by at most one
// `KernelStack` guard at a time, enforced by the `STACK_USED` CAS. No two
// threads ever hold a reference to the same slot's bytes simultaneously.
unsafe impl Sync for StackSlot {}

// Block-scoped const initializers — the standard Rust pattern for repeated
// array init of interior-mutable types. The `#[allow]` matches the
// IPC-notification table above (`TCB_BOUND_NOTIF`) which uses the same shape.
#[allow(clippy::declare_interior_mutable_const)]
static STACK_POOL: [StackSlot; MAX_KERNEL_STACKS] = {
    const SLOT_INIT: StackSlot = StackSlot {
        bytes: UnsafeCell::new([0u8; KERNEL_STACK_SIZE]),
    };
    [SLOT_INIT; MAX_KERNEL_STACKS]
};

#[allow(clippy::declare_interior_mutable_const)]
static STACK_USED: [AtomicBool; MAX_KERNEL_STACKS] = {
    const SLOT_FREE: AtomicBool = AtomicBool::new(false);
    [SLOT_FREE; MAX_KERNEL_STACKS]
};

/// RAII handle for one kernel-stack slot.
///
/// Owns exclusive access to `STACK_POOL[slot_idx].bytes` until dropped. On
/// drop, zeros the bytes (so the next claimant gets a clean stack) and
/// releases the bit in [`STACK_USED`].
pub struct KernelStack {
    slot_idx: usize,
}

impl KernelStack {
    /// Claim a free slot from the pool. Returns `None` if the pool is
    /// exhausted (see [`MAX_KERNEL_STACKS`] for the cap).
    pub fn alloc() -> Option<Self> {
        for (i, used) in STACK_USED.iter().enumerate() {
            if used
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                // SAFETY: we just won the CAS for slot `i`, so we hold
                // exclusive access to its bytes. The Drop impl is the only
                // path that releases the bit, and it runs after the last
                // `as_mut_slice` borrow is gone (RAII).
                let bytes = unsafe { &mut *STACK_POOL[i].bytes.get() };
                // The slot is zero-initialized at boot and re-zeroed on free,
                // so this is a no-op except as a defence-in-depth tripwire if
                // a future code path forgets to zero on free.
                debug_assert!(
                    bytes.iter().all(|&b| b == 0),
                    "kstack slot {} not zeroed before alloc",
                    i
                );
                return Some(KernelStack { slot_idx: i });
            }
        }
        None
    }

    /// Mutable view of the stack bytes.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: we own this slot exclusively (see `alloc`).
        unsafe { &mut *STACK_POOL[self.slot_idx].bytes.get() }
    }

    /// Immutable view of the stack bytes.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: we own this slot exclusively (see `alloc`).
        unsafe { &*STACK_POOL[self.slot_idx].bytes.get() }
    }

    /// `(base, top)` virtual addresses of this stack — `base` is the lowest
    /// byte, `top` is one past the last byte. Matches the prior
    /// `Box<[u8]>::as_ptr() / +len()` semantics.
    pub fn bounds(&self) -> (u64, u64) {
        let s = self.as_slice();
        let base = s.as_ptr() as u64;
        let top = base + s.len() as u64;
        (base, top)
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        // Zero before release so a subsequent allocator sees a clean buffer
        // (and so any stale saved-frame bytes can't be loaded by a misrouted
        // dispatch — defence in depth against the original GPF signature).
        // SAFETY: we still own the slot until the store below.
        unsafe { (*STACK_POOL[self.slot_idx].bytes.get()).fill(0) };
        STACK_USED[self.slot_idx].store(false, Ordering::Release);
    }
}

/// Claim a slot for a permanently-leaked kernel stack and return the
/// 16-byte-aligned top address (one past the highest byte, masked down to
/// 16-byte alignment).
///
/// Used by [`crate::process`] for per-process syscall stacks. The legacy
/// design (`Box::into_raw`) intentionally leaked the heap allocation;
/// process-cleanup-time deallocation is deferred to a later phase. Routing
/// through the pool keeps the same leak semantics but guarantees the memory
/// can never alias with a heap allocation that holds non-stack data — the
/// failure mode that produced the AP-core saved-frame GPF.
///
/// Panics if the pool is exhausted (the current pool size is documented at
/// [`MAX_KERNEL_STACKS`]).
pub fn alloc_leaked_top() -> u64 {
    for (i, used) in STACK_USED.iter().enumerate() {
        if used
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            // SAFETY: we just claimed slot `i`; nobody else can touch it. The
            // slot bit is left set forever (leak by design — see doc above).
            let bytes_ptr = STACK_POOL[i].bytes.get() as *mut u8;
            let top = (bytes_ptr as u64) + KERNEL_STACK_SIZE as u64;
            return top & !15;
        }
    }
    panic!(
        "kstack: leaked-stack pool exhausted (cap = {})",
        MAX_KERNEL_STACKS
    );
}
