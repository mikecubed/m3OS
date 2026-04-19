//! `MockBackend` — pure-logic reference implementation of the
//! `driver_runtime` contract traits.
//!
//! Phase 55b Track A.4 acceptance artifact: this is the authoritative
//! reference against which `DeviceHandleContract`, `MmioContract`,
//! `DmaBufferContract`, and `IrqNotificationContract` are exercised on the
//! host. The Track C safe wrappers (`userspace/lib/driver_runtime`) will
//! be re-run against the real syscall backend via the same contract suite
//! when Track C.2 lands.
//!
//! Scope is deliberately narrow — the mock tracks:
//!
//! - live device claims in a `BTreeMap<DeviceCapKey, ClaimState>` so the
//!   contract suite can verify `claim`/`release` pair up and second-claim
//!   returns `AlreadyClaimed`.
//! - MMIO windows as plain `Vec<u8>` scratch backing so `read_*` observes
//!   the value written by `write_*` at the same offset (the only
//!   observable behavior the contract cares about).
//! - DMA allocations with an `iova` counter so two allocations get
//!   distinct IOVAs; `live_dma_count()` exposes the live count so the
//!   Drop-release acceptance bullet is visible.
//! - IRQ subscriptions as a simple vector-indexed queue with a `deliver`
//!   helper the test suite uses to simulate an interrupt arriving before
//!   `wait` is called.
//!
//! No `unsafe`, no allocation inside trait methods beyond what the
//! reference behavior demands, and no hidden state beyond the counters
//! named below.

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use kernel_core::device_host::{DeviceCapKey, DeviceHostError, DmaHandle, MmioWindowDescriptor};
use kernel_core::driver_runtime::contract::{
    DeviceHandleContract, DmaBufferContract, DmaBufferHandle, DriverRuntimeError,
    IrqNotificationContract, IrqNotificationHandle, MmioContract,
};

// ---------------------------------------------------------------------------
// Shared backend state
// ---------------------------------------------------------------------------

/// Per-device record. `released` is set when `release` is called so a
/// second `release` returns `DriverRuntimeError::from(DeviceHostError::
/// NotClaimed)`.
#[derive(Debug)]
struct ClaimState {
    released: bool,
}

/// Per-MMIO-window scratch buffer. `Vec<u8>` is plenty — the contract
/// only requires read-after-write observability at the same offset.
#[derive(Debug)]
struct MmioState {
    key: DeviceCapKey,
    bar_index: u8,
    bytes: Vec<u8>,
}

/// Per-DMA-allocation record.
#[derive(Debug)]
struct DmaState {
    key: DeviceCapKey,
    released: bool,
    handle: DmaHandle,
}

/// Per-IRQ-subscription state. `pending` is a count of undelivered
/// interrupts; `ack_count` is the number of `ack`s observed. The contract
/// suite uses `deliver` below to simulate the kernel delivering an IRQ.
#[derive(Debug)]
struct IrqState {
    key: DeviceCapKey,
    vector_hint: Option<u8>,
    pending: VecDeque<()>,
    ack_count: u64,
    released: bool,
}

/// Parameters `allocate` records for later inspection by the contract
/// suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaAllocParams {
    pub size: usize,
    pub align: usize,
}

/// Central shared state. Each handle / window / buffer / notif carries an
/// `Rc<RefCell<MockBackendState>>` so Drop on those types can call back
/// into the backend to release the underlying resource — mirroring the
/// Track C wrappers' Drop-releases-handle behavior.
#[derive(Debug)]
pub struct MockBackendState {
    pub claims: BTreeMap<DeviceCapKey, ClaimState>,
    pub mmio_windows: BTreeMap<u64, MmioState>,
    pub dma_buffers: BTreeMap<u64, DmaState>,
    pub irqs: BTreeMap<u64, IrqState>,
    pub next_mmio_id: u64,
    pub next_dma_id: u64,
    pub next_irq_id: u64,
    pub next_iova: u64,
    pub next_user_va: usize,
    /// Ordered log of `allocate(size, align)` calls, so tests can assert
    /// the contract passed values through.
    pub dma_allocs: Vec<DmaAllocParams>,
    /// Error to inject on the next `subscribe` call, exercising the
    /// `IrqUnavailable` branch of the contract.
    pub inject_irq_unavailable: bool,
    /// Error to inject on the next `allocate` call, exercising the
    /// `IovaExhausted` branch.
    pub inject_iova_exhausted: bool,
    /// Error to inject on the next `map` call, exercising the
    /// `InvalidBarIndex` branch.
    pub inject_invalid_bar: bool,
    /// Force the next `subscribe`/`wait` into the `IrqTimeout` branch.
    pub inject_irq_timeout: bool,
    /// Force the next `read_*`/`write_*` into the `UserFaultOnMmio`
    /// branch. The contract reference treats this as a runtime error
    /// observable through a side-channel; the mock records it so the
    /// contract suite can consult `last_mmio_fault`.
    pub inject_mmio_fault: bool,
    /// Records the last `UserFaultOnMmio`-flagged MMIO access.
    pub last_mmio_fault: Option<(u64, usize)>,
}

impl MockBackendState {
    fn new() -> Self {
        Self {
            claims: BTreeMap::new(),
            mmio_windows: BTreeMap::new(),
            dma_buffers: BTreeMap::new(),
            irqs: BTreeMap::new(),
            next_mmio_id: 1,
            next_dma_id: 1,
            next_irq_id: 1,
            next_iova: 0x1_0000_0000,
            next_user_va: 0x7000_0000,
            dma_allocs: Vec::new(),
            inject_irq_unavailable: false,
            inject_iova_exhausted: false,
            inject_invalid_bar: false,
            inject_irq_timeout: false,
            inject_mmio_fault: false,
            last_mmio_fault: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Shared-state mock backend.
///
/// The backend is cheap to clone — every clone shares the same
/// `RefCell<MockBackendState>` so handles and buffers can hold a
/// back-reference without lifetimes. That matches how the Track C
/// wrappers will hold an `Arc<...>` to the real syscall client.
#[derive(Clone, Debug)]
pub struct MockBackend {
    state: Rc<RefCell<MockBackendState>>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    /// Construct a fresh mock backend with empty state.
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(MockBackendState::new())),
        }
    }

    /// Number of devices currently claimed (claim called but not yet released).
    pub fn live_claim_count(&self) -> usize {
        self.state
            .borrow()
            .claims
            .values()
            .filter(|s| !s.released)
            .count()
    }

    /// Number of DMA buffers currently live (allocate'd but not yet Drop'd).
    pub fn live_dma_count(&self) -> usize {
        self.state
            .borrow()
            .dma_buffers
            .values()
            .filter(|s| !s.released)
            .count()
    }

    /// Number of IRQ subscriptions currently live.
    pub fn live_irq_count(&self) -> usize {
        self.state
            .borrow()
            .irqs
            .values()
            .filter(|s| !s.released)
            .count()
    }

    /// Returns the ordered log of `DmaBufferContract::allocate` invocations.
    pub fn dma_alloc_log(&self) -> Vec<DmaAllocParams> {
        self.state.borrow().dma_allocs.clone()
    }

    /// Deliver a simulated IRQ to the subscription backed by `notif`. The
    /// next `wait()` will observe it without blocking.
    pub fn deliver_irq(&self, notif: &MockIrqNotif) {
        let mut st = self.state.borrow_mut();
        if let Some(entry) = st.irqs.get_mut(&notif.id) {
            entry.pending.push_back(());
        }
    }

    /// Read the recorded `ack` count for a live IRQ subscription.
    pub fn ack_count(&self, notif: &MockIrqNotif) -> u64 {
        self.state
            .borrow()
            .irqs
            .get(&notif.id)
            .map(|s| s.ack_count)
            .unwrap_or(0)
    }

    /// Direct access to the shared state for configuring error-injection
    /// flags in tests.
    pub fn state(&self) -> &RefCell<MockBackendState> {
        &self.state
    }
}

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

/// Backend-owned device claim handle. Cheap to clone (shares state via
/// `Rc`) so the contract suite can pass it to many `MmioContract::map`
/// calls without consuming it.
#[derive(Clone, Debug)]
pub struct MockHandle {
    pub key: DeviceCapKey,
    state: Rc<RefCell<MockBackendState>>,
}

impl MockHandle {
    fn is_live(&self) -> bool {
        self.state
            .borrow()
            .claims
            .get(&self.key)
            .map(|s| !s.released)
            .unwrap_or(false)
    }
}

/// Backend MMIO window handle. Holds an `Rc<RefCell<...>>` so the
/// read/write implementations can access the scratch buffer without
/// lifetimes.
#[derive(Clone, Debug)]
pub struct MockMmioWindow {
    id: u64,
    descriptor: MmioWindowDescriptor,
    state: Rc<RefCell<MockBackendState>>,
}

impl MockMmioWindow {
    /// The descriptor the backend synthesized at map time.
    pub fn descriptor(&self) -> MmioWindowDescriptor {
        self.descriptor
    }
}

/// Backend DMA buffer handle. The `Drop` impl releases the backing
/// record in the shared state so `live_dma_count()` observes the
/// reclamation.
#[derive(Debug)]
pub struct MockDmaBuffer {
    id: u64,
    handle: DmaHandle,
    state: Rc<RefCell<MockBackendState>>,
}

impl DmaBufferHandle for MockDmaBuffer {
    fn user_va(&self) -> usize {
        self.handle.user_va
    }
    fn iova(&self) -> u64 {
        self.handle.iova
    }
    fn len(&self) -> usize {
        self.handle.len
    }
}

impl Drop for MockDmaBuffer {
    fn drop(&mut self) {
        // Drop releases the handle — the authoritative behavioral bullet
        // for `DmaBufferContract`. The mock records it in shared state so
        // the contract suite can observe via `live_dma_count()`.
        if let Ok(mut st) = self.state.try_borrow_mut()
            && let Some(rec) = st.dma_buffers.get_mut(&self.id)
        {
            rec.released = true;
        }
    }
}

/// Backend IRQ notification handle. The `Drop` impl releases the
/// subscription so `live_irq_count()` drops accordingly.
#[derive(Debug)]
pub struct MockIrqNotif {
    id: u64,
    state: Rc<RefCell<MockBackendState>>,
}

impl MockIrqNotif {
    /// Opaque subscription identifier — surfaced so the contract suite
    /// can assert uniqueness across subscriptions.
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl IrqNotificationHandle for MockIrqNotif {
    fn wait(&mut self) -> Result<(), DriverRuntimeError> {
        let mut st = self.state.borrow_mut();
        if st.inject_irq_timeout {
            st.inject_irq_timeout = false;
            return Err(DriverRuntimeError::IrqTimeout);
        }
        let entry = st
            .irqs
            .get_mut(&self.id)
            .ok_or(DriverRuntimeError::from(DeviceHostError::Internal))?;
        if entry.pending.pop_front().is_some() {
            Ok(())
        } else {
            Err(DriverRuntimeError::IrqTimeout)
        }
    }

    fn ack(&mut self) -> Result<(), DriverRuntimeError> {
        let mut st = self.state.borrow_mut();
        let entry = st
            .irqs
            .get_mut(&self.id)
            .ok_or(DriverRuntimeError::from(DeviceHostError::Internal))?;
        entry.ack_count = entry.ack_count.wrapping_add(1);
        Ok(())
    }
}

impl Drop for MockIrqNotif {
    fn drop(&mut self) {
        if let Ok(mut st) = self.state.try_borrow_mut()
            && let Some(rec) = st.irqs.get_mut(&self.id)
        {
            rec.released = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Trait impls
// ---------------------------------------------------------------------------

impl DeviceHandleContract for MockBackend {
    type Handle = MockHandle;

    fn claim(&mut self, key: DeviceCapKey) -> Result<Self::Handle, DriverRuntimeError> {
        let mut st = self.state.borrow_mut();
        match st.claims.get(&key) {
            Some(s) if !s.released => {
                return Err(DriverRuntimeError::from(DeviceHostError::AlreadyClaimed));
            }
            _ => {}
        }
        st.claims.insert(key, ClaimState { released: false });
        Ok(MockHandle {
            key,
            state: self.state.clone(),
        })
    }

    fn release(&mut self, handle: Self::Handle) -> Result<(), DriverRuntimeError> {
        let mut st = self.state.borrow_mut();
        let entry = st
            .claims
            .get_mut(&handle.key)
            .ok_or(DriverRuntimeError::from(DeviceHostError::NotClaimed))?;
        if entry.released {
            return Err(DriverRuntimeError::from(DeviceHostError::NotClaimed));
        }
        entry.released = true;
        Ok(())
    }
}

impl MmioContract for MockBackend {
    type MmioWindow = MockMmioWindow;

    fn map(
        &mut self,
        handle: &Self::Handle,
        bar: u8,
    ) -> Result<Self::MmioWindow, DriverRuntimeError> {
        if !handle.is_live() {
            return Err(DriverRuntimeError::from(DeviceHostError::NotClaimed));
        }
        let mut st = self.state.borrow_mut();
        if st.inject_invalid_bar {
            st.inject_invalid_bar = false;
            return Err(DriverRuntimeError::from(DeviceHostError::InvalidBarIndex));
        }
        let id = st.next_mmio_id;
        st.next_mmio_id = st
            .next_mmio_id
            .checked_add(1)
            .ok_or(DriverRuntimeError::from(DeviceHostError::Internal))?;
        let descriptor = MmioWindowDescriptor {
            phys_base: 0xfeb0_0000 + (bar as u64) * 0x1_0000,
            len: 0x1_0000,
            bar_index: bar,
            prefetchable: false,
            cache_mode: kernel_core::device_host::MmioCacheMode::Uncacheable,
        };
        st.mmio_windows.insert(
            id,
            MmioState {
                key: handle.key,
                bar_index: bar,
                bytes: vec![0u8; descriptor.len],
            },
        );
        Ok(MockMmioWindow {
            id,
            descriptor,
            state: self.state.clone(),
        })
    }

    fn read_u8(&self, window: &Self::MmioWindow, offset: usize) -> u8 {
        let mut st = self.state.borrow_mut();
        if st.inject_mmio_fault {
            st.inject_mmio_fault = false;
            st.last_mmio_fault = Some((window.id, offset));
            return 0;
        }
        st.mmio_windows
            .get(&window.id)
            .and_then(|w| w.bytes.get(offset).copied())
            .unwrap_or(0)
    }
    fn read_u16(&self, window: &Self::MmioWindow, offset: usize) -> u16 {
        let mut st = self.state.borrow_mut();
        if st.inject_mmio_fault {
            st.inject_mmio_fault = false;
            st.last_mmio_fault = Some((window.id, offset));
            return 0;
        }
        st.mmio_windows
            .get(&window.id)
            .and_then(|w| w.bytes.get(offset..offset + 2))
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .unwrap_or(0)
    }
    fn read_u32(&self, window: &Self::MmioWindow, offset: usize) -> u32 {
        let mut st = self.state.borrow_mut();
        if st.inject_mmio_fault {
            st.inject_mmio_fault = false;
            st.last_mmio_fault = Some((window.id, offset));
            return 0;
        }
        st.mmio_windows
            .get(&window.id)
            .and_then(|w| w.bytes.get(offset..offset + 4))
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(0)
    }
    fn read_u64(&self, window: &Self::MmioWindow, offset: usize) -> u64 {
        let mut st = self.state.borrow_mut();
        if st.inject_mmio_fault {
            st.inject_mmio_fault = false;
            st.last_mmio_fault = Some((window.id, offset));
            return 0;
        }
        st.mmio_windows
            .get(&window.id)
            .and_then(|w| w.bytes.get(offset..offset + 8))
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            .unwrap_or(0)
    }

    fn write_u8(&mut self, window: &Self::MmioWindow, offset: usize, value: u8) {
        let mut st = self.state.borrow_mut();
        if st.inject_mmio_fault {
            st.inject_mmio_fault = false;
            st.last_mmio_fault = Some((window.id, offset));
            return;
        }
        if let Some(w) = st.mmio_windows.get_mut(&window.id)
            && let Some(slot) = w.bytes.get_mut(offset)
        {
            *slot = value;
        }
    }
    fn write_u16(&mut self, window: &Self::MmioWindow, offset: usize, value: u16) {
        let mut st = self.state.borrow_mut();
        if st.inject_mmio_fault {
            st.inject_mmio_fault = false;
            st.last_mmio_fault = Some((window.id, offset));
            return;
        }
        if let Some(w) = st.mmio_windows.get_mut(&window.id) {
            let bytes = value.to_le_bytes();
            for (i, b) in bytes.iter().enumerate() {
                if let Some(slot) = w.bytes.get_mut(offset + i) {
                    *slot = *b;
                }
            }
        }
    }
    fn write_u32(&mut self, window: &Self::MmioWindow, offset: usize, value: u32) {
        let mut st = self.state.borrow_mut();
        if st.inject_mmio_fault {
            st.inject_mmio_fault = false;
            st.last_mmio_fault = Some((window.id, offset));
            return;
        }
        if let Some(w) = st.mmio_windows.get_mut(&window.id) {
            let bytes = value.to_le_bytes();
            for (i, b) in bytes.iter().enumerate() {
                if let Some(slot) = w.bytes.get_mut(offset + i) {
                    *slot = *b;
                }
            }
        }
    }
    fn write_u64(&mut self, window: &Self::MmioWindow, offset: usize, value: u64) {
        let mut st = self.state.borrow_mut();
        if st.inject_mmio_fault {
            st.inject_mmio_fault = false;
            st.last_mmio_fault = Some((window.id, offset));
            return;
        }
        if let Some(w) = st.mmio_windows.get_mut(&window.id) {
            let bytes = value.to_le_bytes();
            for (i, b) in bytes.iter().enumerate() {
                if let Some(slot) = w.bytes.get_mut(offset + i) {
                    *slot = *b;
                }
            }
        }
    }
}

impl DmaBufferContract for MockBackend {
    type DmaBuffer = MockDmaBuffer;

    fn allocate(
        &mut self,
        handle: &Self::Handle,
        size: usize,
        align: usize,
    ) -> Result<Self::DmaBuffer, DriverRuntimeError> {
        if !handle.is_live() {
            return Err(DriverRuntimeError::from(DeviceHostError::NotClaimed));
        }
        let mut st = self.state.borrow_mut();
        st.dma_allocs.push(DmaAllocParams { size, align });
        if st.inject_iova_exhausted {
            st.inject_iova_exhausted = false;
            return Err(DriverRuntimeError::from(DeviceHostError::IovaExhausted));
        }
        let id = st.next_dma_id;
        st.next_dma_id = st
            .next_dma_id
            .checked_add(1)
            .ok_or(DriverRuntimeError::from(DeviceHostError::Internal))?;
        let iova = st.next_iova;
        st.next_iova = st
            .next_iova
            .checked_add(size as u64)
            .ok_or(DriverRuntimeError::from(DeviceHostError::IovaExhausted))?;
        let user_va = st.next_user_va;
        st.next_user_va = st
            .next_user_va
            .checked_add(size)
            .ok_or(DriverRuntimeError::from(DeviceHostError::CapacityExceeded))?;
        let dma_handle = DmaHandle {
            user_va,
            iova,
            len: size,
        };
        st.dma_buffers.insert(
            id,
            DmaState {
                key: handle.key,
                released: false,
                handle: dma_handle,
            },
        );
        Ok(MockDmaBuffer {
            id,
            handle: dma_handle,
            state: self.state.clone(),
        })
    }
}

impl IrqNotificationContract for MockBackend {
    type IrqNotif = MockIrqNotif;

    fn subscribe(
        &mut self,
        handle: &Self::Handle,
        vector_hint: Option<u8>,
    ) -> Result<Self::IrqNotif, DriverRuntimeError> {
        if !handle.is_live() {
            return Err(DriverRuntimeError::from(DeviceHostError::NotClaimed));
        }
        let mut st = self.state.borrow_mut();
        if st.inject_irq_unavailable {
            st.inject_irq_unavailable = false;
            return Err(DriverRuntimeError::from(DeviceHostError::IrqUnavailable));
        }
        let id = st.next_irq_id;
        st.next_irq_id = st
            .next_irq_id
            .checked_add(1)
            .ok_or(DriverRuntimeError::from(DeviceHostError::Internal))?;
        st.irqs.insert(
            id,
            IrqState {
                key: handle.key,
                vector_hint,
                pending: VecDeque::new(),
                ack_count: 0,
                released: false,
            },
        );
        Ok(MockIrqNotif {
            id,
            state: self.state.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Introspection helpers for the contract suite
// ---------------------------------------------------------------------------

impl MockBackend {
    /// Device key behind an MMIO window, for assertions that a window
    /// was issued from a specific claim.
    pub fn mmio_device(&self, window: &MockMmioWindow) -> Option<DeviceCapKey> {
        self.state.borrow().mmio_windows.get(&window.id).map(|w| w.key)
    }

    /// BAR index a window was mapped for, for assertions that the value
    /// round-tripped through `map`.
    pub fn mmio_bar(&self, window: &MockMmioWindow) -> Option<u8> {
        self.state
            .borrow()
            .mmio_windows
            .get(&window.id)
            .map(|w| w.bar_index)
    }

    /// Device key behind a DMA buffer (live or released), so tests can
    /// assert cross-wiring with the originating claim.
    pub fn dma_device(&self, buffer: &MockDmaBuffer) -> Option<DeviceCapKey> {
        self.state.borrow().dma_buffers.get(&buffer.id).map(|s| s.key)
    }

    /// Device key behind an IRQ subscription.
    pub fn irq_device(&self, notif: &MockIrqNotif) -> Option<DeviceCapKey> {
        self.state.borrow().irqs.get(&notif.id).map(|s| s.key)
    }

    /// Vector hint an IRQ subscription recorded at `subscribe` time.
    pub fn irq_vector_hint(&self, notif: &MockIrqNotif) -> Option<Option<u8>> {
        self.state.borrow().irqs.get(&notif.id).map(|s| s.vector_hint)
    }
}
