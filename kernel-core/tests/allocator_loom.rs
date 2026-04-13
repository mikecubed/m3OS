//! Loom-based concurrency tests for allocator-side ordering-sensitive queues.
//!
//! These tests model the Release/Acquire publication used by the cross-CPU slab
//! free list: producers publish an intrusive next-link and then release-store
//! the new head; the consumer acquires the head with `take_all()` before
//! traversing the chain.
//!
//! Gated behind `#[cfg(loom)]` — run with:
//!   RUSTFLAGS="--cfg loom" cargo test -p kernel-core --target x86_64-unknown-linux-gnu --test allocator_loom

#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use loom::thread;
    use std::vec::Vec;

    const EMPTY: usize = usize::MAX;

    /// Small loom model of the cross-CPU intrusive free list.
    struct QueueModel {
        head: AtomicUsize,
        next: Vec<AtomicUsize>,
        ready: Vec<AtomicBool>,
    }

    impl QueueModel {
        fn new(nodes: usize) -> Self {
            Self {
                head: AtomicUsize::new(EMPTY),
                next: (0..nodes).map(|_| AtomicUsize::new(EMPTY)).collect(),
                ready: (0..nodes).map(|_| AtomicBool::new(false)).collect(),
            }
        }

        fn publish_and_push(&self, node: usize) {
            self.ready[node].store(true, Ordering::Relaxed);
            loop {
                let old_head = self.head.load(Ordering::Relaxed);
                self.next[node].store(old_head, Ordering::Relaxed);
                if self
                    .head
                    .compare_exchange_weak(old_head, node, Ordering::Release, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
            }
        }

        fn take_all(&self) -> usize {
            self.head.swap(EMPTY, Ordering::Acquire)
        }

        fn collect(&self, mut head: usize, seen: &mut [bool]) {
            while head != EMPTY {
                assert!(
                    self.ready[head].load(Ordering::Relaxed),
                    "consumer observed node {} before producer initialization became visible",
                    head
                );
                assert!(!seen[head], "node {} observed twice", head);
                seen[head] = true;
                head = self.next[head].load(Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn take_all_never_loses_concurrent_pushes() {
        loom::model(|| {
            let queue = Arc::new(QueueModel::new(2));

            let q0 = queue.clone();
            let producer0 = thread::spawn(move || {
                q0.publish_and_push(0);
            });

            let q1 = queue.clone();
            let producer1 = thread::spawn(move || {
                q1.publish_and_push(1);
            });

            let qc = queue.clone();
            let consumer = thread::spawn(move || {
                let mut seen = [false; 2];
                qc.collect(qc.take_all(), &mut seen);
                seen
            });

            producer0.join().unwrap();
            producer1.join().unwrap();

            let mut seen = consumer.join().unwrap();
            queue.collect(queue.take_all(), &mut seen);
            assert!(seen.into_iter().all(|bit| bit));
        });
    }

    #[test]
    fn take_all_acquire_observes_published_node_state() {
        loom::model(|| {
            let queue = Arc::new(QueueModel::new(1));

            let producer_queue = queue.clone();
            let producer = thread::spawn(move || {
                producer_queue.publish_and_push(0);
            });

            let consumer_queue = queue.clone();
            let consumer = thread::spawn(move || {
                let mut seen = [false; 1];
                loop {
                    let head = consumer_queue.take_all();
                    if head == EMPTY {
                        thread::yield_now();
                        continue;
                    }
                    consumer_queue.collect(head, &mut seen);
                    return seen[0];
                }
            });

            producer.join().unwrap();
            assert!(consumer.join().unwrap());
        });
    }
}

#[cfg(not(loom))]
mod tests {
    #[test]
    fn loom_tests_require_cfg_loom() {
        // Loom tests are gated behind #[cfg(loom)].
        // Run with:
        // RUSTFLAGS="--cfg loom" cargo test -p kernel-core --target x86_64-unknown-linux-gnu --test allocator_loom
    }
}
