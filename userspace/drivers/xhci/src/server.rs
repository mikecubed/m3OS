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
use kernel_core::usb::descriptor::{
    CLASS_AUDIO, CLASS_HID, CLASS_HUB, CLASS_VIDEO, TRANSFER_TYPE_BULK, TRANSFER_TYPE_INTERRUPT,
    TRANSFER_TYPE_ISOCH,
};
use kernel_core::usb::enumerate::EnumContext;
use kernel_core::usb::xhci::trb::dci;
use syscall_lib::STDOUT_FILENO;
use syscall_lib::write_str;
use usb_core::protocol::{AttachNotice, USB_MSG_MAX, USB_REPLY_LABEL, UsbReply, UsbRequest};

use crate::controller::{Controller, PortChange};
use crate::handle::{pack_handle, unpack_handle};

/// Emitted once the server has registered the `usb` service and bound its IRQ
/// into the command endpoint — i.e. it is ready to accept requests. This does
/// **not** imply HID setup is complete: the `usb-hid` class driver performs
/// `SET_PROTOCOL(0)` / `SET_IDLE(0)` itself via `ControlRequest`, which may run
/// *after* this sentinel. The `usb-smoke` gate waits on it before injecting
/// keys, but it is a server-readiness marker, not a HID-setup ordering
/// guarantee.
pub const USB_SERVER_READY_SENTINEL: &str = "XHCI_USB:server-ready\n";

/// Emitted when a hot-plugged device is enumerated and published as a new
/// `AttachNotice` at runtime (Phase 92 Track C). The `usb-hotplug-smoke` gate
/// waits on this after a QMP `device_add`.
pub const USB_HOTPLUG_ATTACHED_SENTINEL: &str = "USB_HOTPLUG:attached\n";
/// Emitted when a device disconnect is observed, its `AttachNotice` flipped to
/// `attached: false`, and its slot reclaimed via Disable Slot (Phase 92 C.2/C.4).
pub const USB_HOTPLUG_DETACHED_SENTINEL: &str = "USB_HOTPLUG:detached\n";

// errno-style codes carried by `UsbReply::Error`.
const EINVAL: u16 = 22;
const ENOSYS: u16 = 38;

/// Steady-state interrupter-moderation interval applied to every controller
/// once the server loop is reached (Phase 92d). In `IMODI` units of 250 ns,
/// so `4000 × 250 ns = 1 ms` — at most one interrupt per millisecond per
/// controller, coalescing bulk-completion storms. Delivery to class drivers is
/// poll-driven and unaffected; this only thins redundant interrupt wakes (see
/// `Controller::set_interrupt_moderation`). 1 ms also matches the USB HID
/// reporting interval, so a mouse/keyboard is never interrupted faster than it
/// reports. Bring-up keeps `IMOD = 0` (prompt) — this is applied afterwards.
const IMOD_STEADY_STATE_INTERVAL: u16 = 4000;

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
        // Phase 92c Track E: track whether this interface carries an
        // isochronous OUT endpoint (the UAC PCM-out streaming endpoint) or an
        // isochronous IN endpoint (the UVC frame-capture endpoint).
        let mut isoch_out_present = false;
        let mut isoch_in_present = false;

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
                (TRANSFER_TYPE_ISOCH, false) => {
                    isoch_out_present = true;
                }
                (TRANSFER_TYPE_ISOCH, true) => {
                    isoch_in_present = true;
                }
                _ => {}
            }
        }

        let hid_surfaceable = i.b_interface_class == CLASS_HID && ep_in_dci != 0;
        let bulk_surfaceable = bulk_in_dci != 0 && bulk_out_dci != 0;
        // Phase 92c Track E: surface the UAC AudioStreaming interface (the alt
        // setting that carries an isochronous OUT endpoint) so the `usb-audio`
        // daemon can bind it via the `NextAttach` walk. The isoch endpoint has
        // neither an interrupt-IN nor a bulk pair, so without this branch the
        // device produces no `AttachNotice` at all. The notice carries the slot
        // + class; `usb-audio` resolves the isoch OUT DCI itself via a
        // `GetDescriptors` round-trip (the isoch endpoint fields are not on the
        // fixed-width AttachNotice wire format).
        let audio_surfaceable = i.b_interface_class == CLASS_AUDIO && isoch_out_present;
        // Phase 92c Track E.2: surface the UVC VideoStreaming interface (the alt
        // setting carrying a capture IN endpoint — isochronous, or bulk-IN with
        // no OUT pair) so the `usb-video` daemon can bind it via `NextAttach`.
        // Like the audio case, `usb-video` resolves the IN endpoint DCI itself
        // via `GetDescriptors`. (Bare-metal/VFIO-only — QEMU has no UVC model.)
        let video_surfaceable =
            i.b_interface_class == CLASS_VIDEO && (isoch_in_present || bulk_in_dci != 0);
        // Phase 92 Track A: surface CLASS_HUB interfaces so the `usbhub` daemon
        // can bind a hub via the `NextAttach` walk and drive it (GET_DESCRIPTOR
        // (Hub) + per-port PORT_POWER/PORT_RESET over EP0 `ControlRequest`). A hub
        // exposes a status-change interrupt-IN endpoint but no bulk pair, so it
        // scores below HID/NIC interfaces and is surfaced on its own low priority.
        let hub_surfaceable = i.b_interface_class == CLASS_HUB;
        let priority = if hid_surfaceable {
            3
        } else if bulk_surfaceable && i.b_interface_class == CLASS_VENDOR_SPECIFIC {
            2
        } else if bulk_surfaceable {
            1
        } else if hub_surfaceable {
            1
        } else if audio_surfaceable {
            1
        } else if video_surfaceable {
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

/// Run the Enable Slot → Address Device → Configure Endpoint sequence for a
/// freshly connected root-hub `port` (already reset to Enabled at `speed` by
/// the live event path) and return its `AttachNotice` if it surfaced an
/// interface. Reuses the *stateless* `run_enumeration` state machine the
/// bring-up path uses — no global controller re-init (Phase 92 C.3).
fn enumerate_port(
    controller: &mut Controller,
    irq: &IrqNotification,
    port: u8,
    speed: kernel_core::usb::xhci::port::PortSpeed,
) -> Option<AttachNotice> {
    use kernel_core::usb::enumerate::{EnumContext, EnumState, run_enumeration};
    let ctx = EnumContext {
        speed: Some(speed),
        port,
        ep0_ring_iova: 0,
        ..Default::default()
    };
    let mut ops = crate::enumerate::XhciHostOps::new(controller, irq);
    match run_enumeration(EnumState::EnableSlot, ctx, &mut ops) {
        (EnumState::Configured, final_ctx) => device_info_from_ctx(&final_ctx),
        _ => None,
    }
}

/// Map an `EnumerateChild` wire speed code (matching
/// `kernel_core::usb::hub::DOWNSTREAM_SPEED_*`) to a [`PortSpeed`]. Unknown
/// codes default to Full speed — the safe lowest-common-denominator that drives
/// the BSR two-step MaxPacketSize negotiation.
fn port_speed_from_code(code: u8) -> kernel_core::usb::xhci::port::PortSpeed {
    use kernel_core::usb::xhci::port::PortSpeed;
    match code {
        0 => PortSpeed::Low,
        2 => PortSpeed::High,
        3 => PortSpeed::Super,
        _ => PortSpeed::Full,
    }
}

/// Enumerate a device attached **behind a hub** (Phase 92a A.4/A.5).
///
/// Identical to [`enumerate_port`] except the device is addressed by the xHCI
/// **route string** (xHCI §8.9) threaded into the Slot Context dword0, with the
/// `root_hub_port` going into dword1 — the addressing a tier-2+ device requires.
/// The route string is computed by the `usbhub` walker from the topology tree.
fn enumerate_child(
    controller: &mut Controller,
    irq: &IrqNotification,
    route_string: u32,
    root_hub_port: u8,
    speed: kernel_core::usb::xhci::port::PortSpeed,
) -> Option<AttachNotice> {
    use kernel_core::usb::enumerate::{EnumContext, EnumState, run_enumeration};
    let ctx = EnumContext {
        speed: Some(speed),
        port: root_hub_port,
        route_string,
        ep0_ring_iova: 0,
        ..Default::default()
    };
    let mut ops = crate::enumerate::XhciHostOps::new(controller, irq);
    match run_enumeration(EnumState::EnableSlot, ctx, &mut ops) {
        (EnumState::Configured, final_ctx) => device_info_from_ctx(&final_ctx),
        _ => None,
    }
}

/// Drain each controller's queued root-hub port changes (Phase 92 Track C) and
/// act on them: enumerate + publish a newly-connected device, or mark a
/// departing device `attached: false` and reclaim its slot via Disable Slot.
/// Called on every server loop wake after the event rings are drained. The
/// `served` table is append-only so the `NextAttach` cursor stays stable — a
/// detach flips the existing entry's `attached` flag in place; a re-attach
/// pushes a fresh entry.
fn process_port_events(controllers: &mut [ControllerCtx], served: &mut Vec<AttachNotice>) {
    for (ctrl_idx, (c, irq, _devs)) in controllers.iter_mut().enumerate() {
        for ev in c.take_port_events() {
            match ev {
                PortChange::Connect { port, speed } => {
                    // Skip a port already served (bring-up enumerated the boot
                    // devices; a re-reported connect must not double-enumerate).
                    let already = served.iter().any(|n| {
                        n.attached && n.port == port && unpack_handle(n.slot_id).0 == ctrl_idx
                    });
                    if already {
                        continue;
                    }
                    if let Some(mut notice) = enumerate_port(c, irq, port, speed) {
                        match pack_handle(ctrl_idx, notice.slot_id) {
                            Some(handle) => {
                                notice.slot_id = handle;
                                write_str(STDOUT_FILENO, "[xhci] hot-plug attach port ");
                                crate::write_u8_dec(port);
                                write_str(STDOUT_FILENO, " class=");
                                crate::write_u8_dec(notice.interface_class);
                                write_str(STDOUT_FILENO, "\n");
                                served.push(notice);
                                write_str(STDOUT_FILENO, USB_HOTPLUG_ATTACHED_SENTINEL);
                            }
                            None => {
                                // H.5: Enable Slot already allocated a hardware
                                // slot + SlotContext for this device during
                                // `enumerate_port`. If the (controller, slot)
                                // pair can't be packed into the 1-byte handle
                                // (≥5 controllers, or slot > 63), reclaim that
                                // slot via Disable Slot before dropping the
                                // device — otherwise it leaks the very slot H.3
                                // set out to reclaim. `notice.slot_id` still
                                // holds the real hardware slot here (it is only
                                // overwritten with the packed handle in the
                                // `Some` arm). Like the Disconnect path, only
                                // claim "slot reclaimed" when Disable Slot
                                // actually succeeds; otherwise log the failure
                                // honestly so a real slot leak isn't masked.
                                if c.disable_slot(irq, notice.slot_id) {
                                    write_str(
                                        STDOUT_FILENO,
                                        "xhci_driver: hot-plug device dropped — unpackable handle (slot reclaimed)\n",
                                    );
                                } else {
                                    write_str(
                                        STDOUT_FILENO,
                                        "xhci_driver: hot-plug device dropped — unpackable handle; Disable Slot failed, slot not reclaimed\n",
                                    );
                                }
                            }
                        }
                    }
                }
                PortChange::Disconnect { port } => {
                    if let Some(pos) = served.iter().position(|n| {
                        n.attached && n.port == port && unpack_handle(n.slot_id).0 == ctrl_idx
                    }) {
                        let real_slot = unpack_handle(served[pos].slot_id).1;
                        served[pos].attached = false;
                        write_str(STDOUT_FILENO, "[xhci] hot-plug detach port ");
                        crate::write_u8_dec(port);
                        write_str(STDOUT_FILENO, "\n");
                        // Reclaim the slot so a re-attach gets a fresh slot id (H.3).
                        // The hot-plug gate reads USB_HOTPLUG:detached as "slot
                        // reclaimed", so emit it only when Disable Slot actually
                        // succeeds; otherwise log and skip the sentinel so the gate
                        // fails instead of masking a slot-pool leak.
                        if c.disable_slot(irq, real_slot) {
                            write_str(STDOUT_FILENO, USB_HOTPLUG_DETACHED_SENTINEL);
                        } else {
                            write_str(
                                STDOUT_FILENO,
                                "[xhci] Disable Slot failed on detach — slot not reclaimed\n",
                            );
                        }
                    }
                }
            }
        }
    }
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

    // Phase 92d: now that bring-up (which wants IMOD=0 for a prompt first
    // interrupt) is done, apply steady-state interrupt moderation to every
    // controller so a sustained bulk stream coalesces its completions into
    // ~1 interrupt/ms instead of one wake per packet. Class-driver delivery is
    // poll-driven, so this only thins redundant interrupt wakes.
    for (c, _irq, _d) in controllers.iter() {
        c.set_interrupt_moderation(IMOD_STEADY_STATE_INTERVAL);
    }

    write_str(STDOUT_FILENO, USB_SERVER_READY_SENTINEL);

    let mut backend = SyscallBackend::new();
    loop {
        // Receive with a USB_MSG_MAX-sized bulk buffer, NOT the default
        // `recv`'s MAX_BULK_RECV (1522 B, the Ethernet MTU). A `SubmitBulkOut`
        // request carrying a multi-sector BOT WRITE(10) data-OUT (up to ~4089 B
        // of payload + 7 B header) would otherwise be truncated to 1522 B — the
        // server would then program a short bulk-OUT TRB while the CBW told the
        // device to expect the full length, so the device waits forever and the
        // transfer times out. This is what wedged the >1522-byte write path.
        match backend.recv_with_capacity(ep_cap, USB_MSG_MAX) {
            Ok(RecvResult::Notification(bits)) => {
                // Bit-directed draining (Phase 92d Optimization A): controller
                // `i`'s interrupt sets bit `i` in this shared notification (see
                // `Controller::init_interrupter_into`), so drain only the
                // controller(s) that actually fired rather than every controller.
                // For a single controller this is bit 0 → controller 0, so the
                // validated single-controller path is byte-identical. The
                // Message arm below still drains all controllers as a safety net,
                // so an event whose bit is not yet reflected is never lost — it
                // is picked up on the next client poll.
                for (idx, (c, _irq, _d)) in controllers.iter_mut().enumerate() {
                    // Guard the shift: only controllers 0..63 can own a bit in
                    // the 64-bit notification word. A controller at index ≥ 64
                    // (which the 2-bit slot-handle codec already can't address)
                    // is left to the Message-arm safety net below, which drains
                    // every controller — so its events are never lost, and
                    // `1u64 << idx` can never shift-overflow.
                    if idx < 64 && bits & (1u64 << idx) != 0 {
                        c.service_interrupt_events();
                    }
                }
                // A bound-IRQ wake is how a hot-plug Port Status Change arrives;
                // act on any queued connect/disconnect (Phase 92 Track C).
                process_port_events(&mut controllers, &mut served);
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
                // Also service any hot-plug events surfaced by the drain (a
                // class driver's poll wakes the loop between IRQs).
                process_port_events(&mut controllers, &mut served);
                let reply = handle_request(&mut controllers, discovered, &mut served, &frame.bulk);
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

/// Serve a control transfer on the controller owning `handle`, interleaving
/// servicing of the OTHER controllers during the transfer's bounded poll
/// (Phase 92d). A control transfer blocks the single server loop; without this,
/// a slow/dead-device control transfer on one controller would let a co-resident
/// controller (e.g. a busy NIC) overflow its finite event ring while we wait.
///
/// Splits `controllers` into the target (passed to `op`) and the rest (drained
/// by the `drain_others` callback `op` forwards into `control_request`/
/// `control_write` → `wait_for_transfer_event`). The split borrows are disjoint,
/// so the target's transfer and the others' ring drains never alias. Returns
/// `None` for an out-of-range controller index (a stale/forged handle).
fn serve_control<F>(controllers: &mut [ControllerCtx], handle: u8, op: F) -> Option<Vec<u8>>
where
    F: FnOnce(&mut Controller, &IrqNotification, u8, &mut dyn FnMut()) -> Option<Vec<u8>>,
{
    let (ctrl_idx, real_slot) = unpack_handle(handle);
    if ctrl_idx >= controllers.len() {
        return None;
    }
    let (before, rest) = controllers.split_at_mut(ctrl_idx);
    let (target, after) = rest.split_at_mut(1);
    let (c, irq, _d) = &mut target[0];
    let mut drain_others = || {
        for (oc, _oirq, _od) in before.iter_mut() {
            oc.service_interrupt_events();
        }
        for (oc, _oirq, _od) in after.iter_mut() {
            oc.service_interrupt_events();
        }
    };
    op(c, &*irq, real_slot, &mut drain_others)
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
    served: &mut Vec<AttachNotice>,
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
        // Phase 92 H.1: return the device + full configuration descriptor blobs
        // cached at enumeration. A full config exceeds the inline `ControlData`
        // clamp, so this is how a Report-Protocol HID / CDC-ECM class driver
        // reads the descriptors `AttachNotice` does not carry.
        UsbRequest::GetDescriptors { slot_id } => {
            match owner!(slot_id).and_then(|(c, _irq, slot)| c.cached_descriptors(slot)) {
                // H.6: reject a Descriptors reply that would exceed USB_MSG_MAX
                // (a device reporting a hostile `wTotalLength` could otherwise
                // amplify into a ~64 KiB blob). Encode overhead is tag(1) +
                // device len-prefix(2) + config len-prefix(2) = 5 bytes. Fails
                // closed with an explicit error rather than client-side truncation.
                Some((device, config)) if device.len() + config.len() + 5 > USB_MSG_MAX => {
                    write_str(
                        STDOUT_FILENO,
                        "[xhci] GetDescriptors over USB_MSG_MAX — rejected\n",
                    );
                    UsbReply::Error { code: EINVAL }
                }
                Some((device, config)) => UsbReply::Descriptors { device, config },
                None => UsbReply::Error { code: EINVAL },
            }
        }
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
        } => {
            // H.6: a `length` that would overflow the inline ControlData reply
            // (tag(1) + completion_code(1) + u16 len-prefix(2) = 4 bytes of
            // overhead) is rejected with an explicit error instead of a
            // silently-truncated reply or an oversized EP0 scratch allocation.
            // The usb-hid Report-descriptor fetch already clamps its (untrusted,
            // device-declared) length; this fail-closed guard mirrors the
            // SubmitBulkIn/GetDescriptors checks so no future ControlRequest
            // caller can forward an out-of-bounds length across the boundary.
            if length as usize + 4 > USB_MSG_MAX {
                write_str(
                    STDOUT_FILENO,
                    "[xhci] ControlRequest length over USB_MSG_MAX — rejected\n",
                );
                UsbReply::Error { code: EINVAL }
            } else {
                match serve_control(controllers, slot_id, |c, irq, slot, drain| {
                    c.control_request(irq, slot, setup, length, drain)
                }) {
                    Some(data) => UsbReply::ControlData {
                        data,
                        completion_code: 1,
                    },
                    None => UsbReply::ControlData {
                        data: Vec::new(),
                        completion_code: 0xFF,
                    },
                }
            }
        }
        UsbRequest::ControlWrite {
            slot_id,
            setup,
            data,
        } => match serve_control(controllers, slot_id, |c, irq, slot, drain| {
            c.control_write(irq, slot, setup, &data, drain)
        }) {
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
        // Phase 92c Track E — isochronous-OUT for USB audio (UAC PCM-out). One
        // service interval of PCM, scheduled SIA (Start Isoch ASAP); a ring-
        // underrun / missed interval is non-fatal (no retry) and the submitted
        // bytes are still counted as delivered, rather than failing.
        UsbRequest::SubmitIsochOut {
            slot_id,
            dci: target_dci,
            data,
        } => match owner!(slot_id)
            .and_then(|(c, irq, slot)| c.submit_isoch_out(irq, slot, target_dci, &data))
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
        // Phase 92 Track D — synchronous single-TRB bulk-IN for the BOT data +
        // CSW phases (no streaming auto-re-arm; see `Controller::submit_bulk_in`).
        UsbRequest::SubmitBulkIn {
            slot_id,
            dci: target_dci,
            len,
        } => {
            // H.6: a `len` that would overflow the inline BulkData reply
            // (tag(1) + completion_code(1) + u16 len-prefix(2) = 4 bytes of
            // overhead) is rejected with an explicit error instead of a
            // silently-truncated BulkData.
            if len as usize + 4 > USB_MSG_MAX {
                write_str(
                    STDOUT_FILENO,
                    "[xhci] SubmitBulkIn len over USB_MSG_MAX — rejected\n",
                );
                UsbReply::Error { code: EINVAL }
            } else {
                match owner!(slot_id)
                    .and_then(|(c, irq, slot)| c.submit_bulk_in(irq, slot, target_dci, len as u32))
                {
                    Some(data) => UsbReply::BulkData {
                        data,
                        completion_code: 1,
                    },
                    None => UsbReply::BulkData {
                        data: Vec::new(),
                        completion_code: 0xFF,
                    },
                }
            }
        }
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
        // Phase 92a A.4/A.5: enumerate a device behind a hub. The `usbhub`
        // walker supplies the route string + root-hub port; we run the standard
        // enumeration against the route-addressed slot and publish the child so
        // a class driver's `NextAttach` walk discovers it.
        UsbRequest::EnumerateChild {
            parent_slot_id,
            route_string,
            root_hub_port,
            speed,
        } => {
            let (ctrl_idx, _parent_real) = unpack_handle(parent_slot_id);
            match controllers.get_mut(ctrl_idx) {
                Some((c, irq, _d)) => {
                    let ps = port_speed_from_code(speed);
                    match enumerate_child(c, &*irq, route_string, root_hub_port, ps) {
                        Some(mut notice) => match pack_handle(ctrl_idx, notice.slot_id) {
                            Some(handle) => {
                                let real_slot = notice.slot_id;
                                notice.slot_id = handle;
                                // Dedup: a repeated EnumerateChild for the same
                                // route must not push a second cursor entry.
                                if served.iter().any(|n| n.attached && n.slot_id == handle) {
                                    // Already published — reclaim the duplicate
                                    // slot we just enabled so it is not leaked.
                                    c.disable_slot(&*irq, real_slot);
                                } else {
                                    write_str(STDOUT_FILENO, "XHCI_HUB:child-enumerated class=");
                                    crate::write_u8_dec(notice.interface_class);
                                    write_str(STDOUT_FILENO, " port=");
                                    crate::write_u8_dec(root_hub_port);
                                    write_str(STDOUT_FILENO, "\n");
                                    served.push(notice);
                                }
                                UsbReply::Attach {
                                    notice: Some(notice),
                                }
                            }
                            None => {
                                // Reclaim the just-enabled slot so an unpackable
                                // handle does not leak it (H.3/H.5 discipline).
                                c.disable_slot(&*irq, notice.slot_id);
                                write_str(
                                    STDOUT_FILENO,
                                    "[xhci] tier-2 child dropped — unpackable handle\n",
                                );
                                UsbReply::Error { code: EINVAL }
                            }
                        },
                        None => UsbReply::Error { code: EINVAL },
                    }
                }
                None => UsbReply::Error { code: EINVAL },
            }
        }
        // Phase 92a H.4 — zero-copy bulk transfer over a shared-memory region.
        // The server IOMMU-maps the shm into the xHCI device domain and programs
        // a single bulk TRB straight at it (no inline USB_MSG_MAX copy), so a
        // transfer larger than the 4096-byte inline budget completes in one
        // descriptor. `dir_in` is informational — the endpoint's DCI selects the
        // direction.
        UsbRequest::SubmitShmTransfer {
            slot_id,
            dci: target_dci,
            shm_id,
            len,
            dir_in: _,
        } => {
            // Defense-in-depth: reject a caller-supplied `len` that exceeds the
            // shm region's actual size BEFORE any TRB is programmed at it.
            // Otherwise `len > shm_size(shm_id)` makes the controller DMA past
            // the end of the mapped region — and with `--iommu` off / identity
            // fallback there is no hardware backstop to contain the overrun. An
            // unknown/invalid shm_id (`shm_size` → None) is rejected too.
            if !matches!(syscall_lib::shm_size(shm_id), Some(sz) if (len as usize) <= sz) {
                UsbReply::Error { code: EINVAL }
            } else {
                match owner!(slot_id) {
                    Some((c, irq, slot)) => match c.map_shm(shm_id) {
                        Some(iova) => {
                            let res = c.submit_bulk_iova(irq, slot, target_dci, iova, len);
                            c.unmap_shm(iova);
                            match res {
                                Some(transferred) => UsbReply::TransferComplete {
                                    transferred,
                                    completion_code: 1,
                                },
                                None => UsbReply::TransferComplete {
                                    transferred: 0,
                                    completion_code: 0xFF,
                                },
                            }
                        }
                        None => UsbReply::Error { code: EINVAL },
                    },
                    None => UsbReply::Error { code: EINVAL },
                }
            }
        }
        // GetDescriptors / ConfigureEndpoints / SubmitTransfer (page-grant) are
        // not needed by the live paths — descriptors are pre-resolved into
        // AttachNotice during enumeration; endpoints are configured at bring-up.
        _ => UsbReply::Error { code: ENOSYS },
    }
}
