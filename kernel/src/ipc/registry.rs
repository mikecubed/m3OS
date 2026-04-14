//! Service registry — re-exported from kernel-core with global state wrapper.
#![allow(dead_code)]

use spin::{Lazy, Mutex};

use super::EndpointId;

#[allow(unused_imports)]
pub use kernel_core::ipc::registry::RegistryError;

use kernel_core::ipc::registry::Registry;

static REGISTRY: Lazy<Mutex<Registry>> = Lazy::new(|| Mutex::new(Registry::new()));

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
