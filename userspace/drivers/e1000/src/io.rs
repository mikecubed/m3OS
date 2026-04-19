//! E1000 RX / TX hot paths and link-state handling — Phase 55b Track E.3.
//!
//! RED commit: declares the public surface and the tests that pin its
//! behavior, but leaves every body as a no-op / sentinel return so the
//! test suite fails until the GREEN commit lands the real
//! implementation. Every symbol below is either (a) a fail-closed
//! stub or (b) an `AtomicBool` whose semantics the production
//! implementation will preserve. No `.unwrap()` / `panic!()` /
//! `todo!()` in the stubs — the task discipline forbids those in
//! non-test code at every intermediate commit, including RED.

#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(clippy::needless_pass_by_ref_mut)]

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;

use kernel_core::driver_ipc::net::NetDriverError;
use kernel_core::e1000::{E1000RxDesc, E1000TxDesc};

use crate::init::MmioOps;

// ---------------------------------------------------------------------------
// IrqOutcome
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IrqOutcome {
    pub icr: u32,
    pub link_up: bool,
    pub link_up_edge: bool,
    pub rx_drain_needed: bool,
}

/// RED stub — returns a zeroed outcome that does not reflect inputs.
pub fn compute_irq_outcome(_icr: u32, _status: u32, _prev_link_up: bool) -> IrqOutcome {
    IrqOutcome {
        icr: 0,
        link_up: false,
        link_up_edge: false,
        rx_drain_needed: false,
    }
}

/// RED stub — does not read MMIO, does not update the atomic.
pub fn handle_irq<M: MmioOps>(_mmio: &M, _link_up: &AtomicBool) -> IrqOutcome {
    compute_irq_outcome(0, 0, false)
}

// ---------------------------------------------------------------------------
// RX drain
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrainOutcome {
    pub frames: Vec<Vec<u8>>,
    pub advance_rdt_to: Option<u32>,
    pub new_next_to_read: usize,
}

/// RED stub — returns an empty drain outcome regardless of ring state.
pub fn drain_rx_descriptors(
    _descs: &mut [E1000RxDesc],
    _bufs: &[&[u8]],
    _buf_iova: &[u64],
    _next_to_read: usize,
) -> DrainOutcome {
    DrainOutcome {
        frames: Vec::new(),
        advance_rdt_to: None,
        new_next_to_read: 0,
    }
}

/// RED stub — does not drain, does not write `RDT`, does not invoke
/// the publisher.
pub fn drain_rx<M: MmioOps, P: FnMut(&[u8])>(
    _mmio: &M,
    _descs: &mut [E1000RxDesc],
    _bufs: &[&[u8]],
    _buf_iova: &[u64],
    _next_to_read: &mut usize,
    _publisher: P,
) -> usize {
    0
}

// ---------------------------------------------------------------------------
// TX helpers
// ---------------------------------------------------------------------------

/// RED stub — always reports slot as in-flight.
pub fn tx_slot_free(_desc: &E1000TxDesc) -> bool {
    false
}

/// RED stub — returns `InvalidFrame` unconditionally.
pub fn post_tx_descriptor(
    _desc: &mut E1000TxDesc,
    _buf: &mut [u8],
    _buf_iova: u64,
    _frame: &[u8],
) -> Result<(), NetDriverError> {
    Err(NetDriverError::InvalidFrame)
}

/// RED stub — returns 0 drained, does not clear the ring.
pub fn drain_tx_in_flight(_descs: &mut [E1000TxDesc]) -> usize {
    0
}

/// RED stub — always returns `DriverRestarting` so tests that expect
/// happy-path success fail, and negative tests happen to return the
/// wrong discriminant (`LinkDown` / `RingFull` expected).
pub fn handle_tx<M: MmioOps>(
    _mmio: &M,
    _descs: &mut [E1000TxDesc],
    _bufs: &mut [&mut [u8]],
    _buf_iova: &[u64],
    _next_to_write: &mut usize,
    _link_up: &AtomicBool,
    _driver_restarting: &AtomicBool,
    _frame: &[u8],
) -> Result<(), NetDriverError> {
    Err(NetDriverError::DriverRestarting)
}

// ---------------------------------------------------------------------------
// Module-scoped atomics
// ---------------------------------------------------------------------------

static LINK_UP: AtomicBool = AtomicBool::new(false);
static DRIVER_RESTARTING: AtomicBool = AtomicBool::new(false);

/// Read access to the module-scoped link atomic.
#[inline]
pub fn link_state_atomic() -> &'static AtomicBool {
    &LINK_UP
}

/// Read access to the module-scoped driver-restart atomic.
#[inline]
pub fn driver_restarting_atomic() -> &'static AtomicBool {
    &DRIVER_RESTARTING
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::cell::RefCell;
    use core::sync::atomic::Ordering;

    use kernel_core::e1000::{
        E1000Regs, irq_cause, rx_status, status as e_status, tx_cmd,
    };

    use crate::rings::{RX_BUF_SIZE, RX_RING_SIZE, TX_BUF_SIZE, TX_RING_SIZE};

    // -- FakeMmio ---------------------------------------------------------

    struct FakeMmio {
        reads: RefCell<Vec<(usize, u32)>>,
        writes: RefCell<Vec<(usize, u32)>>,
    }

    impl FakeMmio {
        fn new() -> Self {
            Self {
                reads: RefCell::new(Vec::new()),
                writes: RefCell::new(Vec::new()),
            }
        }
        fn set(&self, off: usize, val: u32) {
            let mut r = self.reads.borrow_mut();
            if let Some(slot) = r.iter_mut().find(|(o, _)| *o == off) {
                slot.1 = val;
            } else {
                r.push((off, val));
            }
        }
        fn writes(&self) -> Vec<(usize, u32)> {
            self.writes.borrow().clone()
        }
    }

    impl MmioOps for FakeMmio {
        fn read_u32(&self, offset: usize) -> u32 {
            self.reads
                .borrow()
                .iter()
                .find(|(o, _)| *o == offset)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write_u32(&self, offset: usize, value: u32) {
            self.writes.borrow_mut().push((offset, value));
        }
    }

    // ---------------------------------------------------------------------
    // IRQ outcome / handle_irq
    // ---------------------------------------------------------------------

    #[test]
    fn compute_irq_outcome_no_lsc_preserves_previous_link() {
        let outcome = compute_irq_outcome(irq_cause::RXT0, 0, true);
        assert_eq!(outcome.link_up, true);
        assert!(!outcome.link_up_edge);
        assert!(outcome.rx_drain_needed);

        let outcome = compute_irq_outcome(irq_cause::RXT0, 0, false);
        assert_eq!(outcome.link_up, false);
        assert!(!outcome.link_up_edge);
    }

    #[test]
    fn compute_irq_outcome_lsc_with_lu_flips_link_up() {
        let outcome = compute_irq_outcome(irq_cause::LSC, e_status::LU, false);
        assert!(outcome.link_up);
        assert!(outcome.link_up_edge);
    }

    #[test]
    fn compute_irq_outcome_lsc_without_lu_flips_link_down() {
        let outcome = compute_irq_outcome(irq_cause::LSC, 0, true);
        assert!(!outcome.link_up);
        assert!(!outcome.link_up_edge);
    }

    #[test]
    fn compute_irq_outcome_rx_causes_trigger_drain() {
        assert!(compute_irq_outcome(irq_cause::RXT0, 0, true).rx_drain_needed);
        assert!(compute_irq_outcome(irq_cause::RXDMT0, 0, true).rx_drain_needed);
        assert!(compute_irq_outcome(irq_cause::RXO, 0, true).rx_drain_needed);
        assert!(!compute_irq_outcome(irq_cause::LSC, 0, true).rx_drain_needed);
    }

    #[test]
    fn handle_irq_reads_icr_and_status_and_stores_link_up() {
        let mmio = FakeMmio::new();
        mmio.set(E1000Regs::ICR, irq_cause::LSC | irq_cause::RXT0);
        mmio.set(E1000Regs::STATUS, e_status::LU);
        let link = AtomicBool::new(false);
        let outcome = handle_irq(&mmio, &link);
        assert!(outcome.link_up);
        assert!(outcome.link_up_edge);
        assert!(outcome.rx_drain_needed);
        assert!(link.load(Ordering::Acquire));
    }

    #[test]
    fn handle_irq_no_lsc_preserves_link_atomic() {
        let mmio = FakeMmio::new();
        mmio.set(E1000Regs::ICR, irq_cause::RXT0);
        mmio.set(E1000Regs::STATUS, e_status::LU);
        let link = AtomicBool::new(true);
        let outcome = handle_irq(&mmio, &link);
        assert!(outcome.link_up);
        assert!(link.load(Ordering::Acquire));

        let link_down = AtomicBool::new(false);
        handle_irq(&mmio, &link_down);
        assert!(!link_down.load(Ordering::Acquire));
    }

    #[test]
    fn handle_irq_lsc_link_down_clears_atomic() {
        let mmio = FakeMmio::new();
        mmio.set(E1000Regs::ICR, irq_cause::LSC);
        mmio.set(E1000Regs::STATUS, 0);
        let link = AtomicBool::new(true);
        handle_irq(&mmio, &link);
        assert!(!link.load(Ordering::Acquire));
    }

    // ---------------------------------------------------------------------
    // RX drain
    // ---------------------------------------------------------------------

    fn mk_rx_setup() -> (
        Vec<E1000RxDesc>,
        Vec<Vec<u8>>,
        Vec<u64>,
    ) {
        let descs = vec![E1000RxDesc::default(); RX_RING_SIZE];
        let bufs: Vec<Vec<u8>> = (0..RX_RING_SIZE).map(|_| vec![0u8; RX_BUF_SIZE]).collect();
        let buf_iova: Vec<u64> = (0..RX_RING_SIZE)
            .map(|i| 0x1000_0000_u64 + (i as u64) * RX_BUF_SIZE as u64)
            .collect();
        (descs, bufs, buf_iova)
    }

    fn borrow_bufs(bufs: &Vec<Vec<u8>>) -> Vec<&[u8]> {
        bufs.iter().map(|v| v.as_slice()).collect()
    }

    #[test]
    fn drain_rx_descriptors_empty_ring_returns_no_frames() {
        let (mut descs, bufs, buf_iova) = mk_rx_setup();
        let slices = borrow_bufs(&bufs);
        let outcome = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert!(outcome.frames.is_empty());
        assert_eq!(outcome.advance_rdt_to, None);
        assert_eq!(outcome.new_next_to_read, 0);
    }

    #[test]
    fn drain_rx_descriptors_one_frame_with_dd_eop_set() {
        let (mut descs, mut bufs, buf_iova) = mk_rx_setup();
        bufs[0][..5].copy_from_slice(b"hello");
        descs[0].length = 5;
        descs[0].status = rx_status::DD | rx_status::EOP;
        let slices = borrow_bufs(&bufs);
        let outcome = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert_eq!(outcome.frames.len(), 1);
        assert_eq!(&outcome.frames[0][..], b"hello");
        assert_eq!(outcome.advance_rdt_to, Some(0));
        assert_eq!(outcome.new_next_to_read, 1);
        assert_eq!(descs[0].status, 0);
        assert_eq!(descs[0].length, 0);
        assert_eq!(descs[0].buffer_addr, buf_iova[0]);
    }

    #[test]
    fn drain_rx_descriptors_stops_at_first_undelivered_slot() {
        let (mut descs, mut bufs, buf_iova) = mk_rx_setup();
        for i in 0..3 {
            bufs[i][..4].copy_from_slice(b"FRAM");
            descs[i].length = 4;
            descs[i].status = rx_status::DD | rx_status::EOP;
        }
        let slices = borrow_bufs(&bufs);
        let outcome = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert_eq!(outcome.frames.len(), 3);
        assert_eq!(outcome.advance_rdt_to, Some(2));
        assert_eq!(outcome.new_next_to_read, 3);
    }

    #[test]
    fn drain_rx_descriptors_wraps_ring_index_modulo_size() {
        let (mut descs, mut bufs, buf_iova) = mk_rx_setup();
        let start = RX_RING_SIZE - 2;
        for offset in 0..3 {
            let i = (start + offset) % RX_RING_SIZE;
            bufs[i][..2].copy_from_slice(b"OK");
            descs[i].length = 2;
            descs[i].status = rx_status::DD | rx_status::EOP;
        }
        let slices = borrow_bufs(&bufs);
        let outcome = drain_rx_descriptors(&mut descs, &slices, &buf_iova, start);
        assert_eq!(outcome.frames.len(), 3);
        assert_eq!(outcome.new_next_to_read, 1);
        assert_eq!(outcome.advance_rdt_to, Some(0));
    }

    #[test]
    fn drain_rx_descriptors_skips_non_eop_but_still_recycles() {
        let (mut descs, mut bufs, buf_iova) = mk_rx_setup();
        bufs[0][..4].copy_from_slice(b"PART");
        descs[0].length = 4;
        descs[0].status = rx_status::DD;
        let slices = borrow_bufs(&bufs);
        let outcome = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert!(outcome.frames.is_empty());
        assert_eq!(descs[0].status, 0);
        assert_eq!(outcome.advance_rdt_to, Some(0));
    }

    #[test]
    fn drain_rx_descriptors_clamps_length_to_rx_buf_size() {
        let (mut descs, mut bufs, buf_iova) = mk_rx_setup();
        bufs[0][0..RX_BUF_SIZE].fill(0xAB);
        descs[0].length = (RX_BUF_SIZE as u16).saturating_add(500);
        descs[0].status = rx_status::DD | rx_status::EOP;
        let slices = borrow_bufs(&bufs);
        let outcome = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert_eq!(outcome.frames.len(), 1);
        assert_eq!(outcome.frames[0].len(), RX_BUF_SIZE);
    }

    #[test]
    fn drain_rx_writes_rdt_when_slots_completed() {
        let (mut descs, mut bufs, buf_iova) = mk_rx_setup();
        bufs[0][..3].copy_from_slice(b"RDT");
        descs[0].length = 3;
        descs[0].status = rx_status::DD | rx_status::EOP;
        let slices = borrow_bufs(&bufs);
        let mmio = FakeMmio::new();
        let mut next_to_read = 0;
        let mut seen: Vec<Vec<u8>> = Vec::new();
        let count = drain_rx(
            &mmio,
            &mut descs,
            &slices,
            &buf_iova,
            &mut next_to_read,
            |f| seen.push(f.to_vec()),
        );
        assert_eq!(count, 1);
        assert_eq!(seen.len(), 1);
        assert_eq!(&seen[0][..], b"RDT");
        assert_eq!(next_to_read, 1);
        let writes = mmio.writes();
        let rdt = writes.iter().find(|(o, _)| *o == E1000Regs::RDT);
        assert_eq!(rdt, Some(&(E1000Regs::RDT, 0)));
    }

    #[test]
    fn drain_rx_skips_rdt_write_when_ring_empty() {
        let (mut descs, bufs, buf_iova) = mk_rx_setup();
        let slices = borrow_bufs(&bufs);
        let mmio = FakeMmio::new();
        let mut next_to_read = 0;
        let count = drain_rx(
            &mmio,
            &mut descs,
            &slices,
            &buf_iova,
            &mut next_to_read,
            |_| {},
        );
        assert_eq!(count, 0);
        assert!(mmio
            .writes()
            .iter()
            .all(|(o, _)| *o != E1000Regs::RDT));
    }

    // ---------------------------------------------------------------------
    // TX post / slot-free
    // ---------------------------------------------------------------------

    #[test]
    fn tx_slot_free_fresh_descriptor_is_free() {
        let desc = E1000TxDesc::default();
        assert!(tx_slot_free(&desc));
    }

    #[test]
    fn tx_slot_free_hardware_completed_is_free() {
        let mut desc = E1000TxDesc::default();
        desc.cmd = tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS;
        desc.status = 0x01;
        assert!(tx_slot_free(&desc));
    }

    #[test]
    fn tx_slot_free_in_flight_is_not_free() {
        let mut desc = E1000TxDesc::default();
        desc.cmd = tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS;
        desc.status = 0;
        assert!(!tx_slot_free(&desc));
    }

    #[test]
    fn post_tx_descriptor_fills_every_field_required_by_spec() {
        let mut desc = E1000TxDesc::default();
        let mut buf = vec![0u8; TX_BUF_SIZE];
        let frame = b"TESTFRAME";
        let iova = 0x0000_ABCD_ABCD_0000u64;
        post_tx_descriptor(&mut desc, &mut buf, iova, frame).expect("valid frame");
        assert_eq!(desc.buffer_addr, iova);
        assert_eq!(desc.length as usize, frame.len());
        assert_eq!(desc.cmd, tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS);
        assert_eq!(desc.status, 0);
        assert_eq!(&buf[..frame.len()], frame);
    }

    #[test]
    fn post_tx_descriptor_rejects_empty_frame() {
        let mut desc = E1000TxDesc::default();
        let mut buf = vec![0u8; TX_BUF_SIZE];
        let err = post_tx_descriptor(&mut desc, &mut buf, 0, &[]).unwrap_err();
        assert_eq!(err, NetDriverError::InvalidFrame);
        assert_eq!(desc.cmd, 0);
    }

    #[test]
    fn post_tx_descriptor_rejects_oversize_frame() {
        let mut desc = E1000TxDesc::default();
        let mut buf = vec![0u8; TX_BUF_SIZE];
        let frame = vec![0u8; TX_BUF_SIZE + 1];
        let err = post_tx_descriptor(&mut desc, &mut buf, 0, &frame).unwrap_err();
        assert_eq!(err, NetDriverError::InvalidFrame);
    }

    // ---------------------------------------------------------------------
    // handle_tx — link / restart / ring-full / success
    // ---------------------------------------------------------------------

    fn mk_tx_setup() -> (Vec<E1000TxDesc>, Vec<Vec<u8>>, Vec<u64>) {
        let descs = vec![E1000TxDesc::default(); TX_RING_SIZE];
        let bufs: Vec<Vec<u8>> = (0..TX_RING_SIZE).map(|_| vec![0u8; TX_BUF_SIZE]).collect();
        let buf_iova: Vec<u64> = (0..TX_RING_SIZE)
            .map(|i| 0x2000_0000_u64 + (i as u64) * TX_BUF_SIZE as u64)
            .collect();
        (descs, bufs, buf_iova)
    }

    fn borrow_tx_bufs_mut(bufs: &mut Vec<Vec<u8>>) -> Vec<&mut [u8]> {
        bufs.iter_mut().map(|v| v.as_mut_slice()).collect()
    }

    #[test]
    fn handle_tx_link_down_returns_link_down_error() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx_setup();
        let mut mut_bufs = borrow_tx_bufs_mut(&mut bufs);
        let link = AtomicBool::new(false);
        let restarting = AtomicBool::new(false);
        let mut next = 0usize;
        let err = handle_tx(
            &mmio,
            &mut descs,
            &mut mut_bufs,
            &buf_iova,
            &mut next,
            &link,
            &restarting,
            b"hello",
        )
        .unwrap_err();
        assert_eq!(err, NetDriverError::LinkDown);
        assert_eq!(descs[0].cmd, 0);
        assert!(mmio.writes().iter().all(|(o, _)| *o != E1000Regs::TDT));
        assert_eq!(next, 0);
    }

    #[test]
    fn handle_tx_driver_restarting_shadows_link_down() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx_setup();
        let mut mut_bufs = borrow_tx_bufs_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restarting = AtomicBool::new(true);
        let mut next = 0usize;
        let err = handle_tx(
            &mmio,
            &mut descs,
            &mut mut_bufs,
            &buf_iova,
            &mut next,
            &link,
            &restarting,
            b"hi",
        )
        .unwrap_err();
        assert_eq!(err, NetDriverError::DriverRestarting);
    }

    #[test]
    fn handle_tx_link_up_happy_path_posts_and_rings_tdt() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx_setup();
        let mut mut_bufs = borrow_tx_bufs_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restarting = AtomicBool::new(false);
        let mut next = 0usize;
        let frame = b"PING";
        handle_tx(
            &mmio,
            &mut descs,
            &mut mut_bufs,
            &buf_iova,
            &mut next,
            &link,
            &restarting,
            frame,
        )
        .expect("send must succeed");
        assert_eq!(descs[0].cmd, tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS);
        assert_eq!(descs[0].length as usize, frame.len());
        assert_eq!(descs[0].buffer_addr, buf_iova[0]);
        assert_eq!(&bufs[0][..frame.len()], frame);
        assert_eq!(next, 1);
        let writes = mmio.writes();
        assert!(writes.iter().any(|&(o, v)| o == E1000Regs::TDT && v == 1));
    }

    #[test]
    fn handle_tx_returns_ring_full_when_slot_still_in_flight() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx_setup();
        descs[0].cmd = tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS;
        descs[0].status = 0;
        let mut mut_bufs = borrow_tx_bufs_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restarting = AtomicBool::new(false);
        let mut next = 0usize;
        let err = handle_tx(
            &mmio,
            &mut descs,
            &mut mut_bufs,
            &buf_iova,
            &mut next,
            &link,
            &restarting,
            b"pkt",
        )
        .unwrap_err();
        assert_eq!(err, NetDriverError::RingFull);
        assert_eq!(next, 0);
    }

    #[test]
    fn handle_tx_reuses_slot_after_hardware_completion() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx_setup();
        descs[0].cmd = tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS;
        descs[0].status = 0x01;
        let mut mut_bufs = borrow_tx_bufs_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restarting = AtomicBool::new(false);
        let mut next = 0usize;
        handle_tx(
            &mmio,
            &mut descs,
            &mut mut_bufs,
            &buf_iova,
            &mut next,
            &link,
            &restarting,
            b"X",
        )
        .expect("reuse after DD");
        assert_eq!(descs[0].status, 0);
    }

    #[test]
    fn handle_tx_next_to_write_wraps_modulo_ring_size() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx_setup();
        let mut mut_bufs = borrow_tx_bufs_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restarting = AtomicBool::new(false);
        let mut next = TX_RING_SIZE - 1;
        handle_tx(
            &mmio,
            &mut descs,
            &mut mut_bufs,
            &buf_iova,
            &mut next,
            &link,
            &restarting,
            b"Z",
        )
        .expect("wrap");
        assert_eq!(next, 0);
        let writes = mmio.writes();
        assert!(writes.iter().any(|&(o, v)| o == E1000Regs::TDT && v == 0));
    }

    // ---------------------------------------------------------------------
    // Link-up wrap-around drain
    // ---------------------------------------------------------------------

    #[test]
    fn drain_tx_in_flight_clears_every_slot() {
        let mut descs = vec![E1000TxDesc::default(); TX_RING_SIZE];
        for d in descs.iter_mut() {
            d.cmd = tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS;
            d.status = 0;
            d.length = 1024;
        }
        let drained = drain_tx_in_flight(&mut descs);
        assert_eq!(drained, TX_RING_SIZE);
        for d in &descs {
            assert_eq!(d.cmd, 0);
            assert_eq!(d.status, 0);
            assert_eq!(d.length, 0);
        }
    }

    // ---------------------------------------------------------------------
    // Module-scoped atomics
    // ---------------------------------------------------------------------

    #[test]
    fn link_state_atomic_is_module_scoped_atomic_bool() {
        let a = link_state_atomic();
        let b = link_state_atomic();
        assert!(core::ptr::eq(a, b));
    }

    #[test]
    fn driver_restarting_atomic_is_module_scoped() {
        let a = driver_restarting_atomic();
        let b = driver_restarting_atomic();
        assert!(core::ptr::eq(a, b));
    }
}
