//! Property-based tests for scheduler state machine invariants.
//!
//! These tests operate on an extracted model of the scheduler's task
//! state transitions, not the full kernel scheduler. The model captures
//! the key invariants that must hold regardless of interleaving.

use proptest::prelude::*;

/// Simplified task state model matching kernel TaskState.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskState {
    Ready,
    Running,
    BlockedOnRecv,
    BlockedOnSend,
    BlockedOnReply,
}

/// Simplified task model.
#[derive(Debug, Clone)]
struct TaskModel {
    state: TaskState,
    saved_rsp: u64,
    assigned_core: u8,
    wake_after_switch: bool,
    switching_out: bool,
}

impl TaskModel {
    fn new(rsp: u64, core: u8) -> Self {
        Self {
            state: TaskState::Ready,
            saved_rsp: rsp,
            assigned_core: core,
            wake_after_switch: false,
            switching_out: false,
        }
    }
}

/// Simplified run queue model (one per core).
struct RunQueue {
    queue: Vec<usize>,
}

impl RunQueue {
    fn new() -> Self {
        Self { queue: Vec::new() }
    }

    fn push(&mut self, idx: usize) {
        self.queue.push(idx);
    }

    fn pop(&mut self) -> Option<usize> {
        if self.queue.is_empty() {
            None
        } else {
            Some(self.queue.remove(0))
        }
    }

    fn contains(&self, idx: usize) -> bool {
        self.queue.contains(&idx)
    }
}

/// Model scheduler with 2 cores and up to 8 tasks.
struct SchedulerModel {
    tasks: Vec<TaskModel>,
    run_queues: [RunQueue; 2],
    current_task: [Option<usize>; 2],
}

impl SchedulerModel {
    fn new(tasks: Vec<TaskModel>) -> Self {
        let mut model = Self {
            tasks,
            run_queues: [RunQueue::new(), RunQueue::new()],
            current_task: [None, None],
        };
        // Enqueue all Ready tasks to their assigned core.
        for (i, task) in model.tasks.iter().enumerate() {
            if task.state == TaskState::Ready {
                model.run_queues[task.assigned_core as usize].push(i);
            }
        }
        model
    }

    /// Dispatch: pick next Ready task from core's run queue.
    fn dispatch(&mut self, core: u8) -> Option<usize> {
        let core_idx = core as usize;
        if let Some(idx) = self.run_queues[core_idx].pop() {
            assert_eq!(
                self.tasks[idx].state,
                TaskState::Ready,
                "dispatch: task must be Ready"
            );
            assert_ne!(
                self.tasks[idx].saved_rsp, 0,
                "dispatch: saved_rsp must not be zero"
            );
            self.tasks[idx].state = TaskState::Running;
            self.current_task[core_idx] = Some(idx);
            Some(idx)
        } else {
            None
        }
    }

    /// Block: transition Running → Blocked.
    fn block(&mut self, core: u8, blocked_state: TaskState) {
        let core_idx = core as usize;
        if let Some(idx) = self.current_task[core_idx] {
            assert_eq!(self.tasks[idx].state, TaskState::Running);
            self.tasks[idx].state = blocked_state;
            self.current_task[core_idx] = None;
        }
    }

    /// Wake: transition Blocked → Ready, enqueue to assigned core.
    fn wake(&mut self, idx: usize) -> bool {
        match self.tasks[idx].state {
            TaskState::BlockedOnRecv | TaskState::BlockedOnSend | TaskState::BlockedOnReply => {
                self.tasks[idx].state = TaskState::Ready;
                let core = self.tasks[idx].assigned_core as usize;
                self.run_queues[core].push(idx);
                true
            }
            TaskState::Running => false, // No-op for running task
            TaskState::Ready => false,   // Already ready
        }
    }

    /// Yield: Running → Ready, re-enqueue.
    fn yield_now(&mut self, core: u8) {
        let core_idx = core as usize;
        if let Some(idx) = self.current_task[core_idx] {
            assert_eq!(self.tasks[idx].state, TaskState::Running);
            self.tasks[idx].state = TaskState::Ready;
            self.run_queues[core_idx].push(idx);
            self.current_task[core_idx] = None;
        }
    }
}

proptest! {
    /// A task's saved_rsp is never zero when it transitions to Ready.
    #[test]
    fn saved_rsp_nonzero_on_ready(
        rsp1 in 0x1000u64..0xFFFF_FFFF,
        rsp2 in 0x1000u64..0xFFFF_FFFF,
        rsp3 in 0x1000u64..0xFFFF_FFFF,
    ) {
        let tasks = vec![
            TaskModel::new(rsp1, 0),
            TaskModel::new(rsp2, 0),
            TaskModel::new(rsp3, 1),
        ];
        let mut sched = SchedulerModel::new(tasks);

        // Dispatch on core 0, block, wake — RSP should still be nonzero.
        if let Some(idx) = sched.dispatch(0) {
            sched.block(0, TaskState::BlockedOnRecv);
            sched.wake(idx);
            assert_ne!(sched.tasks[idx].saved_rsp, 0);
            assert_eq!(sched.tasks[idx].state, TaskState::Ready);
        }
    }

    /// A task cannot be in Ready state on two cores' run queues simultaneously.
    #[test]
    fn no_dual_enqueue(
        core_assignment in prop::collection::vec(0u8..2, 4..=4),
    ) {
        let tasks: Vec<TaskModel> = core_assignment
            .iter()
            .enumerate()
            .map(|(i, &core)| TaskModel::new(0x1000 + (i as u64) * 0x100, core))
            .collect();
        let sched = SchedulerModel::new(tasks);

        // Check no task appears in both run queues.
        for i in 0..4 {
            let in_q0 = sched.run_queues[0].contains(i);
            let in_q1 = sched.run_queues[1].contains(i);
            assert!(!(in_q0 && in_q1), "task {} in both queues", i);
        }
    }

    /// wake_task on a Running task is a no-op (no double-enqueue).
    #[test]
    fn wake_running_is_noop(rsp in 0x1000u64..0xFFFF_FFFF) {
        let tasks = vec![TaskModel::new(rsp, 0)];
        let mut sched = SchedulerModel::new(tasks);

        sched.dispatch(0);
        assert_eq!(sched.tasks[0].state, TaskState::Running);

        let woke = sched.wake(0);
        assert!(!woke, "wake on Running must return false");
        assert_eq!(sched.tasks[0].state, TaskState::Running);
        // Must not be re-enqueued.
        assert!(!sched.run_queues[0].contains(0));
    }

    /// block_current followed by wake_task results in Ready state.
    #[test]
    fn block_then_wake_is_ready(rsp in 0x1000u64..0xFFFF_FFFF) {
        let tasks = vec![TaskModel::new(rsp, 0)];
        let mut sched = SchedulerModel::new(tasks);

        sched.dispatch(0);
        sched.block(0, TaskState::BlockedOnSend);
        assert_eq!(sched.tasks[0].state, TaskState::BlockedOnSend);

        let woke = sched.wake(0);
        assert!(woke);
        assert_eq!(sched.tasks[0].state, TaskState::Ready);
        assert!(sched.run_queues[0].contains(0));
    }

    /// Yield returns a task to its core's run queue in Ready state.
    #[test]
    fn yield_returns_to_queue(rsp in 0x1000u64..0xFFFF_FFFF) {
        let tasks = vec![TaskModel::new(rsp, 0)];
        let mut sched = SchedulerModel::new(tasks);

        sched.dispatch(0);
        assert_eq!(sched.tasks[0].state, TaskState::Running);

        sched.yield_now(0);
        assert_eq!(sched.tasks[0].state, TaskState::Ready);
        assert!(sched.run_queues[0].contains(0));
        assert!(sched.current_task[0].is_none());
    }
}
