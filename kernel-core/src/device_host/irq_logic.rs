// Pure-logic IRQ-binding registry — Phase 55b Track B.4.
//
// The kernel-side `sys_device_irq_subscribe` wrapper keeps a side table that
// records `(pid, DeviceCapKey) → (vector, NotifId, bit_index)` so that the
// process-exit sweep can release vectors, disable MSI capabilities, and
// unbind notifications in a single deterministic pass. Keeping the logic
// here in `kernel-core` pins the invariants (per-pid cap, uniqueness of
// vectors across devices, ordering guarantees across release_for_pid) under
// host-testable conditions that do not require a booted kernel.
//
// Invariants exercised by the unit tests:
//
// * A single driver cannot exceed `MAX_IRQ_SUBSCRIPTIONS_PER_PID` bindings.
//   Attempting to do so returns `IrqRegistryError::CapacityExceeded`.
// * A vector may be bound at most once — a subsequent bind of the same
//   vector (even by another PID) returns `VectorBusy`.
// * `release_for_pid` yields every binding owned by the dying PID in a
//   single call so the kernel-side teardown can iterate deterministically.
// * `release_vector` removes a single binding by vector and is used in the
//   unwind path when the capability-table insertion fails after the vector
//   is allocated.

extern crate alloc;

use alloc::vec::Vec;

use super::registry_logic::RegistryPid;
use super::types::DeviceCapKey;

/// Upper bound on concurrent IRQ subscriptions per driver PID.
///
/// Derived from the Phase 55b task list's resource-bound discipline
/// ("initial cap: 8 IRQ subscriptions"). Exceeding the cap surfaces at the
/// syscall boundary as `DeviceHostError::CapacityExceeded` and the driver
/// must release before subscribing again.
pub const MAX_IRQ_SUBSCRIPTIONS_PER_PID: usize = 8;

/// Errors surfaced by the IRQ-binding registry core.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum IrqRegistryError {
    /// Another binding already owns this IDT vector.
    VectorBusy,
    /// The per-PID subscription cap would be exceeded.
    CapacityExceeded,
    /// `release_vector` called with a vector that is not currently bound.
    NotBound,
}

/// One binding in the IRQ registry. Public fields because the struct is a
/// transparent record — the backing code is in the parent module and the
/// unit tests need to pin the shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqBinding {
    pub pid: RegistryPid,
    pub key: DeviceCapKey,
    /// IDT vector (in the device-IRQ bank).
    pub vector: u8,
    /// Notification slot index the ISR signals.
    pub notif_id: u8,
    /// Bit within the notification word the ISR `fetch_or`s on delivery.
    pub bit_index: u8,
}

/// Pure-logic IRQ binding registry.
///
/// Holds one entry per live `sys_device_irq_subscribe` subscription. The
/// kernel-side wrapper maintains the matching ISR dispatch table; this
/// struct is the single source of truth for what is currently bound.
#[derive(Default)]
pub struct IrqBindingRegistryCore {
    entries: Vec<IrqBinding>,
}

impl IrqBindingRegistryCore {
    /// Construct an empty registry.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Total number of active bindings across all PIDs.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no bindings are currently live.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of bindings currently owned by `pid`.
    pub fn count_for_pid(&self, pid: RegistryPid) -> usize {
        self.entries.iter().filter(|e| e.pid == pid).count()
    }

    /// Return `Some(pid)` if `vector` is currently bound.
    pub fn owner_of_vector(&self, vector: u8) -> Option<RegistryPid> {
        self.entries
            .iter()
            .find(|e| e.vector == vector)
            .map(|e| e.pid)
    }

    /// Try to register a new binding.
    ///
    /// Fails if `vector` is already bound (returning `VectorBusy`) or if the
    /// per-PID cap would be exceeded (`CapacityExceeded`). Does not touch any
    /// hardware — the caller is responsible for the MSI/INTx wiring.
    pub fn try_bind(&mut self, binding: IrqBinding) -> Result<(), IrqRegistryError> {
        if self.entries.iter().any(|e| e.vector == binding.vector) {
            return Err(IrqRegistryError::VectorBusy);
        }
        if self.count_for_pid(binding.pid) >= MAX_IRQ_SUBSCRIPTIONS_PER_PID {
            return Err(IrqRegistryError::CapacityExceeded);
        }
        self.entries.push(binding);
        Ok(())
    }

    /// Remove a single binding by vector. Used in unwind paths where the
    /// vector was allocated but the kernel-side capability insertion failed.
    pub fn release_vector(&mut self, vector: u8) -> Result<IrqBinding, IrqRegistryError> {
        let pos = self.entries.iter().position(|e| e.vector == vector);
        match pos {
            Some(i) => Ok(self.entries.swap_remove(i)),
            None => Err(IrqRegistryError::NotBound),
        }
    }

    /// Release every binding owned by `pid`.
    ///
    /// Returns the removed entries so the caller can tear down hardware
    /// state (unregister the device-IRQ handler, disable MSI, etc.) in a
    /// single pass without re-querying the registry.
    pub fn release_for_pid(&mut self, pid: RegistryPid) -> Vec<IrqBinding> {
        let mut freed = Vec::new();
        self.entries.retain(|e| {
            if e.pid == pid {
                freed.push(*e);
                false
            } else {
                true
            }
        });
        freed
    }

    /// Look up the binding for `vector`, if any.
    pub fn get_by_vector(&self, vector: u8) -> Option<IrqBinding> {
        self.entries.iter().copied().find(|e| e.vector == vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: DeviceCapKey = DeviceCapKey::new(0, 0, 3, 0);
    const KEY_B: DeviceCapKey = DeviceCapKey::new(0, 0, 4, 0);

    fn binding(pid: RegistryPid, key: DeviceCapKey, vector: u8) -> IrqBinding {
        IrqBinding {
            pid,
            key,
            vector,
            notif_id: 1,
            bit_index: 0,
        }
    }

    #[test]
    fn first_bind_succeeds_and_is_queryable() {
        let mut reg = IrqBindingRegistryCore::new();
        assert_eq!(reg.len(), 0);
        reg.try_bind(binding(100, KEY_A, 0x60)).expect("first bind");
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.owner_of_vector(0x60), Some(100));
        assert_eq!(reg.count_for_pid(100), 1);
    }

    #[test]
    fn duplicate_vector_bind_is_rejected_as_vector_busy() {
        let mut reg = IrqBindingRegistryCore::new();
        reg.try_bind(binding(100, KEY_A, 0x60)).unwrap();
        assert_eq!(
            reg.try_bind(binding(200, KEY_B, 0x60)),
            Err(IrqRegistryError::VectorBusy),
        );
        // Original binding intact.
        assert_eq!(reg.owner_of_vector(0x60), Some(100));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn per_pid_cap_enforced() {
        let mut reg = IrqBindingRegistryCore::new();
        for i in 0..MAX_IRQ_SUBSCRIPTIONS_PER_PID {
            reg.try_bind(binding(100, KEY_A, 0x60 + i as u8))
                .expect("within cap");
        }
        assert_eq!(
            reg.try_bind(binding(100, KEY_A, 0x6f)),
            Err(IrqRegistryError::CapacityExceeded),
        );
        // Another PID can still bind — the cap is per-PID.
        reg.try_bind(binding(200, KEY_B, 0x6f))
            .expect("different PID not affected");
    }

    #[test]
    fn release_vector_returns_the_removed_binding() {
        let mut reg = IrqBindingRegistryCore::new();
        let b = binding(100, KEY_A, 0x60);
        reg.try_bind(b).unwrap();
        assert_eq!(reg.release_vector(0x60), Ok(b));
        assert_eq!(reg.owner_of_vector(0x60), None);
        assert!(reg.is_empty());
    }

    #[test]
    fn release_vector_of_unbound_returns_not_bound() {
        let mut reg = IrqBindingRegistryCore::new();
        assert_eq!(reg.release_vector(0x60), Err(IrqRegistryError::NotBound));
    }

    #[test]
    fn release_for_pid_sweeps_every_binding_for_that_pid() {
        let mut reg = IrqBindingRegistryCore::new();
        reg.try_bind(binding(100, KEY_A, 0x60)).unwrap();
        reg.try_bind(binding(100, KEY_A, 0x61)).unwrap();
        reg.try_bind(binding(200, KEY_B, 0x62)).unwrap();

        let freed = reg.release_for_pid(100);
        assert_eq!(freed.len(), 2);
        assert!(freed.iter().any(|b| b.vector == 0x60));
        assert!(freed.iter().any(|b| b.vector == 0x61));
        // PID 200 untouched.
        assert_eq!(reg.owner_of_vector(0x62), Some(200));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn release_for_pid_with_no_bindings_returns_empty_vec() {
        let mut reg = IrqBindingRegistryCore::new();
        reg.try_bind(binding(100, KEY_A, 0x60)).unwrap();
        let freed = reg.release_for_pid(999);
        assert!(freed.is_empty());
        // Existing binding untouched.
        assert_eq!(reg.owner_of_vector(0x60), Some(100));
    }

    #[test]
    fn get_by_vector_round_trips_the_binding() {
        let mut reg = IrqBindingRegistryCore::new();
        let b = IrqBinding {
            pid: 100,
            key: KEY_A,
            vector: 0x6a,
            notif_id: 5,
            bit_index: 7,
        };
        reg.try_bind(b).unwrap();
        assert_eq!(reg.get_by_vector(0x6a), Some(b));
        assert_eq!(reg.get_by_vector(0x6b), None);
    }

    #[test]
    fn max_subscriptions_constant_is_eight() {
        // Pin the value — changing this is a task-doc-level decision.
        assert_eq!(MAX_IRQ_SUBSCRIPTIONS_PER_PID, 8);
    }
}
