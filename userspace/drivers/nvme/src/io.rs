//! NVMe I/O queue pair, IRQ handling, and block I/O path — Phase 55b Track D.3.
//!
//! **Red commit:** this file declares the public surface and the tests
//! that pin the NVMe-spec behaviors the Track D.3 acceptance bullets
//! require. The function bodies are deliberate placeholders — the
//! Green commit ports the Phase 55 D.3 implementation over from
//! `kernel/src/blk/nvme.rs` onto the `driver_runtime` HAL.
//!
//! The three surfaces under test are:
//!
//! - [`build_prp_pair`] — single-page, two-page, multi-page
//!   (PRP-list) construction plus size / error handling.
//! - [`drain_step`] / [`advance_cq_cursor`] — completion phase-bit
//!   walk with wrap-around and phase flip.
//! - [`build_read_command`] / [`build_write_command`] — Read / Write
//!   encoding per NVMe §5.11 / §5.15.
//! - [`build_create_io_cq_command`] / [`build_create_io_sq_command`] —
//!   admin Create I/O Queue commands run before I/O is accepted.
//! - [`IoQueueBookkeeping`] — the state machine the production
//!   `IoQueuePair` drives (sq tail, cq head, phase bit, in-flight
//!   slots).

use alloc::vec::Vec;

use kernel_core::nvme as knvme;

use crate::init::NVME_PAGE_BYTES;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// I/O queue depth (Phase 55b D.3 acceptance).
pub const IO_QUEUE_DEPTH: usize = 64;

/// I/O queue identifier. Admin is qid 0; Phase 55b drives a single
/// data queue (qid 1).
pub const IO_QUEUE_ID: u16 = 1;

/// Number of `u64` PRP-list entries per 4 KiB PRP-list page.
pub const PRP_LIST_ENTRIES: usize = NVME_PAGE_BYTES / core::mem::size_of::<u64>();

// ---------------------------------------------------------------------------
// PRP construction
// ---------------------------------------------------------------------------

/// Reason [`build_prp_pair`] could not construct a PRP tuple.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrpBuildError {
    /// Zero-length transfer requested.
    ZeroLength,
    /// PRP-list slice too short for `pages - 1` entries.
    PrpListTooSmall {
        /// Slots provided.
        have: usize,
        /// Slots required.
        need: usize,
    },
}

/// Build the `(PRP1, PRP2)` pair for an NVMe Read / Write command.
///
/// Stub — returns [`PrpBuildError::ZeroLength`] unconditionally so the
/// Red commit's tests fail until the Green commit lands the NVMe §4.3
/// implementation.
pub fn build_prp_pair(
    _buffer_iova: u64,
    _byte_len: usize,
    _prp_list_iova: u64,
    _prp_list: &mut [u64],
) -> Result<(u64, u64), PrpBuildError> {
    Err(PrpBuildError::ZeroLength)
}

// ---------------------------------------------------------------------------
// Read / Write command encoders
// ---------------------------------------------------------------------------

/// Encode an NVMe I/O Read command. Stub returns a zeroed command.
pub fn build_read_command(
    _nsid: u32,
    _cid: u16,
    _lba: u64,
    _sector_count: u32,
    _prp1: u64,
    _prp2: u64,
) -> knvme::NvmeCommand {
    knvme::NvmeCommand::new(0, 0)
}

/// Encode an NVMe I/O Write command. Stub returns a zeroed command.
pub fn build_write_command(
    _nsid: u32,
    _cid: u16,
    _lba: u64,
    _sector_count: u32,
    _prp1: u64,
    _prp2: u64,
) -> knvme::NvmeCommand {
    knvme::NvmeCommand::new(0, 0)
}

// ---------------------------------------------------------------------------
// Create I/O CQ / SQ admin command encoders
// ---------------------------------------------------------------------------

/// Build the Create I/O Completion Queue admin command. Stub returns
/// a zeroed command.
pub fn build_create_io_cq_command(
    _cid: u16,
    _qid: u16,
    _entries: u16,
    _cq_iova: u64,
    _vector: u16,
) -> knvme::NvmeCommand {
    knvme::NvmeCommand::new(0, 0)
}

/// Build the Create I/O Submission Queue admin command. Stub returns
/// a zeroed command.
pub fn build_create_io_sq_command(
    _cid: u16,
    _qid: u16,
    _entries: u16,
    _sq_iova: u64,
    _cq_id: u16,
) -> knvme::NvmeCommand {
    knvme::NvmeCommand::new(0, 0)
}

// ---------------------------------------------------------------------------
// Completion drain
// ---------------------------------------------------------------------------

/// Outcome of inspecting one CQ slot via [`drain_step`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DrainOutcome {
    /// No new completion at this slot.
    Empty,
    /// A completion was observed.
    Consumed {
        cid: u16,
        status_code: u16,
        result: u32,
    },
}

/// Inspect the CQ slot at `cq_head` and decide whether it carries a
/// new completion. Stub always returns [`DrainOutcome::Empty`].
pub fn drain_step(_cq: &[knvme::NvmeCompletion], _cq_head: u16, _phase: bool) -> DrainOutcome {
    DrainOutcome::Empty
}

/// Advance `(cq_head, phase)` by one slot. Stub returns the cursor
/// unchanged so the Red commit's wrap-around assertions fail.
pub fn advance_cq_cursor(cq_head: u16, phase: bool, _entries: u16) -> (u16, bool) {
    (cq_head, phase)
}

// ---------------------------------------------------------------------------
// InFlight slot + queue bookkeeping
// ---------------------------------------------------------------------------

/// One in-flight I/O command.
#[derive(Clone, Copy, Debug, Default)]
pub struct InFlightSlot {
    pub filled: bool,
    pub status_code: u16,
    pub result: u32,
}

/// Pure-logic tracker for the I/O queue pair. Red commit exposes the
/// fields the Green commit will drive; every method is a stub.
#[derive(Clone, Debug)]
pub struct IoQueueBookkeeping {
    entries: u16,
    sq_tail: u16,
    cq_head: u16,
    phase: bool,
    next_cid: u16,
    slots: Vec<InFlightSlot>,
}

impl IoQueueBookkeeping {
    /// Construct — stub leaves the ring unsized so acceptance tests fail.
    pub fn new(_entries: u16) -> Self {
        Self {
            entries: 0,
            sq_tail: 0,
            cq_head: 0,
            phase: false,
            next_cid: 0,
            slots: Vec::new(),
        }
    }
    pub fn entries(&self) -> u16 {
        self.entries
    }
    pub fn sq_tail(&self) -> u16 {
        self.sq_tail
    }
    pub fn cq_head(&self) -> u16 {
        self.cq_head
    }
    pub fn phase(&self) -> bool {
        self.phase
    }
    pub fn allocate_slot(&mut self) -> (u16, u16) {
        (0, 0)
    }
    pub fn record_completion(&mut self, _cid: u16, _status_code: u16, _result: u32) {}
    pub fn advance_cq(&mut self) {}
    pub fn slot(&self, _cid: u16) -> Option<InFlightSlot> {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // ---- PRP construction ----------------------------------------

    #[test]
    fn build_prp_single_page_uses_only_prp1() {
        let mut list = [0u64; PRP_LIST_ENTRIES];
        let (p1, p2) = build_prp_pair(0x1000, 512, 0xDEAD, &mut list).expect("single page");
        assert_eq!(p1, 0x1000);
        assert_eq!(p2, 0);
        assert!(list.iter().all(|&e| e == 0));
    }

    #[test]
    fn build_prp_exactly_one_page_uses_only_prp1() {
        let mut list = [0u64; PRP_LIST_ENTRIES];
        let (p1, p2) =
            build_prp_pair(0x2000, NVME_PAGE_BYTES, 0xBEEF, &mut list).expect("one page");
        assert_eq!(p1, 0x2000);
        assert_eq!(p2, 0);
    }

    #[test]
    fn build_prp_two_pages_uses_inline_prp2() {
        let mut list = [0u64; PRP_LIST_ENTRIES];
        let (p1, p2) =
            build_prp_pair(0x3000, NVME_PAGE_BYTES + 1, 0xFEED, &mut list).expect("two pages");
        assert_eq!(p1, 0x3000);
        assert_eq!(p2, 0x3000 + NVME_PAGE_BYTES as u64);
    }

    #[test]
    fn build_prp_exactly_two_pages_uses_inline_prp2() {
        let mut list = [0u64; PRP_LIST_ENTRIES];
        let (p1, p2) = build_prp_pair(0x4000, 2 * NVME_PAGE_BYTES, 0xC0FE, &mut list)
            .expect("exactly two pages");
        assert_eq!(p1, 0x4000);
        assert_eq!(p2, 0x4000 + NVME_PAGE_BYTES as u64);
    }

    #[test]
    fn build_prp_three_pages_populates_list_with_remaining_ivoas() {
        let mut list = [0u64; PRP_LIST_ENTRIES];
        let base = 0x10_0000u64;
        let (p1, p2) = build_prp_pair(base, 2 * NVME_PAGE_BYTES + 1, 0xABCD_0000, &mut list)
            .expect("three pages");
        assert_eq!(p1, base);
        assert_eq!(p2, 0xABCD_0000);
        assert_eq!(list[0], base + NVME_PAGE_BYTES as u64);
        assert_eq!(list[1], base + 2 * NVME_PAGE_BYTES as u64);
        for (i, &entry) in list.iter().enumerate().skip(2) {
            assert_eq!(entry, 0, "list[{i}] should be untouched");
        }
    }

    #[test]
    fn build_prp_many_pages_populates_list_fully() {
        let mut list = [0u64; PRP_LIST_ENTRIES];
        let base = 0x2000_0000u64;
        let (p1, p2) =
            build_prp_pair(base, 5 * NVME_PAGE_BYTES, 0x1234_0000, &mut list).expect("five pages");
        assert_eq!(p1, base);
        assert_eq!(p2, 0x1234_0000);
        for i in 0..4 {
            assert_eq!(list[i], base + ((i as u64) + 1) * NVME_PAGE_BYTES as u64);
        }
    }

    #[test]
    fn build_prp_zero_length_returns_error() {
        let mut list = [0u64; PRP_LIST_ENTRIES];
        let err = build_prp_pair(0x1000, 0, 0, &mut list).expect_err("zero-length must fail");
        assert_eq!(err, PrpBuildError::ZeroLength);
    }

    #[test]
    fn build_prp_list_too_small_returns_sized_error() {
        let mut list = [0u64; 1];
        let err = build_prp_pair(0x1000, 4 * NVME_PAGE_BYTES, 0x2000, &mut list)
            .expect_err("list too small");
        assert_eq!(err, PrpBuildError::PrpListTooSmall { have: 1, need: 3 });
    }

    // ---- Read / Write command encoders ---------------------------

    #[test]
    fn build_read_command_pins_opcode_nsid_prp_and_lba_fields() {
        let cmd = build_read_command(1, 7, 0x1234_5678_9abc_def0, 4, 0xdead, 0xbeef);
        assert_eq!(cmd.opcode(), knvme::OP_IO_READ);
        assert_eq!(cmd.cid(), 7);
        assert_eq!(cmd.nsid, 1);
        assert_eq!(cmd.prp1, 0xdead);
        assert_eq!(cmd.prp2, 0xbeef);
        assert_eq!(cmd.cdw10, 0x9abc_def0);
        assert_eq!(cmd.cdw11, 0x1234_5678);
        assert_eq!(cmd.cdw12, 3);
    }

    #[test]
    fn build_write_command_pins_opcode_and_zero_based_count() {
        let cmd = build_write_command(2, 9, 0xAA, 1, 0x1, 0x0);
        assert_eq!(cmd.opcode(), knvme::OP_IO_WRITE);
        assert_eq!(cmd.cid(), 9);
        assert_eq!(cmd.nsid, 2);
        assert_eq!(cmd.cdw12, 0);
    }

    #[test]
    fn build_write_command_saturates_zero_count() {
        let cmd = build_write_command(1, 0, 0, 0, 0, 0);
        assert_eq!(cmd.cdw12, 0);
    }

    // ---- Create I/O CQ / SQ encoders -----------------------------

    #[test]
    fn create_io_cq_cmd_encodes_qid_entries_vector_and_flags() {
        let cmd = build_create_io_cq_command(0, 1, 64, 0xCAFE_0000, 0);
        assert_eq!(cmd.opcode(), knvme::OP_CREATE_IO_CQ);
        assert_eq!(cmd.prp1, 0xCAFE_0000);
        assert_eq!(cmd.cdw10, (63 << 16) | 1);
        assert_eq!(cmd.cdw11, 0b11);
    }

    #[test]
    fn create_io_cq_cmd_preserves_nonzero_vector() {
        let cmd = build_create_io_cq_command(0, 1, 16, 0, 3);
        assert_eq!(cmd.cdw11, (3u32 << 16) | 0b11);
    }

    #[test]
    fn create_io_sq_cmd_encodes_qid_entries_and_cq_id() {
        let cmd = build_create_io_sq_command(0, 1, 64, 0xBEEF_0000, 1);
        assert_eq!(cmd.opcode(), knvme::OP_CREATE_IO_SQ);
        assert_eq!(cmd.prp1, 0xBEEF_0000);
        assert_eq!(cmd.cdw10, (63 << 16) | 1);
        assert_eq!(cmd.cdw11, (1u32 << 16) | 1u32);
    }

    // ---- Completion drain ----------------------------------------

    fn make_cq_entry(cid: u16, status_phase: u16, result: u32) -> knvme::NvmeCompletion {
        knvme::NvmeCompletion {
            result,
            reserved: 0,
            sq_head: 0,
            sq_id: 0,
            cid,
            status_phase,
        }
    }

    #[test]
    fn drain_step_empty_when_phase_mismatches() {
        let cq = vec![make_cq_entry(0, 0, 0)];
        assert_eq!(drain_step(&cq, 0, true), DrainOutcome::Empty);
    }

    #[test]
    fn drain_step_consumes_when_phase_matches() {
        let cq = vec![make_cq_entry(0xAB, 0x0001, 0x1234_5678)];
        assert_eq!(
            drain_step(&cq, 0, true),
            DrainOutcome::Consumed {
                cid: 0xAB,
                status_code: 0,
                result: 0x1234_5678,
            }
        );
    }

    #[test]
    fn drain_step_reports_non_zero_status_code() {
        let raw = (0x81u16 << 1) | 1;
        let cq = vec![make_cq_entry(0x10, raw, 0)];
        assert_eq!(
            drain_step(&cq, 0, true),
            DrainOutcome::Consumed {
                cid: 0x10,
                status_code: 0x81,
                result: 0,
            }
        );
    }

    #[test]
    fn drain_step_out_of_bounds_returns_empty() {
        let cq: Vec<knvme::NvmeCompletion> = vec![make_cq_entry(0, 1, 0)];
        assert_eq!(drain_step(&cq, 5, true), DrainOutcome::Empty);
    }

    #[test]
    fn advance_cq_cursor_wraps_and_flips_phase() {
        assert_eq!(advance_cq_cursor(0, true, 4), (1, true));
        assert_eq!(advance_cq_cursor(1, true, 4), (2, true));
        assert_eq!(advance_cq_cursor(2, true, 4), (3, true));
        assert_eq!(advance_cq_cursor(3, true, 4), (0, false));
        assert_eq!(advance_cq_cursor(0, false, 4), (1, false));
        assert_eq!(advance_cq_cursor(3, false, 4), (0, true));
    }

    #[test]
    fn advance_cq_cursor_clamps_zero_entries() {
        let (h, p) = advance_cq_cursor(0, true, 0);
        assert_eq!(h, 0);
        assert!(!p);
    }

    // ---- IoQueueBookkeeping ---------------------------------------

    #[test]
    fn bookkeeping_starts_with_empty_ring() {
        let bk = IoQueueBookkeeping::new(IO_QUEUE_DEPTH as u16);
        assert_eq!(bk.sq_tail(), 0);
        assert_eq!(bk.cq_head(), 0);
        assert!(bk.phase());
        assert_eq!(bk.entries(), IO_QUEUE_DEPTH as u16);
    }

    #[test]
    fn bookkeeping_allocate_slot_advances_tail_and_cid() {
        let mut bk = IoQueueBookkeeping::new(4);
        let (cid0, tail0) = bk.allocate_slot();
        assert_eq!(cid0, 0);
        assert_eq!(tail0, 1);
        let (cid1, tail1) = bk.allocate_slot();
        assert_eq!(cid1, 1);
        assert_eq!(tail1, 2);
    }

    #[test]
    fn bookkeeping_record_completion_marks_slot_filled() {
        let mut bk = IoQueueBookkeeping::new(4);
        let (cid, _) = bk.allocate_slot();
        bk.record_completion(cid, 0, 0xAAAA);
        let slot = bk.slot(cid).expect("slot present");
        assert!(slot.filled);
        assert_eq!(slot.status_code, 0);
        assert_eq!(slot.result, 0xAAAA);
    }

    #[test]
    fn bookkeeping_record_completion_ignores_out_of_range_cid() {
        let mut bk = IoQueueBookkeeping::new(4);
        bk.record_completion(99, 0x42, 0);
        for i in 0..4 {
            let s = bk.slot(i).unwrap();
            assert!(!s.filled);
        }
    }

    #[test]
    fn bookkeeping_advance_cq_wraps_and_flips_phase() {
        let mut bk = IoQueueBookkeeping::new(2);
        assert!(bk.phase());
        bk.advance_cq();
        assert_eq!(bk.cq_head(), 1);
        assert!(bk.phase());
        bk.advance_cq();
        assert_eq!(bk.cq_head(), 0);
        assert!(!bk.phase());
    }

    #[test]
    fn bookkeeping_cid_wraps_after_max() {
        let mut bk = IoQueueBookkeeping::new(4);
        let mut last_cid = 0;
        for _ in 0..9 {
            let (cid, _) = bk.allocate_slot();
            last_cid = cid;
        }
        assert_eq!(last_cid, 0);
    }

    // ---- End-to-end via bookkeeping ------------------------------

    #[test]
    fn read_submit_then_drain_surfaces_completion_to_slot() {
        let mut bk = IoQueueBookkeeping::new(4);
        let (cid, _new_tail) = bk.allocate_slot();
        let _cmd = build_read_command(1, cid, 0, 1, 0x1000, 0);
        let cq = vec![make_cq_entry(cid, 0x0001, 0)];
        match drain_step(&cq, bk.cq_head(), bk.phase()) {
            DrainOutcome::Consumed {
                cid: got_cid,
                status_code,
                result,
            } => {
                assert_eq!(got_cid, cid);
                assert_eq!(status_code, 0);
                bk.record_completion(got_cid, status_code, result);
                bk.advance_cq();
            }
            DrainOutcome::Empty => panic!("expected completion"),
        }
        assert!(bk.slot(cid).unwrap().filled);
    }

    #[test]
    fn write_submit_then_drain_surfaces_error_status_to_slot() {
        let mut bk = IoQueueBookkeeping::new(4);
        let (cid, _) = bk.allocate_slot();
        let _cmd = build_write_command(1, cid, 0, 1, 0x1000, 0);
        let raw_status_phase = (0x42u16 << 1) | 1;
        let cq = vec![make_cq_entry(cid, raw_status_phase, 0)];
        match drain_step(&cq, bk.cq_head(), bk.phase()) {
            DrainOutcome::Consumed {
                cid: got_cid,
                status_code,
                result,
            } => {
                bk.record_completion(got_cid, status_code, result);
                bk.advance_cq();
            }
            DrainOutcome::Empty => panic!("expected completion"),
        }
        let slot = bk.slot(cid).unwrap();
        assert_eq!(slot.status_code, 0x42);
        assert!(slot.filled);
    }
}
