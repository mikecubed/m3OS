//! Loom-based concurrency tests for IPC block/wake protocol.
//!
//! These tests verify that the send/recv and call/reply protocols
//! do not lose messages or wakeups under all possible thread interleavings.
//!
//! Gated behind `#[cfg(loom)]` — run with:
//!   RUSTFLAGS="--cfg loom" cargo test -p kernel-core --test ipc_loom

#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicU8, Ordering};
    use loom::thread;

    /// State constants matching the kernel's IPC task states.
    const RUNNING: u8 = 1;
    const BLOCKED_ON_RECV: u8 = 2;
    const READY: u8 = 0;

    /// Simplified IPC endpoint model: one sender, one receiver.
    /// Verifies that send + recv with concurrent wake never loses a message.
    ///
    /// The receiver re-checks the message after publishing BLOCKED_ON_RECV
    /// to avoid the lost-wakeup window where the sender delivers between
    /// the initial check and the state transition.
    #[test]
    fn send_recv_no_lost_message() {
        loom::model(|| {
            let state = Arc::new(AtomicU8::new(RUNNING));
            let message = Arc::new(AtomicU8::new(0)); // 0 = no message, 1 = delivered

            let state_recv = state.clone();
            let msg_recv = message.clone();
            let state_send = state.clone();
            let msg_send = message.clone();

            // Receiver thread: blocks waiting for a message.
            let receiver = thread::spawn(move || {
                // Try to receive — if no message yet, block.
                if msg_recv.load(Ordering::Acquire) == 0 {
                    state_recv.store(BLOCKED_ON_RECV, Ordering::Release);
                    // Re-check message after publishing blocked state to close
                    // the lost-wakeup window.
                    if msg_recv.load(Ordering::Acquire) != 0 {
                        state_recv.store(READY, Ordering::Release);
                    } else {
                        // Spin until woken (state changed back to READY).
                        while state_recv.load(Ordering::Acquire) == BLOCKED_ON_RECV {
                            loom::thread::yield_now();
                        }
                    }
                }
                // Must have a message now.
                assert_eq!(
                    msg_recv.load(Ordering::Acquire),
                    1,
                    "receiver woke without message"
                );
            });

            // Sender thread: delivers message and wakes receiver.
            let sender = thread::spawn(move || {
                // Deliver message.
                msg_send.store(1, Ordering::Release);
                // Wake receiver if blocked.
                let prev = state_send.load(Ordering::Acquire);
                if prev == BLOCKED_ON_RECV {
                    state_send.store(READY, Ordering::Release);
                }
            });

            sender.join().unwrap();
            receiver.join().unwrap();
        });
    }

    /// Call + reply: caller sends and blocks; server receives and replies.
    /// Verifies the caller always receives the reply.
    ///
    /// The caller re-checks the reply message after publishing BLOCKED_ON_RECV
    /// to close the lost-wakeup window.
    #[test]
    fn call_reply_always_delivers() {
        loom::model(|| {
            let caller_state = Arc::new(AtomicU8::new(RUNNING));
            let reply_msg = Arc::new(AtomicU8::new(0)); // 0 = no reply, 42 = reply
            let request_msg = Arc::new(AtomicU8::new(0)); // 0 = no request, 1 = request

            let cs = caller_state.clone();
            let rm = reply_msg.clone();
            let rq = request_msg.clone();

            let cs2 = caller_state.clone();
            let rm2 = reply_msg.clone();
            let rq2 = request_msg.clone();

            // Caller: send request, then block for reply.
            let caller = thread::spawn(move || {
                // Send request.
                rq.store(1, Ordering::Release);
                // Block waiting for reply.
                cs.store(BLOCKED_ON_RECV, Ordering::Release);
                // Re-check reply after publishing blocked state to close the
                // lost-wakeup window.
                if rm.load(Ordering::Acquire) == 42 {
                    cs.store(READY, Ordering::Release);
                } else {
                    while cs.load(Ordering::Acquire) == BLOCKED_ON_RECV {
                        loom::thread::yield_now();
                    }
                }
                // Must have reply.
                assert_eq!(rm.load(Ordering::Acquire), 42, "caller woke without reply");
            });

            // Server: wait for request, then reply.
            let server = thread::spawn(move || {
                // Spin until request arrives.
                while rq2.load(Ordering::Acquire) == 0 {
                    loom::thread::yield_now();
                }
                // Deliver reply.
                rm2.store(42, Ordering::Release);
                // Wake caller.
                let prev = cs2.load(Ordering::Acquire);
                if prev == BLOCKED_ON_RECV {
                    cs2.store(READY, Ordering::Release);
                }
            });

            caller.join().unwrap();
            server.join().unwrap();
        });
    }
}

/// Non-loom placeholder tests that always pass when loom is not active.
#[cfg(not(loom))]
mod tests {
    #[test]
    fn loom_tests_require_cfg_loom() {
        // Loom tests are gated behind #[cfg(loom)].
        // Run with: RUSTFLAGS="--cfg loom" cargo test -p kernel-core --test ipc_loom
    }
}
