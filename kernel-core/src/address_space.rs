//! First-class address space descriptor — pure logic, host-testable.
//!
//! The kernel's `mm::AddressSpace` wraps an `x86_64::PhysAddr`.  This module
//! mirrors the atomic bitmask and generation-counter logic without pulling in
//! the `x86_64` crate so it can be tested on the host with `cargo test`.

use core::sync::atomic::{AtomicU64, Ordering};

/// Maximum number of cores the bitmask can track (one bit per core).
pub const MAX_CORES: u8 = 64;

/// Per-core tracking and generation counter for an address space.
///
/// Mirrors the kernel's `AddressSpace` struct but uses a plain `u64`
/// instead of `PhysAddr` so it is host-testable.
pub struct AddressSpaceInfo {
    pml4_phys: u64,
    generation: AtomicU64,
    active_on_cores: AtomicU64,
}

impl AddressSpaceInfo {
    pub fn new(pml4_phys: u64) -> Self {
        Self {
            pml4_phys,
            generation: AtomicU64::new(0),
            active_on_cores: AtomicU64::new(0),
        }
    }

    pub fn pml4_phys(&self) -> u64 {
        self.pml4_phys
    }

    /// Set the bit for `core_id` in the active-cores bitmask.
    pub fn activate_on_core(&self, core_id: u8) {
        self.active_on_cores
            .fetch_or(1u64 << core_id, Ordering::Release);
    }

    /// Clear the bit for `core_id` in the active-cores bitmask.
    pub fn deactivate_on_core(&self, core_id: u8) {
        self.active_on_cores
            .fetch_and(!(1u64 << core_id), Ordering::Release);
    }

    /// Increment the generation counter and return the *previous* value.
    pub fn bump_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel)
    }

    /// Read the current generation counter.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Read the current active-cores bitmask.
    pub fn active_cores(&self) -> u64 {
        self.active_on_cores.load(Ordering::Acquire)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_empty() {
        let a = AddressSpaceInfo::new(0x1000);
        assert_eq!(a.pml4_phys(), 0x1000);
        assert_eq!(a.generation(), 0);
        assert_eq!(a.active_cores(), 0);
    }

    #[test]
    fn activate_sets_bit() {
        let a = AddressSpaceInfo::new(0);
        a.activate_on_core(0);
        assert_eq!(a.active_cores(), 1);

        a.activate_on_core(3);
        assert_eq!(a.active_cores(), 0b1001);
    }

    #[test]
    fn deactivate_clears_bit() {
        let a = AddressSpaceInfo::new(0);
        a.activate_on_core(0);
        a.activate_on_core(3);
        a.deactivate_on_core(0);
        assert_eq!(a.active_cores(), 0b1000);
    }

    #[test]
    fn deactivate_idempotent() {
        let a = AddressSpaceInfo::new(0);
        a.deactivate_on_core(5);
        assert_eq!(a.active_cores(), 0);
    }

    #[test]
    fn activate_idempotent() {
        let a = AddressSpaceInfo::new(0);
        a.activate_on_core(2);
        a.activate_on_core(2);
        assert_eq!(a.active_cores(), 1 << 2);
    }

    #[test]
    fn core_63_boundary() {
        let a = AddressSpaceInfo::new(0);
        a.activate_on_core(63);
        assert_eq!(a.active_cores(), 1u64 << 63);

        a.deactivate_on_core(63);
        assert_eq!(a.active_cores(), 0);
    }

    #[test]
    fn all_cores_active() {
        let a = AddressSpaceInfo::new(0);
        for c in 0..MAX_CORES {
            a.activate_on_core(c);
        }
        assert_eq!(a.active_cores(), u64::MAX);

        a.deactivate_on_core(0);
        assert_eq!(a.active_cores(), u64::MAX - 1);
    }

    #[test]
    fn generation_monotonic() {
        let a = AddressSpaceInfo::new(0);
        assert_eq!(a.bump_generation(), 0); // returns previous
        assert_eq!(a.generation(), 1);
        assert_eq!(a.bump_generation(), 1);
        assert_eq!(a.generation(), 2);
    }

    #[test]
    fn generation_many_bumps() {
        let a = AddressSpaceInfo::new(0);
        for i in 0..100 {
            assert_eq!(a.bump_generation(), i);
        }
        assert_eq!(a.generation(), 100);
    }

    #[test]
    fn independent_instances() {
        let a = AddressSpaceInfo::new(0x1000);
        let b = AddressSpaceInfo::new(0x2000);
        a.activate_on_core(0);
        b.activate_on_core(1);
        a.bump_generation();

        assert_eq!(a.active_cores(), 1);
        assert_eq!(b.active_cores(), 2);
        assert_eq!(a.generation(), 1);
        assert_eq!(b.generation(), 0);
    }

    #[test]
    fn activate_deactivate_sequence() {
        let a = AddressSpaceInfo::new(0);

        // Simulate scheduler: core 0 loads AS, then core 1, then core 0 switches away
        a.activate_on_core(0);
        assert_eq!(a.active_cores(), 0b01);

        a.activate_on_core(1);
        assert_eq!(a.active_cores(), 0b11);

        a.deactivate_on_core(0);
        assert_eq!(a.active_cores(), 0b10);

        a.deactivate_on_core(1);
        assert_eq!(a.active_cores(), 0);
    }
}
