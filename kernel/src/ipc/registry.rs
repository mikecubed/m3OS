//! Service registry — static name-to-endpoint mapping.
//!
//! Provides a global table that maps ASCII service names (up to 32 bytes) to
//! [`EndpointId`]s.  The registry is protected by a [`spin::Mutex`] and uses
//! fixed-size arrays so that no heap allocation is required.
//!
//! # Phase 7 scope
//!
//! - Up to [`MAX_SERVICES`] entries.
//! - Names are stored as `[u8; 32]` with an explicit length field.
//! - Syscalls 9 (`ipc_register_service`) and 10 (`ipc_lookup_service`) use
//!   this module via the IPC dispatch path.

#![allow(dead_code)]

use spin::Mutex;

use super::EndpointId;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of services that can be registered simultaneously.
pub const MAX_SERVICES: usize = 8;

/// Maximum byte length of a service name.
const MAX_NAME_LEN: usize = 32;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Internal entry type
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Global registry
// ---------------------------------------------------------------------------

struct Registry {
    entries: [Option<Entry>; MAX_SERVICES],
    count: usize,
}

impl Registry {
    const fn new() -> Self {
        Registry {
            entries: [None; MAX_SERVICES],
            count: 0,
        }
    }
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Register a named service endpoint.
///
/// Returns `Err` if the name is already taken, the name is too long, or the
/// registry is full.
pub fn register(name: &str, ep_id: EndpointId) -> Result<(), RegistryError> {
    let name_bytes = name.as_bytes();
    if name_bytes.len() > MAX_NAME_LEN {
        return Err(RegistryError::NameTooLong);
    }

    let mut reg = REGISTRY.lock();

    // Check for duplicate name.
    for slot in reg.entries.iter().flatten() {
        if slot.name_matches(name_bytes) {
            return Err(RegistryError::AlreadyExists);
        }
    }

    if reg.count >= MAX_SERVICES {
        return Err(RegistryError::Full);
    }

    // Find the first empty slot and insert.
    for slot in reg.entries.iter_mut() {
        if slot.is_none() {
            let mut entry_name = [0u8; MAX_NAME_LEN];
            entry_name[..name_bytes.len()].copy_from_slice(name_bytes);
            *slot = Some(Entry {
                name: entry_name,
                name_len: name_bytes.len(),
                ep_id,
            });
            reg.count += 1;
            return Ok(());
        }
    }

    Err(RegistryError::Full)
}

/// Look up a named service endpoint.
///
/// Returns `Some(EndpointId)` if a service with the given name is registered,
/// or `None` otherwise.
pub fn lookup(name: &str) -> Option<EndpointId> {
    let name_bytes = name.as_bytes();
    if name_bytes.len() > MAX_NAME_LEN {
        return None;
    }

    let reg = REGISTRY.lock();
    for slot in reg.entries.iter().flatten() {
        if slot.name_matches(name_bytes) {
            return Some(slot.ep_id);
        }
    }
    None
}
