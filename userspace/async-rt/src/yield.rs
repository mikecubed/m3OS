//! Cooperative yield-once future for the cooperative executor.
//!
//! `yield_now().await` returns `Pending` once (re-waking itself), then
//! `Ready(())` on the next poll. The executor sees the wake during the
//! same iteration, so the task is re-queued and other tasks get a chance
//! to run before this one polls again.
//!
//! Used by call sites that loop inside a single `poll()` without any
//! other suspension point — without an explicit yield those loops can
//! starve the rest of the executor and the kernel scheduler (the H9 SSH
//! late-wedge in `userspace/sshd/src/session.rs::progress_task`).

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Cooperative single-poll yield. Returns `Pending` first, `Ready(())`
/// on the next poll.
pub fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::task::Wake;

    struct WakeFlag(AtomicBool);

    impl Wake for WakeFlag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn yield_now_pending_then_ready_and_wakes() {
        let flag = Arc::new(WakeFlag(AtomicBool::new(false)));
        let waker = std::task::Waker::from(Arc::clone(&flag));
        let mut cx = Context::from_waker(&waker);
        let mut fut = pin!(yield_now());

        // First poll: must return Pending and must self-wake.
        let poll1 = fut.as_mut().poll(&mut cx);
        assert!(
            matches!(poll1, Poll::Pending),
            "expected Pending on first poll"
        );
        assert!(
            flag.0.load(Ordering::SeqCst),
            "waker must be called on first poll"
        );

        // Second poll: must return Ready.
        let poll2 = fut.as_mut().poll(&mut cx);
        assert!(
            matches!(poll2, Poll::Ready(())),
            "expected Ready on second poll"
        );
    }
}
