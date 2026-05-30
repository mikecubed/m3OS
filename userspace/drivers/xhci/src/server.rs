//! Phase 78c — xHCI USB IPC server.
//!
//! After bring-up + enumeration the xHCI driver becomes a live IPC server: it
//! registers the [`USB_SERVICE_NAME`] service, binds its controller IRQ into
//! the command endpoint (`sys_notif_bind`), and serves [`UsbRequest`]s from
//! class drivers (the `usb-hid` daemon).
//!
//! # Why the setup happens before the loop
//!
//! HID Boot-Protocol setup (`SET_PROTOCOL(0)` + `SET_IDLE(0)`) and interrupt-IN
//! arming use the **blocking** `control_transfer` path (`irq.wait()`), so they
//! run **before** the notification is bound into the endpoint. After binding,
//! the loop never blocks on hardware inside a request handler — exactly the
//! e1000 server discipline. On each bound IRQ wake it drains the event ring
//! (capturing interrupt-IN reports + re-arming); requests are served from that
//! captured state with no hardware wait. A class driver therefore polls
//! [`UsbRequest::PollInterruptIn`] and receives whatever the IRQ path captured.

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

/// One enumerated HID device the server exposes over `AttachNotice`, plus the
/// interface number used for its Boot-Protocol setup.
pub struct DeviceInfo {
    /// The attach notice handed to the class driver.
    pub notice: AttachNotice,
    /// `bInterfaceNumber` of the HID interface (wIndex for SET_PROTOCOL/IDLE).
    pub interface_num: u8,
}

/// Build a [`DeviceInfo`] from a Configured enumeration result if the device
/// exposes a HID interface with an interrupt-IN endpoint. Returns `None` for a
/// non-HID device or a HID interface lacking an interrupt-IN endpoint.
pub fn device_info_from_ctx(ctx: &EnumContext) -> Option<DeviceInfo> {
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
                return Some(DeviceInfo {
                    notice: AttachNotice {
                        port: ctx.port,
                        slot_id: ctx.slot_id,
                        interface_class: i.b_interface_class,
                        interface_sub_class: i.b_interface_sub_class,
                        interface_protocol: i.b_interface_protocol,
                        attached: true,
                        ep_in_dci: dci(ep_num, true),
                        ep_in_mps: ep.w_max_packet_size,
                        ep_in_interval: ep.b_interval,
                    },
                    interface_num: i.b_interface_number,
                });
            }
        }
    }
    None
}

/// Run the xHCI USB IPC server. Never returns.
pub fn run(mut controller: Controller, irq: IrqNotification, devices: Vec<DeviceInfo>) -> ! {
    // 1. Boot-protocol setup + interrupt-IN arm for each HID device. MUST run
    //    before binding the IRQ — it blocks on control transfers.
    for dev in &devices {
        controller.boot_protocol_setup(
            &irq,
            dev.notice.slot_id,
            dev.interface_num,
            dev.notice.ep_in_dci,
        );
    }

    // 2. Command endpoint + `usb` service registration.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        write_str(STDOUT_FILENO, "xhci_driver: server endpoint create failed\n");
        syscall_lib::exit(20);
    }
    let ep = ep as u32;
    if syscall_lib::ipc_register_service(ep, USB_SERVICE_NAME) == u64::MAX {
        write_str(STDOUT_FILENO, "xhci_driver: register 'usb' service failed\n");
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
                let reply = handle_request(&mut controller, &devices, &frame.bulk);
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

/// Decode and serve one request, producing the reply. Handlers never block on
/// hardware: `PollInterruptIn` returns whatever the IRQ path has captured.
fn handle_request(controller: &mut Controller, devices: &[DeviceInfo], bulk: &[u8]) -> UsbReply {
    let Some(req) = UsbRequest::decode(bulk) else {
        return UsbReply::Error { code: EINVAL };
    };
    match req {
        UsbRequest::NextAttach { cursor } => UsbReply::Attach {
            notice: devices.get(cursor as usize).map(|d| d.notice),
        },
        UsbRequest::PollInterruptIn {
            dci: target_dci, ..
        } => match controller.take_interrupt_report(target_dci) {
            Some(data) => UsbReply::InterruptReport {
                data,
                completion_code: 1,
            },
            None => UsbReply::InterruptReport {
                data: Vec::new(),
                completion_code: 0,
            },
        },
        // The live 1.0 HID path issues SET_PROTOCOL/SET_IDLE via the server's
        // pre-bind boot_protocol_setup, so these aren't needed live. Reply with
        // a typed ENOSYS rather than blocking inside the bound loop.
        _ => UsbReply::Error { code: ENOSYS },
    }
}
