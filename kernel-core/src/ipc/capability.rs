use alloc::vec;
use alloc::vec::Vec;

use crate::device_host::types::DeviceCapKey;
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
    /// Phase 55b: ownership of a PCI(e) device, keyed by segment/BDF.
    ///
    /// Held by the driver process that claimed the device via
    /// `sys_device_claim`. All derived capabilities (`Mmio`, `Dma`,
    /// `DeviceIrq`) carry a back-reference to the owning key so the kernel
    /// can revoke them in one sweep when the device is released.
    Device { key: DeviceCapKey },
    /// Phase 55b: access right to a BAR window mapped into the driver's
    /// address space. `bar_index` and `len` must match the descriptor the
    /// kernel returned at map time — the kernel re-validates on every use.
    Mmio {
        device: DeviceCapKey,
        bar_index: u8,
        len: usize,
    },
    /// Phase 55b: access right to a DMA-mapped buffer.
    ///
    /// `iova` is the device-visible I/O virtual address (or identity-mapped
    /// physical address, per Phase 55a's `DmaBuffer<T>` fallback); `len` is
    /// the mapped length in bytes.
    Dma {
        device: DeviceCapKey,
        iova: u64,
        len: usize,
    },
    /// Phase 55b: subscription to a device-originated IRQ, delivered to the
    /// driver via the referenced `Notification` object.
    DeviceIrq {
        device: DeviceCapKey,
        notif: NotifId,
    },
}

impl Capability {
    /// Return the notification object this capability aliases for IPC recv/wait paths.
    ///
    /// Plain [`Capability::Notification`] caps expose the notification directly.
    /// [`Capability::DeviceIrq`] caps carry the same notification ID that the
    /// kernel signals from the ISR shim, so drivers can wait on or bind the IRQ
    /// without needing a second standalone notification capability.
    pub fn ipc_notification_id(self) -> Option<NotifId> {
        match self {
            Capability::Notification(id) => Some(id),
            Capability::DeviceIrq { notif, .. } => Some(notif),
            _ => None,
        }
    }
}

/// Errors returned by capability-table operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    /// The supplied handle is out-of-range or points to an empty slot.
    InvalidHandle,
    /// The slot holds a capability of a different type than requested.
    WrongType,
    /// No free slot and growth failed or is not applicable.
    TableFull,
}

/// Initial number of capability slots per process.
const INITIAL_CAP_SLOTS: usize = 64;

/// Number of slots added each time the table grows.
const CAP_GROW_INCREMENT: usize = 64;

/// Dynamically growable per-process capability table.
///
/// Starts with [`INITIAL_CAP_SLOTS`] slots and grows by
/// [`CAP_GROW_INCREMENT`] when all existing slots are occupied.
pub struct CapabilityTable {
    slots: Vec<Option<Capability>>,
}

impl CapabilityTable {
    /// Initial capacity (kept as a public constant for test compatibility).
    pub const INITIAL_SIZE: usize = INITIAL_CAP_SLOTS;

    /// Create an empty capability table with the default initial capacity.
    pub fn new() -> Self {
        CapabilityTable {
            slots: vec![None; INITIAL_CAP_SLOTS],
        }
    }

    /// Insert a capability into the first free slot, growing if necessary.
    pub fn insert(&mut self, cap: Capability) -> Result<CapHandle, CapError> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(cap);
                return Ok(i as CapHandle);
            }
        }
        // All slots occupied — grow the table.
        let old_len = self.slots.len();
        self.slots.resize(old_len + CAP_GROW_INCREMENT, None);
        self.slots[old_len] = Some(cap);
        Ok(old_len as CapHandle)
    }

    /// Insert a capability at a specific slot, growing if necessary.
    pub fn insert_at(&mut self, handle: CapHandle, cap: Capability) -> Result<(), CapError> {
        let idx = handle as usize;
        // Grow the table if the requested handle is beyond the current length.
        if idx >= self.slots.len() {
            let new_len = idx + 1;
            self.slots.resize(new_len, None);
        }
        let slot = &mut self.slots[idx];
        if slot.is_some() {
            return Err(CapError::TableFull);
        }
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

    /// Return whether the table currently holds a capability for `ep_id`.
    pub fn contains_endpoint(&self, ep_id: EndpointId) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|cap| matches!(cap, Capability::Endpoint(id) if *id == ep_id))
    }

    /// Return the callers currently referenced by reply capabilities.
    pub fn reply_targets(&self) -> Vec<TaskId> {
        self.slots
            .iter()
            .flatten()
            .filter_map(|cap| match cap {
                Capability::Reply(task) => Some(*task),
                _ => None,
            })
            .collect()
    }

    /// Clear every slot whose capability satisfies `pred`, returning the
    /// number of revoked entries. Used to invalidate stale reply caps when
    /// their target is pulled out of an IPC wait by signal delivery.
    pub fn revoke_matching(&mut self, pred: impl Fn(&Capability) -> bool) -> usize {
        let mut revoked = 0;
        for slot in self.slots.iter_mut() {
            if let Some(cap) = slot
                && pred(cap)
            {
                *slot = None;
                revoked += 1;
            }
        }
        revoked
    }

    /// Return the notification IDs currently held in the table.
    pub fn notification_ids(&self) -> Vec<NotifId> {
        self.slots
            .iter()
            .flatten()
            .filter_map(|cap| match cap {
                Capability::Notification(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Return the current number of slots (for diagnostic / test use).
    pub fn capacity(&self) -> usize {
        self.slots.len()
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
    fn contains_endpoint_detects_only_matching_endpoints() {
        let mut table = CapabilityTable::new();
        table.insert(Capability::Notification(NotifId(3))).unwrap();
        table.insert(Capability::Endpoint(EndpointId(7))).unwrap();
        assert!(table.contains_endpoint(EndpointId(7)));
        assert!(!table.contains_endpoint(EndpointId(8)));
    }

    #[test]
    fn reply_targets_collects_only_reply_caps() {
        let mut table = CapabilityTable::new();
        table.insert(Capability::Reply(TaskId(11))).unwrap();
        table.insert(Capability::Endpoint(EndpointId(3))).unwrap();
        table.insert(Capability::Reply(TaskId(29))).unwrap();
        assert_eq!(table.reply_targets(), vec![TaskId(11), TaskId(29)]);
    }

    #[test]
    fn notification_ids_collect_only_notification_caps() {
        let mut table = CapabilityTable::new();
        table.insert(Capability::Notification(NotifId(2))).unwrap();
        table.insert(Capability::Reply(TaskId(11))).unwrap();
        table.insert(Capability::Notification(NotifId(7))).unwrap();
        assert_eq!(table.notification_ids(), vec![NotifId(2), NotifId(7)]);
    }

    #[test]
    fn ipc_notification_id_accepts_plain_notification_caps() {
        let cap = Capability::Notification(NotifId(5));
        assert_eq!(cap.ipc_notification_id(), Some(NotifId(5)));
    }

    #[test]
    fn ipc_notification_id_accepts_device_irq_caps() {
        let cap = Capability::DeviceIrq {
            device: DeviceCapKey::new(0, 0, 3, 0),
            notif: NotifId(9),
        };
        assert_eq!(cap.ipc_notification_id(), Some(NotifId(9)));
    }

    #[test]
    fn ipc_notification_id_rejects_non_notification_caps() {
        let cap = Capability::Endpoint(EndpointId(1));
        assert_eq!(cap.ipc_notification_id(), None);
    }

    #[test]
    fn invalid_handle() {
        let table = CapabilityTable::new();
        assert_eq!(table.get(0), Err(CapError::InvalidHandle));
        // Beyond initial capacity — still returns InvalidHandle (not panic).
        assert_eq!(
            table.get(INITIAL_CAP_SLOTS as CapHandle + 100),
            Err(CapError::InvalidHandle)
        );
    }

    #[test]
    fn wrong_type_is_separate_from_invalid() {
        // CapError::WrongType is used by callers, not by the table itself.
        // Just verify the enum variants are distinct.
        assert_ne!(CapError::InvalidHandle, CapError::WrongType);
        assert_ne!(CapError::WrongType, CapError::TableFull);
    }

    #[test]
    fn table_grows_beyond_initial_capacity() {
        let mut table = CapabilityTable::new();
        // Fill all initial slots.
        for i in 0..INITIAL_CAP_SLOTS {
            table
                .insert(Capability::Endpoint(EndpointId(i as u8)))
                .unwrap();
        }
        // One more insert should succeed by growing the table.
        let handle = table.insert(Capability::Endpoint(EndpointId(99))).unwrap();
        assert_eq!(handle, INITIAL_CAP_SLOTS as CapHandle);
        assert_eq!(table.get(handle), Ok(Capability::Endpoint(EndpointId(99))));
        // The table should have grown.
        assert!(table.capacity() > INITIAL_CAP_SLOTS);
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
    fn insert_at_beyond_initial_capacity_grows() {
        let mut table = CapabilityTable::new();
        // Inserting at handle 100 (beyond initial 64 slots) should grow.
        let cap = Capability::Endpoint(EndpointId(0));
        table.insert_at(100, cap).unwrap();
        assert_eq!(table.get(100), Ok(cap));
        assert!(table.capacity() >= 101);
    }

    #[test]
    fn insert_at_rejects_occupied_slot() {
        let mut table = CapabilityTable::new();
        let cap1 = Capability::Endpoint(EndpointId(1));
        let cap2 = Capability::Endpoint(EndpointId(2));
        table.insert_at(5, cap1).unwrap();
        // Second insert into the same slot must fail.
        assert_eq!(table.insert_at(5, cap2), Err(CapError::TableFull));
        // Original capability is preserved.
        assert_eq!(table.get(5), Ok(cap1));
    }

    #[test]
    fn default_is_empty() {
        let table = CapabilityTable::default();
        for i in 0..CapabilityTable::INITIAL_SIZE {
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
    fn grant_to_full_table_grows_destination() {
        let mut src = CapabilityTable::new();
        let mut dst = CapabilityTable::new();

        // Fill all initial slots of the destination table.
        for i in 0..INITIAL_CAP_SLOTS {
            dst.insert(Capability::Notification(NotifId(i as u8)))
                .unwrap();
        }

        let cap = Capability::Endpoint(EndpointId(1));
        let src_handle = src.insert(cap).unwrap();

        // Grant now succeeds because the destination table grows.
        let dst_handle = src.grant(src_handle, &mut dst).unwrap();
        assert_eq!(dst.get(dst_handle), Ok(cap));
        // Source slot is cleared after grant.
        assert_eq!(src.get(src_handle), Err(CapError::InvalidHandle));
        // Destination grew beyond its initial capacity.
        assert!(dst.capacity() > INITIAL_CAP_SLOTS);
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
    fn insert_128_capabilities_succeeds() {
        let mut table = CapabilityTable::new();
        for i in 0..128u32 {
            let cap = Capability::Endpoint(EndpointId((i % 256) as u8));
            let handle = table.insert(cap).unwrap();
            assert_eq!(handle, i);
        }
        assert!(table.capacity() >= 128);
        // Verify a cap in the grown region.
        assert_eq!(table.get(100), Ok(Capability::Endpoint(EndpointId(100))));
    }

    #[test]
    fn freed_slots_are_reused() {
        let mut table = CapabilityTable::new();
        let cap = Capability::Endpoint(EndpointId(1));
        let h0 = table.insert(cap).unwrap();
        let h1 = table.insert(cap).unwrap();
        // Free slot 0.
        table.remove(h0).unwrap();
        // Next insert should reuse slot 0.
        let h2 = table.insert(cap).unwrap();
        assert_eq!(h2, h0);
        // h1 is still valid.
        assert_eq!(table.get(h1), Ok(cap));
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

    // --- G.1: additional grant logic tests ---

    #[test]
    fn grant_then_revoke() {
        let mut src = CapabilityTable::new();
        let mut dst = CapabilityTable::new();
        let cap = Capability::Endpoint(EndpointId(10));
        let src_handle = src.insert(cap).unwrap();

        // Grant from src to dst.
        let dst_handle = src.grant(src_handle, &mut dst).unwrap();

        // Source slot is cleared after grant.
        assert_eq!(src.get(src_handle), Err(CapError::InvalidHandle));
        // Destination has the cap.
        assert_eq!(dst.get(dst_handle), Ok(cap));

        // Now revoke (remove) from destination.
        assert_eq!(dst.remove(dst_handle), Ok(cap));
        // Both tables now have the slot empty.
        assert_eq!(src.get(src_handle), Err(CapError::InvalidHandle));
        assert_eq!(dst.get(dst_handle), Err(CapError::InvalidHandle));
    }

    #[test]
    fn double_grant_chain() {
        let mut a = CapabilityTable::new();
        let mut b = CapabilityTable::new();
        let mut c = CapabilityTable::new();

        let cap = Capability::Notification(NotifId(5));
        let a_handle = a.insert(cap).unwrap();

        // Grant A -> B.
        let b_handle = a.grant(a_handle, &mut b).unwrap();
        assert_eq!(a.get(a_handle), Err(CapError::InvalidHandle));
        assert_eq!(b.get(b_handle), Ok(cap));

        // Grant B -> C.
        let c_handle = b.grant(b_handle, &mut c).unwrap();
        assert_eq!(b.get(b_handle), Err(CapError::InvalidHandle));
        assert_eq!(c.get(c_handle), Ok(cap));

        // Only C holds the capability now.
        assert_eq!(a.get(a_handle), Err(CapError::InvalidHandle));
        assert_eq!(b.get(b_handle), Err(CapError::InvalidHandle));
        assert_eq!(c.get(c_handle), Ok(cap));
    }

    #[test]
    fn grant_across_different_capability_types() {
        let mut src = CapabilityTable::new();
        let mut dst = CapabilityTable::new();

        let ep_cap = Capability::Endpoint(EndpointId(1));
        let notif_cap = Capability::Notification(NotifId(2));
        let reply_cap = Capability::Reply(TaskId(3));
        let grant_cap = Capability::Grant {
            frame: 0x4000,
            page_count: 8,
            writable: true,
        };

        let h_ep = src.insert(ep_cap).unwrap();
        let h_notif = src.insert(notif_cap).unwrap();
        let h_reply = src.insert(reply_cap).unwrap();
        let h_grant = src.insert(grant_cap).unwrap();

        // Grant all four types to dst.
        let d_ep = src.grant(h_ep, &mut dst).unwrap();
        let d_notif = src.grant(h_notif, &mut dst).unwrap();
        let d_reply = src.grant(h_reply, &mut dst).unwrap();
        let d_grant = src.grant(h_grant, &mut dst).unwrap();

        // All source slots empty.
        assert_eq!(src.get(h_ep), Err(CapError::InvalidHandle));
        assert_eq!(src.get(h_notif), Err(CapError::InvalidHandle));
        assert_eq!(src.get(h_reply), Err(CapError::InvalidHandle));
        assert_eq!(src.get(h_grant), Err(CapError::InvalidHandle));

        // All dest slots populated with correct types.
        assert_eq!(dst.get(d_ep), Ok(ep_cap));
        assert_eq!(dst.get(d_notif), Ok(notif_cap));
        assert_eq!(dst.get(d_reply), Ok(reply_cap));
        assert_eq!(dst.get(d_grant), Ok(grant_cap));
    }
}
