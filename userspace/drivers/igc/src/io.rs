//! igc RX / TX hot paths + EICR single-vector interrupt handling — Phase 79
//! Track B.2.
//!
//! Structurally identical to the igb IO path (`userspace/drivers/igb/src/io.rs`)
//! because igc is a direct descendant of igb: advanced read/write-back
//! descriptors + a single-vector EICR/EIMS interrupt block. The only
//! family-specific addition for igc is the Clause-45 MMD PHY accessor in
//! `init.rs`; the descriptor + interrupt control flow is shared verbatim.
//!
//! igc has **no QEMU model**, so this path is hardware-only; every pure helper
//! is host-tested so the bring-up is correct-by-construction against Linux
//! `drivers/net/ethernet/intel/igc`.

#![allow(dead_code)] // hardware-only family; the run loop + tests consume these.

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use driver_runtime::{AdvRxDesc, AdvTxDesc, Advanced, NicDescriptors, adv_rx_wb};
use kernel_core::driver_ipc::net::{MAX_FRAME_BYTES, NetDriverError};

use crate::init::{IgcMmioOps, IgcRegs, eicr, status as e_status};
use crate::rings::{RX_BUF_SIZE, RX_RING_SIZE, TX_BUF_SIZE, TX_RING_SIZE};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IrqOutcome {
    pub eicr: u32,
    pub link_up: bool,
    pub link_up_edge: bool,
    pub rx_drain_needed: bool,
}

/// Pure helper: decode an `(EICR, STATUS)` pair into an [`IrqOutcome`]. The igc
/// single-vector path refreshes link from STATUS every wake and drains RX on
/// any EICR assertion (RX/TX/link share the vector).
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

pub fn handle_irq<M: IgcMmioOps>(mmio: &M, link_up: &AtomicBool) -> IrqOutcome {
    let eicr = mmio.read_u32(IgcRegs::EICR);
    let status = mmio.read_u32(IgcRegs::STATUS);
    let prev = link_up.load(Ordering::Acquire);
    let outcome = compute_irq_outcome(eicr, status, prev);
    link_up.store(outcome.link_up, Ordering::Release);
    outcome
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrainOutcome {
    pub frames: Vec<Vec<u8>>,
    pub advance_rdt_to: Option<u32>,
    pub new_next_to_read: usize,
}

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

pub fn drain_tx_in_flight(descs: &mut [AdvTxDesc]) -> usize {
    debug_assert_eq!(descs.len(), TX_RING_SIZE);
    for d in descs.iter_mut() {
        *d = AdvTxDesc::default();
    }
    TX_RING_SIZE
}

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

pub fn drain_rx<M: IgcMmioOps, P: FnMut(&[u8])>(
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
        mmio.write_u32(IgcRegs::RDT0, rdt);
    }
    count
}

#[allow(clippy::too_many_arguments)]
pub fn handle_tx<M: IgcMmioOps>(
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
    mmio.write_u32(IgcRegs::TDT0, new_tdt);
    Ok(())
}

// ---------------------------------------------------------------------------
// Device integration — IgcDevice + NetServer + IrqNotification wiring.
// ---------------------------------------------------------------------------

use driver_runtime::ipc::net::NetServer;
use driver_runtime::ipc::{EndpointCap, IpcBackend};
use driver_runtime::{DeviceHandle, IrqNotification, SyscallBackend as IrqSyscallBackend};
use kernel_core::driver_ipc::net::NetLinkEvent;

use crate::init::IgcDevice;

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

pub fn subscribe_irq(
    device: &DeviceHandle,
) -> Result<IrqNotification<IrqSyscallBackend>, driver_runtime::DriverRuntimeError> {
    let view = DeviceCapView::new(device);
    IrqNotification::<IrqSyscallBackend>::subscribe(&view, None)
}

pub fn arm_irqs<M: IgcMmioOps>(mmio: &M) {
    mmio.write_u32(IgcRegs::EIMS, eicr::VEC0);
}

pub fn subscribe_and_bind(
    device: &IgcDevice,
    endpoint: EndpointCap,
) -> Result<IrqNotification<IrqSyscallBackend>, driver_runtime::DriverRuntimeError> {
    let irq = subscribe_irq(&device.pci)?;
    irq.bind_to_endpoint(endpoint)?;
    arm_irqs(&device.mmio);
    Ok(irq)
}

pub fn drain_rx_to_server<B: IpcBackend>(
    device: &mut IgcDevice,
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

pub fn send_frame(device: &mut IgcDevice, frame: &[u8]) -> Result<(), NetDriverError> {
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

pub fn drain_tx_on_link_up(device: &mut IgcDevice) -> usize {
    let descs: &mut [AdvTxDesc; TX_RING_SIZE] = &mut device.tx.descs;
    let drained = drain_tx_in_flight(descs.as_mut_slice());
    device.tx.next_to_write = 0;
    device.mmio.write_u32(IgcRegs::TDT0, 0);
    drained
}

pub fn handle_irq_and_drain<B: IpcBackend>(
    device: &mut IgcDevice,
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

pub fn run_io_loop(
    device: IgcDevice,
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
            if let Some(s) = r.iter_mut().find(|(o, _)| *o == off) {
                s.1 = val;
            } else {
                r.push((off, val));
            }
        }
        fn writes(&self) -> Vec<(usize, u32)> {
            self.writes.borrow().clone()
        }
    }
    impl IgcMmioOps for FakeMmio {
        fn read_u32(&self, off: usize) -> u32 {
            self.reads
                .borrow()
                .iter()
                .find(|(o, _)| *o == off)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write_u32(&self, off: usize, v: u32) {
            self.writes.borrow_mut().push((off, v));
        }
    }

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
    fn set_rx_done(d: &mut AdvRxDesc, len: u16) {
        let se = adv_rx_wb::DD | adv_rx_wb::EOP;
        d.hi = (se as u64) | ((len as u64) << 32);
    }

    #[test]
    fn compute_irq_outcome_refreshes_link_and_drains_on_eicr() {
        let o = compute_irq_outcome(eicr::VEC0, e_status::LU, false);
        assert!(o.link_up && o.link_up_edge && o.rx_drain_needed);
        let o = compute_irq_outcome(0, e_status::LU, true);
        assert!(o.link_up && !o.rx_drain_needed);
    }

    #[test]
    fn handle_irq_reads_eicr_status() {
        let m = FakeMmio::new();
        m.set(IgcRegs::EICR, eicr::VEC0);
        m.set(IgcRegs::STATUS, e_status::LU);
        let link = AtomicBool::new(false);
        let o = handle_irq(&m, &link);
        assert!(o.link_up && o.link_up_edge);
        assert!(link.load(Ordering::Acquire));
    }

    #[test]
    fn arm_irqs_writes_vec0_to_eims() {
        let m = FakeMmio::new();
        arm_irqs(&m);
        assert!(
            m.writes()
                .iter()
                .any(|&(o, v)| o == IgcRegs::EIMS && v == eicr::VEC0)
        );
    }

    #[test]
    fn drain_rx_advanced_one_frame_and_writes_rdt() {
        let (mut descs, mut bufs, buf_iova) = mk_rx();
        bufs[0][..5].copy_from_slice(b"hello");
        set_rx_done(&mut descs[0], 5);
        let slices = borrow(&bufs);
        let m = FakeMmio::new();
        let mut next = 0;
        let mut seen: Vec<Vec<u8>> = Vec::new();
        let n = drain_rx(&m, &mut descs, &slices, &buf_iova, &mut next, |f| {
            seen.push(f.to_vec())
        });
        assert_eq!(n, 1);
        assert_eq!(&seen[0][..], b"hello");
        assert_eq!(next, 1);
        assert!(
            m.writes()
                .iter()
                .any(|&(o, v)| o == IgcRegs::RDT0 && v == 0)
        );
        // Slot recycled.
        assert_eq!(descs[0].lo, buf_iova[0]);
        assert!(!Advanced::rx_done(&descs[0]));
    }

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
    fn handle_tx_advanced_happy_path() {
        let m = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx();
        let mut mb = borrow_mut(&mut bufs);
        let link = AtomicBool::new(true);
        let restart = AtomicBool::new(false);
        let mut next = 0;
        handle_tx(
            &m, &mut descs, &mut mb, &buf_iova, &mut next, &link, &restart, b"PING",
        )
        .expect("ok");
        assert_eq!(next, 1);
        assert!(
            m.writes()
                .iter()
                .any(|&(o, v)| o == IgcRegs::TDT0 && v == 1)
        );
        assert_eq!(descs[0].cmd_type_len() & adv_tx::DTALEN_MASK, 4);
    }

    #[test]
    fn handle_tx_advanced_link_down_and_ring_full() {
        let m = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx();
        let mut mb = borrow_mut(&mut bufs);
        let down = AtomicBool::new(false);
        let restart = AtomicBool::new(false);
        let mut next = 0;
        assert_eq!(
            handle_tx(
                &m, &mut descs, &mut mb, &buf_iova, &mut next, &down, &restart, b"x"
            )
            .unwrap_err(),
            NetDriverError::LinkDown
        );
        // In-flight slot => RingFull.
        descs[0] = Advanced::encode_tx(buf_iova[0], 64);
        let up = AtomicBool::new(true);
        assert_eq!(
            handle_tx(
                &m, &mut descs, &mut mb, &buf_iova, &mut next, &up, &restart, b"y"
            )
            .unwrap_err(),
            NetDriverError::RingFull
        );
    }

    #[test]
    fn handle_tx_advanced_reuses_after_dd() {
        let m = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx();
        descs[0] = Advanced::encode_tx(buf_iova[0], 64);
        descs[0].cmd_olinfo = (descs[0].cmd_olinfo & !0xFFFF_FFFF) | (adv_tx_wb::DD as u64);
        let mut mb = borrow_mut(&mut bufs);
        let up = AtomicBool::new(true);
        let restart = AtomicBool::new(false);
        let mut next = 0;
        handle_tx(
            &m, &mut descs, &mut mb, &buf_iova, &mut next, &up, &restart, b"Z",
        )
        .expect("reuse after DD");
        assert_eq!(descs[0].cmd_type_len() & adv_tx::DTALEN_MASK, 1);
    }

    #[test]
    fn post_tx_rejects_empty_and_oversize() {
        let mut d = AdvTxDesc::default();
        let mut buf = vec![0u8; TX_BUF_SIZE];
        assert_eq!(
            post_tx_descriptor(&mut d, &mut buf, 0, &[]).unwrap_err(),
            NetDriverError::InvalidFrame
        );
        let big = vec![0u8; TX_BUF_SIZE + 1];
        assert_eq!(
            post_tx_descriptor(&mut d, &mut buf, 0, &big).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }
}
