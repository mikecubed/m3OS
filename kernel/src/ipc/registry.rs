//! Service registry — re-exported from kernel-core with global state wrapper.
#![allow(dead_code)]

use spin::Mutex;

use super::EndpointId;

#[allow(unused_imports)]
pub use kernel_core::ipc::registry::{MAX_SERVICES, RegistryError};

use kernel_core::ipc::registry::Registry;

static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

/// Register a named service endpoint.
pub fn register(name: &str, ep_id: EndpointId) -> Result<(), RegistryError> {
    REGISTRY.lock().register(name, ep_id)
}

/// Look up a named service endpoint.
pub fn lookup(name: &str) -> Option<EndpointId> {
    REGISTRY.lock().lookup(name)
}
