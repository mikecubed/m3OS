//! Service registry — re-exported from kernel-core with global state wrapper.
#![allow(dead_code)]

use spin::Lazy;

use super::EndpointId;
use crate::task::scheduler::IrqSafeMutex;

#[allow(unused_imports)]
pub use kernel_core::ipc::registry::RegistryError;

use kernel_core::ipc::registry::Registry;

/// Service registry global.
///
/// Phase 57b G.6 — `IrqSafeMutex` inherits Track F.1's preempt-discipline
/// (lock raises `preempt_count`, drop lowers it).  Only acquired from task
/// context (registry lookups during ipc syscalls); no ISR ever reaches it.
/// Pure type swap — callsites compile unchanged via auto-deref.
static REGISTRY: Lazy<IrqSafeMutex<Registry>> = Lazy::new(|| IrqSafeMutex::new(Registry::new()));

/// Register a named service endpoint.
pub fn register(name: &str, ep_id: EndpointId) -> Result<(), RegistryError> {
    REGISTRY.lock().register(name, ep_id)
}

/// Register a named service endpoint with an owning task ID.
pub fn register_with_owner(name: &str, ep_id: EndpointId, owner: u64) -> Result<(), RegistryError> {
    REGISTRY.lock().register_with_owner(name, ep_id, owner)
}

/// Replace a dead task's service entry with a new registration.
pub fn replace_service(
    name: &str,
    ep_id: EndpointId,
    old_owner: u64,
    new_owner: u64,
) -> Result<(), RegistryError> {
    REGISTRY
        .lock()
        .replace_service(name, ep_id, old_owner, new_owner)
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
