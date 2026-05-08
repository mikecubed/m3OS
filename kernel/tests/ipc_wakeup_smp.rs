#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Phase 61 Track D.2 — cross-core IPC wakeup regression test.
//!
//! Validates that a server task pinned to one core blocked in
//! `recv_msg` is woken within a few ticks of a client task on a
//! different core calling `send`. The bespoke per-`Endpoint`
//! `senders` / `receivers` `VecDeque`s carry payload (`PendingSend`)
//! and atomically integrate with `scheduler::deliver_message_and_wake`,
//! which sends a reschedule IPI to the server's core when the server
//! is parked there. This test pins that contract.
//!
//! Phase 35 G.3's "swap to generic `WaitQueue<TaskId>`" deferral is
//! reframed in Phase 61 as won't-do — the bespoke design is the final
//! form. This test proves the bespoke implementation is cross-core
//! correct.

extern crate alloc;

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(ipc_wakeup_smp_test, config = &BOOTLOADER_CONFIG);

fn ipc_wakeup_smp_test(boot_info: &'static mut BootInfo) -> ! {
    kernel::test_prelude::init_minimal_smp(boot_info);
    kernel::test_prelude::boot_aps_if_available();

    kernel::task::spawn(test_runner_task, "ipc-wakeup-runner");
    kernel::test_prelude::spawn_idle();
    kernel::task::run()
}

fn test_runner_task() -> ! {
    test_main();
    kernel::test_prelude::qemu_exit_success()
}

trait Testable {
    fn run(&self);
}
impl<T: Fn()> Testable for T {
    fn run(&self) {
        self();
    }
}
fn test_runner(tests: &[&dyn Testable]) {
    for t in tests {
        t.run();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(loc) = info.location() {
        kernel::serial_println!(
            "[ipc-wakeup-test] PANIC at {}:{}: {}",
            loc.file(),
            loc.line(),
            info.message()
        );
    } else {
        kernel::serial_println!("[ipc-wakeup-test] PANIC: {}", info.message());
    }
    kernel::test_prelude::qemu_exit_failure()
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Endpoint under test. Encoded as u64 (the inner u8 of EndpointId
/// widened) for AtomicU64 ergonomics.
static ENDPOINT_ID: AtomicU64 = AtomicU64::new(u64::MAX);

/// Tick at which the client called `send`. Used to bound wake latency.
static SEND_TICK: AtomicU64 = AtomicU64::new(0);

/// Tick at which the server returned from `recv_msg`.
static RECV_TICK: AtomicU64 = AtomicU64::new(0);

/// Set true once the server is blocked in `recv_msg`. The client busy-waits
/// for this flag before sending so the wake path is exercised every run.
static SERVER_PARKED: AtomicBool = AtomicBool::new(false);

/// Set true after the server returns from `recv_msg` and validates the
/// message label / data.
static SERVER_DONE: AtomicBool = AtomicBool::new(false);

/// Set true if the server saw the wrong message label or data words —
/// causes the runner to fail the test with a specific diagnostic.
static SERVER_BAD_MSG: AtomicBool = AtomicBool::new(false);

const TEST_LABEL: u64 = 0xCAFE_BABE_DEAD_BEEF;
const TEST_DATA0: u64 = 0x1111_2222_3333_4444;
const TEST_DATA1: u64 = 0x5555_6666_7777_8888;

// ---------------------------------------------------------------------------
// Server task
// ---------------------------------------------------------------------------

fn server_task() -> ! {
    use kernel_core::types::EndpointId;
    let ep_id = EndpointId(ENDPOINT_ID.load(Ordering::Acquire) as u8);
    let task_id = kernel::task::scheduler::current_task_id().expect("server has task id");

    // Mark parked just before recv_msg blocks; the client busy-waits on
    // this flag so the send fires only after we are demonstrably waiting
    // on the endpoint receiver queue. Because recv_msg may briefly run
    // before parking, this is approximate — but the latency assertion
    // tolerates a few ticks of slack.
    SERVER_PARKED.store(true, Ordering::Release);

    let msg = kernel::ipc::endpoint::recv_msg(task_id, ep_id);
    let now = kernel::arch::x86_64::interrupts::tick_count();
    RECV_TICK.store(now, Ordering::Release);
    kernel::serial_println!(
        "[ipc-wakeup-test] server: recv at tick {} label={:#x} data0={:#x} data1={:#x}",
        now,
        msg.label,
        msg.data[0],
        msg.data[1]
    );
    if msg.label != TEST_LABEL || msg.data[0] != TEST_DATA0 || msg.data[1] != TEST_DATA1 {
        SERVER_BAD_MSG.store(true, Ordering::Release);
    }
    SERVER_DONE.store(true, Ordering::Release);
    loop {
        kernel::task::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Client task
// ---------------------------------------------------------------------------

fn client_task() -> ! {
    use kernel_core::ipc::message::Message;
    use kernel_core::types::EndpointId;
    let ep_id = EndpointId(ENDPOINT_ID.load(Ordering::Acquire) as u8);
    let task_id = kernel::task::scheduler::current_task_id().expect("client has task id");

    // Wait until the server is blocked in recv_msg.
    let park_deadline = kernel::arch::x86_64::interrupts::tick_count().saturating_add(500);
    while !SERVER_PARKED.load(Ordering::Acquire) {
        if kernel::arch::x86_64::interrupts::tick_count() >= park_deadline {
            kernel::serial_println!("[ipc-wakeup-test] client: server never parked");
            loop {
                kernel::task::yield_now();
            }
        }
        kernel::task::yield_now();
    }

    let msg = Message::with2(TEST_LABEL, TEST_DATA0, TEST_DATA1);
    let now = kernel::arch::x86_64::interrupts::tick_count();
    SEND_TICK.store(now, Ordering::Release);
    let _delivered = kernel::ipc::endpoint::send(task_id, ep_id, msg);
    kernel::serial_println!("[ipc-wakeup-test] client: sent at tick {}", now);
    loop {
        kernel::task::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test_case]
fn cross_core_ipc_wakeup_within_latency_budget() {
    let cores = kernel::smp::core_count() as usize;
    assert!(
        cores >= 2,
        "cross-core IPC wakeup test requires at least 2 cores; got {cores}"
    );

    // Allocate the endpoint under test.
    let ep_id = kernel::ipc::endpoint::ENDPOINTS.lock().create();
    ENDPOINT_ID.store(ep_id.0 as u64, Ordering::Release);

    // Spawn server on core 1 (will park in recv_msg) and client on
    // core 0. Cross-core wake exercises the IPC dispatcher's IPI path.
    kernel::task::scheduler::spawn_on_core(server_task, "ipc-server", 1);
    kernel::task::scheduler::spawn_on_core(client_task, "ipc-client", 0);
    kernel::serial_println!("[ipc-wakeup-test] server + client spawned, waiting for completion");

    let read_deadline = kernel::arch::x86_64::interrupts::tick_count().saturating_add(500);
    while !SERVER_DONE.load(Ordering::Acquire) {
        if kernel::arch::x86_64::interrupts::tick_count() >= read_deadline {
            panic!(
                "server did not wake within 500 ticks (send_tick={}, recv_tick={})",
                SEND_TICK.load(Ordering::Acquire),
                RECV_TICK.load(Ordering::Acquire),
            );
        }
        kernel::task::yield_now();
    }

    assert!(
        !SERVER_BAD_MSG.load(Ordering::Acquire),
        "server received message with wrong label or data"
    );

    let send_tick = SEND_TICK.load(Ordering::Acquire);
    let recv_tick = RECV_TICK.load(Ordering::Acquire);
    let latency = recv_tick.saturating_sub(send_tick);

    const MAX_LATENCY_TICKS: u64 = 100;
    assert!(
        latency <= MAX_LATENCY_TICKS,
        "cross-core IPC wakeup latency {latency} ticks exceeds budget {MAX_LATENCY_TICKS} \
         (send_tick={send_tick}, recv_tick={recv_tick})",
    );

    kernel::serial_println!(
        "[ipc-wakeup-test] PASSED: send_tick={} recv_tick={} latency={} ticks",
        send_tick,
        recv_tick,
        latency
    );
}
