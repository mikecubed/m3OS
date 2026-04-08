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
    /// Right to access a contiguous range of physical page frames.
    ///
    /// - `frame`: physical frame number (not byte address).
    /// - `page_count`: number of contiguous 4 KiB pages.
    /// - `writable`: whether the receiver may write to the pages.
    Grant {
        frame: u64,
        page_count: u16,
        writable: bool,
    },
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

    /// Atomically transfer a capability from `self[source_handle]` to `dest_table`.
    ///
    /// On success the source slot is cleared and the new handle in the
    /// destination table is returned.  On failure (invalid source handle or
    /// destination table full) the source retains its capability and an error
    /// is returned — no side effects.
    pub fn grant(
        &mut self,
        source_handle: CapHandle,
        dest_table: &mut CapabilityTable,
    ) -> Result<CapHandle, CapError> {
        // Validate source handle first, without removing yet.
        let cap = self.get(source_handle)?;

        // Try to insert into destination — if it fails, source keeps the cap.
        let dest_handle = dest_table.insert(cap)?;

        // Destination succeeded — now remove from source (infallible since we
        // already validated the handle above and hold &mut self).
        let _ = self.remove(source_handle);

        Ok(dest_handle)
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

    // --- B.1 / B.2: capability grant tests ---

    #[test]
    fn grant_moves_cap_from_source_to_dest() {
        let mut src = CapabilityTable::new();
        let mut dst = CapabilityTable::new();
        let cap = Capability::Endpoint(EndpointId(7));
        let src_handle = src.insert(cap).unwrap();

        let dst_handle = src.grant(src_handle, &mut dst).unwrap();

        // Source slot is cleared.
        assert_eq!(src.get(src_handle), Err(CapError::InvalidHandle));
        // Destination slot is populated with the same capability.
        assert_eq!(dst.get(dst_handle), Ok(cap));
    }

    #[test]
    fn grant_to_full_table_returns_table_full() {
        let mut src = CapabilityTable::new();
        let mut dst = CapabilityTable::new();

        // Fill the destination table.
        for i in 0..CapabilityTable::SIZE {
            dst.insert(Capability::Notification(NotifId(i as u8)))
                .unwrap();
        }

        let cap = Capability::Endpoint(EndpointId(1));
        let src_handle = src.insert(cap).unwrap();

        // Grant must fail with TableFull.
        assert_eq!(src.grant(src_handle, &mut dst), Err(CapError::TableFull));
        // Source must still have the capability (no side effects).
        assert_eq!(src.get(src_handle), Ok(cap));
    }

    #[test]
    fn grant_invalid_handle_returns_invalid() {
        let mut src = CapabilityTable::new();
        let mut dst = CapabilityTable::new();

        // Handle 0 is empty — no capability was inserted.
        assert_eq!(src.grant(0, &mut dst), Err(CapError::InvalidHandle));
        // Out-of-range handle.
        assert_eq!(src.grant(100, &mut dst), Err(CapError::InvalidHandle));
    }

    // --- C.4: Grant capability variant tests ---

    #[test]
    fn insert_and_get_grant_capability() {
        let mut table = CapabilityTable::new();
        let cap = Capability::Grant {
            frame: 0x1000,
            page_count: 16,
            writable: true,
        };
        let handle = table.insert(cap).unwrap();
        assert_eq!(table.get(handle), Ok(cap));

        // Verify fields through pattern match.
        if let Capability::Grant {
            frame,
            page_count,
            writable,
        } = table.get(handle).unwrap()
        {
            assert_eq!(frame, 0x1000);
            assert_eq!(page_count, 16);
            assert!(writable);
        } else {
            panic!("expected Grant capability");
        }
    }

    #[test]
    fn grant_a_grant_capability_between_tables() {
        let mut src = CapabilityTable::new();
        let mut dst = CapabilityTable::new();

        let cap = Capability::Grant {
            frame: 0x2000,
            page_count: 4,
            writable: false,
        };
        let src_handle = src.insert(cap).unwrap();

        // Transfer the Grant capability from src to dst.
        let dst_handle = src.grant(src_handle, &mut dst).unwrap();

        // Source no longer holds it.
        assert_eq!(src.get(src_handle), Err(CapError::InvalidHandle));
        // Destination has the exact same capability.
        assert_eq!(dst.get(dst_handle), Ok(cap));
    }

    #[test]
    fn grant_read_only_vs_writable() {
        let mut table = CapabilityTable::new();
        let ro = Capability::Grant {
            frame: 0x100,
            page_count: 1,
            writable: false,
        };
        let rw = Capability::Grant {
            frame: 0x100,
            page_count: 1,
            writable: true,
        };
        let h_ro = table.insert(ro).unwrap();
        let h_rw = table.insert(rw).unwrap();
        assert_ne!(table.get(h_ro), table.get(h_rw));
    }
}
