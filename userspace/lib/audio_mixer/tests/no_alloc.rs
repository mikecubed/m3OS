//! No-allocation contract for `Mixer::step`.
//!
//! Per the Track A.2 acceptance: "No heap allocation in `step`
//! (verified by a host test that installs a `#[global_allocator]`
//! shim that aborts on any allocation, then runs `step` for 10 000
//! iterations against a pre-seeded mixer)."
//!
//! The shim is armed *only* around the `step` calls — the test
//! prologue still needs to allocate sample / output buffers, and we
//! disarm before they drop. While armed, any allocation aborts the
//! process with a panic message tagged for grep.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, Ordering};

struct Tripwire;

static ARMED: AtomicBool = AtomicBool::new(false);

unsafe impl GlobalAlloc for Tripwire {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::SeqCst) {
            panic!("audio_mixer::step heap-allocated while armed");
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Tripwire = Tripwire;

#[test]
fn step_does_not_allocate_for_10000_iterations() {
    use audio_mixer::{BYTES_PER_FRAME, Mixer};

    let samples: Vec<u8> = (0..256).map(|i| (i & 0xFF) as u8).collect();
    let mut out = vec![0u8; 256 * BYTES_PER_FRAME];
    let mut mixer = Mixer::new(8);
    for i in 0..8 {
        // SAFETY: `samples` outlives the channel's active period
        // (this test owns the buffer to end of function).
        unsafe {
            mixer.set_channel(i, &samples, 22_050, 64, 64);
        }
    }

    ARMED.store(true, Ordering::SeqCst);
    for _ in 0..10_000 {
        let n = mixer.step(&mut out, 256);
        // Re-seed channels in a loop body that itself must not allocate;
        // `set_channel` only stores a raw pointer, no heap traffic.
        if n == 0 {
            for i in 0..8 {
                // SAFETY: same as the initial seeding above.
                unsafe {
                    mixer.set_channel(i, &samples, 22_050, 64, 64);
                }
            }
        }
    }
    ARMED.store(false, Ordering::SeqCst);
}
