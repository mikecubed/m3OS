//! Phase 78c — xHCI USB IPC server.
//!
//! After bring-up + enumeration the xHCI driver becomes a live IPC server: it
//! registers the [`USB_SERVICE_NAME`] service, binds its controller IRQ into
//! the command endpoint (`sys_notif_bind`), and serves [`UsbRequest`]s from
//! class drivers (the `usb-hid` daemon).
//!
//! # Request shapes and hardware waits
//!
//! After enumeration the server binds its controller IRQ into the endpoint and
//! runs the request loop. HID Boot-Protocol setup is **not** done here up front
//! — the `usb-hid` class driver issues `SET_PROTOCOL(0)` / `SET_IDLE(0)` itself
//! via [`UsbRequest::ControlRequest`], so the server serves two request shapes
//! with different blocking behaviour:
//!
//! - [`UsbRequest::PollInterruptIn`] is **non-blocking**: on each bound IRQ wake
//!   the server drains the event ring, capturing interrupt-IN reports and
//!   re-arming the endpoint, and the poll just returns whatever was captured.
//! - [`UsbRequest::ControlRequest`] runs a **real EP0 control transfer** whose
//!   `control_transfer` path waits on the IRQ notification (`notify_wait`).
//!   This handler therefore **does** block on hardware even though the IRQ is
//!   already bound. It is safe because the server is single-threaded:
//!   `notify_wait` drains the same `PENDING` word the bound `ipc_recv_msg`
//!   does, and the server is never in both at once.

use alloc::vec::Vec;

use driver_runtime::IrqNotification;
use driver_runtime::ipc::{EndpointCap, IpcBackend, RecvResult, SyscallBackend};
use kernel_core::usb::descriptor::{CLASS_HID, TRANSFER_TYPE_BULK, TRANSFER_TYPE_INTERRUPT};
use kernel_core::usb::enumerate::EnumContext;
use kernel_core::usb::xhci::trb::dci;
use syscall_lib::STDOUT_FILENO;
use syscall_lib::write_str;
use usb_core::protocol::{AttachNotice, USB_REPLY_LABEL, UsbReply, UsbRequest};

use crate::controller::Controller;

/// Emitted once the server has registered the `usb` service and bound its IRQ
/// into the command endpoint — i.e. it is ready to accept requests. This does
/// **not** imply HID setup is complete: the `usb-hid` class driver performs
/// `SET_PROTOCOL(0)` / `SET_IDLE(0)` itself via `ControlRequest`, which may run
/// *after* this sentinel. The `usb-smoke` gate waits on it before injecting
/// keys, but it is a server-readiness marker, not a HID-setup ordering
/// guarantee.
pub const USB_SERVER_READY_SENTINEL: &str = "XHCI_USB:server-ready\n";

// errno-style codes carried by `UsbReply::Error`.
const EINVAL: u16 = 22;
const ENOSYS: u16 = 38;

/// Build an [`AttachNotice`] from a Configured enumeration result if the device
/// exposes a surfaceable interface: a HID interface with an interrupt-IN
/// endpoint (Phase 78c), or — Phase 96 — any interface exposing a bulk IN+OUT
/// endpoint pair (USB-Ethernet / mass-storage class drivers). The device's
/// `idVendor`/`idProduct` are included so a class driver can match a specific
/// device (e.g. Realtek `0bda:8156`) without a `GetDescriptors` round-trip.
/// Returns `None` for a device with no surfaceable interface.
pub fn device_info_from_ctx(ctx: &EnumContext) -> Option<AttachNotice> {
    /// USB vendor-specific class. A composite USB-Ethernet dongle (e.g. the
    /// RTL8156) exposes both this — the *native* interface a vendor driver like
    /// `ure` drives with raw Realtek framing — and a CDC/RNDIS interface
    /// (`class=0xE0`/`0x02`). They have identical bulk IN+OUT pairs, but the
    /// native datapath only works on the vendor one, so we must prefer it.
    const CLASS_VENDOR_SPECIFIC: u8 = 0xFF;

    let cfg = ctx.parsed_config.as_ref()?;
    let (vendor_id, product_id) = ctx
        .device_descriptor
        .as_ref()
        .map(|d| (d.id_vendor, d.id_product))
        .unwrap_or((0, 0));

    // Score each surfaceable interface and keep the best. Priority:
    //   3 = HID with an interrupt-IN endpoint (keyboard/mouse)
    //   2 = vendor-specific interface with a bulk IN+OUT pair (native NIC)
    //   1 = any other interface with a bulk IN+OUT pair (CDC/RNDIS fallback)
    // Returning the *first* surfaceable interface (the old behaviour) handed a
    // class driver whichever interface the device happened to list first — for
    // an RNDIS-first dongle that was the wrong one, so the NIC never linked.
    let mut best: Option<(u8, AttachNotice)> = None;

    for iface in &cfg.interfaces {
        let i = &iface.interface;
        let mut ep_in_dci = 0u8;
        let mut ep_in_mps = 0u16;
        let mut ep_in_interval = 0u8;
        let mut bulk_in_dci = 0u8;
        let mut bulk_in_mps = 0u16;
        let mut bulk_out_dci = 0u8;
        let mut bulk_out_mps = 0u16;

        for ep in &iface.endpoints {
            let is_in = ep.b_endpoint_address & 0x80 != 0;
            let ep_num = ep.b_endpoint_address & 0x0F;
            match (ep.transfer_type(), is_in) {
                (TRANSFER_TYPE_INTERRUPT, true) if ep_in_dci == 0 => {
                    ep_in_dci = dci(ep_num, true);
                    ep_in_mps = ep.w_max_packet_size;
                    ep_in_interval = ep.b_interval;
                }
                (TRANSFER_TYPE_BULK, true) if bulk_in_dci == 0 => {
                    bulk_in_dci = dci(ep_num, true);
                    bulk_in_mps = ep.w_max_packet_size;
                }
                (TRANSFER_TYPE_BULK, false) if bulk_out_dci == 0 => {
                    bulk_out_dci = dci(ep_num, false);
                    bulk_out_mps = ep.w_max_packet_size;
                }
                _ => {}
            }
        }

        let hid_surfaceable = i.b_interface_class == CLASS_HID && ep_in_dci != 0;
        let bulk_surfaceable = bulk_in_dci != 0 && bulk_out_dci != 0;
        let priority = if hid_surfaceable {
            3
        } else if bulk_surfaceable && i.b_interface_class == CLASS_VENDOR_SPECIFIC {
            2
        } else if bulk_surfaceable {
            1
        } else {
            continue;
        };

        if best.as_ref().is_none_or(|(p, _)| priority > *p) {
            best = Some((
                priority,
                AttachNotice {
                    port: ctx.port,
                    slot_id: ctx.slot_id,
                    interface_class: i.b_interface_class,
                    interface_sub_class: i.b_interface_sub_class,
                    interface_protocol: i.b_interface_protocol,
                    attached: true,
                    ep_in_dci,
                    ep_in_mps,
                    ep_in_interval,
                    interface_num: i.b_interface_number,
                    vendor_id,
                    product_id,
                    bulk_in_dci,
                    bulk_in_mps,
                    bulk_out_dci,
                    bulk_out_mps,
                },
            ));
        }
    }
    best.map(|(_, notice)| notice)
}

/// A brought-up controller plus the IRQ notification and enumerated devices it
/// owns. The server multiplexes the request loop across a `Vec` of these.
pub type ControllerCtx = (Controller, IrqNotification, Vec<AttachNotice>);

/// Pack a `(controller index, hardware slot id)` pair into the single `u8`
/// `slot_id` field the [`AttachNotice`] protocol carries. The top two bits index
/// the controller (up to 4) and the low six bits the slot. For controller 0 the
/// handle equals the raw slot id, so the single-controller path (and the
/// QEMU smoke gates) are byte-for-byte unchanged. xHCI assigns the few attached
/// devices small slot ids (1..N), well within six bits.
///
/// **Fails closed**: returns `None` when the pair cannot be encoded losslessly
/// (controller index > 3 or slot id > 63) rather than silently truncating into
/// a colliding handle. A colliding handle would route the device's later
/// control/bulk transfers to a *different* controller/slot — so an unencodable
/// device is dropped (not served) at the call site instead of misrouted.
fn pack_handle(ctrl_idx: usize, slot_id: u8) -> Option<u8> {
    if ctrl_idx > 0b11 || slot_id > 0x3F {
        return None;
    }
    Some(((ctrl_idx as u8) << 6) | (slot_id & 0x3F))
}

/// Inverse of [`pack_handle`]: recover `(controller index, hardware slot id)`.
fn unpack_handle(handle: u8) -> (usize, u8) {
    ((handle >> 6) as usize, handle & 0x3F)
}

/// Run the xHCI USB IPC server across every brought-up controller. Never returns.
///
/// `ep` is the command endpoint, already registered under [`USB_SERVICE_NAME`]
/// by `program_main` *before* the slow per-port enumeration ran — so the
/// `usb-hid` class driver's bounded `ipc_wait_service("usb")` succeeds promptly
/// and its first `NextAttach` simply blocks on the IPC rendezvous until this
/// loop is reached (no 10 s service-wait timeout fires while enumeration runs).
///
/// The kernel binds at most one notification per task ([`sys_notif_bind`]), so
/// only the primary controller's IRQ wakes the recv loop. Non-primary
/// controllers are serviced opportunistically: every loop wake (a primary IRQ
/// *or* any inbound IPC request) drains **all** controllers' event rings. Since
/// the HID and NIC class drivers poll their IN endpoints, each poll arrives as
/// an IPC message that wakes the loop and re-drains every controller, so devices
/// on a non-primary controller are served without their own bound IRQ wake.
pub fn run(ep: u32, discovered: u8, mut controllers: Vec<ControllerCtx>) -> ! {
    // Build the merged device table. Each device's client-facing `slot_id` is
    // rewritten to a global handle that encodes its owning controller, so a
    // request the client later sends routes back to the right controller.
    let mut served: Vec<AttachNotice> = Vec::new();
    for (ctrl_idx, (_c, _irq, devices)) in controllers.iter().enumerate() {
        for d in devices {
            let mut notice = *d;
            match pack_handle(ctrl_idx, notice.slot_id) {
                Some(handle) => {
                    notice.slot_id = handle;
                    served.push(notice);
                }
                None => {
                    // Fail closed: a (controller, slot) pair that doesn't fit
                    // the 1-byte handle (>=4 controllers, or slot > 63) is
                    // dropped here rather than packed into a colliding handle
                    // that would misroute its transfers. Surface the drop so it
                    // is diagnosable instead of a silently-missing device.
                    write_str(
                        STDOUT_FILENO,
                        "xhci_driver: WARNING dropped device — unpackable handle (ctrl=",
                    );
                    crate::write_u8_dec(ctrl_idx as u8);
                    write_str(STDOUT_FILENO, " slot=");
                    crate::write_u8_dec(notice.slot_id);
                    write_str(STDOUT_FILENO, ")\n");
                }
            }
        }
    }

    // Bind only the primary controller's IRQ — the kernel stores one bound
    // notification per task. Non-primary controllers are drained by polling on
    // every loop wake (see the fn-level doc).
    let ep_cap = EndpointCap::new(ep);
    if let Some((_c, irq, _d)) = controllers.first()
        && irq.bind_to_endpoint(ep_cap).is_err()
    {
        write_str(STDOUT_FILENO, "xhci_driver: irq bind_to_endpoint failed\n");
        syscall_lib::exit(22);
    }

    write_str(STDOUT_FILENO, USB_SERVER_READY_SENTINEL);

    let mut backend = SyscallBackend::new();
    loop {
        match backend.recv(ep_cap) {
            Ok(RecvResult::Notification(bits)) => {
                for (c, _irq, _d) in controllers.iter_mut() {
                    c.service_interrupt_events();
                }
                if let Some((_c, irq, _d)) = controllers.first() {
                    let _ = irq.ack(bits);
                }
            }
            Ok(RecvResult::Message(frame)) => {
                // Drain every controller's event ring so polled devices on a
                // non-primary controller observe their completions before we
                // answer this request.
                for (c, _irq, _d) in controllers.iter_mut() {
                    c.service_interrupt_events();
                }
                let reply = handle_request(&mut controllers, discovered, &served, &frame.bulk);
                let bytes = reply.encode();
                // Fail closed: if staging the reply bulk fails, reply with the
                // `u64::MAX` sentinel label so the client's `usb_call` returns
                // `None` instead of decoding a stale/empty pending bulk as a
                // valid `UsbReply`. Mirrors the `kbd_server` / `mouse_server`
                // `ipc_store_reply_bulk`-failure path.
                if backend.store_reply_bulk(&bytes).is_err() {
                    write_str(
                        STDOUT_FILENO,
                        "xhci_driver: store_reply_bulk failed; replying with sentinel\n",
                    );
                    let _ = backend.reply(u64::MAX, 0);
                } else {
                    let _ = backend.reply(USB_REPLY_LABEL, 0);
                }
            }
            Err(_) => {
                // Transient recv error — re-loop rather than exit the daemon.
            }
        }
    }
}

/// Decode and serve one request, producing the reply.
///
/// `PollInterruptIn` returns whatever the IRQ path has captured (non-blocking).
/// `ControlRequest` runs a real EP0 control transfer — `control_transfer` waits
/// on the IRQ notification via `notify_wait`, which drains the same `PENDING`
/// word the bound `ipc_recv_msg` does, so it works correctly inside the bound
/// loop (the server is single-threaded — it is never in both at once).
fn handle_request(
    controllers: &mut [ControllerCtx],
    discovered: u8,
    served: &[AttachNotice],
    bulk: &[u8],
) -> UsbReply {
    let Some(req) = UsbRequest::decode(bulk) else {
        return UsbReply::Error { code: EINVAL };
    };
    // Resolve the `(controller, irq)` that owns a client-supplied slot handle.
    // `None` for an out-of-range controller index (a malformed/stale handle).
    macro_rules! owner {
        ($handle:expr) => {{
            let (ctrl_idx, real_slot) = unpack_handle($handle);
            match controllers.get_mut(ctrl_idx) {
                Some((c, irq, _d)) => Some((c, &*irq, real_slot)),
                None => None,
            }
        }};
    }
    match req {
        UsbRequest::NextAttach { cursor } => UsbReply::Attach {
            notice: served.get(cursor as usize).copied(),
        },
        UsbRequest::PollInterruptIn {
            slot_id,
            dci: target_dci,
            ..
        } => match owner!(slot_id)
            .and_then(|(c, _irq, slot)| c.take_interrupt_report(slot, target_dci))
        {
            Some(data) => UsbReply::InterruptReport {
                data,
                completion_code: 1,
            },
            None => UsbReply::InterruptReport {
                data: Vec::new(),
                completion_code: 0,
            },
        },
        UsbRequest::ControlRequest {
            slot_id,
            setup,
            length,
        } => match owner!(slot_id)
            .and_then(|(c, irq, slot)| c.control_request(irq, slot, setup, length))
        {
            Some(data) => UsbReply::ControlData {
                data,
                completion_code: 1,
            },
            None => UsbReply::ControlData {
                data: Vec::new(),
                completion_code: 0xFF,
            },
        },
        UsbRequest::ControlWrite {
            slot_id,
            setup,
            data,
        } => match owner!(slot_id)
            .and_then(|(c, irq, slot)| c.control_write(irq, slot, setup, &data))
        {
            // OUT control transfers carry no device-to-host data; the status is
            // the completion code. An empty `ControlData` mirrors the IN path.
            Some(_) => UsbReply::ControlData {
                data: Vec::new(),
                completion_code: 1,
            },
            None => UsbReply::ControlData {
                data: Vec::new(),
                completion_code: 0xFF,
            },
        },
        UsbRequest::PollBulkIn {
            slot_id,
            dci: target_dci,
            len,
        } => match owner!(slot_id)
            .and_then(|(c, _irq, slot)| c.take_bulk_report(slot, target_dci, len as u32))
        {
            Some(data) => UsbReply::BulkData {
                data,
                completion_code: 1,
            },
            None => UsbReply::BulkData {
                data: Vec::new(),
                completion_code: 0,
            },
        },
        UsbRequest::SubmitBulkOut {
            slot_id,
            dci: target_dci,
            data,
        } => match owner!(slot_id)
            .and_then(|(c, irq, slot)| c.submit_bulk_out(irq, slot, target_dci, &data))
        {
            Some(transferred) => UsbReply::TransferComplete {
                transferred,
                completion_code: 1,
            },
            None => UsbReply::TransferComplete {
                transferred: 0,
                completion_code: 0xFF,
            },
        },
        UsbRequest::Topology => {
            // Snapshot every brought-up controller's root-hub ports live. The
            // per-controller port count records the bring-up set; each connected
            // (CCS) port is listed with its speed so a bare-metal heartbeat can
            // localize a missing device to discovery vs bring-up vs enumeration.
            let mut port_counts: Vec<u8> = Vec::with_capacity(controllers.len());
            let mut ports: Vec<usb_core::protocol::TopoPort> = Vec::new();
            for (ctrl_idx, (c, _irq, _d)) in controllers.iter().enumerate() {
                let max = c.max_ports();
                port_counts.push(max);
                for port in 1..=max {
                    let flags = c.port_status_flags(port);
                    // bit0 = CCS: only surface connected ports to keep the line
                    // short enough to photograph off the framebuffer.
                    if flags & 0x01 != 0 {
                        ports.push(usb_core::protocol::TopoPort {
                            ctrl: ctrl_idx as u8,
                            port,
                            flags,
                        });
                    }
                }
            }
            UsbReply::Topology {
                discovered,
                port_counts,
                ports,
            }
        }
        // GetDescriptors / ConfigureEndpoints / SubmitTransfer are not needed by
        // the live HID-boot path (descriptors are pre-resolved into AttachNotice
        // during enumeration; endpoints are configured at bring-up). The
        // page-grant `SubmitTransfer` path remains for Phase 90 (mass storage).
        _ => UsbReply::Error { code: ENOSYS },
    }
}
