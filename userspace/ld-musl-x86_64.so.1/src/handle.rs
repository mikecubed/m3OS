//! `HandleTable` — Phase 76c slab allocator for `dlopen`/`dlclose`
//! handles.
//!
//! `dlopen` returns an opaque `*mut c_void` that `dlclose` later
//! validates. A forged or already-freed handle must be detected and
//! rejected — otherwise `dlclose` could unmap an unrelated DSO or
//! invoke a stale `DT_FINI_ARRAY` pointer.
//!
//! The table is a fixed-size array of [`HandleSlot`]. Each slot stores
//! `(dso_id, generation)`. `insert` returns a pointer to one of the
//! slot records (the slot itself lives in the table, which lives in
//! the linker's static [`crate::dl::DlState`]). `resolve` walks the
//! slot array to find the slot whose address matches the passed-in
//! handle pointer, then validates the generation matches.
//!
//! ### Pure-logic kernel
//!
//! The `HandleTable` body uses only `core::` and stable indices, so
//! it is host-testable under `cargo test`. The runtime `*mut c_void`
//! shape only appears at the [`crate::dl`] boundary.

use core::ffi::c_void;

use crate::dynlink::DsoId;

/// Hard upper bound on the number of simultaneously-live `dlopen`
/// handles. The Phase 76c smoke test uses 1; a generous cap keeps us
/// from regressing on workloads that hold many handles open at once
/// (e.g. plugin hosts that open one .so per plugin).
pub const MAX_HANDLES: usize = 64;

/// One handle-table slot. `in_use == false` means the slot is free
/// and may be reused on the next `insert`. `generation` increments
/// every time a slot is re-used so an old handle pointer cannot be
/// confused with a fresh one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandleSlot {
    pub dso_id: DsoId,
    pub generation: u32,
    pub in_use: bool,
}

impl HandleSlot {
    pub const fn empty() -> Self {
        Self {
            dso_id: DsoId(0),
            generation: 0,
            in_use: false,
        }
    }
}

/// Refcount-tracked handle table. The runtime stores a single
/// instance in [`crate::dl::DlState`].
#[derive(Debug)]
pub struct HandleTable {
    slots: [HandleSlot; MAX_HANDLES],
    /// Monotonic counter used as the generation token for every
    /// newly-allocated handle. Wrapping at `u32::MAX` is acceptable —
    /// the generation only has to *differ* between an old and a
    /// reused slot, not be globally unique.
    next_gen: u32,
}

/// Public errors `HandleTable` operations can return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    /// All `MAX_HANDLES` slots are in use.
    TableFull,
    /// The passed-in `*mut c_void` does not match any slot in the
    /// table, OR the slot's generation has been bumped since the
    /// handle was issued (e.g. after `dlclose` reused the slot).
    InvalidHandle,
}

impl HandleTable {
    pub const fn new() -> Self {
        Self {
            slots: [HandleSlot::empty(); MAX_HANDLES],
            next_gen: 1,
        }
    }

    /// Reserve a slot for `dso_id` and return an opaque pointer to
    /// the slot. The pointer is stable as long as the [`HandleTable`]
    /// itself is not moved (it lives in static [`crate::dl::DlState`],
    /// so the address is stable for the lifetime of the process).
    pub fn insert(&mut self, dso_id: DsoId) -> Result<*mut c_void, HandleError> {
        for i in 0..MAX_HANDLES {
            if !self.slots[i].in_use {
                let generation = self.next_gen;
                // `wrapping_add` is fine: the generation token only
                // has to differ between a freed slot's old handle and
                // its reuse, not be unique across the entire process.
                self.next_gen = self.next_gen.wrapping_add(1);
                self.slots[i] = HandleSlot {
                    dso_id,
                    generation,
                    in_use: true,
                };
                return Ok(self.slot_ptr(i));
            }
        }
        Err(HandleError::TableFull)
    }

    /// Look up the `DsoId` backing `handle`. Returns `Err(InvalidHandle)`
    /// for forged handles, freed handles, or handles whose generation
    /// no longer matches the live slot's generation.
    pub fn resolve(&self, handle: *mut c_void) -> Result<DsoId, HandleError> {
        let idx = self.slot_index(handle).ok_or(HandleError::InvalidHandle)?;
        let slot = &self.slots[idx];
        if !slot.in_use {
            return Err(HandleError::InvalidHandle);
        }
        Ok(slot.dso_id)
    }

    /// Remove `handle` from the table. The slot is marked free and
    /// its generation is bumped on the next `insert` (via `next_gen`)
    /// so a leaked copy of the old pointer fails validation. Returns
    /// the freed `DsoId` so the caller can take any per-DSO action.
    pub fn remove(&mut self, handle: *mut c_void) -> Result<DsoId, HandleError> {
        let idx = self.slot_index(handle).ok_or(HandleError::InvalidHandle)?;
        let slot = &mut self.slots[idx];
        if !slot.in_use {
            return Err(HandleError::InvalidHandle);
        }
        let dso_id = slot.dso_id;
        slot.in_use = false;
        // Don't reset dso_id/generation — the generation bump on the
        // next insert is what invalidates a leaked pointer.
        Ok(dso_id)
    }

    /// Iterate every live slot. Used by [`crate::dl::DlState`] to
    /// recompute refcounts across the handle table.
    pub fn iter_live(&self) -> impl Iterator<Item = (DsoId, u32)> + '_ {
        self.slots
            .iter()
            .filter(|s| s.in_use)
            .map(|s| (s.dso_id, s.generation))
    }

    /// Number of in-use slots — host-test helper.
    #[cfg(test)]
    pub fn live_count(&self) -> usize {
        self.slots.iter().filter(|s| s.in_use).count()
    }

    /// Compute the slot index from a handle pointer by pointer
    /// arithmetic. Returns `None` if `handle` is `null` or does not
    /// lie inside the slot array.
    fn slot_index(&self, handle: *mut c_void) -> Option<usize> {
        if handle.is_null() {
            return None;
        }
        let base = self.slots.as_ptr() as usize;
        let h = handle as usize;
        if h < base {
            return None;
        }
        let off = h - base;
        let stride = core::mem::size_of::<HandleSlot>();
        if !off.is_multiple_of(stride) {
            return None;
        }
        let idx = off / stride;
        if idx >= MAX_HANDLES {
            return None;
        }
        Some(idx)
    }

    fn slot_ptr(&self, idx: usize) -> *mut c_void {
        &self.slots[idx] as *const HandleSlot as *mut c_void
    }
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_returns_distinct_pointers() {
        let mut t = HandleTable::new();
        let h1 = t.insert(DsoId(1)).unwrap();
        let h2 = t.insert(DsoId(2)).unwrap();
        assert_ne!(h1, h2);
        assert_eq!(t.live_count(), 2);
    }

    #[test]
    fn resolve_returns_dso_id() {
        let mut t = HandleTable::new();
        let h = t.insert(DsoId(42)).unwrap();
        assert_eq!(t.resolve(h).unwrap(), DsoId(42));
    }

    #[test]
    fn resolve_null_handle_errors() {
        let t = HandleTable::new();
        assert_eq!(
            t.resolve(core::ptr::null_mut()),
            Err(HandleError::InvalidHandle)
        );
    }

    #[test]
    fn resolve_forged_handle_errors() {
        let t = HandleTable::new();
        let forged = 0xDEAD_BEEF_usize as *mut c_void;
        assert_eq!(t.resolve(forged), Err(HandleError::InvalidHandle));
    }

    #[test]
    fn remove_then_resolve_errors() {
        let mut t = HandleTable::new();
        let h = t.insert(DsoId(7)).unwrap();
        assert_eq!(t.remove(h).unwrap(), DsoId(7));
        // The slot is now free; resolve must reject the stale pointer.
        assert_eq!(t.resolve(h), Err(HandleError::InvalidHandle));
        // Re-removing also errors.
        assert_eq!(t.remove(h), Err(HandleError::InvalidHandle));
    }

    #[test]
    fn reinsert_bumps_generation() {
        let mut t = HandleTable::new();
        let h1 = t.insert(DsoId(1)).unwrap();
        let g1 = unsafe { (*(h1 as *const HandleSlot)).generation };
        t.remove(h1).unwrap();
        let h2 = t.insert(DsoId(2)).unwrap();
        // The first free slot gets reused.
        assert_eq!(h1, h2);
        let g2 = unsafe { (*(h2 as *const HandleSlot)).generation };
        assert_ne!(g1, g2, "generation must change on slot reuse");
    }

    #[test]
    fn insert_fails_when_table_full() {
        let mut t = HandleTable::new();
        for i in 0..MAX_HANDLES {
            assert!(t.insert(DsoId(i as u32)).is_ok());
        }
        assert_eq!(t.insert(DsoId(99)), Err(HandleError::TableFull));
    }

    #[test]
    fn iter_live_yields_only_in_use_slots() {
        let mut t = HandleTable::new();
        let _ = t.insert(DsoId(10)).unwrap();
        let h2 = t.insert(DsoId(20)).unwrap();
        let _ = t.insert(DsoId(30)).unwrap();
        t.remove(h2).unwrap();
        let live: heapless::Vec<DsoId, 16> = t.iter_live().map(|(id, _)| id).collect();
        assert_eq!(live.len(), 2);
        assert!(live.contains(&DsoId(10)));
        assert!(live.contains(&DsoId(30)));
        assert!(!live.contains(&DsoId(20)));
    }

    #[test]
    fn misaligned_pointer_errors() {
        let t = HandleTable::new();
        let base = t.slots.as_ptr() as usize;
        // Pointer 1 byte past the first slot is misaligned and lies
        // inside slot 0 — must reject.
        let bad = (base + 1) as *mut c_void;
        assert_eq!(t.resolve(bad), Err(HandleError::InvalidHandle));
    }
}
