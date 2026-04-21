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
