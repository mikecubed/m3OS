//! igb RX / TX hot paths + EICR single-vector interrupt handling — Phase 79
//! Track B.1.
//!
//! The control flow mirrors `userspace/drivers/e1000/src/io.rs`, but two things
//! differ for the igb family:
//!
//! 1. **Descriptors are advanced** (`driver_runtime::Advanced`): RX completion
//!    reads `status_error.DD`/`length` out of the write-back upper qword, and
//!    TX completion reads `DD` out of the dword the driver wrote `cmd_type_len`
//!    into. Encode/decode lives in `driver_runtime::net_ring`; the drain / post
//!    state machines here are generic over those pure functions.
//! 2. **Interrupts use the EICR/EIMS block** (not ICR): the driver arms a
//!    single vector (`eicr::VEC0`) in `EIMS` and reads-to-clear `EICR` on every
//!    wake. The link state still comes from `STATUS.LU`. Because the 1.0 igb
//!    path routes every cause (RX/TX/link) onto one vector, any EICR assertion
//!    triggers a full RX drain + a link re-check.
//!
//! As with e1000, every entry point splits along a pure-logic / MMIO seam so
//! the decode + drain + post state machines are host-testable without a real
//! `Mmio` or a claimed device.

#![allow(dead_code)] // the run loop + smoke tests consume every symbol.

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use driver_runtime::{AdvRxDesc, AdvTxDesc, Advanced, NicDescriptors, adv_rx_wb};
use kernel_core::driver_ipc::net::{MAX_FRAME_BYTES, NetDriverError};

use crate::init::{IgbMmioOps, IgbRegs, eicr, status as e_status};
use crate::rings::{RX_BUF_SIZE, RX_RING_SIZE, TX_BUF_SIZE, TX_RING_SIZE};

// ---------------------------------------------------------------------------
// IrqOutcome
// ---------------------------------------------------------------------------

/// Decoded per-IRQ outcome from [`compute_irq_outcome`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IrqOutcome {
    /// Raw `EICR` snapshot (read-to-clear).
    pub eicr: u32,
    /// New link state (`STATUS.LU`). On the single-vector path STATUS is read
    /// on every wake, so the link is always refreshed.
    pub link_up: bool,
    /// `true` when the link transitioned `0 -> 1` this wake.
    pub link_up_edge: bool,
    /// `true` when an RX drain should run. On the single-vector path any EICR
    /// assertion warrants a drain (RX/TX/link all share the vector).
    pub rx_drain_needed: bool,
}

/// Pure helper: decode an `(EICR, STATUS)` pair into an [`IrqOutcome`].
///
/// Unlike the e1000's ICR (whose individual RX/LSC cause bits gate the drain),
/// the igb 1.0 path collapses every cause onto one vector. STATUS is always
/// read, so the link state is refreshed unconditionally; a non-zero EICR means
/// "something happened" and we drain RX and re-check link.
#[inline]
pub fn compute_irq_outcome(eicr: u32, status: u32, prev_link_up: bool) -> IrqOutcome {
    let link_up = status & e_status::LU != 0;
    let link_up_edge = link_up && !prev_link_up;
    let rx_drain_needed = eicr != 0;
    IrqOutcome {
        eicr,
        link_up,
        link_up_edge,
        rx_drain_needed,
    }
}

/// Called on every IRQ wake: reads `EICR` (read-to-clear), reads `STATUS`,
/// refreshes `link_up`, and returns the decoded [`IrqOutcome`].
pub fn handle_irq<M: IgbMmioOps>(mmio: &M, link_up: &AtomicBool) -> IrqOutcome {
    let eicr = mmio.read_u32(IgbRegs::EICR);
    let status = mmio.read_u32(IgbRegs::STATUS);
    let prev = link_up.load(Ordering::Acquire);
    let outcome = compute_irq_outcome(eicr, status, prev);
    link_up.store(outcome.link_up, Ordering::Release);
    outcome
}

// ---------------------------------------------------------------------------
// RX drain (advanced descriptor)
// ---------------------------------------------------------------------------

/// Result of a pure [`drain_rx_descriptors`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrainOutcome {
    pub frames: Vec<Vec<u8>>,
    pub advance_rdt_to: Option<u32>,
    pub new_next_to_read: usize,
}

/// Pure helper: drain every completed advanced RX descriptor from
/// `next_to_read`, copying out the payload and recycling each slot into the
/// advanced **read** format pointing back at its buffer.
pub fn drain_rx_descriptors(
    descs: &mut [AdvRxDesc],
    bufs: &[&[u8]],
    buf_iova: &[u64],
    next_to_read: usize,
) -> DrainOutcome {
    debug_assert_eq!(descs.len(), RX_RING_SIZE);
    debug_assert_eq!(bufs.len(), RX_RING_SIZE);
    debug_assert_eq!(buf_iova.len(), RX_RING_SIZE);

    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut idx = next_to_read;
    let mut last_consumed: Option<usize> = None;
    for _ in 0..RX_RING_SIZE {
        if !Advanced::rx_done(&descs[idx]) {
            break;
        }
        let desc = &descs[idx];
        let len = (Advanced::rx_len(desc) as usize).min(RX_BUF_SIZE);
        let has_eop = desc.status_error() & adv_rx_wb::EOP != 0;
        if has_eop && len > 0 {
            let slot = &bufs[idx];
            let take = len.min(slot.len());
            frames.push(slot[..take].to_vec());
        }
        // Recycle: rewrite the read format pointing at this slot's buffer.
        descs[idx] = Advanced::rx_init(buf_iova[idx]);
        last_consumed = Some(idx);
        idx = (idx + 1) % RX_RING_SIZE;
    }
    DrainOutcome {
        frames,
        advance_rdt_to: last_consumed.map(|i| i as u32),
        new_next_to_read: idx,
    }
}

// ---------------------------------------------------------------------------
// TX post (advanced descriptor)
// ---------------------------------------------------------------------------

/// Pure helper: copy `frame` into `buf` and program the advanced TX descriptor.
///
/// Returns [`NetDriverError::InvalidFrame`] on empty / oversize input.
pub fn post_tx_descriptor(
    desc: &mut AdvTxDesc,
    buf: &mut [u8],
    buf_iova: u64,
    frame: &[u8],
) -> Result<(), NetDriverError> {
    if frame.is_empty() {
        return Err(NetDriverError::InvalidFrame);
    }
    if frame.len() > TX_BUF_SIZE {
        return Err(NetDriverError::InvalidFrame);
    }
    if frame.len() > MAX_FRAME_BYTES as usize {
        return Err(NetDriverError::InvalidFrame);
    }
    debug_assert!(buf.len() >= TX_BUF_SIZE);
    buf[..frame.len()].copy_from_slice(frame);
    *desc = Advanced::encode_tx(buf_iova, frame.len() as u16);
    Ok(())
}

/// Pure helper: drain every in-flight TX descriptor by resetting it to the
/// all-zero (never-programmed / free) state. Called on a link-up edge.
pub fn drain_tx_in_flight(descs: &mut [AdvTxDesc]) -> usize {
    debug_assert_eq!(descs.len(), TX_RING_SIZE);
    for d in descs.iter_mut() {
        *d = AdvTxDesc::default();
    }
    TX_RING_SIZE
}

// ---------------------------------------------------------------------------
// Module-scoped link/restart atomics (single-device driver).
// ---------------------------------------------------------------------------

static LINK_UP: AtomicBool = AtomicBool::new(false);
static DRIVER_RESTARTING: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn link_state_atomic() -> &'static AtomicBool {
    &LINK_UP
}

#[inline]
pub fn driver_restarting_atomic() -> &'static AtomicBool {
    &DRIVER_RESTARTING
}

// ---------------------------------------------------------------------------
// Production wrappers.
// ---------------------------------------------------------------------------

/// Drain the RX ring through `mmio`, publishing each frame via `publisher`, then
/// advance `RDT0` to the last consumed slot.
pub fn drain_rx<M: IgbMmioOps, P: FnMut(&[u8])>(
    mmio: &M,
    descs: &mut [AdvRxDesc],
    bufs: &[&[u8]],
    buf_iova: &[u64],
    next_to_read: &mut usize,
    mut publisher: P,
) -> usize {
    let outcome = drain_rx_descriptors(descs, bufs, buf_iova, *next_to_read);
    let count = outcome.frames.len();
    for frame in &outcome.frames {
        publisher(frame);
    }
    *next_to_read = outcome.new_next_to_read;
    if let Some(rdt) = outcome.advance_rdt_to {
        mmio.write_u32(IgbRegs::RDT0, rdt);
    }
    count
}

/// Find the next free TX slot, post the descriptor, and ring `TDT0`.
#[allow(clippy::too_many_arguments)]
pub fn handle_tx<M: IgbMmioOps>(
    mmio: &M,
    descs: &mut [AdvTxDesc],
    bufs: &mut [&mut [u8]],
    buf_iova: &[u64],
    next_to_write: &mut usize,
    link_up: &AtomicBool,
    driver_restarting: &AtomicBool,
    frame: &[u8],
) -> Result<(), NetDriverError> {
    if driver_restarting.load(Ordering::Acquire) {
        return Err(NetDriverError::DriverRestarting);
    }
    if !link_up.load(Ordering::Acquire) {
        return Err(NetDriverError::LinkDown);
    }
    let idx = *next_to_write;
    if !Advanced::tx_slot_free(&descs[idx]) {
        return Err(NetDriverError::RingFull);
    }
    post_tx_descriptor(&mut descs[idx], bufs[idx], buf_iova[idx], frame)?;
    let new_tdt = ((idx + 1) % TX_RING_SIZE) as u32;
    *next_to_write = new_tdt as usize;
    core::sync::atomic::fence(Ordering::Release);
    mmio.write_u32(IgbRegs::TDT0, new_tdt);
    Ok(())
}

// ---------------------------------------------------------------------------
// Device integration — IgbDevice + NetServer + IrqNotification wiring.
// ---------------------------------------------------------------------------

use driver_runtime::ipc::net::NetServer;
use driver_runtime::ipc::{EndpointCap, IpcBackend};
use driver_runtime::{DeviceHandle, IrqNotification, SyscallBackend as IrqSyscallBackend};
use kernel_core::driver_ipc::net::NetLinkEvent;

use crate::init::IgbDevice;

/// Orphan-rule-safe local view of a `DeviceHandle` as a `DeviceCapHandle`.
pub struct DeviceCapView<'a> {
    inner: &'a DeviceHandle,
}

impl<'a> DeviceCapView<'a> {
    pub fn new(inner: &'a DeviceHandle) -> Self {
        Self { inner }
    }
}

impl driver_runtime::DeviceCapHandle for DeviceCapView<'_> {
    fn cap_handle(&self) -> u32 {
        self.inner.cap()
    }
}

/// Subscribe to the igb's MSI / INTx vector.
pub fn subscribe_irq(
    device: &DeviceHandle,
) -> Result<IrqNotification<IrqSyscallBackend>, driver_runtime::DriverRuntimeError> {
    let view = DeviceCapView::new(device);
    IrqNotification::<IrqSyscallBackend>::subscribe(&view, None)
}

/// Arm the single EICR vector in `EIMS` (the un-mask that lets RX/TX/link wake
/// the driver). Bring-up left every cause masked via `EIMC`.
pub fn arm_irqs<M: IgbMmioOps>(mmio: &M) {
    mmio.write_u32(IgbRegs::EIMS, eicr::VEC0);
}

/// Subscribe → bind to the command endpoint → arm EIMS, in that order.
pub fn subscribe_and_bind(
    device: &IgbDevice,
    endpoint: EndpointCap,
) -> Result<IrqNotification<IrqSyscallBackend>, driver_runtime::DriverRuntimeError> {
    let irq = subscribe_irq(&device.pci)?;
    irq.bind_to_endpoint(endpoint)?;
    arm_irqs(&device.mmio);
    Ok(irq)
}

/// Drain the RX ring of `device`, publishing every frame to the kernel net
/// stack. Returns `(drained, dropped)`.
pub fn drain_rx_to_server<B: IpcBackend>(
    device: &mut IgbDevice,
    net_server: &NetServer<B>,
) -> (usize, usize) {
    let buf_iova = device.rx.buf_iova.clone();
    let bufs: alloc::vec::Vec<&[u8]> = device
        .rx
        .bufs
        .iter()
        .map(|b| {
            let arr: &[u8; RX_BUF_SIZE] = core::ops::Deref::deref(b);
            arr.as_slice()
        })
        .collect();
    let descs: &mut [AdvRxDesc; RX_RING_SIZE] = &mut device.rx.descs;
    let mut next_to_read = device.rx.next_to_read;
    let mut collected: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
    let drained = drain_rx(
        &device.mmio,
        descs.as_mut_slice(),
        &bufs,
        &buf_iova,
        &mut next_to_read,
        |frame| collected.push(frame.to_vec()),
    );
    device.rx.next_to_read = next_to_read;
    let dropped = if collected.is_empty() {
        0
    } else {
        let frame_refs: alloc::vec::Vec<&[u8]> = collected.iter().map(|v| v.as_slice()).collect();
        match net_server.publish_rx_frames(&frame_refs) {
            Ok(()) => 0,
            Err(_) => collected.len(),
        }
    };
    (drained, dropped)
}

/// Post `frame` to `device`'s TX ring.
pub fn send_frame(device: &mut IgbDevice, frame: &[u8]) -> Result<(), NetDriverError> {
    let buf_iova = device.tx.buf_iova.clone();
    let mut bufs: alloc::vec::Vec<&mut [u8]> = device
        .tx
        .bufs
        .iter_mut()
        .map(|b| {
            let arr: &mut [u8; TX_BUF_SIZE] = core::ops::DerefMut::deref_mut(b);
            arr.as_mut_slice()
        })
        .collect();
    let descs: &mut [AdvTxDesc; TX_RING_SIZE] = &mut device.tx.descs;
    let mut next_to_write = device.tx.next_to_write;
    let result = handle_tx(
        &device.mmio,
        descs.as_mut_slice(),
        &mut bufs,
        &buf_iova,
        &mut next_to_write,
        link_state_atomic(),
        driver_restarting_atomic(),
        frame,
    );
    device.tx.next_to_write = next_to_write;
    result
}

/// Flush every in-flight TX descriptor on a link-up edge and reset `TDT0`.
pub fn drain_tx_on_link_up(device: &mut IgbDevice) -> usize {
    let descs: &mut [AdvTxDesc; TX_RING_SIZE] = &mut device.tx.descs;
    let drained = drain_tx_in_flight(descs.as_mut_slice());
    device.tx.next_to_write = 0;
    device.mmio.write_u32(IgbRegs::TDT0, 0);
    drained
}

/// Handle exactly one IRQ wake: refresh link, handle the link-up edge, drain RX.
pub fn handle_irq_and_drain<B: IpcBackend>(
    device: &mut IgbDevice,
    net_server: &NetServer<B>,
) -> (IrqOutcome, usize, usize) {
    let outcome = handle_irq(&device.mmio, link_state_atomic());
    if outcome.link_up_edge {
        let _ = drain_tx_on_link_up(device);
        let _ = net_server.publish_link_state(NetLinkEvent {
            up: true,
            mac: device.mac,
            speed_mbps: 0,
        });
    } else if !outcome.link_up {
        let _ = net_server.publish_link_state(NetLinkEvent {
            up: false,
            mac: device.mac,
            speed_mbps: 0,
        });
    }
    let (drained, dropped) = if outcome.rx_drain_needed {
        drain_rx_to_server(device, net_server)
    } else {
        (0, 0)
    };
    (outcome, drained, dropped)
}

/// Main driver loop: subscribe + bind the IRQ, init the link atomic, and
/// dispatch TX requests / IRQ wakes through `NetServer::handle_next`. Never
/// returns.
pub fn run_io_loop(
    device: IgbDevice,
    command_endpoint: EndpointCap,
    ingress_endpoint: Option<EndpointCap>,
) -> ! {
    let irq = match subscribe_and_bind(&device, command_endpoint) {
        Ok(n) => n,
        Err(_) => syscall_lib::exit(4),
    };
    let initial_link_up = device.link_up_initial();
    let initial_mac = device.mac();
    link_state_atomic().store(initial_link_up, Ordering::Release);

    let device = core::cell::RefCell::new(device);
    let net_server = match ingress_endpoint {
        Some(ep) => NetServer::new(command_endpoint).with_ingress_endpoint(ep),
        None => NetServer::new(command_endpoint),
    };
    let _ = net_server.publish_link_state(NetLinkEvent {
        up: initial_link_up,
        mac: initial_mac,
        speed_mbps: 0,
    });

    loop {
        let irq_bits = core::cell::Cell::new(0u64);
        let _ = net_server.handle_next(
            |req| {
                let mut dev = device.borrow_mut();
                let status = match send_frame(&mut dev, &req.frame) {
                    Ok(()) => NetDriverError::Ok,
                    Err(e) => e,
                };
                driver_runtime::ipc::net::NetReply { status }
            },
            |bits| {
                irq_bits.set(bits);
            },
        );
        let bits = irq_bits.get();
        if bits != 0 {
            let mut dev = device.borrow_mut();
            let _ = handle_irq_and_drain(&mut dev, &net_server);
            drop(dev);
            let _ = irq.ack(bits);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::cell::RefCell;
    use driver_runtime::{adv_tx, adv_tx_wb};

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
    impl IgbMmioOps for FakeMmio {
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

    // -- IRQ outcome --

    #[test]
    fn compute_irq_outcome_refreshes_link_from_status() {
        let o = compute_irq_outcome(eicr::VEC0, e_status::LU, false);
        assert!(o.link_up);
        assert!(o.link_up_edge);
        assert!(o.rx_drain_needed);
        let o = compute_irq_outcome(eicr::VEC0, 0, true);
        assert!(!o.link_up);
        assert!(!o.link_up_edge);
    }

    #[test]
    fn compute_irq_outcome_zero_eicr_skips_drain() {
        let o = compute_irq_outcome(0, e_status::LU, true);
        assert!(!o.rx_drain_needed);
        assert!(o.link_up);
    }

    #[test]
    fn handle_irq_reads_eicr_status_and_stores_link() {
        let mmio = FakeMmio::new();
        mmio.set(IgbRegs::EICR, eicr::VEC0);
        mmio.set(IgbRegs::STATUS, e_status::LU);
        let link = AtomicBool::new(false);
        let o = handle_irq(&mmio, &link);
        assert!(o.link_up);
        assert!(o.link_up_edge);
        assert!(link.load(Ordering::Acquire));
    }

    #[test]
    fn arm_irqs_writes_vec0_to_eims() {
        let mmio = FakeMmio::new();
        arm_irqs(&mmio);
        assert!(
            mmio.writes()
                .iter()
                .any(|&(o, v)| o == IgbRegs::EIMS && v == eicr::VEC0)
        );
    }

    // -- RX drain (advanced) --

    fn mk_rx() -> (Vec<AdvRxDesc>, Vec<Vec<u8>>, Vec<u64>) {
        let descs = vec![AdvRxDesc::default(); RX_RING_SIZE];
        let bufs: Vec<Vec<u8>> = (0..RX_RING_SIZE).map(|_| vec![0u8; RX_BUF_SIZE]).collect();
        let buf_iova: Vec<u64> = (0..RX_RING_SIZE)
            .map(|i| 0x1000_0000u64 + (i as u64) * RX_BUF_SIZE as u64)
            .collect();
        (descs, bufs, buf_iova)
    }
    fn borrow(bufs: &[Vec<u8>]) -> Vec<&[u8]> {
        bufs.iter().map(|v| v.as_slice()).collect()
    }
    fn set_rx_done(desc: &mut AdvRxDesc, len: u16) {
        let se = adv_rx_wb::DD | adv_rx_wb::EOP;
        desc.hi = (se as u64) | ((len as u64) << 32);
    }

    #[test]
    fn drain_rx_advanced_one_frame() {
        let (mut descs, mut bufs, buf_iova) = mk_rx();
        bufs[0][..5].copy_from_slice(b"hello");
        set_rx_done(&mut descs[0], 5);
        let slices = borrow(&bufs);
        let o = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert_eq!(o.frames.len(), 1);
        assert_eq!(&o.frames[0][..], b"hello");
        assert_eq!(o.advance_rdt_to, Some(0));
        assert_eq!(o.new_next_to_read, 1);
        // Slot recycled into the read format pointing back at its buffer.
        assert_eq!(descs[0].lo, buf_iova[0]);
        assert_eq!(descs[0].hi, 0);
        assert!(!Advanced::rx_done(&descs[0]));
    }

    #[test]
    fn drain_rx_advanced_stops_at_first_undelivered() {
        let (mut descs, mut bufs, buf_iova) = mk_rx();
        for i in 0..3 {
            bufs[i][..4].copy_from_slice(b"FRAM");
            set_rx_done(&mut descs[i], 4);
        }
        let slices = borrow(&bufs);
        let o = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert_eq!(o.frames.len(), 3);
        assert_eq!(o.advance_rdt_to, Some(2));
        assert_eq!(o.new_next_to_read, 3);
    }

    #[test]
    fn drain_rx_advanced_clamps_length() {
        let (mut descs, mut bufs, buf_iova) = mk_rx();
        bufs[0][0..RX_BUF_SIZE].fill(0xAB);
        set_rx_done(&mut descs[0], (RX_BUF_SIZE as u16).saturating_add(500));
        let slices = borrow(&bufs);
        let o = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert_eq!(o.frames.len(), 1);
        assert_eq!(o.frames[0].len(), RX_BUF_SIZE);
    }

    #[test]
    fn drain_rx_advanced_writes_rdt() {
        let (mut descs, mut bufs, buf_iova) = mk_rx();
        bufs[0][..3].copy_from_slice(b"RDT");
        set_rx_done(&mut descs[0], 3);
        let slices = borrow(&bufs);
        let mmio = FakeMmio::new();
        let mut next = 0;
        let mut seen: Vec<Vec<u8>> = Vec::new();
        let n = drain_rx(&mmio, &mut descs, &slices, &buf_iova, &mut next, |f| {
            seen.push(f.to_vec())
        });
        assert_eq!(n, 1);
        assert_eq!(next, 1);
        assert!(
            mmio.writes()
                .iter()
                .any(|&(o, v)| o == IgbRegs::RDT0 && v == 0)
        );
    }

    // -- TX post (advanced) --

    fn mk_tx() -> (Vec<AdvTxDesc>, Vec<Vec<u8>>, Vec<u64>) {
        let descs = vec![AdvTxDesc::default(); TX_RING_SIZE];
        let bufs: Vec<Vec<u8>> = (0..TX_RING_SIZE).map(|_| vec![0u8; TX_BUF_SIZE]).collect();
        let buf_iova: Vec<u64> = (0..TX_RING_SIZE)
            .map(|i| 0x2000_0000u64 + (i as u64) * TX_BUF_SIZE as u64)
            .collect();
        (descs, bufs, buf_iova)
    }
    fn borrow_mut(bufs: &mut [Vec<u8>]) -> Vec<&mut [u8]> {
        bufs.iter_mut().map(|v| v.as_mut_slice()).collect()
    }

    #[test]
    fn post_tx_advanced_fills_descriptor() {
        let mut desc = AdvTxDesc::default();
        let mut buf = vec![0u8; TX_BUF_SIZE];
        let iova = 0xABCD_0000u64;
        post_tx_descriptor(&mut desc, &mut buf, iova, b"TESTFRAME").expect("ok");
        assert_eq!(desc.buffer_addr, iova);
        assert_eq!(desc.cmd_type_len() & adv_tx::DTALEN_MASK, 9);
        assert_eq!(&buf[..9], b"TESTFRAME");
    }

    #[test]
    fn post_tx_advanced_rejects_empty_and_oversize() {
        let mut desc = AdvTxDesc::default();
        let mut buf = vec![0u8; TX_BUF_SIZE];
        assert_eq!(
            post_tx_descriptor(&mut desc, &mut buf, 0, &[]).unwrap_err(),
            NetDriverError::InvalidFrame
        );
        let big = vec![0u8; TX_BUF_SIZE + 1];
        assert_eq!(
            post_tx_descriptor(&mut desc, &mut buf, 0, &big).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }

    #[test]
    fn handle_tx_advanced_link_down_errors() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx();
        let mut mb = borrow_mut(&mut bufs);
        let link = AtomicBool::new(false);
        let restart = AtomicBool::new(false);
        let mut next = 0;
        let e = handle_tx(
            &mmio, &mut descs, &mut mb, &buf_iova, &mut next, &link, &restart, b"x",
        )
        .unwrap_err();
        assert_eq!(e, NetDriverError::LinkDown);
        assert!(mmio.writes().iter().all(|(o, _)| *o != IgbRegs::TDT0));
    }

    #[test]
    fn handle_tx_advanced_happy_path_rings_tdt() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx();
        let mut mb = borrow_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restart = AtomicBool::new(false);
        let mut next = 0;
        handle_tx(
            &mmio, &mut descs, &mut mb, &buf_iova, &mut next, &link, &restart, b"PING",
        )
        .expect("ok");
        assert_eq!(next, 1);
        assert!(
            mmio.writes()
                .iter()
                .any(|&(o, v)| o == IgbRegs::TDT0 && v == 1)
        );
        assert_eq!(descs[0].cmd_type_len() & adv_tx::DTALEN_MASK, 4);
    }

    #[test]
    fn handle_tx_advanced_ring_full_when_inflight() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx();
        // Slot 0 programmed but not yet DD => in flight.
        descs[0] = Advanced::encode_tx(buf_iova[0], 64);
        let mut mb = borrow_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restart = AtomicBool::new(false);
        let mut next = 0;
        let e = handle_tx(
            &mmio, &mut descs, &mut mb, &buf_iova, &mut next, &link, &restart, b"y",
        )
        .unwrap_err();
        assert_eq!(e, NetDriverError::RingFull);
    }

    #[test]
    fn handle_tx_advanced_reuses_slot_after_dd() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx();
        // Slot 0 programmed and completed (DD written into low dword).
        descs[0] = Advanced::encode_tx(buf_iova[0], 64);
        descs[0].cmd_olinfo = (descs[0].cmd_olinfo & !0xFFFF_FFFF) | (adv_tx_wb::DD as u64);
        let mut mb = borrow_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restart = AtomicBool::new(false);
        let mut next = 0;
        handle_tx(
            &mmio, &mut descs, &mut mb, &buf_iova, &mut next, &link, &restart, b"Z",
        )
        .expect("reuse after DD");
        assert_eq!(descs[0].cmd_type_len() & adv_tx::DTALEN_MASK, 1);
    }

    #[test]
    fn handle_tx_advanced_restart_shadows_link() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx();
        let mut mb = borrow_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restart = AtomicBool::new(true);
        let mut next = 0;
        let e = handle_tx(
            &mmio, &mut descs, &mut mb, &buf_iova, &mut next, &link, &restart, b"q",
        )
        .unwrap_err();
        assert_eq!(e, NetDriverError::DriverRestarting);
    }

    #[test]
    fn drain_tx_in_flight_clears_every_slot() {
        let mut descs = vec![AdvTxDesc::default(); TX_RING_SIZE];
        for d in descs.iter_mut() {
            *d = Advanced::encode_tx(0x1000, 100);
        }
        let n = drain_tx_in_flight(&mut descs);
        assert_eq!(n, TX_RING_SIZE);
        for d in &descs {
            assert!(Advanced::tx_slot_free(d));
        }
    }
}
