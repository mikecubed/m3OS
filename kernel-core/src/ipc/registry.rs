use crate::types::EndpointId;

/// Maximum number of services that can be registered simultaneously.
pub const MAX_SERVICES: usize = 8;

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
}

impl Entry {
    fn name_matches(&self, other: &[u8]) -> bool {
        self.name_len == other.len() && self.name[..self.name_len] == *other
    }
}

/// Service registry — static name-to-endpoint mapping.
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

    /// Register a named service endpoint.
    pub fn register(&mut self, name: &str, ep_id: EndpointId) -> Result<(), RegistryError> {
        let name_bytes = name.as_bytes();
        if name_bytes.len() > MAX_NAME_LEN {
            return Err(RegistryError::NameTooLong);
        }

        for slot in self.entries.iter().flatten() {
            if slot.name_matches(name_bytes) {
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
                });
                self.count += 1;
                return Ok(());
            }
        }

        Err(RegistryError::Full)
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
    fn duplicate_name() {
        let mut reg = Registry::new();
        reg.register("vfs", EndpointId(0)).unwrap();
        assert_eq!(
            reg.register("vfs", EndpointId(1)),
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
}
