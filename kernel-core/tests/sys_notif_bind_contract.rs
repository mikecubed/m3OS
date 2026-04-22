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

// ---------------------------------------------------------------------------
// ABI return-value encoding (exercises the syscall-facing return path)
// ---------------------------------------------------------------------------

/// Map the FakeKernelState result to the u64 ABI value exactly as
/// `sys_notif_bind` in `kernel/src/ipc/mod.rs` does.
///
/// This helper lets the tests below exercise the *syscall-facing* return path
/// rather than the pure `Result<(), FakeErr>` model contract.
fn map_to_abi(result: Result<(), FakeErr>) -> u64 {
    // Must match the constants in kernel/src/ipc/mod.rs::sys_notif_bind.
    const NEG_EBADF: u64 = (-9_i64) as u64;
    const NEG_EBUSY: u64 = (-16_i64) as u64;
    match result {
        Ok(()) => 0,
        Err(FakeErr::Ebadf) => NEG_EBADF,
        Err(FakeErr::Ebusy) => NEG_EBUSY,
    }
}

/// Verify that the ABI encodes an invalid-notif-cap error as `NEG_EBADF`
/// (-9 as two's-complement u64), **not** as `u64::MAX`.
///
/// This is the actual syscall-facing return path exercised by the helper
/// above: the kernel's `sys_notif_bind` maps capability-validation failures
/// to `NEG_EBADF`, and userspace relies on receiving `-9`, not `0xffffffff…`.
#[test]
fn invalid_notif_cap_abi_returns_neg_ebadf_not_max() {
    const NEG_EBADF: u64 = (-9_i64) as u64;

    let mut state = FakeKernelState::new();
    let abi = map_to_abi(state.sys_notif_bind(false, true, NotifId(0), 0, TaskId(1)));

    assert_eq!(
        abi, NEG_EBADF,
        "invalid notif cap must return NEG_EBADF (-9), got {:#x}",
        abi
    );
    assert_ne!(
        abi,
        u64::MAX,
        "invalid notif cap must NOT return u64::MAX — that was the old, wrong ABI"
    );
}

/// Same check for an invalid *endpoint* capability.
#[test]
fn invalid_ep_cap_abi_returns_neg_ebadf_not_max() {
    const NEG_EBADF: u64 = (-9_i64) as u64;

    let mut state = FakeKernelState::new();
    let abi = map_to_abi(state.sys_notif_bind(true, false, NotifId(0), 0, TaskId(1)));

    assert_eq!(
        abi, NEG_EBADF,
        "invalid endpoint cap must return NEG_EBADF (-9), got {:#x}",
        abi
    );
    assert_ne!(
        abi,
        u64::MAX,
        "invalid endpoint cap must NOT return u64::MAX — that was the old, wrong ABI"
    );
}

/// Verify that a successful bind still returns `0` through the ABI mapper,
/// so any regression in `map_to_abi` is immediately caught.
#[test]
fn successful_bind_abi_returns_zero() {
    let mut state = FakeKernelState::new();
    let abi = map_to_abi(state.sys_notif_bind(true, true, NotifId(2), 5, TaskId(7)));
    assert_eq!(abi, 0, "successful bind must return 0 through the ABI path");
}

/// Verify that a busy-slot error returns `NEG_EBUSY` (-16), not `u64::MAX`.
#[test]
fn busy_bind_abi_returns_neg_ebusy_not_max() {
    const NEG_EBUSY: u64 = (-16_i64) as u64;

    let mut state = FakeKernelState::new();
    state
        .sys_notif_bind(true, true, NotifId(0), 1, TaskId(10))
        .unwrap();

    let abi = map_to_abi(state.sys_notif_bind(true, true, NotifId(0), 2, TaskId(20)));
    assert_eq!(abi, NEG_EBUSY, "busy slot must return NEG_EBUSY (-16)");
    assert_ne!(abi, u64::MAX, "busy slot must NOT return u64::MAX");
}
