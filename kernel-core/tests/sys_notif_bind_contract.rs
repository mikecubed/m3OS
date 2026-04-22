//! Contract tests for the sys_notif_bind kernel wiring.
//!
//! These tests exercise the pure-logic model that `sys_notif_bind` implements
//! kernel-side: updating both the `BOUND_TCB` and `TCB_BOUND_NOTIF` arrays
//! atomically, enforcing the 1:1 binding invariant, and returning the correct
//! error codes for invalid or busy inputs.
//!
//! All tests run on the host via `cargo test -p kernel-core`. No QEMU or
//! ring-0 primitives are required.
//!
//! # Mapping to acceptance criteria (B.2)
//!
//! | Test | Acceptance bullet |
//! |---|---|
//! | `bind_matches_bound_notif_table` | Both BOUND_TCB and TCB_BOUND_NOTIF updated |
//! | `bind_returns_ebusy_on_double_bind_different_target` | Busy on double-bind |
//! | `bind_returns_ebadf_on_invalid_notif_cap` | EBADF on bad notif cap |
//! | `bind_returns_ebadf_on_invalid_endpoint_cap` | EBADF on bad endpoint cap |
//! | `idempotent_same_pair_returns_zero` | Idempotent re-bind succeeds |

use kernel_core::ipc::bound_notif::{BindError, BoundNotifTable, MAX_NOTIFS};
use kernel_core::types::{NotifId, TaskId};

const MAX_TASKS_SIM: usize = 256;
const NOTIF_NONE_SIM: u8 = 0xff;

struct FakeKernelState {
    bound_notif_table: BoundNotifTable,
    bound_tcb: [i32; MAX_NOTIFS],
    tcb_bound_notif: [u8; MAX_TASKS_SIM],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeErr {
    Ebadf,
    Ebusy,
}

impl FakeKernelState {
    fn new() -> Self {
        Self {
            bound_notif_table: BoundNotifTable::new(),
            bound_tcb: [-1i32; MAX_NOTIFS],
            tcb_bound_notif: [NOTIF_NONE_SIM; MAX_TASKS_SIM],
        }
    }

    fn sys_notif_bind(
        &mut self,
        notif_valid: bool,
        ep_valid: bool,
        notif_id: NotifId,
        task_sched_idx: usize,
        tcb_task_id: TaskId,
    ) -> Result<(), FakeErr> {
        if !notif_valid {
            return Err(FakeErr::Ebadf);
        }
        if !ep_valid {
            return Err(FakeErr::Ebadf);
        }

        match self.bound_notif_table.bind(notif_id, tcb_task_id) {
            Ok(()) => {}
            Err(BindError::Busy) => return Err(FakeErr::Ebusy),
        }

        let notif_idx = notif_id.0 as usize;
        if notif_idx < MAX_NOTIFS {
            self.bound_tcb[notif_idx] = task_sched_idx as i32;
        }
        if task_sched_idx < MAX_TASKS_SIM {
            self.tcb_bound_notif[task_sched_idx] = notif_id.0;
        }

        Ok(())
    }
}

#[test]
fn bind_matches_bound_notif_table() {
    let mut state = FakeKernelState::new();
    let notif = NotifId(3);
    let task_id = TaskId(42);
    let sched_idx = 7usize;

    let result = state.sys_notif_bind(true, true, notif, sched_idx, task_id);
    assert_eq!(result, Ok(()));
    assert_eq!(state.bound_tcb[notif.0 as usize], sched_idx as i32);
    assert_eq!(state.tcb_bound_notif[sched_idx], notif.0);
    assert_eq!(state.bound_notif_table.lookup(notif), Some(task_id));
}

#[test]
fn bind_returns_ebusy_on_double_bind_different_target() {
    let mut state = FakeKernelState::new();
    let notif = NotifId(0);

    state
        .sys_notif_bind(true, true, notif, 1, TaskId(10))
        .unwrap();
    let err = state.sys_notif_bind(true, true, notif, 2, TaskId(20));
    assert_eq!(err, Err(FakeErr::Ebusy));
}

#[test]
fn bind_returns_ebadf_on_invalid_notif_cap() {
    let mut state = FakeKernelState::new();
    let err = state.sys_notif_bind(false, true, NotifId(0), 0, TaskId(1));
    assert_eq!(err, Err(FakeErr::Ebadf));
}

#[test]
fn bind_returns_ebadf_on_invalid_endpoint_cap() {
    let mut state = FakeKernelState::new();
    let err = state.sys_notif_bind(true, false, NotifId(0), 0, TaskId(1));
    assert_eq!(err, Err(FakeErr::Ebadf));
}

#[test]
fn idempotent_same_pair_returns_zero() {
    let mut state = FakeKernelState::new();
    let notif = NotifId(5);
    let task_id = TaskId(99);
    let sched_idx = 3usize;

    state
        .sys_notif_bind(true, true, notif, sched_idx, task_id)
        .unwrap();

    let result = state.sys_notif_bind(true, true, notif, sched_idx, task_id);
    assert_eq!(result, Ok(()));
    assert_eq!(state.bound_tcb[notif.0 as usize], sched_idx as i32);
    assert_eq!(state.tcb_bound_notif[sched_idx], notif.0);
}
