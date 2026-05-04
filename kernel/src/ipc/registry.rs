//! Service registry — re-exported from kernel-core with global state wrapper.
#![allow(dead_code)]

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use spin::Lazy;

use super::EndpointId;
use crate::task::scheduler::IrqSafeMutex;
use crate::task::{TaskId, TaskState, scheduler};

#[allow(unused_imports)]
pub use kernel_core::ipc::registry::RegistryError;

use kernel_core::ipc::registry::{MAX_NAME_LEN, Registry};

/// Service registry global.
///
/// Phase 57b G.6 — `IrqSafeMutex` inherits Track F.1's preempt-discipline
/// (lock raises `preempt_count`, drop lowers it).  Only acquired from task
/// context (registry lookups during ipc syscalls); no ISR ever reaches it.
/// Pure type swap — callsites compile unchanged via auto-deref.
static REGISTRY: Lazy<IrqSafeMutex<Registry>> = Lazy::new(|| IrqSafeMutex::new(Registry::new()));

struct ServiceWaiter {
    name: [u8; MAX_NAME_LEN],
    name_len: usize,
    task_id: TaskId,
    woken: Arc<AtomicBool>,
}

impl ServiceWaiter {
    fn new(name: &str, task_id: TaskId, woken: Arc<AtomicBool>) -> Option<Self> {
        let name_bytes = name.as_bytes();
        if name_bytes.is_empty() || name_bytes.len() > MAX_NAME_LEN {
            return None;
        }
        let mut stored_name = [0u8; MAX_NAME_LEN];
        stored_name[..name_bytes.len()].copy_from_slice(name_bytes);
        Some(Self {
            name: stored_name,
            name_len: name_bytes.len(),
            task_id,
            woken,
        })
    }

    fn name_matches(&self, name: &str) -> bool {
        let name_bytes = name.as_bytes();
        self.name_len == name_bytes.len() && self.name[..self.name_len] == *name_bytes
    }
}

static SERVICE_WAITERS: Lazy<IrqSafeMutex<Vec<ServiceWaiter>>> =
    Lazy::new(|| IrqSafeMutex::new(Vec::new()));
static PENDING_SERVICE_WAKES: Lazy<IrqSafeMutex<Vec<TaskId>>> =
    Lazy::new(|| IrqSafeMutex::new(Vec::new()));

/// Register a named service endpoint.
pub fn register(name: &str, ep_id: EndpointId) -> Result<(), RegistryError> {
    let result = REGISTRY.lock().register(name, ep_id);
    if result.is_ok() {
        wake_registered_waiters(name);
    }
    result
}

/// Register a named service endpoint with an owning task ID.
pub fn register_with_owner(name: &str, ep_id: EndpointId, owner: u64) -> Result<(), RegistryError> {
    let result = REGISTRY.lock().register_with_owner(name, ep_id, owner);
    if result.is_ok() {
        wake_registered_waiters(name);
    }
    result
}

/// Replace a dead task's service entry with a new registration.
pub fn replace_service(
    name: &str,
    ep_id: EndpointId,
    old_owner: u64,
    new_owner: u64,
) -> Result<(), RegistryError> {
    let result = REGISTRY
        .lock()
        .replace_service(name, ep_id, old_owner, new_owner);
    if result.is_ok() {
        wake_registered_waiters(name);
    }
    result
}

/// Remove all registry entries owned by a specific task.
pub fn remove_by_owner(owner: u64) {
    REGISTRY.lock().remove_by_owner(owner);
}

/// Look up a named service endpoint.
pub fn lookup(name: &str) -> Option<EndpointId> {
    REGISTRY.lock().lookup(name)
}

/// Look up a named service endpoint and run `f` while the registry lock is
/// still held. This lets callers couple the lookup with follow-up bookkeeping
/// so cleanup cannot remove or recycle the service entry in between.
pub fn with_lookup<R>(name: &str, f: impl FnOnce(EndpointId) -> R) -> Option<R> {
    let reg = REGISTRY.lock();
    reg.lookup(name).map(f)
}

/// Phase 54: check if a named service is currently registered.
pub fn is_registered(name: &str) -> bool {
    REGISTRY.lock().lookup(name).is_some()
}

/// Phase 54: look up a named service and return its endpoint ID directly.
/// Convenience alias for [`lookup`] used by the kernel VFS routing layer.
pub fn lookup_endpoint_id(name: &str) -> Option<EndpointId> {
    lookup(name)
}

/// Look up a service and return `(endpoint_id, owner_task_id)`.
///
/// Used by kernel facades that need to verify the registering task is a
/// trusted / privileged process before binding kernel resources to the
/// endpoint (see `kernel::blk::remote::is_registered`). `owner` is `0` for
/// kernel-registered entries, or the ring-3 task id for user-registered
/// services.
pub fn lookup_endpoint_with_owner(name: &str) -> Option<(EndpointId, u64)> {
    REGISTRY.lock().lookup_with_owner(name)
}

/// Block until `name` is registered or `deadline_ticks` expires.
///
/// This is a readiness-only primitive: it never grants endpoint capabilities,
/// so it is safe for private services such as `vfs`.
pub fn wait_until_registered(name: &str, task_id: TaskId, deadline_ticks: Option<u64>) -> bool {
    if REGISTRY.lock().lookup(name).is_some() {
        return true;
    }

    let woken = Arc::new(AtomicBool::new(false));
    {
        let Some(waiter) = ServiceWaiter::new(name, task_id, woken.clone()) else {
            return false;
        };
        let mut waiters = SERVICE_WAITERS.lock();
        waiters.push(waiter);
    }
    if REGISTRY.lock().lookup(name).is_some() {
        remove_service_waiter(name, task_id);
        return true;
    }

    let outcome =
        scheduler::block_current_until(TaskState::BlockedOnService, &woken, deadline_ticks);
    remove_service_waiter(name, task_id);
    matches!(
        outcome,
        scheduler::BlockOutcome::Woken | scheduler::BlockOutcome::AlreadyTrue
    ) || REGISTRY.lock().lookup(name).is_some()
}

fn wake_registered_waiters(name: &str) {
    let waiters_to_wake = {
        let mut waiters = SERVICE_WAITERS.lock();
        let mut ready = Vec::new();
        let mut idx = 0usize;
        while idx < waiters.len() {
            if waiters[idx].name_matches(name) {
                let waiter = waiters.swap_remove(idx);
                ready.push((waiter.task_id, waiter.woken));
            } else {
                idx += 1;
            }
        }
        ready
    };

    for (task_id, woken) in waiters_to_wake {
        woken.store(true, Ordering::Release);
        PENDING_SERVICE_WAKES.lock().push(task_id);
    }
}

/// Drain service-readiness wakes for tasks assigned to the current core.
///
/// Service registration can race a waiter that has marked itself blocked but
/// has not completed its switch-out yet. Deferring the wake to the target
/// core's scheduler loop avoids making the registering task spin in
/// `wake_task_v2` before the waiter's saved stack is published.
pub fn drain_pending_service_waiters() {
    let Some(pc) = crate::smp::try_per_core() else {
        return;
    };
    let core_id = pc.core_id;
    let mut ready = Vec::new();
    {
        let mut pending = PENDING_SERVICE_WAKES.lock();
        let mut idx = 0usize;
        while idx < pending.len() {
            let task_id = pending[idx];
            match scheduler::task_state_and_assigned_core(task_id) {
                Some((TaskState::BlockedOnService, assigned)) if assigned == core_id => {
                    ready.push(pending.swap_remove(idx));
                }
                Some((TaskState::BlockedOnService, _)) => {
                    idx += 1;
                }
                _ => {
                    pending.swap_remove(idx);
                }
            }
        }
    }
    for task_id in ready {
        let _ = scheduler::wake_task_v2(task_id);
    }
}

fn remove_service_waiter(name: &str, task_id: TaskId) {
    let mut waiters = SERVICE_WAITERS.lock();
    let mut idx = 0usize;
    while idx < waiters.len() {
        if waiters[idx].task_id == task_id && waiters[idx].name_matches(name) {
            waiters.swap_remove(idx);
        } else {
            idx += 1;
        }
    }
}
