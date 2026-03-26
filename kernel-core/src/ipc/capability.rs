use crate::types::{EndpointId, NotifId, TaskId};

/// An opaque integer index into a [`CapabilityTable`].
pub type CapHandle = u32;

/// A single capability slot — what a handle actually grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Right to send/receive on a kernel endpoint.
    Endpoint(EndpointId),
    /// Right to signal or wait on a notification object.
    Notification(NotifId),
    /// One-shot right to reply to a specific blocked caller.
    Reply(TaskId),
}

/// Errors returned by capability-table operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    /// The supplied handle is out-of-range or points to an empty slot.
    InvalidHandle,
    /// The slot holds a capability of a different type than requested.
    WrongType,
    /// All 64 slots are occupied; cannot insert a new capability.
    TableFull,
}

/// Fixed-size per-process capability table (64 slots).
pub struct CapabilityTable {
    slots: [Option<Capability>; Self::SIZE],
}

impl CapabilityTable {
    /// Maximum number of capabilities a single process may hold.
    pub const SIZE: usize = 64;

    /// Create an empty capability table.
    pub const fn new() -> Self {
        CapabilityTable {
            slots: [None; Self::SIZE],
        }
    }

    /// Insert a capability and return its handle.
    pub fn insert(&mut self, cap: Capability) -> Result<CapHandle, CapError> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(cap);
                return Ok(i as CapHandle);
            }
        }
        Err(CapError::TableFull)
    }

    /// Insert a capability at a specific slot (used during task initialisation).
    pub fn insert_at(&mut self, handle: CapHandle, cap: Capability) -> Result<(), CapError> {
        let slot = self
            .slots
            .get_mut(handle as usize)
            .ok_or(CapError::InvalidHandle)?;
        *slot = Some(cap);
        Ok(())
    }

    /// Look up a capability by handle without consuming it.
    pub fn get(&self, handle: CapHandle) -> Result<Capability, CapError> {
        self.slots
            .get(handle as usize)
            .and_then(|s| *s)
            .ok_or(CapError::InvalidHandle)
    }

    /// Remove and return the capability at `handle`, clearing the slot.
    pub fn remove(&mut self, handle: CapHandle) -> Result<Capability, CapError> {
        let slot = self
            .slots
            .get_mut(handle as usize)
            .ok_or(CapError::InvalidHandle)?;
        slot.take().ok_or(CapError::InvalidHandle)
    }
}

impl Default for CapabilityTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let mut table = CapabilityTable::new();
        let cap = Capability::Endpoint(EndpointId(0));
        let handle = table.insert(cap).unwrap();
        assert_eq!(table.get(handle), Ok(cap));
    }

    #[test]
    fn remove_clears_slot() {
        let mut table = CapabilityTable::new();
        let cap = Capability::Notification(NotifId(3));
        let handle = table.insert(cap).unwrap();
        assert_eq!(table.remove(handle), Ok(cap));
        assert_eq!(table.get(handle), Err(CapError::InvalidHandle));
    }

    #[test]
    fn invalid_handle() {
        let table = CapabilityTable::new();
        assert_eq!(table.get(0), Err(CapError::InvalidHandle));
        assert_eq!(table.get(100), Err(CapError::InvalidHandle));
    }

    #[test]
    fn wrong_type_is_separate_from_invalid() {
        // CapError::WrongType is used by callers, not by the table itself.
        // Just verify the enum variants are distinct.
        assert_ne!(CapError::InvalidHandle, CapError::WrongType);
        assert_ne!(CapError::WrongType, CapError::TableFull);
    }

    #[test]
    fn table_full() {
        let mut table = CapabilityTable::new();
        for i in 0..CapabilityTable::SIZE {
            table
                .insert(Capability::Endpoint(EndpointId(i as u8)))
                .unwrap();
        }
        assert_eq!(
            table.insert(Capability::Endpoint(EndpointId(99))),
            Err(CapError::TableFull)
        );
    }

    #[test]
    fn insert_at_specific_slot() {
        let mut table = CapabilityTable::new();
        let cap = Capability::Reply(TaskId(42));
        table.insert_at(10, cap).unwrap();
        assert_eq!(table.get(10), Ok(cap));
        // Slot 0 is still empty
        assert_eq!(table.get(0), Err(CapError::InvalidHandle));
    }

    #[test]
    fn insert_at_out_of_range() {
        let mut table = CapabilityTable::new();
        assert_eq!(
            table.insert_at(100, Capability::Endpoint(EndpointId(0))),
            Err(CapError::InvalidHandle)
        );
    }

    #[test]
    fn default_is_empty() {
        let table = CapabilityTable::default();
        for i in 0..CapabilityTable::SIZE {
            assert_eq!(table.get(i as CapHandle), Err(CapError::InvalidHandle));
        }
    }
}
