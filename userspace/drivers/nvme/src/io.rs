//! NVMe I/O queue pair, IRQ handling, and block I/O path — Phase 55b Track D.3.
//!
//! This module lifts the Phase 55 D.3 ring-0 NVMe I/O hot path
//! (`kernel/src/blk/nvme.rs`) onto the Phase 55b ring-3
//! [`driver_runtime`] HAL. Register semantics, PRP construction, and
//! completion-phase walk stay byte-for-byte equivalent; only the MMIO
//! / DMA / IRQ substrate changes.
//!
//! # Module shape
//!
//! | Layer                                     | Purpose                                                           | Tested where |
//! |-------------------------------------------|-------------------------------------------------------------------|--------------|
//! | Pure helpers (this file, host-testable)   | PRP construction, Read / Write / Create-I/O-CQ / Create-I/O-SQ encoders, phase-bit drain, bookkeeping | `#[cfg(test)]` tests in this file |
//! | [`IoQueuePair`] (non-test, `driver_runtime`-backed) | DMA rings + doorbell MMIO + phase bookkeeping; owned by the driver | In-QEMU smoke (D.3 stretch goal) |
//! | [`handle_read`] / [`handle_write`] / [`drain_completions`] | Glue the block IPC request onto the I/O queue and wait on the IRQ notification | In-QEMU smoke |
//!
//! # PRP construction
//!
//! NVMe §4.3 gives three shapes:
//!
//! - **Single page (byte_len <= 4 096):** `PRP1 = buffer_iova`, `PRP2 = 0`.
//! - **Two pages (byte_len <= 8 192):** `PRP1 = buffer_iova`,
//!   `PRP2 = buffer_iova + 4 096`.
//! - **More than two pages:** `PRP1 = buffer_iova`, `PRP2 = prp_list_iova`;
//!   the PRP-list page holds one `u64` IOVA per subsequent page.
//!
//! # Completion drain
//!
//! Walk the CQ starting at `cq_head`; each slot whose phase bit matches
//! the expected phase is a new completion. Consume it, advance
//! `cq_head` (mod `entries`), flip `phase` on wrap-around, write the
//! CQ-head doorbell once the drain is finished.

use alloc::vec;
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

/// Build the `(PRP1, PRP2)` pair for an NVMe Read / Write command
/// whose data buffer starts at `buffer_iova` and covers `byte_len`
/// bytes.
///
/// `prp_list_iova` is the IOVA of the caller's PRP-list page (only
/// used when `pages > 2`). `prp_list` is the writable slice backing
/// that page — the helper fills entries `0..pages-1` with the IOVAs
/// of pages `1..pages` of the buffer. The caller is responsible for
/// allocating and keeping the PRP-list alive for the duration of the
/// command.
///
/// # Errors
///
/// - [`PrpBuildError::ZeroLength`] — `byte_len == 0`.
/// - [`PrpBuildError::PrpListTooSmall`] — more than two pages of
///   transfer requested but the provided PRP-list slice is shorter
///   than `pages - 1`.
pub fn build_prp_pair(
    buffer_iova: u64,
    byte_len: usize,
    prp_list_iova: u64,
    prp_list: &mut [u64],
) -> Result<(u64, u64), PrpBuildError> {
    if byte_len == 0 {
        return Err(PrpBuildError::ZeroLength);
    }
    let page = NVME_PAGE_BYTES as u64;
    let pages = byte_len.div_ceil(NVME_PAGE_BYTES);
    if pages <= 1 {
        return Ok((buffer_iova, 0));
    }
    if pages == 2 {
        return Ok((buffer_iova, buffer_iova + page));
    }
    let needed = pages - 1;
    if prp_list.len() < needed {
        return Err(PrpBuildError::PrpListTooSmall {
            have: prp_list.len(),
            need: needed,
        });
    }
    for (i, slot) in prp_list.iter_mut().take(needed).enumerate() {
        *slot = buffer_iova + ((i as u64) + 1) * page;
    }
    Ok((buffer_iova, prp_list_iova))
}

// ---------------------------------------------------------------------------
// Read / Write command encoders
// ---------------------------------------------------------------------------

/// Encode an NVMe I/O Read command (opcode 0x02) for the given
/// namespace, LBA, and sector count.
///
/// NVMe §5.11 "Read Command": CDW10/11 carry the starting LBA as
/// little-endian halves, CDW12 bits 15:0 hold `NLB = count - 1`
/// (zero-based).
pub fn build_read_command(
    nsid: u32,
    cid: u16,
    lba: u64,
    sector_count: u32,
    prp1: u64,
    prp2: u64,
) -> knvme::NvmeCommand {
    let mut cmd = knvme::NvmeCommand::new(knvme::OP_IO_READ, cid);
    cmd.nsid = nsid;
    cmd.prp1 = prp1;
    cmd.prp2 = prp2;
    cmd.cdw10 = (lba & 0xFFFF_FFFF) as u32;
    cmd.cdw11 = (lba >> 32) as u32;
    cmd.cdw12 = sector_count.saturating_sub(1) & 0xFFFF;
    cmd
}

/// Encode an NVMe I/O Write command (opcode 0x01) — identical field
/// layout to [`build_read_command`] per NVMe §5.15.
pub fn build_write_command(
    nsid: u32,
    cid: u16,
    lba: u64,
    sector_count: u32,
    prp1: u64,
    prp2: u64,
) -> knvme::NvmeCommand {
    let mut cmd = knvme::NvmeCommand::new(knvme::OP_IO_WRITE, cid);
    cmd.nsid = nsid;
    cmd.prp1 = prp1;
    cmd.prp2 = prp2;
    cmd.cdw10 = (lba & 0xFFFF_FFFF) as u32;
    cmd.cdw11 = (lba >> 32) as u32;
    cmd.cdw12 = sector_count.saturating_sub(1) & 0xFFFF;
    cmd
}

// ---------------------------------------------------------------------------
// Create I/O CQ / SQ admin command encoders
// ---------------------------------------------------------------------------

/// Build the Create I/O Completion Queue admin command (opcode 0x05).
///
/// - CDW10: `((entries - 1) << 16) | qid`
/// - CDW11: `(vector << 16) | IEN (bit 1) | PC (bit 0)`
///
/// `vector` is the MSI / MSI-X vector index the ISR shim maps onto
/// the driver's notification word; Track D.3 always uses vector 0 so
/// the admin and I/O completions share a single `IrqNotification`.
pub fn build_create_io_cq_command(
    cid: u16,
    qid: u16,
    entries: u16,
    cq_iova: u64,
    vector: u16,
) -> knvme::NvmeCommand {
    let mut cmd = knvme::NvmeCommand::new(knvme::OP_CREATE_IO_CQ, cid);
    cmd.prp1 = cq_iova;
    cmd.cdw10 = ((entries.saturating_sub(1) as u32) << 16) | (qid as u32);
    // IEN (bit 1) + PC (bit 0) + vector in bits 31:16.
    cmd.cdw11 = ((vector as u32) << 16) | 0b11;
    cmd
}

/// Build the Create I/O Submission Queue admin command (opcode 0x01).
///
/// - CDW10: `((entries - 1) << 16) | qid`
/// - CDW11: `(cq_id << 16) | QPRIO (14:13) | PC (bit 0)`
///
/// `QPRIO = 0` (urgent / medium) matches every target we care about
/// and the Phase 55 D.3 kernel-side choice.
pub fn build_create_io_sq_command(
    cid: u16,
    qid: u16,
    entries: u16,
    sq_iova: u64,
    cq_id: u16,
) -> knvme::NvmeCommand {
    let mut cmd = knvme::NvmeCommand::new(knvme::OP_CREATE_IO_SQ, cid);
    cmd.prp1 = sq_iova;
    cmd.cdw10 = ((entries.saturating_sub(1) as u32) << 16) | (qid as u32);
    cmd.cdw11 = ((cq_id as u32) << 16) | 1u32; // PC=1, QPRIO=0
    cmd
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
/// new completion. Pure function — callers own the `cq` slice, the
/// `cq_head`, and the expected `phase`.
///
/// The helper does **not** mutate `cq_head` or `phase`; the caller
/// advances them via [`advance_cq_cursor`] after dispatching the
/// result so the book-keeping stays in one place.
pub fn drain_step(cq: &[knvme::NvmeCompletion], cq_head: u16, phase: bool) -> DrainOutcome {
    let idx = cq_head as usize;
    if idx >= cq.len() {
        // Defensive — an out-of-range cq_head is a driver bug; surface
        // as Empty so the drain loop terminates rather than indexing
        // out of bounds.
        return DrainOutcome::Empty;
    }
    let entry = cq[idx];
    if knvme::completion_phase(&entry) != phase {
        return DrainOutcome::Empty;
    }
    DrainOutcome::Consumed {
        cid: entry.cid,
        status_code: knvme::completion_status_code(&entry),
        result: entry.result,
    }
}

/// Advance `(cq_head, phase)` by one slot per NVMe §4.6. Returns the
/// new `(cq_head, phase)`. `entries` is clamped to at least 1 so `%`
/// on a zero denominator is not reachable.
pub fn advance_cq_cursor(cq_head: u16, phase: bool, entries: u16) -> (u16, bool) {
    let denom = entries.max(1);
    let next = (cq_head + 1) % denom;
    let next_phase = if next == 0 { !phase } else { phase };
    (next, next_phase)
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

/// Pure-logic tracker for the I/O queue pair.
///
/// Owns the submission-tail / completion-head cursors, the phase bit,
/// the next-CID counter, and the per-CID in-flight slots. The DMA
/// rings and the MMIO doorbells live in the production
/// [`IoQueuePair`] (non-test builds) — this struct exists so the
/// state transitions stay host-testable.
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
    /// Construct a fresh tracker. `entries` is clamped to at least 2
    /// because NVMe forbids a queue with fewer slots than one for
    /// submission + one for completion.
    pub fn new(entries: u16) -> Self {
        let entries = entries.max(2);
        Self {
            entries,
            sq_tail: 0,
            cq_head: 0,
            phase: true,
            next_cid: 0,
            slots: vec![InFlightSlot::default(); entries as usize],
        }
    }

    /// Number of SQ / CQ entries this queue owns.
    pub fn entries(&self) -> u16 {
        self.entries
    }

    /// Current SQ tail pointer.
    pub fn sq_tail(&self) -> u16 {
        self.sq_tail
    }

    /// Current CQ head pointer.
    pub fn cq_head(&self) -> u16 {
        self.cq_head
    }

    /// Current expected phase bit.
    pub fn phase(&self) -> bool {
        self.phase
    }

    /// Allocate the next CID, reset its slot, advance `sq_tail`.
    /// Returns `(cid, new_sq_tail)`.
    pub fn allocate_slot(&mut self) -> (u16, u16) {
        let cid = self.next_cid % self.entries;
        self.slots[cid as usize] = InFlightSlot::default();
        self.sq_tail = (self.sq_tail + 1) % self.entries;
        self.next_cid = self.next_cid.wrapping_add(1);
        (cid, self.sq_tail)
    }

    /// Record a completion at `cid`'s slot. Out-of-range `cid` is
    /// ignored (defensive — a malformed CQ entry is a device bug).
    pub fn record_completion(&mut self, cid: u16, status_code: u16, result: u32) {
        let idx = cid as usize;
        if idx < self.slots.len() {
            self.slots[idx] = InFlightSlot {
                filled: true,
                status_code,
                result,
            };
        }
    }

    /// Advance `(cq_head, phase)` after draining one entry.
    pub fn advance_cq(&mut self) {
        let (next, next_phase) = advance_cq_cursor(self.cq_head, self.phase, self.entries);
        self.cq_head = next;
        self.phase = next_phase;
    }

    /// Snapshot of the in-flight slot at `cid`.
    pub fn slot(&self, cid: u16) -> Option<InFlightSlot> {
        self.slots.get(cid as usize).copied()
    }
}

// ---------------------------------------------------------------------------
// Driver-facing layer — DMA-backed I/O queue pair + block IPC glue.
// ---------------------------------------------------------------------------
//
// The code below is non-test only because it consumes
// `driver_runtime::{DmaBuffer, Mmio, IrqNotification}` whose concrete
// syscall paths are the ring-3 substrate. Tests for the pure logic
// already cover the PRP math, the drain walk, and the bookkeeping; the
// in-QEMU 512 B LBA-0 round-trip is the Track D.3 stretch goal that
// depends on F.1 / D.4 and lives in the xtask test harness.

#[cfg(not(test))]
mod driver_layer {
    use super::*;
    use alloc::vec;
    use driver_runtime::{DeviceHandle, DmaBuffer, DriverRuntimeError, IrqNotification, Mmio};
    use kernel_core::driver_ipc::block::{BlkReplyHeader, BlkRequestHeader, BlockDriverError};
    use syscall_lib::STDOUT_FILENO;

    use crate::NvmeRegsTag;

    /// Bound on the polled-completion fallback spin. Each iteration
    /// reads the CQ slot and, if empty, yields via `core::hint::spin_loop`.
    /// 8 M iterations matches the bring-up timeout budget and stays well
    /// below the service-manager restart window.
    pub const IO_SPIN_BUDGET: u64 = 8_000_000;

    /// Bound on how many times [`IoQueuePair::drain_completions`] walks
    /// the CQ in one pass. A single pending request produces at most one
    /// new entry; the bound is defensive so a phase-bit glitch cannot
    /// drive the drain into an unbounded loop.
    pub const DRAIN_MAX_PASS: usize = IO_QUEUE_DEPTH;

    /// DMA-backed NVMe I/O queue pair.
    ///
    /// Three const-generic DMA buffers:
    ///
    /// - `sq`: `[NvmeCommand; IO_QUEUE_DEPTH]` — submission queue.
    /// - `cq`: `[NvmeCompletion; IO_QUEUE_DEPTH]` — completion queue.
    /// - `prp_list`: `[u64; PRP_LIST_ENTRIES]` — overflow PRP list page
    ///   (a single page holds up to `PRP_LIST_ENTRIES` entries; larger
    ///   transfers chain via the last slot per NVMe §4.3, a future
    ///   extension).
    ///
    /// Bookkeeping (`sq_tail`, `cq_head`, `phase`, next-CID) lives in
    /// [`IoQueueBookkeeping`] so the state transitions are host-tested.
    pub struct IoQueuePair {
        pub sq: DmaBuffer<[knvme::NvmeCommand; IO_QUEUE_DEPTH]>,
        pub cq: DmaBuffer<[knvme::NvmeCompletion; IO_QUEUE_DEPTH]>,
        pub prp_list: DmaBuffer<[u64; PRP_LIST_ENTRIES]>,
        pub bookkeeping: IoQueueBookkeeping,
        pub doorbell_stride: usize,
        pub irq: Option<IrqNotification>,
    }

    impl IoQueuePair {
        /// Allocate the DMA rings + PRP-list page.
        ///
        /// Byte sizes come from `core::mem::size_of::<[T; N]>()` so the
        /// const-generic allocation matches the NVMe spec layout exactly
        /// (64-byte SQ entries * 64 entries = 4 096 B — one page; 16-byte
        /// CQ entries * 64 entries = 1 024 B — padded to a full page).
        pub fn allocate(
            device: &DeviceHandle,
            doorbell_stride: usize,
        ) -> Result<Self, DriverRuntimeError> {
            let sq_bytes = core::mem::size_of::<[knvme::NvmeCommand; IO_QUEUE_DEPTH]>();
            let sq = DmaBuffer::<[knvme::NvmeCommand; IO_QUEUE_DEPTH]>::allocate(
                device,
                sq_bytes,
                NVME_PAGE_BYTES,
            )?;
            let cq = DmaBuffer::<[knvme::NvmeCompletion; IO_QUEUE_DEPTH]>::allocate(
                device,
                NVME_PAGE_BYTES,
                NVME_PAGE_BYTES,
            )?;
            let prp_list = DmaBuffer::<[u64; PRP_LIST_ENTRIES]>::allocate(
                device,
                NVME_PAGE_BYTES,
                NVME_PAGE_BYTES,
            )?;
            Ok(Self {
                sq,
                cq,
                prp_list,
                bookkeeping: IoQueueBookkeeping::new(IO_QUEUE_DEPTH as u16),
                doorbell_stride,
                irq: None,
            })
        }

        /// IOVA of the submission queue (programmed into the Create I/O
        /// SQ admin command).
        pub fn sq_iova(&self) -> u64 {
            self.sq.iova()
        }

        /// IOVA of the completion queue (programmed into the Create I/O
        /// CQ admin command).
        pub fn cq_iova(&self) -> u64 {
            self.cq.iova()
        }

        /// IOVA of the PRP-list page.
        pub fn prp_list_iova(&self) -> u64 {
            self.prp_list.iova()
        }

        /// Install the IRQ subscription the driver waits on for
        /// completion notifications. Called after `bring_up` so an
        /// early-abort during Create I/O CQ/SQ does not leak a
        /// subscription.
        pub fn set_irq(&mut self, irq: IrqNotification) {
            self.irq = Some(irq);
        }

        /// Write a command into the SQ ring at the pre-allocated slot.
        ///
        /// # Safety
        ///
        /// The caller has just returned from
        /// [`IoQueueBookkeeping::allocate_slot`], so `slot_index ==
        /// (bookkeeping.sq_tail() - 1) % entries` is a valid in-bounds
        /// index. The write is volatile so the device sees it once we
        /// release the fence.
        fn submit_command(&mut self, slot_index: u16, cmd: knvme::NvmeCommand) {
            let base = self.sq.user_ptr() as *mut knvme::NvmeCommand;
            // SAFETY: `slot_index < IO_QUEUE_DEPTH` by construction
            // (bookkeeping clamps `entries` at construction and every
            // advance is `% entries`). The DMA region is live for the
            // wrapper's lifetime.
            unsafe {
                core::ptr::write_volatile(base.add(slot_index as usize), cmd);
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        }

        /// Ring the SQ doorbell.
        fn ring_sq_doorbell(&self, mmio: &Mmio<NvmeRegsTag>) {
            let offset = knvme::NvmeRegs::doorbell_offset(IO_QUEUE_ID, false, self.doorbell_stride);
            mmio.write_reg::<u32>(offset, self.bookkeeping.sq_tail() as u32);
        }

        /// Ring the CQ doorbell.
        fn ring_cq_doorbell(&self, mmio: &Mmio<NvmeRegsTag>) {
            let offset = knvme::NvmeRegs::doorbell_offset(IO_QUEUE_ID, true, self.doorbell_stride);
            mmio.write_reg::<u32>(offset, self.bookkeeping.cq_head() as u32);
        }

        /// Drain every completion entry the device has posted since the
        /// last drain. Walks the phase bit, advances `cq_head`,
        /// records completions into the bookkeeping, and writes the
        /// CQ-head doorbell once the walk is done.
        ///
        /// Returns the number of completions drained.
        pub fn drain_completions(&mut self, mmio: &Mmio<NvmeRegsTag>) -> usize {
            let mut drained = 0usize;
            // SAFETY: the CQ DMA region is page-sized and owned by
            // this wrapper; the device writes entries before publishing
            // the phase bit, which the release fence above the write in
            // `submit_command` pairs with via the device's own
            // memory-ordering rules.
            let cq_slice: &[knvme::NvmeCompletion] = unsafe {
                core::slice::from_raw_parts(
                    self.cq.user_ptr() as *const knvme::NvmeCompletion,
                    IO_QUEUE_DEPTH,
                )
            };
            while drained < DRAIN_MAX_PASS {
                match drain_step(
                    cq_slice,
                    self.bookkeeping.cq_head(),
                    self.bookkeeping.phase(),
                ) {
                    DrainOutcome::Empty => break,
                    DrainOutcome::Consumed {
                        cid,
                        status_code,
                        result,
                    } => {
                        self.bookkeeping.record_completion(cid, status_code, result);
                        self.bookkeeping.advance_cq();
                        drained += 1;
                    }
                }
            }
            if drained > 0 {
                self.ring_cq_doorbell(mmio);
            }
            drained
        }

        /// Wait for a completion matching `cid`. Uses the IRQ
        /// notification when present and falls back to a bounded polled
        /// drain otherwise. Returns the slot snapshot — caller reads
        /// `status_code` to classify the outcome.
        fn wait_completion(&mut self, mmio: &Mmio<NvmeRegsTag>, cid: u16) -> Option<InFlightSlot> {
            // Fast path: drain any completions already published before
            // parking on the IRQ wait.
            self.drain_completions(mmio);
            if let Some(slot) = self.bookkeeping.slot(cid)
                && slot.filled
            {
                return Some(slot);
            }
            if self.irq.is_some() {
                let mut rounds: u32 = 0;
                while rounds < 64 {
                    // Borrow the IRQ for just the wait+ack call, then
                    // release so `drain_completions` can reborrow
                    // `&mut self`. `wait` takes `&self` per C.3.
                    let bits = match self.irq.as_ref() {
                        Some(irq) => irq.wait(),
                        None => 0,
                    };
                    if bits != 0
                        && let Some(irq) = self.irq.as_ref()
                    {
                        let _ = irq.ack(bits);
                    }
                    self.drain_completions(mmio);
                    if let Some(slot) = self.bookkeeping.slot(cid)
                        && slot.filled
                    {
                        return Some(slot);
                    }
                    rounds += 1;
                }
                // After too many spurious wake-ups, treat as a failure
                // so the IPC client observes an IoError.
                None
            } else {
                // Polled fallback when IRQ subscription failed —
                // bounded by IO_SPIN_BUDGET so a wedged controller
                // cannot stall the driver forever.
                let mut i: u64 = 0;
                while i < IO_SPIN_BUDGET {
                    self.drain_completions(mmio);
                    if let Some(slot) = self.bookkeeping.slot(cid)
                        && slot.filled
                    {
                        return Some(slot);
                    }
                    core::hint::spin_loop();
                    i += 1;
                }
                None
            }
        }
    }

    /// Issue a Read command and return the bulk payload on success.
    ///
    /// `nsid`, `sector_bytes`: namespace-level context from D.2's
    /// Identify Namespace result. `header` carries the client's LBA
    /// and sector count.
    ///
    /// On device error or driver-level timeout the reply header carries
    /// [`BlockDriverError::IoError`] and `bulk` is empty, matching the
    /// D.3 acceptance rule ("failure modes ... return BlockDriverError
    /// rather than panicking").
    pub fn handle_read(
        mmio: &Mmio<NvmeRegsTag>,
        queue: &mut IoQueuePair,
        device: &DeviceHandle,
        nsid: u32,
        sector_bytes: u32,
        header: &BlkRequestHeader,
    ) -> (BlkReplyHeader, alloc::vec::Vec<u8>) {
        let bytes_needed = match (sector_bytes as usize).checked_mul(header.sector_count as usize) {
            Some(v) if v > 0 => v,
            _ => {
                return (
                    error_reply(header.cmd_id, BlockDriverError::InvalidRequest),
                    Vec::new(),
                );
            }
        };
        // Allocate the per-request DMA landing buffer. The PRP list is
        // reused across requests (owned by the queue); the data buffer
        // is a fresh allocation freed when the DmaBuffer drops at the
        // end of this function — "freed on reply".
        let data = match alloc_data_buffer(device, bytes_needed) {
            Ok(b) => b,
            Err(_) => {
                return (
                    error_reply(header.cmd_id, BlockDriverError::IoError),
                    Vec::new(),
                );
            }
        };

        let (prp1, prp2) = match build_prp_pair(
            data.iova(),
            bytes_needed,
            queue.prp_list_iova(),
            prp_list_slice_mut(&mut queue.prp_list),
        ) {
            Ok(p) => p,
            Err(_) => {
                return (
                    error_reply(header.cmd_id, BlockDriverError::IoError),
                    Vec::new(),
                );
            }
        };

        let (cid, _tail) = queue.bookkeeping.allocate_slot();
        let cmd = build_read_command(nsid, cid, header.lba, header.sector_count, prp1, prp2);
        let slot_index = (queue.bookkeeping.sq_tail() + queue.bookkeeping.entries() - 1)
            % queue.bookkeeping.entries();
        queue.submit_command(slot_index, cmd);
        queue.ring_sq_doorbell(mmio);

        match queue.wait_completion(mmio, cid) {
            Some(slot) if slot.status_code == 0 => {
                let bulk = copy_out(&data, bytes_needed);
                (
                    BlkReplyHeader {
                        cmd_id: header.cmd_id,
                        status: BlockDriverError::Ok,
                        bytes: bytes_needed as u32,
                    },
                    bulk,
                )
            }
            Some(_) => (
                error_reply(header.cmd_id, BlockDriverError::IoError),
                Vec::new(),
            ),
            None => (
                error_reply(header.cmd_id, BlockDriverError::IoError),
                Vec::new(),
            ),
        }
    }

    /// Issue a Write command. `data` is the client's bulk payload —
    /// copied into a DMA buffer then programmed via PRP.
    pub fn handle_write(
        mmio: &Mmio<NvmeRegsTag>,
        queue: &mut IoQueuePair,
        device: &DeviceHandle,
        nsid: u32,
        sector_bytes: u32,
        header: &BlkRequestHeader,
        data: &[u8],
    ) -> BlkReplyHeader {
        let bytes_needed = match (sector_bytes as usize).checked_mul(header.sector_count as usize) {
            Some(v) if v > 0 => v,
            _ => return error_reply(header.cmd_id, BlockDriverError::InvalidRequest),
        };
        if data.len() < bytes_needed {
            return error_reply(header.cmd_id, BlockDriverError::InvalidRequest);
        }
        let dma = match alloc_data_buffer(device, bytes_needed) {
            Ok(b) => b,
            Err(_) => return error_reply(header.cmd_id, BlockDriverError::IoError),
        };
        // SAFETY: the DMA region is sized to at least `bytes_needed`
        // (allocator rounds up to a page), and `data` is a borrowed
        // slice of at least `bytes_needed` bytes; the copy has no
        // aliasing.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), dma.user_ptr(), bytes_needed);
        }

        let (prp1, prp2) = match build_prp_pair(
            dma.iova(),
            bytes_needed,
            queue.prp_list_iova(),
            prp_list_slice_mut(&mut queue.prp_list),
        ) {
            Ok(p) => p,
            Err(_) => return error_reply(header.cmd_id, BlockDriverError::IoError),
        };

        let (cid, _tail) = queue.bookkeeping.allocate_slot();
        let cmd = build_write_command(nsid, cid, header.lba, header.sector_count, prp1, prp2);
        let slot_index = (queue.bookkeeping.sq_tail() + queue.bookkeeping.entries() - 1)
            % queue.bookkeeping.entries();
        queue.submit_command(slot_index, cmd);
        queue.ring_sq_doorbell(mmio);

        match queue.wait_completion(mmio, cid) {
            Some(slot) if slot.status_code == 0 => BlkReplyHeader {
                cmd_id: header.cmd_id,
                status: BlockDriverError::Ok,
                bytes: bytes_needed as u32,
            },
            _ => error_reply(header.cmd_id, BlockDriverError::IoError),
        }
    }

    /// Build an error reply header.
    fn error_reply(cmd_id: u64, status: BlockDriverError) -> BlkReplyHeader {
        BlkReplyHeader {
            cmd_id,
            status,
            bytes: 0,
        }
    }

    /// Allocate a page-aligned DMA buffer of at least `bytes` bytes.
    /// Rounds up to a full `NVME_PAGE_BYTES` page so the PRP math
    /// stays simple.
    fn alloc_data_buffer(
        device: &DeviceHandle,
        bytes: usize,
    ) -> Result<DmaBuffer<u8>, DriverRuntimeError> {
        let rounded = bytes.div_ceil(NVME_PAGE_BYTES) * NVME_PAGE_BYTES;
        DmaBuffer::<u8>::allocate(device, rounded, NVME_PAGE_BYTES)
    }

    /// Copy the first `bytes` bytes out of `data` into a fresh `Vec<u8>`
    /// for the IPC reply.
    fn copy_out(data: &DmaBuffer<u8>, bytes: usize) -> Vec<u8> {
        let mut out = vec![0u8; bytes];
        // SAFETY: the DMA region is sized to at least `bytes` bytes and
        // lives for the duration of this function.
        unsafe {
            core::ptr::copy_nonoverlapping(data.user_ptr(), out.as_mut_ptr(), bytes);
        }
        let _ = STDOUT_FILENO; // silence unused-import in debug builds
        out
    }

    /// Expose the PRP-list page as a writable `[u64]` slice for the
    /// pure [`build_prp_pair`] helper.
    fn prp_list_slice_mut(prp: &mut DmaBuffer<[u64; PRP_LIST_ENTRIES]>) -> &mut [u64] {
        // SAFETY: the DMA region is sized for exactly
        // `PRP_LIST_ENTRIES * size_of::<u64>()` bytes (one page) and
        // owned uniquely via `&mut`. No aliasing.
        unsafe { core::slice::from_raw_parts_mut(prp.user_ptr() as *mut u64, PRP_LIST_ENTRIES) }
    }
}

#[cfg(not(test))]
pub use driver_layer::{DRAIN_MAX_PASS, IO_SPIN_BUDGET, IoQueuePair, handle_read, handle_write};

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
