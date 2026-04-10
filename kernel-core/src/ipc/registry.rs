use alloc::vec;
use alloc::vec::Vec;

use crate::types::EndpointId;

/// Initial number of service registry slots.
const INITIAL_SERVICES: usize = 16;

/// Number of slots added each time the registry grows.
const REGISTRY_GROW_INCREMENT: usize = 16;

/// Maximum byte length of a service name.
pub const MAX_NAME_LEN: usize = 32;

/// Errors returned by registry operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    /// No free slot could be found (should not normally happen with growable pool).
    Full,
    /// The supplied name exceeds [`MAX_NAME_LEN`] bytes.
    NameTooLong,
    /// A service with this name is already registered.
    AlreadyExists,
    /// No service with this name exists, or the entry is owned by a different task.
    NotFound,
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

/// Dynamically growable service registry — name-to-endpoint mapping with
/// ownership tracking.
pub struct Registry {
    entries: Vec<Option<Entry>>,
    count: usize,
}

impl Registry {
    /// Create a new empty registry with the default initial capacity.
    pub fn new() -> Self {
        Registry {
            entries: vec![None; INITIAL_SERVICES],
            count: 0,
        }
    }

    /// Return the current slot capacity (for diagnostics / tests).
    pub fn capacity(&self) -> usize {
        self.entries.len()
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
    ///
    /// Grows the internal slot array if no free slot is available.
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

        // Find a free slot.
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

        // No free slot — grow the pool and use the first new slot.
        let old_len = self.entries.len();
        self.entries.resize(old_len + REGISTRY_GROW_INCREMENT, None);
        let mut entry_name = [0u8; MAX_NAME_LEN];
        entry_name[..name_bytes.len()].copy_from_slice(name_bytes);
        self.entries[old_len] = Some(Entry {
            name: entry_name,
            name_len: name_bytes.len(),
            ep_id,
            owner,
        });
        self.count += 1;
        Ok(())
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

        Err(RegistryError::NotFound)
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
    fn registry_grows_beyond_initial_capacity() {
        let mut reg = Registry::new();
        let initial_cap = reg.capacity();
        // Fill all initial slots.
        for i in 0..initial_cap {
            let name = alloc::format!("svc{}", i);
            reg.register(&name, EndpointId(i as u8)).unwrap();
        }
        // One more registration should succeed by growing.
        reg.register("overflow", EndpointId(99)).unwrap();
        assert_eq!(reg.lookup("overflow"), Some(EndpointId(99)));
        assert!(reg.capacity() > initial_cap);
    }

    #[test]
    fn initial_capacity_is_at_least_16() {
        let reg = Registry::new();
        assert!(reg.capacity() >= 16);
    }

    #[test]
    fn register_32_services_succeeds() {
        let mut reg = Registry::new();
        for i in 0..32 {
            let name = alloc::format!("svc{}", i);
            reg.register(&name, EndpointId(i as u8)).unwrap();
        }
        // All 32 should be findable.
        for i in 0..32 {
            let name = alloc::format!("svc{}", i);
            assert_eq!(reg.lookup(&name), Some(EndpointId(i as u8)));
        }
    }

    #[test]
    fn freed_registry_slots_are_reused() {
        let mut reg = Registry::new();
        // Register and then unregister by replacing with a new owner (simulates free).
        reg.register_with_owner("tmp", EndpointId(1), 10).unwrap();
        let cap_before = reg.capacity();
        // Replace (acts like free + re-register).
        reg.replace_service("tmp", EndpointId(2), 10, 20).unwrap();
        assert_eq!(reg.lookup("tmp"), Some(EndpointId(2)));
        // Capacity should not have grown.
        assert_eq!(reg.capacity(), cap_before);
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
        assert_eq!(
            reg.replace_service("svc", EndpointId(2), 99, 20),
            Err(RegistryError::NotFound)
        );
    }

    #[test]
    fn kernel_register_sets_owner_zero() {
        let mut reg = Registry::new();
        reg.register("ksvc", EndpointId(3)).unwrap();
        // Same owner (0) should allow re-registration
        reg.register("ksvc", EndpointId(4)).unwrap();
        assert_eq!(reg.lookup("ksvc"), Some(EndpointId(4)));
    }

    // --- G.2: additional registry ownership and re-registration tests ---

    #[test]
    fn register_with_owner_and_lookup() {
        let mut reg = Registry::new();
        let ep = EndpointId(42);
        reg.register_with_owner("display", ep, 100).unwrap();
        assert_eq!(reg.lookup("display"), Some(ep));
        // Other names still missing.
        assert_eq!(reg.lookup("audio"), None);
    }

    #[test]
    fn re_register_same_owner_updates() {
        let mut reg = Registry::new();
        reg.register_with_owner("net", EndpointId(1), 50).unwrap();
        assert_eq!(reg.lookup("net"), Some(EndpointId(1)));

        // Same owner re-registers with a new endpoint.
        reg.register_with_owner("net", EndpointId(99), 50).unwrap();
        assert_eq!(reg.lookup("net"), Some(EndpointId(99)));
    }

    #[test]
    fn replace_after_death() {
        let mut reg = Registry::new();
        // Original owner (pid 10) registers the service.
        reg.register_with_owner("crashed", EndpointId(1), 10)
            .unwrap();

        // After pid 10 dies, a supervisor replaces it with pid 20.
        reg.replace_service("crashed", EndpointId(2), 10, 20)
            .unwrap();
        assert_eq!(reg.lookup("crashed"), Some(EndpointId(2)));

        // New owner (20) can re-register.
        reg.register_with_owner("crashed", EndpointId(3), 20)
            .unwrap();
        assert_eq!(reg.lookup("crashed"), Some(EndpointId(3)));
    }

    #[test]
    fn replace_while_alive_wrong_old_owner_returns_error() {
        let mut reg = Registry::new();
        reg.register_with_owner("alive", EndpointId(1), 10).unwrap();

        // Try to replace with wrong old_owner — should fail with NotFound.
        assert_eq!(
            reg.replace_service("alive", EndpointId(2), 99, 20),
            Err(RegistryError::NotFound)
        );

        // Original service unchanged.
        assert_eq!(reg.lookup("alive"), Some(EndpointId(1)));
    }
}
