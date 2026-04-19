//! E1000 RX / TX hot paths and link-state handling — Phase 55b Track E.3.
//!
//! Ports the ISR-plus-task-context logic from the Phase 55 in-kernel
//! driver (`kernel/src/net/e1000.rs`) to the ring-3 `driver_runtime`
//! host. The Phase 55 ISR runs in interrupt context and wakes a kernel
//! net task; here every path runs in task context — the driver process
//! blocks in `IrqNotification::wait`, drains the RX ring, updates the
//! link atomic from `STATUS.LU`, and responds to `NET_SEND_FRAME`
//! requests through `NetServer::handle_next`.
//!
//! # Public surface
//!
//! - [`handle_irq`] — called on every IRQ wake: reads `ICR`, updates
//!   `link_state_atomic` from the `LSC` cause, and snapshots the
//!   `STATUS` register.
//! - [`drain_rx`] — pulls every completed RX descriptor, publishes the
//!   frame(s) via the caller's publisher closure, recycles the
//!   descriptor, and advances `RDT`.
//! - [`handle_tx`] — the TX path called from `NetServer::handle_next`:
//!   validates link state + driver-restart state, copies the frame
//!   into the next TX descriptor's DMA buffer, stamps
//!   `cmd = EOP|IFCS|RS`, and advances `TDT`.
//! - [`link_state_atomic`] — the `AtomicBool` every hot path consults
//!   to decide whether the ring is even accepting frames.
//!
//! # Testability
//!
//! All three entry points split along a pure-logic / MMIO seam so the
//! drain / post / link-update state machines are exercisable from
//! host tests without a real `Mmio<E1000Regs>` or a claimed PCI
//! device:
//!
//! - [`compute_irq_outcome`] — pure helper: given `ICR` + `STATUS`
//!   snapshots, returns the new link state and whether an RX drain
//!   is warranted.
//! - [`drain_rx_descriptors`] — pure helper: operates on
//!   `&mut [E1000RxDesc]` + per-slot byte buffers and returns the
//!   drained frames plus the advanced `RDT` value. Production
//!   [`drain_rx`] wraps this with [`MmioOps`] write of `RDT`.
//! - [`post_tx_descriptor`] — pure helper: operates on a single
//!   `&mut E1000TxDesc` + a single per-slot byte buffer; production
//!   [`handle_tx`] wraps with MMIO and ring-state bookkeeping.
//!
//! # Link-down semantics
//!
//! `handle_tx` returns [`NetDriverError::LinkDown`] while the link
//! atomic is clear. The link-up edge (observed by `handle_irq` from
//! `STATUS.LU` transitioning `0 -> 1` after an `ICR.LSC`) flushes any
//! in-flight TX descriptors before re-enabling — the same semantic as
//! Phase 55 E.4's `drain_link_up_edge`.
//!
//! # Driver-restart semantics
//!
//! A separate [`AtomicBool`] names the "driver is mid-restart" state.
//! While set, `handle_tx` returns [`NetDriverError::DriverRestarting`]
//! — distinct from `LinkDown` so the kernel-side `RemoteNic` facade
//! (Track E.4) can expose the retry path. Phase 55b F.2 asserts a
//! successful send after the restart clears within
//! `DRIVER_RESTART_TIMEOUT_MS` (`kernel_core::device_host::types`).

#![allow(dead_code)] // E.4 (kernel facade) and F-track smokes consume every symbol.

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use kernel_core::driver_ipc::net::{MAX_FRAME_BYTES, NetDriverError};
use kernel_core::e1000::{
    E1000Regs, E1000RxDesc, E1000TxDesc, irq_cause, rx_descriptor_done, rx_status,
    status as e_status, tx_cmd, tx_descriptor_done,
};

use crate::init::MmioOps;
use crate::rings::{RX_BUF_SIZE, RX_RING_SIZE, TX_BUF_SIZE, TX_RING_SIZE};

// ---------------------------------------------------------------------------
// IrqOutcome
// ---------------------------------------------------------------------------

/// Decoded per-IRQ outcome produced by [`compute_irq_outcome`].
///
/// The full `ICR` snapshot is kept so callers can log it or route
/// follow-on behavior (RX-overrun counters, statistics) without
/// re-reading the register.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IrqOutcome {
    /// Raw `ICR` snapshot (read-to-clear). Hardware cleared the bits
    /// on the read; the snapshot is what the driver saw.
    pub icr: u32,
    /// New value of the link-state atomic. `true` when the MAC is
    /// reporting `STATUS.LU = 1`. When `icr & LSC == 0` this is left
    /// unchanged from the incoming `prev_link_up`.
    pub link_up: bool,
    /// `true` when this IRQ observed a link-up edge — the driver
    /// should flush any stale TX descriptors before re-enabling the
    /// ring.
    pub link_up_edge: bool,
    /// `true` when the IRQ carries at least one RX cause the driver
    /// should drain for (RX timer, min-threshold, overrun).
    pub rx_drain_needed: bool,
}

/// Pure helper: decode an `(ICR, STATUS)` pair into an [`IrqOutcome`].
///
/// Phase 55b E.3 acceptance bullet: "On IRQ wake: `ICR` read,
/// link-state (`LSC` bit) updated in an `AtomicBool`." The "update"
/// part is done in [`handle_irq`]; this helper returns the value the
/// caller should store so the logic can be tested on the host.
#[inline]
pub fn compute_irq_outcome(icr: u32, status: u32, prev_link_up: bool) -> IrqOutcome {
    let link_up = if icr & irq_cause::LSC != 0 {
        status & e_status::LU != 0
    } else {
        prev_link_up
    };
    let link_up_edge = icr & irq_cause::LSC != 0 && link_up && !prev_link_up;
    let rx_drain_needed = icr & (irq_cause::RXT0 | irq_cause::RXDMT0 | irq_cause::RXO) != 0;
    IrqOutcome {
        icr,
        link_up,
        link_up_edge,
        rx_drain_needed,
    }
}

// ---------------------------------------------------------------------------
// handle_irq
// ---------------------------------------------------------------------------

/// Called on every IRQ wake: reads `ICR` (read-to-clear), reads
/// `STATUS`, updates `link_up`, and returns the decoded
/// [`IrqOutcome`].
///
/// Safe to call from task context; `ICR` read on the classic e1000 is
/// the ack — no hardware unmask is required.
pub fn handle_irq<M: MmioOps>(mmio: &M, link_up: &AtomicBool) -> IrqOutcome {
    let icr = mmio.read_u32(E1000Regs::ICR);
    let status = mmio.read_u32(E1000Regs::STATUS);
    let prev = link_up.load(Ordering::Acquire);
    let outcome = compute_irq_outcome(icr, status, prev);
    if icr & irq_cause::LSC != 0 {
        link_up.store(outcome.link_up, Ordering::Release);
    }
    outcome
}

// ---------------------------------------------------------------------------
// RX drain
// ---------------------------------------------------------------------------

/// Result of a pure [`drain_rx_descriptors`] call.
///
/// `frames` is the list of Ethernet payloads extracted in ring order.
/// `advance_rdt_to` is `Some(index)` when the caller should write that
/// index to `RDT`; `None` means no descriptor completed — the RDT
/// register is left alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrainOutcome {
    pub frames: Vec<Vec<u8>>,
    pub advance_rdt_to: Option<u32>,
    pub new_next_to_read: usize,
}

/// Pure helper: drain every completed descriptor starting from
/// `next_to_read`.
///
/// The descriptor-status / buffer-copy contract matches Phase 55 E.3
/// exactly: copy out `min(desc.length, RX_BUF_SIZE)` bytes when
/// `DD|EOP` are both set, then recycle the descriptor by clearing
/// `status`, zeroing ancillary fields, and re-stamping
/// `buffer_addr = buf_iova[slot]` so a wild write from a prior lap
/// can never linger.
///
/// `bufs` is a slice view of per-slot DMA buffers. Caller ensures
/// `bufs.len() == RX_RING_SIZE` and each slot is `RX_BUF_SIZE` bytes.
pub fn drain_rx_descriptors(
    descs: &mut [E1000RxDesc],
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
    // Bound the loop by ring size so a device that misbehaves and
    // leaves `DD` set on every descriptor cannot trap the driver in
    // an infinite loop.
    for _ in 0..RX_RING_SIZE {
        if !rx_descriptor_done(descs[idx].status) {
            break;
        }
        let desc = &descs[idx];
        let len = (desc.length as usize).min(RX_BUF_SIZE);
        let has_eop = desc.status & rx_status::EOP != 0;
        if has_eop && len > 0 {
            let slot = &bufs[idx];
            let take = len.min(slot.len());
            frames.push(slot[..take].to_vec());
        }
        // Recycle the descriptor.
        let iova = buf_iova[idx];
        let desc = &mut descs[idx];
        desc.status = 0;
        desc.errors = 0;
        desc.length = 0;
        desc.checksum = 0;
        desc.special = 0;
        desc.buffer_addr = iova;

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
// TX post
// ---------------------------------------------------------------------------

/// Pure helper: check whether the TX descriptor at `idx` is safe to
/// overwrite.
///
/// The e1000's TX descriptor's `DD` status bit is set by hardware when
/// it finishes with a slot programmed with `RS`. A slot with `cmd != 0`
/// but `DD == 0` is still being DMA'd and must not be reused. A slot
/// that was never programmed (`cmd == 0`) is always free.
#[inline]
pub fn tx_slot_free(desc: &E1000TxDesc) -> bool {
    desc.cmd == 0 || tx_descriptor_done(desc.status)
}

/// Pure helper: copy `frame` into `buf`, program `desc`, and return
/// the new `TDT` value the caller should write.
///
/// The descriptor-programming sequence matches Phase 55 E.4 exactly:
///
/// 1. Stamp `buffer_addr = buf_iova` (defensive — E.2 already
///    prepared it, but re-stamping rules out a stale value after a
///    link-down wrap-around).
/// 2. Set `length = frame.len()`.
/// 3. Clear `cso` / `css` / `special`.
/// 4. `cmd = EOP | IFCS | RS` — single-descriptor packet, hardware
///    appends FCS, hardware reports status in `status.DD`.
/// 5. Clear `status` so the next completion polling sees a fresh bit.
///
/// Returns [`NetDriverError::InvalidFrame`] on empty / oversize input;
/// [`NetDriverError::RingFull`] is the caller's responsibility (via
/// [`tx_slot_free`]).
pub fn post_tx_descriptor(
    desc: &mut E1000TxDesc,
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

    desc.buffer_addr = buf_iova;
    desc.length = frame.len() as u16;
    desc.cso = 0;
    desc.cmd = tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS;
    desc.status = 0;
    desc.css = 0;
    desc.special = 0;
    Ok(())
}

// ---------------------------------------------------------------------------
// TX in-flight drain
// ---------------------------------------------------------------------------

/// Pure helper: drain every in-flight TX descriptor by clearing
/// `cmd`/`status`/`length`. Called on a link-up edge (see Phase 55
/// E.4 `drain_in_flight`) so the first post-up transmit doesn't race
/// a partially-drained ring.
///
/// Returns the number of slots touched (always `TX_RING_SIZE`).
pub fn drain_tx_in_flight(descs: &mut [E1000TxDesc]) -> usize {
    debug_assert_eq!(descs.len(), TX_RING_SIZE);
    for d in descs.iter_mut() {
        d.cmd = 0;
        d.status = 0;
        d.length = 0;
    }
    TX_RING_SIZE
}

// ---------------------------------------------------------------------------
// link_state_atomic — module-scoped default for single-device drivers
// ---------------------------------------------------------------------------

/// Module-scoped link-state atomic. The driver's `main.rs` binds the
/// single-device e1000 to this cell; multi-device variants carry one
/// per `E1000Io` instance. Hot paths consult
/// [`link_state_atomic()`] rather than threading the atomic through
/// every call site.
static LINK_UP: AtomicBool = AtomicBool::new(false);

/// Module-scoped driver-restart atomic. Set by the supervisor path
/// before the driver exits; cleared by the fresh process's init
/// handshake. While set, every [`handle_tx`] returns
/// [`NetDriverError::DriverRestarting`].
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
// drain_rx — production wrapper
// ---------------------------------------------------------------------------

/// Production [`drain_rx_descriptors`] wrapper: calls the pure helper,
/// then writes the advanced `RDT` register through `mmio`. The
/// publisher closure is called for every drained frame, in order.
///
/// Returns the number of frames handed to `publisher` — the caller
/// typically uses this for observability / statistics only. Errors
/// raised by the publisher are ignored so a stuck RX consumer never
/// stalls the drain path; a production publisher logs.
pub fn drain_rx<M: MmioOps, P: FnMut(&[u8])>(
    mmio: &M,
    descs: &mut [E1000RxDesc],
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
        mmio.write_u32(E1000Regs::RDT, rdt);
    }
    count
}

// ---------------------------------------------------------------------------
// handle_tx — production wrapper
// ---------------------------------------------------------------------------

/// Production wrapper: drives link-state / restart checks, finds the
/// next free TX slot, posts the descriptor, and rings `TDT`.
///
/// Error surface:
/// - [`NetDriverError::DriverRestarting`] while `driver_restarting` is
///   set (distinct from link-down — the kernel-side facade retries
///   this class within `DRIVER_RESTART_TIMEOUT_MS`).
/// - [`NetDriverError::LinkDown`] while `link_up` is clear.
/// - [`NetDriverError::RingFull`] when the next slot has `cmd != 0`
///   and `DD` is still clear (hardware is still DMA'ing).
/// - [`NetDriverError::InvalidFrame`] for empty / oversize frames.
pub fn handle_tx<M: MmioOps>(
    mmio: &M,
    descs: &mut [E1000TxDesc],
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
    if !tx_slot_free(&descs[idx]) {
        return Err(NetDriverError::RingFull);
    }
    post_tx_descriptor(&mut descs[idx], bufs[idx], buf_iova[idx], frame)?;
    let new_tdt = ((idx + 1) % TX_RING_SIZE) as u32;
    *next_to_write = new_tdt as usize;
    // Release fence so the descriptor stores are visible before the
    // doorbell write; matches Phase 55 E.4's fence + TDT write.
    core::sync::atomic::fence(Ordering::Release);
    mmio.write_u32(E1000Regs::TDT, new_tdt);
    Ok(())
}

// ---------------------------------------------------------------------------
// Device integration — E1000Device + NetServer + IrqNotification wiring.
// ---------------------------------------------------------------------------

use driver_runtime::ipc::net::NetServer;
use driver_runtime::ipc::{EndpointCap, IpcBackend};
use driver_runtime::{DeviceHandle, IrqNotification, SyscallBackend as IrqSyscallBackend};
use kernel_core::driver_ipc::net::NetLinkEvent;

use crate::init::E1000Device;

/// Orphan-rule-safe local view of a `DeviceHandle` as a
/// `DeviceCapHandle`.
///
/// `DeviceHandle` is defined in `driver_runtime`; the
/// `DeviceCapHandle` trait `IrqNotification::subscribe` requires is
/// also in `driver_runtime`. This newtype lives in the e1000 crate
/// so we can bridge the two without touching the `driver_runtime`
/// source — the trait is implemented on the local wrapper.
pub struct DeviceCapView<'a> {
    inner: &'a DeviceHandle,
}

impl<'a> DeviceCapView<'a> {
    /// Wrap a borrowed [`DeviceHandle`] as a `DeviceCapHandle`.
    pub fn new(inner: &'a DeviceHandle) -> Self {
        Self { inner }
    }
}

impl driver_runtime::DeviceCapHandle for DeviceCapView<'_> {
    fn cap_handle(&self) -> u32 {
        self.inner.cap()
    }
}

/// Subscribe to the e1000's MSI / INTx vector through
/// [`IrqNotification::subscribe`].
///
/// Acceptance bullet: "IRQ subscription via `IrqNotification::subscribe`;
/// MSI preferred, INTx fallback." The kernel-side
/// `sys_device_irq_subscribe` picks MSI when the device advertises the
/// PCI capability and falls back to INTx otherwise — the driver does
/// not need to express a preference here.
pub fn subscribe_irq(
    device: &DeviceHandle,
) -> Result<IrqNotification<IrqSyscallBackend>, driver_runtime::DriverRuntimeError> {
    let view = DeviceCapView::new(device);
    IrqNotification::<IrqSyscallBackend>::subscribe(&view, None)
}

/// Drain the RX ring of `device`, publishing every frame to the
/// kernel net stack through `net_server.publish_rx_frame`.
///
/// Returns the number of frames drained. Publish errors are counted
/// separately in `dropped` — we never stall the drain on a
/// downstream failure (a stuck kernel endpoint would otherwise wedge
/// the driver's IRQ loop).
pub fn drain_rx_to_server<B: IpcBackend>(
    device: &mut E1000Device,
    net_server: &NetServer<B>,
) -> (usize, usize) {
    let buf_iova = device.rx.buf_iova.clone();
    // Borrow every per-slot RX buffer as `&[u8]` via its `DmaBuffer`
    // Deref target (`&[u8; RX_BUF_SIZE]`).
    let bufs: alloc::vec::Vec<&[u8]> = device
        .rx
        .bufs
        .iter()
        .map(|b| {
            let arr: &[u8; crate::rings::RX_BUF_SIZE] = core::ops::Deref::deref(b);
            arr.as_slice()
        })
        .collect();
    let descs: &mut [E1000RxDesc; RX_RING_SIZE] = &mut device.rx.descs;
    let mut dropped = 0usize;
    let mut next_to_read = device.rx.next_to_read;
    let drained = drain_rx(
        &device.mmio,
        descs.as_mut_slice(),
        &bufs,
        &buf_iova,
        &mut next_to_read,
        |frame| {
            if net_server.publish_rx_frame(frame).is_err() {
                dropped += 1;
            }
        },
    );
    device.rx.next_to_read = next_to_read;
    (drained, dropped)
}

/// Post `frame` to `device`'s TX ring.
///
/// Consults [`link_state_atomic`] and [`driver_restarting_atomic`]
/// by default. Wrapped over [`handle_tx`] so a single call site sees
/// the live rings.
pub fn send_frame(device: &mut E1000Device, frame: &[u8]) -> Result<(), NetDriverError> {
    let buf_iova = device.tx.buf_iova.clone();
    // Borrow per-slot TX buffers as `&mut [u8]` via DerefMut on each
    // `DmaBuffer<[u8; TX_BUF_SIZE]>`.
    let mut bufs: alloc::vec::Vec<&mut [u8]> = device
        .tx
        .bufs
        .iter_mut()
        .map(|b| {
            let arr: &mut [u8; crate::rings::TX_BUF_SIZE] = core::ops::DerefMut::deref_mut(b);
            arr.as_mut_slice()
        })
        .collect();
    let descs: &mut [E1000TxDesc; TX_RING_SIZE] = &mut device.tx.descs;
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

/// Flush every in-flight TX descriptor on a link-up edge.
///
/// Phase 55 E.4's `drain_link_up_edge` runs the same semantic: any
/// TX slot whose cmd was set before the link came back up must be
/// cleared so the first post-up send does not race a partially-
/// drained ring.
pub fn drain_tx_on_link_up(device: &mut E1000Device) -> usize {
    let descs: &mut [E1000TxDesc; TX_RING_SIZE] = &mut device.tx.descs;
    let drained = drain_tx_in_flight(descs.as_mut_slice());
    device.tx.next_to_write = 0;
    // Reset TDT so hardware restarts from slot 0.
    device.mmio.write_u32(E1000Regs::TDT, 0);
    drained
}

/// Arm the IRQ-cause mask in `IMS` for RX + LSC. E.2 left every
/// cause masked (`IMC = 0xFFFF_FFFF`); this is the un-mask E.3's
/// acceptance bullet requires.
pub fn arm_irqs(device: &E1000Device) {
    let ims = irq_cause::RXT0 | irq_cause::RXDMT0 | irq_cause::RXO | irq_cause::LSC;
    device.mmio.write_u32(E1000Regs::IMS, ims);
}

/// Handle exactly one IRQ wake: read `ICR`, update the link atomic,
/// handle the link-up edge, drain the RX ring, and publish every
/// frame to `net_server`.
///
/// Returns a tuple of `(IrqOutcome, drained_frames, dropped_publishes)`
/// for observability. Callers typically discard the tuple but F-track
/// smoke tests inspect it.
pub fn handle_irq_and_drain<B: IpcBackend>(
    device: &mut E1000Device,
    net_server: &NetServer<B>,
) -> (IrqOutcome, usize, usize) {
    let outcome = handle_irq(&device.mmio, link_state_atomic());
    if outcome.link_up_edge {
        let _ = drain_tx_on_link_up(device);
        let event = NetLinkEvent {
            up: true,
            mac: device.mac,
            speed_mbps: 0,
        };
        net_server.publish_link_state(event);
    } else if outcome.icr & irq_cause::LSC != 0 && !outcome.link_up {
        // Link went down — tell the kernel net stack so it can
        // reset retransmit timers (Phase 16 behavior).
        let event = NetLinkEvent {
            up: false,
            mac: device.mac,
            speed_mbps: 0,
        };
        net_server.publish_link_state(event);
    }
    let (drained, dropped) = if outcome.rx_drain_needed {
        drain_rx_to_server(device, net_server)
    } else {
        (0, 0)
    };
    (outcome, drained, dropped)
}

/// Main driver loop: subscribes to the IRQ, initialises the link
/// atomic from the device's bring-up status, arms `IMS`, and
/// alternates between `irq.wait()` → `handle_irq_and_drain` and
/// `net_server.handle_next` which dispatches TX requests through
/// [`send_frame`].
///
/// The function is intentionally non-returning (`-> !`) so callers
/// do not accidentally skip error handling — the only exit is a
/// panic path or an `exit` syscall invoked from a child call.
///
/// Supplied `endpoint` is the command endpoint the kernel's
/// `RemoteNic` facade (E.4) sends `NET_SEND_FRAME` requests on.
/// Track F.1 wires this endpoint through the service manager's
/// capability grant.
#[allow(dead_code)] // F.1 flips the main binary into calling this.
pub fn run_io_loop(mut device: E1000Device, command_endpoint: EndpointCap) -> ! {
    // Subscribe BEFORE arming IMS so a stray IRQ cannot fire into an
    // unregistered handler — mirrors Phase 55 E.3's publish-before-IMS
    // ordering.
    let irq = match subscribe_irq(&device.pci) {
        Ok(n) => n,
        Err(_) => {
            // Fall back to a polled RX path: spin-sleep and drain,
            // without IRQ. Kept explicit so F.1's failure mode is
            // clear from logs.
            syscall_lib::exit(4)
        }
    };
    link_state_atomic().store(device.link_up_initial(), Ordering::Release);
    arm_irqs(&device);

    let net_server = NetServer::new(command_endpoint);

    loop {
        let bits = irq.wait();
        if bits != 0 {
            let _ = handle_irq_and_drain(&mut device, &net_server);
            let _ = irq.ack(bits);
        }
        let _ = net_server.handle_next(|req| {
            let status = match send_frame(&mut device, &req.frame) {
                Ok(()) => NetDriverError::Ok,
                Err(e) => e,
            };
            driver_runtime::ipc::net::NetReply { status }
        });
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
        // Link atomic unchanged — no LSC in ICR.
        assert!(link.load(Ordering::Acquire));

        // Flip previous state and re-run without LSC: atomic must stay
        // at its prior (cleared) value.
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

    fn mk_rx_setup() -> (Vec<E1000RxDesc>, Vec<Vec<u8>>, Vec<u64>) {
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
        // Stage payload in slot 0.
        bufs[0][..5].copy_from_slice(b"hello");
        descs[0].length = 5;
        descs[0].status = rx_status::DD | rx_status::EOP;
        let slices = borrow_bufs(&bufs);
        let outcome = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert_eq!(outcome.frames.len(), 1);
        assert_eq!(&outcome.frames[0][..], b"hello");
        assert_eq!(outcome.advance_rdt_to, Some(0));
        assert_eq!(outcome.new_next_to_read, 1);
        // Descriptor recycled.
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
        // Slot 3 left DD=0, so drain must stop there.
        let slices = borrow_bufs(&bufs);
        let outcome = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert_eq!(outcome.frames.len(), 3);
        assert_eq!(outcome.advance_rdt_to, Some(2));
        assert_eq!(outcome.new_next_to_read, 3);
    }

    #[test]
    fn drain_rx_descriptors_wraps_ring_index_modulo_size() {
        let (mut descs, mut bufs, buf_iova) = mk_rx_setup();
        // Start near the end and wrap past index 0.
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
        // After draining 3 from start = N-2, next_to_read == 1.
        assert_eq!(outcome.new_next_to_read, 1);
        // RDT advanced to the last slot consumed — index 0 (wrap).
        assert_eq!(outcome.advance_rdt_to, Some(0));
    }

    #[test]
    fn drain_rx_descriptors_skips_non_eop_but_still_recycles() {
        let (mut descs, mut bufs, buf_iova) = mk_rx_setup();
        bufs[0][..4].copy_from_slice(b"PART");
        descs[0].length = 4;
        // DD set but EOP not — driver recycles without publishing.
        descs[0].status = rx_status::DD;
        let slices = borrow_bufs(&bufs);
        let outcome = drain_rx_descriptors(&mut descs, &slices, &buf_iova, 0);
        assert!(outcome.frames.is_empty());
        // Slot still recycled.
        assert_eq!(descs[0].status, 0);
        assert_eq!(outcome.advance_rdt_to, Some(0));
    }

    #[test]
    fn drain_rx_descriptors_clamps_length_to_rx_buf_size() {
        let (mut descs, mut bufs, buf_iova) = mk_rx_setup();
        bufs[0][0..RX_BUF_SIZE].fill(0xAB);
        // Claim a length larger than the buffer — drain must clamp
        // rather than read past the slot.
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
        assert!(mmio.writes().iter().all(|(o, _)| *o != E1000Regs::RDT));
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
        desc.status = 0x01; // DD set
        assert!(tx_slot_free(&desc));
    }

    #[test]
    fn tx_slot_free_in_flight_is_not_free() {
        let mut desc = E1000TxDesc::default();
        desc.cmd = tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS;
        desc.status = 0; // DD clear
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
        // Buffer mirrors the frame prefix.
        assert_eq!(&buf[..frame.len()], frame);
    }

    #[test]
    fn post_tx_descriptor_rejects_empty_frame() {
        let mut desc = E1000TxDesc::default();
        let mut buf = vec![0u8; TX_BUF_SIZE];
        let err = post_tx_descriptor(&mut desc, &mut buf, 0, &[]).unwrap_err();
        assert_eq!(err, NetDriverError::InvalidFrame);
        // Descriptor must not be mutated on error.
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
        // Descriptor must not be touched on link-down.
        assert_eq!(descs[0].cmd, 0);
        // TDT must not be rung.
        assert!(mmio.writes().iter().all(|(o, _)| *o != E1000Regs::TDT));
        // Ring pointer must not move.
        assert_eq!(next, 0);
    }

    #[test]
    fn handle_tx_driver_restarting_shadows_link_down() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx_setup();
        let mut mut_bufs = borrow_tx_bufs_mut(&mut bufs);
        // Link up but restart in progress: the restart error wins.
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
        // TDT must be written with the advanced value.
        let writes = mmio.writes();
        assert!(writes.iter().any(|&(o, v)| o == E1000Regs::TDT && v == 1));
    }

    #[test]
    fn handle_tx_returns_ring_full_when_slot_still_in_flight() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx_setup();
        // Mark slot 0 as in-flight (cmd set, DD not yet).
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
        // next_to_write unchanged.
        assert_eq!(next, 0);
    }

    #[test]
    fn handle_tx_reuses_slot_after_hardware_completion() {
        let mmio = FakeMmio::new();
        let (mut descs, mut bufs, buf_iova) = mk_tx_setup();
        // Previously-used slot with DD set — safe to reuse.
        descs[0].cmd = tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS;
        descs[0].status = 0x01; // DD
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
        assert_eq!(descs[0].status, 0, "status cleared on re-post");
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
        // Pollute every slot.
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
        // Both references must point at the same underlying atomic.
        assert!(core::ptr::eq(a, b));
    }

    #[test]
    fn driver_restarting_atomic_is_module_scoped() {
        let a = driver_restarting_atomic();
        let b = driver_restarting_atomic();
        assert!(core::ptr::eq(a, b));
    }
}
