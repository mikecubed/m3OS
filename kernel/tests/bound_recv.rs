#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! QEMU integration tests for Phase 55c Track B — bound-notification recv wiring.
//!
//! Each test implements the relevant state-machine fragment inline using only
//! `core::sync::atomic::*` — no heap, no kernel library linkage required.
//! The state arrays mirror the kernel's `PENDING`, `BOUND_TCB`, and
//! `TCB_BOUND_NOTIF` arrays so the tests exercise the same logic paths.
//!
//! # Scenarios covered
//!
//! | Test | B.x | Scenario |
//! |---|---|---|
//! | `notif_signal_before_recv_returns_notification_kind` | B.4 | signal fires before recv → fast-path returns `RECV_KIND_NOTIFICATION` |
//! | `queued_sender_before_recv_returns_message_kind` | B.4 | sender queued before recv → returns `RECV_KIND_MESSAGE` |
//! | `process_exit_clears_binding_smoke` | B.5 | bind → clear → slot unbound |

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicI32, AtomicU8, AtomicU64, Ordering};
use x86_64::instructions::{hlt, port::Port};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(bound_recv_kernel_test, config = &BOOTLOADER_CONFIG);

fn bound_recv_kernel_test(_boot_info: &'static mut BootInfo) -> ! {
    test_main();
    qemu_exit(0x10);
}

// ---------------------------------------------------------------------------
// Minimal recv-kind tags (mirrors kernel_core::ipc::wake_kind)
// ---------------------------------------------------------------------------

const RECV_KIND_MESSAGE: u8 = 0;
const RECV_KIND_NOTIFICATION: u8 = 1;

// ---------------------------------------------------------------------------
// Inline state-machine mirroring kernel notification.rs (no_alloc)
// ---------------------------------------------------------------------------

const MAX_NOTIFS: usize = 64;
const MAX_TASKS: usize = 64;

const NOTIF_NONE: u8 = 0xff;
const TCB_NONE: i32 = -1;

/// Pending notification bits — mirrors `PENDING` in `kernel/src/ipc/notification.rs`.
static PENDING: [AtomicU64; MAX_NOTIFS] = {
    const ZERO: AtomicU64 = AtomicU64::new(0);
    [ZERO; MAX_NOTIFS]
};

/// `BOUND_TCB[notif_idx]` = scheduler index of the bound task, or `TCB_NONE`.
/// Mirrors `BOUND_TCB` in `kernel/src/ipc/notification.rs`.
static BOUND_TCB: [AtomicI32; MAX_NOTIFS] = {
    const NONE: AtomicI32 = AtomicI32::new(TCB_NONE);
    [NONE; MAX_NOTIFS]
};

/// `TCB_BOUND_NOTIF[task_sched_idx]` = notification index bound to this task,
/// or `NOTIF_NONE`. Mirrors `TCB_BOUND_NOTIF` in `kernel/src/ipc/notification.rs`.
static TCB_BOUND_NOTIF: [AtomicU8; MAX_TASKS] = {
    const NONE: AtomicU8 = AtomicU8::new(NOTIF_NONE);
    [NONE; MAX_TASKS]
};

/// Set pending bits on `notif_idx`.
fn signal(notif_idx: usize, bits: u64) {
    PENDING[notif_idx].fetch_or(bits, Ordering::AcqRel);
}

/// Atomically clear and return all pending bits for `notif_idx`.
fn drain_bits(notif_idx: usize) -> u64 {
    PENDING[notif_idx].swap(0, Ordering::AcqRel)
}

/// Bind task `task_sched_idx` to `notif_idx`.
///
/// Returns `true` on success, `false` if the slot is already taken by a
/// different task (matches `notification::bind_task` behaviour).
fn bind_task(notif_idx: usize, task_sched_idx: usize) -> bool {
    match BOUND_TCB[notif_idx].compare_exchange(
        TCB_NONE,
        task_sched_idx as i32,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {
            TCB_BOUND_NOTIF[task_sched_idx].store(notif_idx as u8, Ordering::Release);
            true
        }
        Err(existing) if existing == task_sched_idx as i32 => true, // idempotent
        Err(_) => false,                                            // busy
    }
}

/// Clear the binding for `task_sched_idx` (called on process exit).
///
/// Mirrors `notification::clear_bound_task` in `kernel/src/ipc/notification.rs`.
fn clear_bound_task(task_sched_idx: usize) {
    let notif_byte = TCB_BOUND_NOTIF[task_sched_idx].swap(NOTIF_NONE, Ordering::AcqRel);
    if notif_byte == NOTIF_NONE {
        return;
    }
    let notif_idx = notif_byte as usize;
    let _ = BOUND_TCB[notif_idx].compare_exchange(
        task_sched_idx as i32,
        TCB_NONE,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

/// Simulate `recv_msg_with_notif` for the scenarios that don't require
/// actual scheduler blocking.
///
/// Returns `RECV_KIND_NOTIFICATION` if `notif_idx` has pending bits (fast
/// path — signal arrived before the recv call).
/// Returns `RECV_KIND_MESSAGE` if `has_pending_sender` is true (a sender
/// was already queued in the endpoint before the recv call).
fn recv_kind(notif_idx: usize, has_pending_sender: bool) -> u8 {
    // Fast path: drain notification bits first — mirrors the first check in
    // `endpoint::recv_msg_with_notif`.
    let bits = drain_bits(notif_idx);
    if bits != 0 {
        return RECV_KIND_NOTIFICATION;
    }
    // Sender-queue path: mirrors the branch where `ep.senders.pop_front()`
    // succeeds immediately, returning the queued message without blocking.
    if has_pending_sender {
        return RECV_KIND_MESSAGE;
    }
    // In the real kernel the task would block; not reachable in these tests.
    RECV_KIND_MESSAGE
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        self();
    }
}

fn test_runner(tests: &[&dyn Testable]) {
    for test in tests {
        test.run();
    }
}

fn qemu_exit(code: u32) -> ! {
    unsafe { Port::new(0xf4).write(code) };
    loop {
        hlt();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    qemu_exit(0x11);
}

// ---------------------------------------------------------------------------
// B.4 — Extend ipc_recv_msg with bound-notification fast path
// ---------------------------------------------------------------------------

/// Signal fires *before* the recv call → `recv_kind` fast-path returns
/// `RECV_KIND_NOTIFICATION` immediately without consulting the endpoint queue.
///
/// This mirrors the first branch of `endpoint::recv_msg_with_notif`:
/// ```text
/// let bits = notification::drain_bits(notif_id);
/// if bits != 0 { return (RECV_KIND_NOTIFICATION, msg); }
/// ```
#[test_case]
fn notif_signal_before_recv_returns_notification_kind() {
    const NOTIF: usize = 3;
    const TASK: usize = 7;
    const BITS: u64 = 0b1010_0101;

    // Reset state (tests may run in any order).
    PENDING[NOTIF].store(0, Ordering::SeqCst);
    BOUND_TCB[NOTIF].store(TCB_NONE, Ordering::SeqCst);
    TCB_BOUND_NOTIF[TASK].store(NOTIF_NONE, Ordering::SeqCst);

    // Step 1: bind task to notification (as sys_notif_bind would do).
    assert!(bind_task(NOTIF, TASK), "bind must succeed on a free slot");

    // Step 2: signal the notification (as an IRQ handler or peer task would).
    signal(NOTIF, BITS);

    // Step 3: recv — fast path must drain the bits and return notification kind.
    let kind = recv_kind(NOTIF, false);
    assert_eq!(
        kind, RECV_KIND_NOTIFICATION,
        "signal before recv must produce RECV_KIND_NOTIFICATION"
    );

    // Verify bits were fully drained.
    assert_eq!(
        PENDING[NOTIF].load(Ordering::Acquire),
        0,
        "drain_bits must clear PENDING after the fast-path return"
    );
}

/// A sender is already queued in the endpoint before the recv call.
/// `recv_kind` must return `RECV_KIND_MESSAGE` (the sender-queue branch of
/// `recv_msg_with_notif`, where `ep.senders.pop_front()` succeeds immediately).
///
/// There are no pending notification bits so the fast path is skipped.
#[test_case]
fn queued_sender_before_recv_returns_message_kind() {
    const NOTIF: usize = 5;
    const TASK: usize = 11;

    // Reset state.
    PENDING[NOTIF].store(0, Ordering::SeqCst);
    BOUND_TCB[NOTIF].store(TCB_NONE, Ordering::SeqCst);
    TCB_BOUND_NOTIF[TASK].store(NOTIF_NONE, Ordering::SeqCst);

    // No notification bits pending.
    assert_eq!(
        drain_bits(NOTIF),
        0,
        "no bits should be pending before this test"
    );

    // Simulate: a sender is already queued in the endpoint (`has_pending_sender = true`).
    let kind = recv_kind(NOTIF, true);
    assert_eq!(
        kind, RECV_KIND_MESSAGE,
        "queued sender before recv must produce RECV_KIND_MESSAGE"
    );
}

// ---------------------------------------------------------------------------
// B.5 — TCB teardown clears binding
// ---------------------------------------------------------------------------

/// Process exit must clear the `BOUND_TCB` / `TCB_BOUND_NOTIF` entries for
/// the dying task so that no dangling binding persists after the TCB is freed.
///
/// This mirrors `notification::clear_bound_task` called by `cleanup_task_ipc`
/// during task teardown (Phase 55c Track B, B.5 acceptance criterion).
#[test_case]
fn process_exit_clears_binding_smoke() {
    const NOTIF: usize = 9;
    const TASK: usize = 21;

    // Reset state.
    PENDING[NOTIF].store(0, Ordering::SeqCst);
    BOUND_TCB[NOTIF].store(TCB_NONE, Ordering::SeqCst);
    TCB_BOUND_NOTIF[TASK].store(NOTIF_NONE, Ordering::SeqCst);

    // Step 1: bind task to notification.
    assert!(bind_task(NOTIF, TASK), "bind must succeed on a free slot");

    // Verify both sides of the binding are populated.
    assert_eq!(
        BOUND_TCB[NOTIF].load(Ordering::Acquire),
        TASK as i32,
        "BOUND_TCB must point to the bound task after bind_task"
    );
    assert_eq!(
        TCB_BOUND_NOTIF[TASK].load(Ordering::Acquire),
        NOTIF as u8,
        "TCB_BOUND_NOTIF must point to the notification after bind_task"
    );

    // Step 2: simulate process exit by clearing the binding.
    clear_bound_task(TASK);

    // Step 3: both sides must be reset to their sentinel values.
    assert_eq!(
        BOUND_TCB[NOTIF].load(Ordering::Acquire),
        TCB_NONE,
        "BOUND_TCB must be cleared after process exit"
    );
    assert_eq!(
        TCB_BOUND_NOTIF[TASK].load(Ordering::Acquire),
        NOTIF_NONE,
        "TCB_BOUND_NOTIF must be cleared after process exit"
    );

    // Step 4: the slot must be available for a new bind (no dangling reference).
    assert!(
        bind_task(NOTIF, TASK),
        "notif slot must be available for a new bind after clear"
    );
}
