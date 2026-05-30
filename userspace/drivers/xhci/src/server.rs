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
use kernel_core::usb::descriptor::{CLASS_HID, TRANSFER_TYPE_INTERRUPT};
use kernel_core::usb::enumerate::EnumContext;
use kernel_core::usb::xhci::trb::dci;
use syscall_lib::STDOUT_FILENO;
use syscall_lib::write_str;
use usb_core::protocol::{AttachNotice, USB_REPLY_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest};

use crate::controller::Controller;

/// Emitted once the server is registered, the IRQ is bound, and HID setup is
/// complete. The `usb-smoke` gate can wait on this before injecting keys.
pub const USB_SERVER_READY_SENTINEL: &str = "XHCI_USB:server-ready\n";

// errno-style codes carried by `UsbReply::Error`.
const EINVAL: u16 = 22;
const ENOSYS: u16 = 38;

/// Build an [`AttachNotice`] from a Configured enumeration result if the device
/// exposes a HID interface with an interrupt-IN endpoint. Returns `None` for a
/// non-HID device or a HID interface lacking an interrupt-IN endpoint.
pub fn device_info_from_ctx(ctx: &EnumContext) -> Option<AttachNotice> {
    let cfg = ctx.parsed_config.as_ref()?;
    for iface in &cfg.interfaces {
        let i = &iface.interface;
        if i.b_interface_class != CLASS_HID {
            continue;
        }
        for ep in &iface.endpoints {
            let is_in = ep.b_endpoint_address & 0x80 != 0;
            if ep.transfer_type() == TRANSFER_TYPE_INTERRUPT && is_in {
                let ep_num = ep.b_endpoint_address & 0x0F;
                return Some(AttachNotice {
                    port: ctx.port,
                    slot_id: ctx.slot_id,
                    interface_class: i.b_interface_class,
                    interface_sub_class: i.b_interface_sub_class,
                    interface_protocol: i.b_interface_protocol,
                    attached: true,
                    ep_in_dci: dci(ep_num, true),
                    ep_in_mps: ep.w_max_packet_size,
                    ep_in_interval: ep.b_interval,
                    interface_num: i.b_interface_number,
                });
            }
        }
    }
    None
}

/// Run the xHCI USB IPC server. Never returns.
pub fn run(mut controller: Controller, irq: IrqNotification, devices: Vec<AttachNotice>) -> ! {
    // 1. Command endpoint + `usb` service registration. The `usb-hid` daemon
    //    issues SET_PROTOCOL(0)/SET_IDLE(0) itself via `ControlRequest`, and the
    //    interrupt-IN endpoint arms lazily on the first `PollInterruptIn`, so no
    //    pre-bind hardware setup is needed here.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        write_str(
            STDOUT_FILENO,
            "xhci_driver: server endpoint create failed\n",
        );
        syscall_lib::exit(20);
    }
    let ep = ep as u32;
    if syscall_lib::ipc_register_service(ep, USB_SERVICE_NAME) == u64::MAX {
        write_str(
            STDOUT_FILENO,
            "xhci_driver: register 'usb' service failed\n",
        );
        syscall_lib::exit(21);
    }

    // 3. Bind the controller IRQ into the endpoint so one recv loop multiplexes
    //    IPC requests and transfer-completion IRQ wakes.
    let ep_cap = EndpointCap::new(ep);
    if irq.bind_to_endpoint(ep_cap).is_err() {
        write_str(STDOUT_FILENO, "xhci_driver: irq bind_to_endpoint failed\n");
        syscall_lib::exit(22);
    }

    write_str(STDOUT_FILENO, USB_SERVER_READY_SENTINEL);

    let mut backend = SyscallBackend::new();
    loop {
        match backend.recv(ep_cap) {
            Ok(RecvResult::Notification(bits)) => {
                controller.service_interrupt_events();
                let _ = irq.ack(bits);
            }
            Ok(RecvResult::Message(frame)) => {
                let reply = handle_request(&mut controller, &irq, &devices, &frame.bulk);
                let bytes = reply.encode();
                let _ = backend.store_reply_bulk(&bytes);
                let _ = backend.reply(USB_REPLY_LABEL, 0);
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
    controller: &mut Controller,
    irq: &IrqNotification,
    devices: &[AttachNotice],
    bulk: &[u8],
) -> UsbReply {
    let Some(req) = UsbRequest::decode(bulk) else {
        return UsbReply::Error { code: EINVAL };
    };
    match req {
        UsbRequest::NextAttach { cursor } => UsbReply::Attach {
            notice: devices.get(cursor as usize).copied(),
        },
        UsbRequest::PollInterruptIn {
            slot_id,
            dci: target_dci,
            ..
        } => match controller.take_interrupt_report(slot_id, target_dci) {
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
        } => match controller.control_request(irq, slot_id, setup, length) {
            Some(data) => UsbReply::ControlData {
                data,
                completion_code: 1,
            },
            None => UsbReply::ControlData {
                data: Vec::new(),
                completion_code: 0xFF,
            },
        },
        // GetDescriptors / ConfigureEndpoints / SubmitTransfer are not needed by
        // the live HID-boot path (descriptors are pre-resolved into AttachNotice
        // during enumeration; endpoints are configured at bring-up). Served live
        // in Phase 90 (USB Class Expansion: hub child-device config + bulk mass
        // storage) — typed ENOSYS for now.
        _ => UsbReply::Error { code: ENOSYS },
    }
}
