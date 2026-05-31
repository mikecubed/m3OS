//! r8169 RX/TX servicing loop (Track C.1).
//!
//! The Realtek C+ ring differs from the e1000 legacy ring — ownership lives in
//! the descriptor `opts1` OWN bit (not a status DD byte) and TX is nudged via
//! the TxPoll doorbell (not a tail register) — so this is a Realtek-specific
//! loop rather than a reuse of `e1000::io::run_io_loop`. It mirrors the same
//! Phase 55c bound-notification pattern: a single `NetServer::handle_next`
//! multiplexes TX requests and the IRQ notification.

extern crate alloc;

use alloc::vec::Vec;

use driver_runtime::ipc::net::{NetReply, NetServer};
use driver_runtime::ipc::{EndpointCap, IpcBackend};
use driver_runtime::{DeviceCapHandle, IrqNotification, SyscallBackend as IrqSyscallBackend};
use kernel_core::driver_ipc::net::NetDriverError;
use kernel_core::r8169 as hw;

use crate::init::Nic;

/// Orphan-rule-safe view of the device handle as a `DeviceCapHandle` for IRQ
/// subscription (mirrors the e1000 `DeviceCapView`).
struct DeviceCapView {
    cap: u32,
}

impl DeviceCapHandle for DeviceCapView {
    fn cap_handle(&self) -> u32 {
        self.cap
    }
}

/// Drain every RX descriptor the NIC has handed back (OWN cleared), forwarding
/// each frame to `net_server` and re-arming the slot. Returns frames drained.
/// Bounded by the ring size so a misbehaving device cannot trap the loop.
pub fn drain_rx<B: IpcBackend>(nic: &mut Nic, net_server: &NetServer<B>) -> usize {
    let count = nic.rx.count;
    let mut collected: Vec<Vec<u8>> = Vec::new();
    for _ in 0..count {
        let slot = nic.rx.idx;
        let opts1 = nic.rx.opts1(slot);
        if hw::desc_is_owned_by_nic(opts1) {
            // NIC still owns this slot — nothing more to drain.
            break;
        }
        let len = hw::desc_frame_len(opts1) as usize;
        if len > 0 {
            collected.push(nic.rx.rx_slice(slot, len).to_vec());
        }
        nic.rx.rearm_rx(slot);
        nic.rx.idx = (slot + 1) % count;
    }
    let drained = collected.len();
    if drained > 0 {
        let refs: Vec<&[u8]> = collected.iter().map(|v| v.as_slice()).collect();
        let _ = net_server.publish_rx_frames(&refs);
    }
    drained
}

/// Post `frame` to the next free TX slot and ring the TxPoll doorbell.
///
/// A slot is free when the NIC has cleared its OWN bit (TX complete) or the slot
/// was never used. Returns `RingFull` when the slot is still NIC-owned.
pub fn send_frame(nic: &mut Nic, frame: &[u8]) -> Result<(), NetDriverError> {
    if frame.is_empty() {
        return Err(NetDriverError::InvalidFrame);
    }
    let slot = nic.tx.idx;
    let opts1 = nic.tx.opts1(slot);
    if hw::desc_is_owned_by_nic(opts1) {
        return Err(NetDriverError::RingFull);
    }
    if !nic.tx.post_tx(slot, frame) {
        return Err(NetDriverError::InvalidFrame);
    }
    nic.tx.idx = (slot + 1) % nic.tx.count;
    // Release fence so the descriptor stores land before the doorbell.
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    nic.kick_tx();
    Ok(())
}

/// Main driver loop: subscribe to the IRQ, bind it into the command endpoint's
/// recv path, and dispatch TX requests + IRQ wakes through one
/// `NetServer::handle_next` per iteration. Never returns.
pub fn run_io_loop(
    nic: Nic,
    command_endpoint: EndpointCap,
    ingress_endpoint: Option<EndpointCap>,
) -> ! {
    let view = DeviceCapView { cap: nic.pci.cap() };
    let irq = match IrqNotification::<IrqSyscallBackend>::subscribe(&view, None) {
        Ok(n) => n,
        Err(_) => syscall_lib::exit(4),
    };
    if irq.bind_to_endpoint(command_endpoint).is_err() {
        syscall_lib::exit(5);
    }

    let nic = core::cell::RefCell::new(nic);
    let net_server = match ingress_endpoint {
        Some(ep) => NetServer::new(command_endpoint).with_ingress_endpoint(ep),
        None => NetServer::new(command_endpoint),
    };

    loop {
        let irq_bits = core::cell::Cell::new(0u64);
        let _ = net_server.handle_next(
            |req| {
                let mut dev = nic.borrow_mut();
                let status = match send_frame(&mut dev, &req.frame) {
                    Ok(()) => NetDriverError::Ok,
                    Err(e) => e,
                };
                NetReply { status }
            },
            |bits| {
                irq_bits.set(bits);
            },
        );

        let bits = irq_bits.get();
        if bits != 0 {
            let mut dev = nic.borrow_mut();
            // Ack the device's interrupt status, then drain RX.
            let _isr = dev.ack_isr();
            let _ = drain_rx(&mut dev, &net_server);
            drop(dev);
            let _ = irq.ack(bits);
        }
    }
}
