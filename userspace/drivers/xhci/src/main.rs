//! Ring-3 xHCI USB host-controller driver — Phase 78a (host-controller
//! bring-up) + Phase 78b Track A-glue (live device enumeration to Configured).
//!
//! Phase 78a stands the controller up: claim the `qemu-xhci` controller,
//! map BAR0, discover the register regions, perform the BIOS/OS handoff and
//! controller reset, program the DCBAA + scratchpad + command ring + event
//! ring (ERST), wire an MSI-X interrupter, set the controller running, and
//! reach a first `Enable Slot` Command Completion event delivered off the
//! event ring **by interrupt**.
//!
//! Phase 78b Track A-glue drives `run_enumeration` from `kernel_core` against
//! the real `qemu-xhci` + `usb-kbd` hardware, printing the full descriptor
//! tree on success and emitting `XHCI_ENUM:configured` as the load-bearing
//! acceptance sentinel.
//!
//! # Module layout
//!
//! | Module | Purpose |
//! |---|---|
//! | [`regs`] (kernel-core) | Capability-register decoders (host-tested) |
//! | [`trb`] (kernel-core)  | TRB encode/decode + cycle bit + DCI (host-tested) |
//! | [`port`] (kernel-core) | PORTSC bit logic + speed→MPS (host-tested) |
//!
//! The pure-logic layer lives in `kernel_core::usb::xhci`; this crate is the
//! MMIO / DMA / IRQ glue that the host-test layer cannot cover.
//!
//! # Run-time flow
//!
//! 1. `program_main` claims [`SENTINEL_BDF`] and maps BAR0. A missing device
//!    (QEMU launched without `-device qemu-xhci`) is logged and the process
//!    exits cleanly so the service manager marks it permanently stopped
//!    rather than burning its restart budget.
//! 2. The capability registers are discovered and `[xhci] N ports detected`
//!    is emitted.
//! 3. Bring-up (reset → DCBAA/scratchpad/contexts → rings → MSI-X → run →
//!    Enable Slot) runs; on the first interrupt-delivered Command Completion
//!    the driver emits [`ENABLE_SLOT_OK_SENTINEL`].
//! 4. For the connected root-hub port, `run_enumeration` drives the full
//!    xHCI enumeration sequence to Configured; on success the descriptor tree
//!    is printed and [`XHCI_ENUM_CONFIGURED_SENTINEL`] is emitted.
//! 5. `event_loop` runs indefinitely handling hotplug/interrupt endpoint events.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

/// Pure controller-multiplexing slot handle codec. Compiled in *both* configs
/// (no driver-runtime deps) so its round-trip + fail-closed behaviour is
/// host-unit-testable even though `server` — its only production caller — is
/// `#[cfg(not(test))]`.
mod handle;

/// Bring-up glue (MMIO / DMA / IRQ) around the host-tested
/// `kernel_core::usb::xhci` pure logic. Compiled only for the OS target —
/// it speaks the syscall ABI and has no host-test surface.
#[cfg(not(test))]
mod controller;

/// Phase 78b Track A-glue: real `UsbHostOps` implementation over the
/// Controller's DMA rings.
#[cfg(not(test))]
mod enumerate;

/// Phase 78c: the live USB IPC server (registers `usb`, binds the IRQ, serves
/// `UsbRequest`s from class drivers, captures HID reports).
#[cfg(not(test))]
mod server;

#[cfg(not(test))]
use crate::controller::{BringUpError, Controller, XhciBar0};
#[cfg(not(test))]
use core::alloc::Layout;
#[cfg(not(test))]
use driver_runtime::{
    DeviceCapKey, DeviceHandle, DriverRuntimeError, IrqNotification, Mmio, enumerate_pci_class,
};
#[cfg(not(test))]
use kernel_core::device_host::DeviceHostError;
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "xhci_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "xhci_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Boot-log marker written when the driver scaffold starts.
pub const BOOT_LOG_MARKER: &str = "xhci_driver: spawned\n";

/// Sentinel emitted on the first interrupt-delivered `Enable Slot` Command
/// Completion event. The `xhci-bringup-smoke` gate (Track C.1) asserts this
/// exact line; a `[xhci] N ports detected` line alone is **not** sufficient
/// for PASS. The spelling is load-bearing.
pub const ENABLE_SLOT_OK_SENTINEL: &str = "XHCI_BRINGUP:enable-slot:OK\n";

/// Sentinel emitted when the first USB device connected to the root hub
/// reaches Configured state after a full enumeration sequence. The
/// `xhci-enum-smoke` gate asserts this exact line. The spelling is
/// load-bearing.
pub const XHCI_ENUM_CONFIGURED_SENTINEL: &str = "XHCI_ENUM:configured\n";

/// Fallback PCI BDF used when `sys_device_pci_enumerate` returns no matches.
///
/// QEMU assigns this address to `-device qemu-xhci,addr=0x6` under m3OS
/// (bus 0, device 6, function 0). In the normal QEMU boot the real-hardware
/// path (`enumerate_pci_class`) discovers this controller and claims it
/// directly — the fallback is exercised only when the enumeration syscall
/// returns no results (e.g. on a platform that lacks xHCI or when the
/// syscall is unavailable to the caller). This is an interim bootstrap; a
/// future phase will remove it once `sys_device_pci_enumerate` is available
/// on all supported platforms.
///
/// Slot +6 is the next free slot after the net (3), nvme (4), and audio (5)
/// family — see the AC'97 device comment in `xtask/src/main.rs`.
#[cfg(not(test))]
const SENTINEL_BDF: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x06, 0);

/// PCI class/subclass/prog_if triple that identifies an xHCI USB host controller.
/// Class 0x0C = Serial Bus Controller, subclass 0x03 = USB, prog_if 0x30 = xHCI.
#[cfg(not(test))]
const XHCI_CLASS: u8 = 0x0C;
#[cfg(not(test))]
const XHCI_SUBCLASS: u8 = 0x03;
#[cfg(not(test))]
const XHCI_PROG_IF: u8 = 0x30;

/// BAR0 length the driver asks the kernel to map. The xHCI register space
/// (Capability + Operational + Runtime + Doorbell + the MSI-X table) fits
/// comfortably in 64 KiB on `qemu-xhci`; the kernel maps the actual BAR
/// size and this bound only governs the wrapper's debug bounds-check.
#[cfg(not(test))]
const BAR0_EXPECTED_BYTES: usize = 0x1_0000;

/// Per-controller bring-up, port scan, and USB enumeration.
///
/// Claims `key`, maps BAR0, runs the full xHCI bring-up sequence (BIOS
/// handoff → reset → DCBAA/scratchpad → rings → MSI-X → run), scans
/// ports, and if a device is connected runs the full USB enumeration state
/// machine to Configured.
///
/// Returns `Ok((controller, irq))` when the controller is fully up and ready
/// for the event loop, or `Err(exit_code)` when a fatal error occurred (the
/// caller should exit with that code).
///
/// The EXACT bring-up ordering from Phase 78a is preserved unchanged. Both
/// sentinel lines (`XHCI_BRINGUP:enable-slot:OK` and `XHCI_ENUM:configured`)
/// are emitted here, maintaining gate compatibility.
#[cfg(not(test))]
fn bring_up_controller(
    key: DeviceCapKey,
    shared: Option<(u32, u8)>,
) -> Result<
    (
        Controller,
        IrqNotification,
        alloc::vec::Vec<usb_core::protocol::AttachNotice>,
    ),
    i32,
> {
    let mut devices: alloc::vec::Vec<usb_core::protocol::AttachNotice> = alloc::vec::Vec::new();
    let handle = match DeviceHandle::claim(key) {
        Ok(h) => h,
        // The controller is not available to us. `NotClaimed` (ENODEV — QEMU
        // launched without `-device qemu-xhci`) and `AlreadyClaimed` (EBUSY —
        // the slot is occupied by an unrelated device) both mean "no xHCI
        // here"; exit cleanly so init's `on-failure` policy stops the service
        // rather than restarting against a device that will never appear.
        Err(DriverRuntimeError::Device(
            DeviceHostError::NotClaimed | DeviceHostError::AlreadyClaimed,
        )) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "xhci_driver: no controller at BDF — exiting cleanly\n",
            );
            return Err(0);
        }
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "xhci_driver: device claim failed\n");
            return Err(3);
        }
    };

    let bar0 = match Mmio::<XhciBar0>::map(&handle, 0, BAR0_EXPECTED_BYTES) {
        Ok(m) => m,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "xhci_driver: BAR0 map failed\n");
            return Err(4);
        }
    };

    // Log the claimed BDF (segment:bus:dev.func).
    syscall_lib::write_str(STDOUT_FILENO, "[xhci] claimed ");
    write_bdf(key);
    syscall_lib::write_str(STDOUT_FILENO, "\n");

    // Discover the register regions + capabilities (A.2).
    let mut controller = Controller::new(handle, bar0);
    write_ports_detected(controller.max_ports());
    // Context size (32 vs 64) is selected from HCCPARAMS1.CSZ and threaded
    // into all later context allocation; report it during discovery.
    syscall_lib::write_str(STDOUT_FILENO, "[xhci] context size ");
    write_u8_dec(controller.context_size() as u8);
    syscall_lib::write_str(STDOUT_FILENO, " bytes\n");

    // Ordered bring-up (A.3 checklist): handoff → reset(CNR) → MaxSlotsEn →
    // DCBAA(+scratchpad) → command ring → event ring(ERST) → MSI-X
    // interrupter → run → Enable Slot. Any stage failure exits non-zero so
    // the service manager observes it.
    if let Err(e) = controller.release_bios_ownership() {
        return Err(bringup_failed(e));
    }
    if let Err(e) = controller.reset() {
        return Err(bringup_failed(e));
    }
    controller.program_max_slots();
    if let Err(e) = controller.init_dcbaa() {
        return Err(bringup_failed(e));
    }
    if let Err(e) = controller.init_command_ring() {
        return Err(bringup_failed(e));
    }
    if let Err(e) = controller.init_event_ring() {
        return Err(bringup_failed(e));
    }
    // Subscribe the controller IRQ. The primary controller (`shared == None`)
    // allocates a fresh notification (bit 0) that the server binds to its recv
    // loop; a secondary controller subscribes its IRQ INTO the primary's
    // notification at `bit` (= its controller index), so the single recv loop
    // wakes on this controller's interrupt too (Phase 92d multiplexed path).
    let irq = match shared {
        None => controller.init_interrupter(),
        Some((notif_cap, bit)) => controller.init_interrupter_into(notif_cap, bit),
    };
    let irq = match irq {
        Ok(irq) => irq,
        Err(e) => return Err(bringup_failed(e)),
    };
    if let Err(e) = controller.run() {
        return Err(bringup_failed(e));
    }

    // A.7: reset every device already connected at the root hub (e.g. a
    // `usb-kbd` and a `usb-mouse` present at machine creation) so each port
    // reaches Enabled with its speed decoded. Phase 78c enumerates ALL of them.
    let connected = controller.scan_ports();

    // Milestone: the first Enable Slot is fired as part of run_enumeration
    // (which calls ops.enable_slot → issue_command_and_wait). The
    // XHCI_BRINGUP:enable-slot:OK sentinel is emitted inside
    // on_command_completion which runs within drain_for_command_completion.
    // This preserves the 78a gate's requirement.

    if connected.is_empty() {
        // No device connected: fire a standalone Enable Slot to satisfy the
        // 78a bringup-smoke gate (it expects XHCI_BRINGUP:enable-slot:OK).
        controller.enqueue_enable_slot();
    } else {
        use crate::enumerate::XhciHostOps;
        use kernel_core::usb::enumerate::{EnumContext, EnumState, run_enumeration};

        // Phase 78c: enumerate each connected port into its own slot context so
        // a keyboard AND mouse are both Configured and served.
        for (port_num, speed) in connected {
            syscall_lib::write_str(STDOUT_FILENO, "[xhci] starting enumeration on port ");
            write_u8_dec(port_num);
            syscall_lib::write_str(STDOUT_FILENO, "\n");

            // ep0_ring_iova is 0 here; enable_slot allocates the per-slot
            // context and the ops impl patches the real IOVA before Address
            // Device builds its Input Context.
            let ctx = EnumContext {
                speed: Some(speed),
                port: port_num,
                ep0_ring_iova: 0,
                ..Default::default()
            };

            let mut ops = XhciHostOps::new(&mut controller, &irq);
            let (final_state, final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);

            match final_state {
                EnumState::Configured => {
                    syscall_lib::write_str(STDOUT_FILENO, "[xhci] enumeration complete\n");
                    // Bare-metal diagnostic: m3OS has no hub driver, so any device
                    // behind a hub (e.g. a keyboard with a built-in USB hub) is
                    // invisible. Flag a hub loudly so the boot log explains a
                    // "missing" keyboard rather than leaving it a silent mystery.
                    let is_hub = final_ctx
                        .device_descriptor
                        .as_ref()
                        .map(|d| d.b_device_class == kernel_core::usb::descriptor::CLASS_HUB)
                        .unwrap_or(false)
                        || final_ctx
                            .parsed_config
                            .as_ref()
                            .map(|c| {
                                c.interfaces.iter().any(|i| {
                                    i.interface.b_interface_class
                                        == kernel_core::usb::descriptor::CLASS_HUB
                                })
                            })
                            .unwrap_or(false);
                    if is_hub {
                        syscall_lib::write_str(STDOUT_FILENO, "[xhci] *** USB HUB on port ");
                        write_u8_dec(port_num);
                        syscall_lib::write_str(
                            STDOUT_FILENO,
                            " — devices behind it are NOT enumerated (no hub driver yet)\n",
                        );
                    }
                    print_descriptor_tree(&final_ctx);
                    syscall_lib::write_str(STDOUT_FILENO, XHCI_ENUM_CONFIGURED_SENTINEL);
                    // Phase 78c/96: record any surfaceable interface (HID, or a
                    // bulk IN+OUT pair for the NIC) so the server can hand it to a
                    // class driver. The log line names the device so a bare-metal
                    // boot shows exactly what was surfaced (keyboard vs NIC vs …).
                    if let Some(info) = crate::server::device_info_from_ctx(&final_ctx) {
                        syscall_lib::write_str(STDOUT_FILENO, "[xhci] surfaced device vid=0x");
                        write_u16_hex(info.vendor_id);
                        syscall_lib::write_str(STDOUT_FILENO, " pid=0x");
                        write_u16_hex(info.product_id);
                        syscall_lib::write_str(STDOUT_FILENO, " class=");
                        write_u8_dec(info.interface_class);
                        syscall_lib::write_str(STDOUT_FILENO, " proto=");
                        write_u8_dec(info.interface_protocol);
                        syscall_lib::write_str(STDOUT_FILENO, "\n");
                        devices.push(info);
                    }
                }
                EnumState::Error { code } => {
                    syscall_lib::write_str(STDOUT_FILENO, "[xhci] enumeration error code ");
                    write_u8_dec(code);
                    syscall_lib::write_str(STDOUT_FILENO, "\n");
                }
                EnumState::Timeout => {
                    syscall_lib::write_str(STDOUT_FILENO, "[xhci] enumeration timeout\n");
                }
                _ => {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "[xhci] enumeration ended in unexpected state\n",
                    );
                }
            }
        }
    }

    Ok((controller, irq, devices))
}

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    // Step 1: discover xHCI controllers via PCI class enumeration.
    //
    // `enumerate_pci_class(0x0C, 0x03, 0x30)` issues `sys_device_pci_enumerate`
    // which scans the kernel's PCI device list for controllers with class
    // 0x0C (Serial Bus), subclass 0x03 (USB), prog_if 0x30 (xHCI). In QEMU
    // the single `qemu-xhci` controller at 0000:00:06.0 is class 0x0C0330
    // and will be returned in the list.
    let discovered = enumerate_pci_class(XHCI_CLASS, XHCI_SUBCLASS, XHCI_PROG_IF)
        .unwrap_or_else(|_| alloc::vec![]);
    // Count of controllers discovered via PCI, reported in the `Topology`
    // diagnostic so a bare-metal heartbeat distinguishes "controller failed
    // bring-up and was skipped" (discovered > up) from "never discovered".
    let discovered_count = discovered.len() as u8;

    // Determine the ordered list of BDFs to bring up.
    let bdf_list: alloc::vec::Vec<DeviceCapKey> = if !discovered.is_empty() {
        // Real-hardware path: enumeration found controllers.
        syscall_lib::write_str(STDOUT_FILENO, "[xhci] discovered ");
        write_u8_dec(discovered.len() as u8);
        syscall_lib::write_str(STDOUT_FILENO, " controller(s) via PCI class enumeration\n");
        for key in &discovered {
            syscall_lib::write_str(STDOUT_FILENO, "[xhci]  controller at ");
            write_bdf(*key);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
        }
        discovered
    } else {
        // Fallback path: enumeration returned no results. This may happen if
        // the syscall returned -EACCES (exec-path check failed), returned 0
        // matches (no xHCI on this platform), or the driver was started before
        // the PCI scan populated the kernel's device list.
        syscall_lib::write_str(
            STDOUT_FILENO,
            "[xhci] PCI enumeration empty — falling back to sentinel BDF 0000:00:06.0 (interim bootstrap)\n",
        );
        alloc::vec![SENTINEL_BDF]
    };

    // Step 2: create the command endpoint and register the `usb` service
    // *before* the slow per-port enumeration below. On a multi-controller
    // machine (e.g. a laptop with a PCH xHCI plus a Thunderbolt/TCSS xHCI)
    // enumerating every connected device serially can take well over ten
    // seconds. The `usb-hid` class driver waits a bounded 10 s for the `usb`
    // service to appear; registering the name up front lets that wait succeed
    // immediately, after which its first `NextAttach` simply blocks on the IPC
    // rendezvous until this driver finishes enumerating and enters `server::run`
    // — so a slow enumeration no longer trips the timeout and silently kills the
    // keyboard/NIC class drivers.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "xhci_driver: server endpoint create failed\n",
        );
        return 20;
    }
    let ep = ep as u32;
    if syscall_lib::ipc_register_service(ep, usb_core::protocol::USB_SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "xhci_driver: register 'usb' service failed\n",
        );
        return 21;
    }

    // Step 3: bring up and enumerate every discovered controller, collecting all
    // of them so the server can serve devices on each (the keyboard and a
    // USB-C-attached NIC frequently land on different controllers). The kernel
    // binds one IRQ per task, so `server::run` binds the primary controller's
    // IRQ and drains the rest by polling on every loop wake — see its doc.
    let mut controllers: alloc::vec::Vec<server::ControllerCtx> = alloc::vec::Vec::new();
    // Phase 92d multiplexed interrupts: the first controller brought up (index 0,
    // the "primary") subscribes a fresh notification (bit 0) that `server::run`
    // binds to its recv loop; every later controller subscribes its IRQ INTO
    // that same notification at bit = its controller index, so the single recv
    // loop wakes on any controller's interrupt. We capture the primary's
    // notification as a plain cap handle so we don't borrow into `controllers`
    // while pushing. `bit == controller index` (the 1-byte slot-handle codec
    // caps at 4 controllers / 2-bit index, well within the 64-bit word), so a
    // `Notification(bits)` wake names exactly which controller(s) fired.
    let mut primary_notif_cap: Option<u32> = None;
    for key in bdf_list {
        let this_idx = controllers.len();
        let shared = primary_notif_cap.map(|cap| (cap, this_idx as u8));
        match bring_up_controller(key, shared) {
            Ok((c, irq, devs)) => {
                if primary_notif_cap.is_none() {
                    primary_notif_cap = Some(irq.cap_handle());
                } else {
                    // A secondary controller is up and multiplexed into the
                    // primary's bound notification — announce it. The
                    // `usb-multi-controller-smoke` gate waits on
                    // `XHCI:controller-1:ready`.
                    syscall_lib::write_str(STDOUT_FILENO, "XHCI:controller-");
                    write_u8_dec(this_idx as u8);
                    syscall_lib::write_str(STDOUT_FILENO, ":ready\n");
                }
                controllers.push((c, irq, devs));
            }
            Err(0) => {
                // Clean exit requested (no controller at this BDF) — skip it.
            }
            Err(code) => {
                // Hard bring-up failure on one controller. Log it but continue
                // attempting the rest (a secondary controller failure should not
                // prevent another from coming up).
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "xhci_driver: controller bring-up failed, continuing\n",
                );
                let _ = code;
            }
        }
    }

    if controllers.is_empty() {
        // No controller came up. The `usb` service is already registered, so we
        // must still enter the server loop (rather than exit, which would leave
        // the registered name pointing at a dead endpoint and hang `usb-hid` on
        // its first `NextAttach`). `server::run` with an empty controller set
        // answers `NextAttach` with "no devices", letting `usb-hid` exit cleanly.
        syscall_lib::write_str(
            STDOUT_FILENO,
            "xhci_driver: no controller brought up — serving empty USB device set\n",
        );
    }

    // Step 4: enter the multi-controller USB IPC server loop. It never returns.
    // This line is the bring-up watershed: until it prints, no client's
    // NextAttach/Control request is answered (they block on the IPC rendezvous),
    // so on bare metal its presence distinguishes "bring-up still grinding" from
    // "server live, devices surfaced".
    syscall_lib::write_str(STDOUT_FILENO, "[xhci] bring-up done: ");
    write_u8_dec(controllers.len() as u8);
    syscall_lib::write_str(STDOUT_FILENO, " controller(s) up — entering server loop\n");
    server::run(ep, discovered_count, controllers)
}

/// Print the USB descriptor tree for a successfully enumerated device.
#[cfg(not(test))]
fn print_descriptor_tree(ctx: &kernel_core::usb::enumerate::EnumContext) {
    use kernel_core::usb::descriptor::CLASS_HID;

    if let Some(ref dev) = ctx.device_descriptor {
        syscall_lib::write_str(STDOUT_FILENO, "[xhci] Device: VID=");
        write_u16_hex(dev.id_vendor);
        syscall_lib::write_str(STDOUT_FILENO, " PID=");
        write_u16_hex(dev.id_product);
        syscall_lib::write_str(STDOUT_FILENO, " class=");
        write_u8_dec(dev.b_device_class);
        syscall_lib::write_str(STDOUT_FILENO, "\n");
    }

    if let Some(ref cfg) = ctx.parsed_config {
        syscall_lib::write_str(STDOUT_FILENO, "[xhci] Config: value=");
        write_u8_dec(cfg.config.b_configuration_value);
        syscall_lib::write_str(STDOUT_FILENO, " interfaces=");
        write_u8_dec(cfg.config.b_num_interfaces);
        syscall_lib::write_str(STDOUT_FILENO, "\n");

        for iface in &cfg.interfaces {
            let i = &iface.interface;
            syscall_lib::write_str(STDOUT_FILENO, "[xhci]  Interface: class=");
            write_u8_dec(i.b_interface_class);
            syscall_lib::write_str(STDOUT_FILENO, " sub=");
            write_u8_dec(i.b_interface_sub_class);
            syscall_lib::write_str(STDOUT_FILENO, " proto=");
            write_u8_dec(i.b_interface_protocol);
            if i.b_interface_class == CLASS_HID {
                syscall_lib::write_str(STDOUT_FILENO, " (HID)");
            }
            syscall_lib::write_str(STDOUT_FILENO, "\n");

            for ep in &iface.endpoints {
                syscall_lib::write_str(STDOUT_FILENO, "[xhci]   EP addr=");
                write_u8_hex(ep.b_endpoint_address);
                syscall_lib::write_str(STDOUT_FILENO, " type=");
                write_u8_dec(ep.transfer_type());
                syscall_lib::write_str(STDOUT_FILENO, " mps=");
                write_u16_dec(ep.w_max_packet_size);
                syscall_lib::write_str(STDOUT_FILENO, " interval=");
                write_u8_dec(ep.b_interval);
                syscall_lib::write_str(STDOUT_FILENO, "\n");
            }
        }
    }
}

/// Map a bring-up stage failure to a stable non-zero exit code + log line.
#[cfg(not(test))]
fn bringup_failed(err: BringUpError) -> i32 {
    let (msg, code) = match err {
        BringUpError::BiosHandoffTimeout => (
            "xhci_driver: BIOS/OS handoff timeout (still BIOS-owned)\n",
            9,
        ),
        BringUpError::ResetTimeout => ("xhci_driver: controller reset timeout\n", 5),
        BringUpError::RunTimeout => ("xhci_driver: controller run timeout (HCH stuck)\n", 6),
        BringUpError::DmaAlloc => ("xhci_driver: DMA allocation failed\n", 7),
        BringUpError::IrqSubscribe => ("xhci_driver: MSI-X IRQ subscribe failed\n", 8),
    };
    syscall_lib::write_str(STDOUT_FILENO, msg);
    code
}

/// Write a BDF in `SSSS:BB:DD.F` notation to stdout.
///
/// Formats `key` as the four-field PCI address `segment:bus:dev.func`
/// zero-padded to the conventional widths (`0000:00:06.0` etc.).
#[cfg(not(test))]
fn write_bdf(key: DeviceCapKey) {
    // segment — 4 hex digits
    write_u8_hex((key.segment >> 8) as u8);
    write_u8_hex((key.segment & 0xFF) as u8);
    syscall_lib::write_str(STDOUT_FILENO, ":");
    // bus — 2 hex digits
    write_u8_hex(key.bus);
    syscall_lib::write_str(STDOUT_FILENO, ":");
    // device — 2 hex digits
    write_u8_hex(key.dev);
    syscall_lib::write_str(STDOUT_FILENO, ".");
    // function — 1 decimal digit (0–7)
    let f = [b'0' + (key.func & 0x7)];
    // SAFETY: f contains only ASCII digits.
    let s = unsafe { core::str::from_utf8_unchecked(&f) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}

/// Print `[xhci] N ports detected` without pulling in `alloc::format!`.
#[cfg(not(test))]
fn write_ports_detected(n: u8) {
    syscall_lib::write_str(STDOUT_FILENO, "[xhci] ");
    write_u8_dec(n);
    syscall_lib::write_str(STDOUT_FILENO, " ports detected\n");
}

/// Write a `u8` as decimal to stdout (max three digits).
#[cfg(not(test))]
pub(crate) fn write_u8_dec(mut n: u8) {
    let mut buf = [0u8; 3];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10);
        n /= 10;
        if n == 0 {
            break;
        }
    }
    // SAFETY: `buf[i..]` only ever contains ASCII digits.
    let s = unsafe { core::str::from_utf8_unchecked(&buf[i..]) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}

/// Write a `u16` as decimal to stdout.
#[cfg(not(test))]
fn write_u16_dec(mut n: u16) {
    let mut buf = [0u8; 5];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let s = unsafe { core::str::from_utf8_unchecked(&buf[i..]) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}

/// Write a `u8` as hex (2 digits) to stdout.
#[cfg(not(test))]
fn write_u8_hex(n: u8) {
    let hi = (n >> 4) & 0xF;
    let lo = n & 0xF;
    let hex_digit = |d: u8| -> u8 { if d < 10 { b'0' + d } else { b'a' + d - 10 } };
    let buf = [hex_digit(hi), hex_digit(lo)];
    let s = unsafe { core::str::from_utf8_unchecked(&buf) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}

/// Write a `u16` as hex (4 digits) to stdout.
#[cfg(not(test))]
fn write_u16_hex(n: u16) {
    write_u8_hex((n >> 8) as u8);
    write_u8_hex((n & 0xFF) as u8);
}

#[cfg(test)]
mod tests {
    use super::{BOOT_LOG_MARKER, ENABLE_SLOT_OK_SENTINEL, XHCI_ENUM_CONFIGURED_SENTINEL};

    #[test]
    fn boot_log_marker_matches_acceptance() {
        assert_eq!(BOOT_LOG_MARKER, "xhci_driver: spawned\n");
    }

    #[test]
    fn enable_slot_sentinel_matches_acceptance() {
        // The xhci-bringup-smoke gate (Track C.1) greps for this exact line.
        assert_eq!(ENABLE_SLOT_OK_SENTINEL, "XHCI_BRINGUP:enable-slot:OK\n");
    }

    #[test]
    fn enum_configured_sentinel_matches_acceptance() {
        // The xhci-enum-smoke gate asserts this exact line.
        assert_eq!(XHCI_ENUM_CONFIGURED_SENTINEL, "XHCI_ENUM:configured\n");
    }
}
