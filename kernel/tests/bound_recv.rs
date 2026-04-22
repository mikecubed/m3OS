#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! QEMU integration tests for Phase 55c Track B — bound-notification recv wiring.
//!
//! Each test exercises a **real** shared seam from `kernel_core`, not a
//! hand-rolled copy of the kernel's internal logic.
//!
//! | Test | B.x | Seam called | Scenario |
//! |---|---|---|---|
//! | `notif_signal_before_recv_returns_notification_kind` | B.4 | `kernel_core::ipc::wake_kind::classify_recv` | non-zero bits → `RECV_KIND_NOTIFICATION` fast path |
//! | `queued_sender_before_recv_returns_message_kind` | B.4 | `kernel_core::ipc::wake_kind::classify_recv` | zero bits → `RECV_KIND_MESSAGE` (endpoint-queue branch) |
//! | `process_exit_clears_binding_smoke` | B.5 | `kernel_core::ipc::bound_notif::BoundNotifTable` | bind → unbind → slot free |
//!
//! # Why these seams?
//!
//! `classify_recv` is the function `endpoint::recv_msg_with_notif` calls for
//! its initial priority check (Phase 55c Track B wiring).  Any change to the
//! notification-wins-over-message rule breaks both the production path and
//! these tests simultaneously.
//!
//! `BoundNotifTable` is the pure-logic model of the TCB binding lifecycle that
//! lives in `kernel_core` (host-testable).  The ISR-safe atomic implementation
//! (`notification::clear_bound_task`) is exercised by the inline `#[test_case]`
//! blocks inside `kernel/src/ipc/notification.rs` which call the real function
//! on the real global state arrays.

extern crate alloc;

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use kernel_core::ipc::bound_notif::BoundNotifTable;
use kernel_core::ipc::wake_kind::{RECV_KIND_MESSAGE, RECV_KIND_NOTIFICATION, classify_recv};
use kernel_core::types::{NotifId, TaskId};
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
// Stub global allocator — satisfies the linker; tests must not actually
// allocate (all kernel_core types used here are stack-only / const-new).
// ---------------------------------------------------------------------------

struct NoAlloc;

unsafe impl GlobalAlloc for NoAlloc {
    unsafe fn alloc(&self, _: Layout) -> *mut u8 {
        core::ptr::null_mut()
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

#[global_allocator]
static STUB_ALLOC: NoAlloc = NoAlloc;

// ---------------------------------------------------------------------------
// Test infrastructure
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
//
// Both tests drive `classify_recv` — the shared function that
// `endpoint::recv_msg_with_notif` calls as its first priority check.
// Regression in the "notification wins over queued sender" rule
// will fail both the production recv path and these tests.
// ---------------------------------------------------------------------------

/// Non-zero pending bits → `classify_recv` returns `RECV_KIND_NOTIFICATION`.
///
/// This mirrors the fast-path opening of `endpoint::recv_msg_with_notif`:
/// ```text
/// let bits = notification::drain_bits(notif_id);
/// if classify_recv(bits) == RECV_KIND_NOTIFICATION {
///     return (RECV_KIND_NOTIFICATION, msg);
/// }
/// ```
/// A signal that arrives **before** the recv call leaves non-zero bits in
/// `PENDING`; `drain_bits` returns those bits and `classify_recv` selects the
/// notification branch without ever consulting the endpoint sender queue.
#[test_case]
fn notif_signal_before_recv_returns_notification_kind() {
    const BITS: u64 = 0b1010_0101;

    // classify_recv is the actual function recv_msg_with_notif delegates to.
    let kind = classify_recv(BITS);
    assert_eq!(
        kind, RECV_KIND_NOTIFICATION,
        "non-zero pending bits must select RECV_KIND_NOTIFICATION",
    );

    // After drain the pending word is zero: the next recv takes the message path.
    let kind_after_drain = classify_recv(0);
    assert_eq!(
        kind_after_drain, RECV_KIND_MESSAGE,
        "zero bits (post-drain) must select RECV_KIND_MESSAGE",
    );
}

/// Zero pending bits + queued sender → `classify_recv` returns `RECV_KIND_MESSAGE`.
///
/// When `drain_bits` returns 0, `recv_msg_with_notif` skips the notification
/// fast path and falls through to `ep.senders.pop_front()`.  `classify_recv(0)`
/// encodes that decision: zero bits always routes to the message / sender path.
///
/// There are no pending notification bits so the fast path is skipped.
#[test_case]
fn queued_sender_before_recv_returns_message_kind() {
    // No notification bits pending (as if drain_bits returned 0).
    let pending_bits: u64 = 0;
    let kind = classify_recv(pending_bits);
    assert_eq!(
        kind, RECV_KIND_MESSAGE,
        "zero pending bits must route to the message path (queued-sender branch)",
    );
}

// ---------------------------------------------------------------------------
// B.5 — TCB teardown clears binding
//
// Drives `BoundNotifTable` — the kernel-core shared model of the bind/unbind
// lifecycle.  The ISR-safe atomic implementation (`notification::clear_bound_task`
// called by `cleanup_task_ipc`) is exercised by the inline `#[test_case]`
// in `kernel/src/ipc/notification.rs` which runs against the real global arrays.
// ---------------------------------------------------------------------------

/// Process exit must clear both sides of the `notif ↔ task` binding so that
/// no dangling entry persists after the TCB is freed.
///
/// This test drives `BoundNotifTable::bind` / `unbind` — the shared pure-logic
/// helpers that model the same invariant the kernel enforces with atomic arrays.
#[test_case]
fn process_exit_clears_binding_smoke() {
    let mut table = BoundNotifTable::new();
    let notif = NotifId(9);
    let tcb = TaskId(21);

    // Step 1: bind task to notification (as sys_notif_bind does).
    assert!(
        table.bind(notif, tcb).is_ok(),
        "bind must succeed on an unbound slot",
    );
    assert_eq!(
        table.lookup(notif),
        Some(tcb),
        "lookup must return the bound task after bind",
    );

    // Step 2: simulate process exit — clear the binding via the shared helper.
    let was = table.unbind(notif);
    assert_eq!(
        was,
        Some(tcb),
        "unbind must return the previously bound task",
    );

    // Step 3: slot must be unbound and available for a new bind.
    assert_eq!(
        table.lookup(notif),
        None,
        "lookup must return None after unbind",
    );
    assert!(
        table.bind(notif, tcb).is_ok(),
        "re-bind must succeed after unbind (no dangling reference)",
    );
}
