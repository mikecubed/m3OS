use crate::types::EndpointId;

/// Maximum number of services that can be registered simultaneously.
pub const MAX_SERVICES: usize = 16;

/// Maximum byte length of a service name.
pub const MAX_NAME_LEN: usize = 32;

/// Errors returned by registry operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    /// The registry already holds [`MAX_SERVICES`] entries.
    Full,
    /// The supplied name exceeds [`MAX_NAME_LEN`] bytes.
    NameTooLong,
    /// A service with this name is already registered.
    AlreadyExists,
}

#[derive(Clone, Copy)]
struct Entry {
    name: [u8; MAX_NAME_LEN],
    name_len: usize,
    ep_id: EndpointId,
    /// Task that owns this service registration. `0` means kernel-registered
    /// (no specific owner).
    owner: u64,
}

impl Entry {
    fn name_matches(&self, other: &[u8]) -> bool {
        self.name_len == other.len() && self.name[..self.name_len] == *other
    }
}

/// Service registry — static name-to-endpoint mapping with ownership tracking.
pub struct Registry {
    entries: [Option<Entry>; MAX_SERVICES],
    count: usize,
}

impl Registry {
    /// Create a new empty registry.
    pub const fn new() -> Self {
        Registry {
            entries: [None; MAX_SERVICES],
            count: 0,
        }
    }

    /// Register a named service endpoint (kernel-owned, no specific task owner).
    ///
    /// This is the legacy API used by kernel init code that registers services
    /// from ring 0. The entry's owner is set to `0` (kernel).
    pub fn register(&mut self, name: &str, ep_id: EndpointId) -> Result<(), RegistryError> {
        self.register_with_owner(name, ep_id, 0)
    }

    /// Register a named service endpoint with an owning task ID.
    ///
    /// If a service with the same name already exists and is owned by a
    /// different task, returns [`RegistryError::AlreadyExists`]. If the same
    /// name exists with the same owner (re-registration), the endpoint is
    /// updated in place.
    pub fn register_with_owner(
        &mut self,
        name: &str,
        ep_id: EndpointId,
        owner: u64,
    ) -> Result<(), RegistryError> {
        let name_bytes = name.as_bytes();
        if name_bytes.len() > MAX_NAME_LEN {
            return Err(RegistryError::NameTooLong);
        }

        // Check for existing entry with the same name.
        for slot in self.entries.iter_mut().flatten() {
            if slot.name_matches(name_bytes) {
                if slot.owner == owner {
                    // Re-registration by the same owner: update endpoint.
                    slot.ep_id = ep_id;
                    return Ok(());
                }
                return Err(RegistryError::AlreadyExists);
            }
        }

        if self.count >= MAX_SERVICES {
            return Err(RegistryError::Full);
        }

        for slot in self.entries.iter_mut() {
            if slot.is_none() {
                let mut entry_name = [0u8; MAX_NAME_LEN];
                entry_name[..name_bytes.len()].copy_from_slice(name_bytes);
                *slot = Some(Entry {
                    name: entry_name,
                    name_len: name_bytes.len(),
                    ep_id,
                    owner,
                });
                self.count += 1;
                return Ok(());
            }
        }

        Err(RegistryError::Full)
    }

    /// Replace a dead task's service entry with a new registration.
    ///
    /// If a service named `name` exists and is owned by `old_owner`, it is
    /// replaced with the new `ep_id` and `new_owner`. Returns `Ok(())` on
    /// success, or `Err` if the name is not found or not owned by `old_owner`.
    pub fn replace_service(
        &mut self,
        name: &str,
        ep_id: EndpointId,
        old_owner: u64,
        new_owner: u64,
    ) -> Result<(), RegistryError> {
        let name_bytes = name.as_bytes();
        if name_bytes.len() > MAX_NAME_LEN {
            return Err(RegistryError::NameTooLong);
        }

        for slot in self.entries.iter_mut().flatten() {
            if slot.name_matches(name_bytes) && slot.owner == old_owner {
                slot.ep_id = ep_id;
                slot.owner = new_owner;
                return Ok(());
            }
        }

        Err(RegistryError::AlreadyExists)
    }

    /// Look up a named service endpoint.
    pub fn lookup(&self, name: &str) -> Option<EndpointId> {
        let name_bytes = name.as_bytes();
        if name_bytes.len() > MAX_NAME_LEN {
            return None;
        }

        for slot in self.entries.iter().flatten() {
            if slot.name_matches(name_bytes) {
                return Some(slot.ep_id);
            }
        }
        None
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let mut reg = Registry::new();
        let ep = EndpointId(1);
        reg.register("console", ep).unwrap();
        assert_eq!(reg.lookup("console"), Some(ep));
    }

    #[test]
    fn lookup_missing() {
        let reg = Registry::new();
        assert_eq!(reg.lookup("nonexistent"), None);
    }

    #[test]
    fn duplicate_name_different_owner() {
        let mut reg = Registry::new();
        reg.register_with_owner("vfs", EndpointId(0), 1).unwrap();
        assert_eq!(
            reg.register_with_owner("vfs", EndpointId(1), 2),
            Err(RegistryError::AlreadyExists)
        );
    }

    #[test]
    fn name_too_long() {
        let mut reg = Registry::new();
        let long_name = "a]".repeat(MAX_NAME_LEN + 1);
        assert_eq!(
            reg.register(&long_name, EndpointId(0)),
            Err(RegistryError::NameTooLong)
        );
    }

    #[test]
    fn registry_full() {
        let mut reg = Registry::new();
        for i in 0..MAX_SERVICES {
            let name = alloc::format!("svc{}", i);
            reg.register(&name, EndpointId(i as u8)).unwrap();
        }
        assert_eq!(
            reg.register("overflow", EndpointId(99)),
            Err(RegistryError::Full)
        );
    }

    #[test]
    fn max_services_is_at_least_16() {
        assert!(MAX_SERVICES >= 16);
    }

    #[test]
    fn register_with_owner_tracks_owner() {
        let mut reg = Registry::new();
        reg.register_with_owner("myservice", EndpointId(5), 42)
            .unwrap();
        assert_eq!(reg.lookup("myservice"), Some(EndpointId(5)));
    }

    #[test]
    fn reregister_same_owner_updates_endpoint() {
        let mut reg = Registry::new();
        reg.register_with_owner("svc", EndpointId(1), 10).unwrap();
        reg.register_with_owner("svc", EndpointId(2), 10).unwrap();
        assert_eq!(reg.lookup("svc"), Some(EndpointId(2)));
    }

    #[test]
    fn reregister_different_owner_rejected() {
        let mut reg = Registry::new();
        reg.register_with_owner("svc", EndpointId(1), 10).unwrap();
        assert_eq!(
            reg.register_with_owner("svc", EndpointId(2), 20),
            Err(RegistryError::AlreadyExists)
        );
    }

    #[test]
    fn replace_service_by_old_owner() {
        let mut reg = Registry::new();
        reg.register_with_owner("svc", EndpointId(1), 10).unwrap();
        reg.replace_service("svc", EndpointId(2), 10, 20).unwrap();
        assert_eq!(reg.lookup("svc"), Some(EndpointId(2)));
    }

    #[test]
    fn replace_service_wrong_owner_fails() {
        let mut reg = Registry::new();
        reg.register_with_owner("svc", EndpointId(1), 10).unwrap();
        assert!(reg.replace_service("svc", EndpointId(2), 99, 20).is_err());
    }

    #[test]
    fn kernel_register_sets_owner_zero() {
        let mut reg = Registry::new();
        reg.register("ksvc", EndpointId(3)).unwrap();
        // Same owner (0) should allow re-registration
        reg.register("ksvc", EndpointId(4)).unwrap();
        assert_eq!(reg.lookup("ksvc"), Some(EndpointId(4)));
    }
}
