//! xHCI host-controller bring-up — Phase 78a Tracks A.3–A.7 + Phase 78b
//! Track A-glue (synchronous command/transfer rings, per-slot context DMA,
//! `UsbHostOps` backed by real DMA rings).
//!
//! [`Controller`] owns the claimed device, the BAR0 MMIO window, and every
//! controller-visible DMA structure (DCBAA, scratchpad, command ring, event
//! ring + ERST). It drives the mandatory bring-up handshake — BIOS/OS
//! handoff, `HCRST` reset + `CNR` wait, ordered structure programming, MSI-X
//! interrupter, `Run` — and reaches a first `Enable Slot` Command Completion
//! event consumed off the event ring **by interrupt**.
//!
//! Every register offset is computed at runtime from `CAPLENGTH` / `RTSOFF` /
//! `DBOFF` (never hardcoded), and every DMA structure is programmed by its
//! IOMMU-routed [`DmaBuffer::iova`] (never a raw CPU pointer), so the
//! controller is confined to granted pages.

use core::sync::atomic::{Ordering, fence};

use driver_runtime::{DeviceHandle, DmaBuffer, IrqNotification, Mmio};
use kernel_core::usb::xhci::{
    context::{input_control_offset, input_endpoint_offset, input_slot_offset},
    port, regs, trb,
};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::write_str;

use crate::ENABLE_SLOT_OK_SENTINEL;

/// Typestate marker for the xHCI BAR0 MMIO window.
pub struct XhciBar0;

/// Local newtype so we can provide the `DeviceCapHandle` impl that
/// [`IrqNotification::subscribe`] requires — the orphan rule blocks a direct
/// impl on `DeviceHandle`. Zero-cost: a borrow wrapper, the same escape hatch
/// the NVMe driver uses.
struct DeviceCap<'a>(&'a DeviceHandle);

impl driver_runtime::irq::DeviceCapHandle for DeviceCap<'_> {
    fn cap_handle(&self) -> u32 {
        self.0.cap()
    }
}

// ---------------------------------------------------------------------------
// Operational register offsets (from the Operational base = BAR + CAPLENGTH)
// ---------------------------------------------------------------------------

mod op_reg {
    pub const USBCMD: usize = 0x00;
    pub const USBSTS: usize = 0x04;
    pub const PAGESIZE: usize = 0x08;
    pub const CRCR: usize = 0x18;
    pub const DCBAAP: usize = 0x30;
    pub const CONFIG: usize = 0x38;
    pub const PORTSC_BASE: usize = 0x400;
    pub const PORTSC_STRIDE: usize = 0x10;
}

const USBCMD_RS: u32 = 1 << 0;
const USBCMD_HCRST: u32 = 1 << 1;
const USBCMD_INTE: u32 = 1 << 2;
const USBSTS_HCH: u32 = 1 << 0;
const USBSTS_CNR: u32 = 1 << 11;
const CRCR_RCS: u64 = 1 << 0;

// ---------------------------------------------------------------------------
// Runtime / interrupter register offsets (from Runtime base = BAR + RTSOFF)
// ---------------------------------------------------------------------------

mod rt_reg {
    /// Interrupter Register Set 0 begins 0x20 past the Runtime base.
    pub const IR0: usize = 0x20;
    pub const IMAN: usize = IR0;
    pub const IMOD: usize = IR0 + 0x04;
    pub const ERSTSZ: usize = IR0 + 0x08;
    pub const ERSTBA: usize = IR0 + 0x10;
    pub const ERDP: usize = IR0 + 0x18;
}

const IMAN_IP: u32 = 1 << 0;
const IMAN_IE: u32 = 1 << 1;
/// ERDP bit 3 — Event Handler Busy (RW1C; write 1 to clear after draining).
const ERDP_EHB: u64 = 1 << 3;

/// Tight event-ring polls performed BEFORE falling back to 1 ms sleep-polling in
/// the completion waits (command / control-transfer / bulk-OUT).
///
/// USB transfers complete in microseconds, but the kernel timer granularity is
/// 1 ms, so `nanosleep_for(0, 1_000_000)` cannot sleep less than ~1 ms. With a
/// pure sleep-poll EVERY transfer that isn't already complete on the first check
/// eats a full ~1 ms — and the ring-3 NIC driver issues many USB ops per loop
/// iteration, so that 1 ms quantum compounds into the interactive SSH lag and
/// "freezes when typing fast" backpressure observed on bare metal. A brief
/// busy-poll on the (cache-coherent) event ring catches the common fast
/// completion in microseconds; genuinely slow or absent completions still fall
/// through to the bounded 1 ms sleep phase, so the timeout behaviour is
/// unchanged. Empty polls are cheap (event-ring memory reads, no MMIO writes).
const COMPLETION_SPIN_POLLS: u32 = 4000;

// ---------------------------------------------------------------------------
// Extended-capability IDs (xECP walk)
// ---------------------------------------------------------------------------

const XECP_ID_LEGACY: u8 = 1;
/// USBLEGSUP bit 16 — HC BIOS Owned Semaphore.
const USBLEGSUP_BIOS_OWNED: u32 = 1 << 16;
/// USBLEGSUP bit 24 — HC OS Owned Semaphore.
const USBLEGSUP_OS_OWNED: u32 = 1 << 24;

// ---------------------------------------------------------------------------
// Ring / structure sizing
// ---------------------------------------------------------------------------

/// TRBs per ring segment (command ring and the single event-ring segment).
/// 64 entries × 16 bytes = 1024 bytes — sufficient for full enumeration
/// command/transfer traffic during 78b; still small enough to not waste DMA.
const RING_TRBS: usize = 64;
const RING_BYTES: usize = RING_TRBS * trb::TRB_SIZE;
/// 64-byte alignment is the strictest xHCI requirement for rings, the DCBAA,
/// the ERST, and contexts.
const XHCI_ALIGN: usize = 64;

/// Number of Normal TRBs an **IN** endpoint keeps outstanding at once (Phase 96
/// RX-wedge work). Each TRB points at its own buffer in the endpoint's cyclic
/// `data_bufs` ring, so the device always has somewhere to DMA the next frame
/// even while the host is busy draining a TX burst and slow to re-arm. A depth
/// of 1 (the original design) let the RTL8156's RX FIFO back up under TX load —
/// the device completed a transfer, the host could not re-arm before the next
/// frame arrived, and the dongle's MAC stalled with no USB error. Well under
/// the `RING_TRBS - 1` usable ring slots. OUT endpoints use a depth of 1.
const RX_QUEUE_DEPTH: usize = 4;

/// Device context size: Slot + 31 EP contexts (32 entries total).
/// With entry_size=32: 32*32 = 1024 bytes; with entry_size=64: 2048 bytes.
const DEVICE_CONTEXT_ENTRIES: usize = 32;
/// Input context has one extra entry (Input Control Context) before the 32
/// device-context entries: total (DEVICE_CONTEXT_ENTRIES + 1) entries.
const INPUT_CONTEXT_ENTRIES: usize = DEVICE_CONTEXT_ENTRIES + 1;

/// Real-time poll budget for register handshakes, in 100µs iterations.
///
/// The old fixed-iteration busy-spin (5M tight MMIO reads, no yield) was a
/// bare-metal trap: a controller whose status bit never flips — e.g. an empty
/// or D3/unpowered Thunderbolt/TCSS root port returning all-ones — burned
/// ~5 s of pure CPU per loop, and with one such loop per port across a 22-port
/// controller that stalled the *single-threaded* driver for minutes before it
/// reached `server::run`. Every other ring-3 task (the NIC's `ure`, the HID
/// driver) blocks on its first `NextAttach` IPC until then, so the whole USB
/// subsystem looks hung. `poll_yield` instead sleeps 100µs per iteration:
/// the CPU is released to those tasks, and the worst case is a bounded
/// wall-clock timeout rather than a multi-second spin. 1 s covers the slow
/// CNR-clear after `HCRST` (xHCI allows the controller to stay Not-Ready for a
/// while); 250 ms covers a USB2 port reset's PRC latch.
const POLL_ITERS_1S: u32 = 10_000;
const POLL_ITERS_250MS: u32 = 2_500;

/// Poll `ready` every 100µs, yielding the CPU, until it returns `true` or
/// `max_iters` elapse. Returns whether `ready` ultimately held. Unlike a tight
/// fixed-iteration spin, each iteration sleeps so a wedged register cannot starve
/// the other ring-3 tasks waiting on this driver's IPC server.
fn poll_yield(max_iters: u32, mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..max_iters {
        if ready() {
            return true;
        }
        let _ = syscall_lib::nanosleep_for(0, 100_000);
    }
    ready()
}

/// Outcome of the bring-up sequence: either the controller is live (with the
/// IRQ subscription handed to the event loop) or a stage failed.
pub enum BringUpError {
    BiosHandoffTimeout,
    ResetTimeout,
    RunTimeout,
    DmaAlloc,
    IrqSubscribe,
}

// ---------------------------------------------------------------------------
// Per-slot device context bookkeeping
// ---------------------------------------------------------------------------

/// A configured non-EP0 endpoint on a slot. Phase 78c: owns the endpoint's
/// transfer ring, its producer cursor, and persistent DMA buffers the
/// controller writes each report into. Captured reports queue here until a
/// class driver polls them. Phase 96 reuses this for **bulk** endpoints too —
/// the differences are frame-sized `data_bufs` (vs HID's `mps`) and the
/// `armed_len` the TRBs are programmed with; direction is derived from `dci`
/// parity (odd = IN, even = OUT, per `trb::dci`).
///
/// Phase 96 RX-wedge work: an IN endpoint keeps **`depth` Normal TRBs
/// outstanding** at once, each pointing at its own buffer in the cyclic
/// `data_bufs` ring, so the device always has somewhere to DMA the next frame.
/// A single-TRB queue let the RTL8156's internal RX FIFO back up under TX load
/// (the host could not re-arm fast enough between the device's back-to-back
/// completions), stalling its MAC with no USB error. OUT endpoints use `depth`
/// = 1 (only `data_bufs[0]`, the bulk-OUT source buffer). Completions on a
/// single endpoint ring are delivered in enqueue order, so `arm_next` /
/// `drain_next` track the cyclic buffer in lockstep without matching TRB
/// pointers.
pub struct InterruptEndpoint {
    /// Device Context Index of this endpoint.
    pub dci: u8,
    /// `wMaxPacketSize` — the HID report size / the interrupt Normal-TRB length.
    pub mps: u16,
    /// Transfer ring for this endpoint (kept alive for the slot's lifetime).
    ring: DmaBuffer<u8>,
    /// IOVA of `ring`.
    ring_iova: u64,
    /// Producer cursor for `ring`.
    producer: trb::ProducerRing,
    /// Cyclic ring of DMA buffers, one per outstanding IN TRB (`depth` of them).
    /// The controller writes each interrupt-IN report / bulk-IN frame into the
    /// buffer its TRB points at. `data_bufs[0]` is also the bulk-OUT source
    /// buffer. Grown on demand for bulk (always while a buffer is free).
    data_bufs: alloc::vec::Vec<DmaBuffer<u8>>,
    /// FIFO of captured reports/frames awaiting a class-driver poll (bounded).
    reports: alloc::collections::VecDeque<alloc::vec::Vec<u8>>,
    /// Number of Normal TRBs currently armed-but-not-yet-completed (`<= depth`).
    in_flight: usize,
    /// Index in `data_bufs` of the next buffer to arm a TRB into.
    arm_next: usize,
    /// Index in `data_bufs` of the next buffer expected to complete (drain
    /// order matches arm order on a single ring).
    drain_next: usize,
    /// Target number of outstanding IN TRBs: `RX_QUEUE_DEPTH` for IN endpoints
    /// (odd `dci`), 1 for OUT endpoints (even `dci`).
    depth: usize,
    /// Byte length the currently-pending Normal TRBs were programmed with.
    /// Equals `mps` for interrupt endpoints; a frame-sized value for bulk-IN.
    /// Capture and re-arm both use this so a bulk endpoint is not re-armed at
    /// `mps`.
    armed_len: u32,
}

/// Per-slot DMA context allocated during Address Device.
pub struct SlotContext {
    /// xHCI Slot ID this context belongs to.
    pub slot_id: u8,
    /// Output Device Context — the controller writes state back here.
    pub output_ctx: DmaBuffer<u8>,
    /// EP0 transfer ring (command and data TRBs).
    pub ep0_ring: DmaBuffer<u8>,
    /// EP0 ring producer cursor.
    pub ep0_producer: trb::ProducerRing,
    /// IOVA of the EP0 transfer ring (written into the EP0 context).
    pub ep0_ring_iova: u64,
    /// Input Context DMA buffer — rebuilt from an `InputContextSnapshot`
    /// before every command that needs one.
    pub input_ctx: DmaBuffer<u8>,
    /// Additional (interrupt/bulk) endpoints configured for this slot, each
    /// owning its transfer ring + report buffer. Populated during Configure
    /// Endpoint (Phase 78c).
    pub interrupt_eps: alloc::vec::Vec<InterruptEndpoint>,
    /// Reusable EP0 control-transfer data-stage scratch buffer, grown on demand.
    /// `control_transfer` reuses this across calls instead of allocating a fresh
    /// `DmaBuffer` each time — `DmaBuffer::drop` is a no-op (the kernel reclaims
    /// the DMA region on process exit), so a per-call allocation leaks an IOVA +
    /// device-host DMA cap for the driver's whole lifetime. The ure NIC driver
    /// polls registers via OCP control reads continuously, so that unbounded
    /// growth would eventually exhaust the device-host DMA cap table. `None`
    /// until the first data-stage transfer on this slot.
    pub ep0_data_buf: Option<DmaBuffer<u8>>,
    /// Raw 18-byte Device Descriptor captured at enumeration (Phase 92 H.1).
    /// Returned by `GetDescriptors` so a class driver can inspect device-level
    /// fields without a fresh control read. Empty until the enumerator caches it.
    pub device_desc: alloc::vec::Vec<u8>,
    /// Raw Configuration Descriptor blob (`wTotalLength` bytes — interfaces +
    /// endpoints + class functional descriptors) captured at enumeration. A full
    /// config exceeds the inline `ControlData` clamp, so `GetDescriptors` is the
    /// only way a class driver reads it whole. Empty until cached.
    pub config_desc: alloc::vec::Vec<u8>,
}

/// A root-hub port change the live event path observed and queued for the
/// server loop to act on (Phase 92 Track C hot-plug). `on_port_status_change`
/// records these as Port Status Change events arrive; `take_port_events` drains
/// them so the single-threaded server can run enumeration / teardown outside the
/// event-ring drain borrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortChange {
    /// A device newly connected on `port` and the port reached Enabled at
    /// `speed` (the live path already issued the reset). The server runs
    /// Enable Slot → Address Device → Configure Endpoint and publishes an
    /// `AttachNotice`.
    Connect { port: u8, speed: port::PortSpeed },
    /// The device on `port` disconnected. The server marks its `AttachNotice`
    /// `attached: false` and issues Disable Slot to reclaim the slot.
    Disconnect { port: u8 },
}

pub struct Controller {
    handle: DeviceHandle,
    bar: Mmio<XhciBar0>,
    op_base: usize,
    rt_base: usize,
    db_base: usize,
    max_slots: u8,
    max_ports: u8,
    context_size: usize,
    max_scratchpad: u32,
    xecp_off: Option<usize>,

    // Controller-visible DMA structures (kept alive for the controller's
    // lifetime — Drop releases them on process exit).
    dcbaa: Option<DmaBuffer<u8>>,
    scratchpad_array: Option<DmaBuffer<u8>>,
    scratchpad_pages: alloc::vec::Vec<DmaBuffer<u8>>,
    cmd_ring: Option<DmaBuffer<u8>>,
    event_ring: Option<DmaBuffer<u8>>,
    erst: Option<DmaBuffer<u8>>,

    cmd_ring_iova: u64,
    event_ring_iova: u64,
    producer: trb::ProducerRing,
    consumer: trb::EventConsumer,
    enable_slot_emitted: bool,

    /// Per-slot contexts, one per enumerated device (allocated when each
    /// device's Address Device is issued). Phase 78c: a Vec rather than a
    /// single Option so a USB keyboard AND mouse can both be enumerated and
    /// served — every per-slot operation looks up its `SlotContext` by
    /// `slot_id`, so a second device never clobbers the first.
    slots: alloc::vec::Vec<SlotContext>,

    /// Bounded counter for the transfer-error diagnostic (see
    /// `service_interrupt_events`) so a halted endpoint logs a few times rather
    /// than flooding the serial/framebuffer console.
    xfer_err_logged: u32,

    /// Root-hub port changes observed by `on_port_status_change` and awaiting
    /// the server's attention (Phase 92 Track C). Drained by `take_port_events`.
    port_events: alloc::vec::Vec<PortChange>,
}

/// xHCI completion code for a short-packet transfer (xHCI §6.4.5) — a normal,
/// non-error outcome for a bulk-IN read smaller than the armed buffer.
const COMPLETION_SHORT_PACKET: u8 = 13;

/// xHCI Missed Service Error completion code (xHCI §6.4.5) — an isochronous
/// service interval elapsed before the controller serviced the endpoint.
/// Isoch has no retry, so the interval's data is dropped and the host resyncs
/// on the next interval; this is **not** a transport failure.
const COMPLETION_MISSED_SERVICE_ERROR: u8 = 26;

/// xHCI Ring Underrun completion code (xHCI §6.4.5) — posted on an isochronous
/// OUT endpoint when the controller's periodic scheduler finds the transfer
/// ring empty (the producer hasn't queued the next interval yet). For a
/// fire-and-forget audio-OUT stream this is the expected steady-state event
/// between interval submissions, **not** a transport failure.
const COMPLETION_RING_UNDERRUN: u8 = 14;

/// Outcome of draining one isochronous-OUT batch in [`Controller::submit_isoch_out`].
enum IsochBatchDrain {
    /// Either all requested intervals were serviced, or a Ring-Underrun /
    /// Missed-Service event showed the controller drained the transfer ring (so
    /// in-flight TRBs are zero and the next batch is safe to enqueue). Both are
    /// non-fatal for isochronous transfers, which have no retry.
    Drained,
    /// The bounded poll budget expired before the batch completed and without an
    /// underrun — the device is servicing too slowly. The caller abandons the
    /// rest of the submission, matching the pre-existing "no event" behaviour.
    Timeout,
    /// A genuine transaction error / STALL completion code on this endpoint.
    Error(u8),
}

impl Controller {
    /// Read the capability registers and compute the runtime register-region
    /// bases. No MMIO writes happen here.
    pub fn new(handle: DeviceHandle, bar: Mmio<XhciBar0>) -> Self {
        let cap0 = bar.read_reg::<u32>(regs::CAP_CAPLENGTH_HCIVERSION);
        let caplength = regs::caplength(cap0);
        let hcsparams1 = regs::Hcsparams1(bar.read_reg::<u32>(regs::CAP_HCSPARAMS1));
        let hcsparams2 = regs::Hcsparams2(bar.read_reg::<u32>(regs::CAP_HCSPARAMS2));
        let hccparams1 = regs::Hccparams1(bar.read_reg::<u32>(regs::CAP_HCCPARAMS1));
        let dboff = regs::dboff(bar.read_reg::<u32>(regs::CAP_DBOFF));
        let rtsoff = regs::rtsoff(bar.read_reg::<u32>(regs::CAP_RTSOFF));

        Self {
            op_base: regs::operational_offset(caplength),
            rt_base: regs::runtime_offset(rtsoff),
            db_base: regs::doorbell_offset(dboff),
            max_slots: hcsparams1.max_slots(),
            max_ports: hcsparams1.max_ports(),
            context_size: hccparams1.context_size_bytes(),
            max_scratchpad: hcsparams2.max_scratchpad_buffers(),
            xecp_off: hccparams1.xecp_byte_offset(),
            handle,
            bar,
            dcbaa: None,
            scratchpad_array: None,
            scratchpad_pages: alloc::vec::Vec::new(),
            cmd_ring: None,
            event_ring: None,
            erst: None,
            cmd_ring_iova: 0,
            event_ring_iova: 0,
            producer: trb::ProducerRing::new(RING_TRBS),
            consumer: trb::EventConsumer::new(&[RING_TRBS]),
            enable_slot_emitted: false,
            slots: alloc::vec::Vec::new(),
            xfer_err_logged: 0,
            port_events: alloc::vec::Vec::new(),
        }
    }

    /// Look up an enumerated slot's context by its xHCI Slot ID.
    fn slot(&self, slot_id: u8) -> Option<&SlotContext> {
        self.slots.iter().find(|s| s.slot_id == slot_id)
    }

    /// Mutable variant of [`Self::slot`].
    fn slot_mut(&mut self, slot_id: u8) -> Option<&mut SlotContext> {
        self.slots.iter_mut().find(|s| s.slot_id == slot_id)
    }

    pub fn max_ports(&self) -> u8 {
        self.max_ports
    }

    /// Live `PORTSC` snapshot for a 1-based root-hub `port`, packed into the
    /// `TopoPort` wire flag byte (bit0 CCS, bit1 PED, bit2 PP, bits4..8 the
    /// Port Speed PSI). Read-only — used by the `Topology` diagnostic to
    /// surface, on bare metal, which controller a connected device sits on and
    /// what speed it trained to. Returns `0` for an out-of-range port.
    pub fn port_status_flags(&self, port: u8) -> u8 {
        if port == 0 || port > self.max_ports {
            return 0;
        }
        let p = port::Portsc(self.op_u32(self.portsc(port)));
        usb_core::protocol::TopoPort::pack(p.ccs(), p.ped(), p.pp(), p.port_speed())
    }

    /// Context entry size (32 or 64 bytes) selected from `HCCPARAMS1.CSZ`.
    /// Threaded into every later context allocation (78b `Address Device` /
    /// `Configure Endpoint`); reported during discovery so the selection is
    /// observable in the boot log.
    pub fn context_size(&self) -> usize {
        self.context_size
    }

    // -- register accessors -------------------------------------------------

    fn op_u32(&self, off: usize) -> u32 {
        self.bar.read_reg::<u32>(self.op_base + off)
    }
    fn op_write_u32(&self, off: usize, v: u32) {
        self.bar.write_reg::<u32>(self.op_base + off, v);
    }
    fn op_write_u64(&self, off: usize, v: u64) {
        self.bar.write_reg::<u64>(self.op_base + off, v);
    }
    fn rt_write_u32(&self, off: usize, v: u32) {
        self.bar.write_reg::<u32>(self.rt_base + off, v);
    }
    fn rt_write_u64(&self, off: usize, v: u64) {
        self.bar.write_reg::<u64>(self.rt_base + off, v);
    }
    fn portsc(&self, port: u8) -> usize {
        op_reg::PORTSC_BASE + op_reg::PORTSC_STRIDE * (port as usize - 1)
    }
    /// Ring a doorbell. `slot` selects the doorbell (0 = Command Ring);
    /// `target` is the Device Context Index for slot doorbells (ignored for
    /// the command doorbell, which uses target 0). A write barrier precedes
    /// the doorbell so the queued TRB is globally visible first.
    fn ring_doorbell(&self, slot: u8, target: u8) {
        fence(Ordering::SeqCst);
        self.bar
            .write_reg::<u32>(self.db_base + 4 * slot as usize, target as u32);
    }

    // -- A.3: BIOS/OS handoff, reset, run ----------------------------------

    /// Walk the xECP capability list and, if a USBLEGSUP capability is
    /// present, request OS ownership and poll until the BIOS-owned bit
    /// clears. `qemu-xhci` advertises no USBLEGSUP, so this is a documented
    /// no-op there; the walk is implemented for real hardware.
    ///
    /// Returns [`BringUpError::BiosHandoffTimeout`] if the poll budget is
    /// exhausted while the controller is still BIOS-owned — proceeding into
    /// reset/run in that state is exactly what the handoff exists to prevent,
    /// so it is surfaced as a bring-up failure rather than a logged success.
    pub fn release_bios_ownership(&self) -> Result<(), BringUpError> {
        let mut off = match self.xecp_off {
            Some(o) => o,
            None => {
                write_str(STDOUT_FILENO, "[xhci] no extended capabilities\n");
                return Ok(());
            }
        };
        // Bounded walk: the list is short and `next == 0` terminates it; the
        // cap guards against a malformed self-referential list.
        for _ in 0..64 {
            // The USBLEGSUP register overlays the capability-header dword: bits
            // [7:0] = cap id, [15:8] = next pointer (in dwords), bit 16 = HC
            // BIOS Owned, bit 24 = HC OS Owned. So this single read is both the
            // header and (for the legacy cap) the semaphore register.
            let dword = self.bar.read_reg::<u32>(off);
            let id = (dword & 0xFF) as u8;
            let next = ((dword >> 8) & 0xFF) as usize;
            if id == XECP_ID_LEGACY {
                if dword & USBLEGSUP_BIOS_OWNED != 0 {
                    self.bar.write_reg::<u32>(off, dword | USBLEGSUP_OS_OWNED);
                    poll_yield(POLL_ITERS_1S, || {
                        self.bar.read_reg::<u32>(off) & USBLEGSUP_BIOS_OWNED == 0
                    });
                    // If the BIOS-owned bit never cleared, the handoff failed.
                    // Treating the timeout as success would let bring-up proceed
                    // into HCRST/Run while the controller is still BIOS-owned —
                    // surface it as a failure so the driver aborts instead.
                    if self.bar.read_reg::<u32>(off) & USBLEGSUP_BIOS_OWNED != 0 {
                        write_str(
                            STDOUT_FILENO,
                            "[xhci] BIOS/OS handoff timed out (still BIOS-owned)\n",
                        );
                        return Err(BringUpError::BiosHandoffTimeout);
                    }
                }
                write_str(STDOUT_FILENO, "[xhci] BIOS/OS handoff complete\n");
                return Ok(());
            }
            if next == 0 {
                break;
            }
            off += next * 4;
        }
        // qemu-xhci advertises Supported-Protocol xECPs but no USBLEGSUP, so
        // the walk terminates here with nothing to hand off — documented
        // no-op on QEMU.
        write_str(
            STDOUT_FILENO,
            "[xhci] no USBLEGSUP (no BIOS handoff needed)\n",
        );
        Ok(())
    }

    /// Stop the controller (if running) and issue `HCRST`, then wait for the
    /// reset to self-clear **and** `USBSTS.CNR` to clear before any operational
    /// pointer is programmed.
    pub fn reset(&self) -> Result<(), BringUpError> {
        // Stop: clear R/S, wait for HCHalted.
        let cmd = self.op_u32(op_reg::USBCMD);
        if cmd & USBCMD_RS != 0 {
            self.op_write_u32(op_reg::USBCMD, cmd & !USBCMD_RS);
        }
        if !poll_yield(POLL_ITERS_1S, || {
            self.op_u32(op_reg::USBSTS) & USBSTS_HCH != 0
        }) {
            return Err(BringUpError::ResetTimeout);
        }

        // Reset: set HCRST, wait until it self-clears AND CNR clears.
        self.op_write_u32(op_reg::USBCMD, USBCMD_HCRST);
        if !poll_yield(POLL_ITERS_1S, || {
            let cmd = self.op_u32(op_reg::USBCMD);
            let sts = self.op_u32(op_reg::USBSTS);
            cmd & USBCMD_HCRST == 0 && sts & USBSTS_CNR == 0
        }) {
            return Err(BringUpError::ResetTimeout);
        }
        write_str(
            STDOUT_FILENO,
            "[xhci] controller reset complete (CNR clear)\n",
        );
        Ok(())
    }

    /// Program `CONFIG.MaxSlotsEn` from `HCSPARAMS1.MaxSlots`.
    pub fn program_max_slots(&self) {
        let max = self.max_slots.max(1);
        let config = self.op_u32(op_reg::CONFIG);
        // MaxSlotsEn is the low byte of CONFIG.
        let config = (config & !0xFF) | max as u32;
        self.op_write_u32(op_reg::CONFIG, config);
    }

    // -- A.4: DCBAA + scratchpad -------------------------------------------

    /// Allocate the DCBAA (and scratchpad array + pages when the controller
    /// requires them) and program `DCBAAP` with the DCBAA IOVA.
    pub fn init_dcbaa(&mut self) -> Result<(), BringUpError> {
        let entries = self.max_slots as usize + 1;
        let dcbaa = DmaBuffer::<u8>::allocate(&self.handle, entries * 8, XHCI_ALIGN)
            .map_err(|_| BringUpError::DmaAlloc)?;
        zero_dma(&dcbaa);

        if self.max_scratchpad > 0 {
            let page_size = self.page_size();
            let count = self.max_scratchpad as usize;
            let array = DmaBuffer::<u8>::allocate(&self.handle, count * 8, XHCI_ALIGN)
                .map_err(|_| BringUpError::DmaAlloc)?;
            zero_dma(&array);
            for i in 0..count {
                let page = DmaBuffer::<u8>::allocate(&self.handle, page_size, page_size)
                    .map_err(|_| BringUpError::DmaAlloc)?;
                zero_dma(&page);
                write_u64_at(&array, i * 8, page.iova());
                self.scratchpad_pages.push(page);
            }
            // DCBAA[0] points at the scratchpad-buffer IOVA array.
            write_u64_at(&dcbaa, 0, array.iova());
            self.scratchpad_array = Some(array);
        }

        self.op_write_u64(op_reg::DCBAAP, dcbaa.iova());
        self.dcbaa = Some(dcbaa);
        Ok(())
    }

    fn page_size(&self) -> usize {
        // PAGESIZE bits[15:0] are a *bitmask* of supported page sizes: bit n set
        // means the controller supports a 2^(n+12)-byte page. More than one bit
        // may be set, so software must select a single supported size — shifting
        // the whole mask would yield a non-power-of-two (e.g. bits 0|1 -> 12 KiB)
        // that the DMA allocator rejects as a scratchpad alignment. Pick the
        // lowest supported size (smallest is always a valid choice, and matches
        // what the Linux xhci driver selects).
        let pagesize = self.op_u32(op_reg::PAGESIZE) & 0xFFFF;
        if pagesize == 0 {
            4096
        } else {
            4096usize << pagesize.trailing_zeros()
        }
    }

    // -- A.5: command ring + event ring + ERST -----------------------------

    /// Allocate the command ring, install its trailing Link TRB (Toggle Cycle
    /// set), and program `CRCR` with the ring IOVA | RCS.
    pub fn init_command_ring(&mut self) -> Result<(), BringUpError> {
        let ring = DmaBuffer::<u8>::allocate(&self.handle, RING_BYTES, XHCI_ALIGN)
            .map_err(|_| BringUpError::DmaAlloc)?;
        zero_dma(&ring);
        let iova = ring.iova();
        // Trailing Link TRB at the last slot, cycle = 0 (initially invalid),
        // Toggle Cycle set so the producer cycle flips on wrap.
        write_trb(
            &ring,
            self.producer.link_index(),
            trb::Trb::link(iova, true, false),
        );
        // CRCR: ring pointer | Ring Cycle State (PCS starts at 1).
        self.op_write_u64(op_reg::CRCR, iova | CRCR_RCS);
        self.cmd_ring_iova = iova;
        self.cmd_ring = Some(ring);
        Ok(())
    }

    /// Allocate the event-ring segment + a one-entry ERST and arm the
    /// interrupter's dequeue state: program `ERSTSZ`, then `ERSTBA`, then
    /// `ERDP` (in that order).
    pub fn init_event_ring(&mut self) -> Result<(), BringUpError> {
        let seg = DmaBuffer::<u8>::allocate(&self.handle, RING_BYTES, XHCI_ALIGN)
            .map_err(|_| BringUpError::DmaAlloc)?;
        zero_dma(&seg);
        let seg_iova = seg.iova();

        // ERST: one 16-byte entry {segment base IOVA (u64), size (u32),
        // reserved (u32)}.
        let erst = DmaBuffer::<u8>::allocate(&self.handle, 16, XHCI_ALIGN)
            .map_err(|_| BringUpError::DmaAlloc)?;
        zero_dma(&erst);
        write_u64_at(&erst, 0, seg_iova);
        write_u32_at(&erst, 8, RING_TRBS as u32);

        // ERSTSZ first, then ERSTBA, then ERDP (initial dequeue = segment
        // base, EHB clear).
        self.rt_write_u32(rt_reg::ERSTSZ, 1);
        self.rt_write_u64(rt_reg::ERSTBA, erst.iova());
        self.rt_write_u64(rt_reg::ERDP, seg_iova);

        self.event_ring_iova = seg_iova;
        self.event_ring = Some(seg);
        self.erst = Some(erst);
        Ok(())
    }

    // -- A.6: MSI-X interrupter --------------------------------------------

    /// Subscribe the controller IRQ (MSI-X preferred — the kernel substrate
    /// programs the MSI-X table + enable bit during `sys_device_irq_subscribe`)
    /// and enable interrupter 0: set `IMAN.IE`, clear any pending `IMAN.IP`,
    /// and set the moderation interval.
    pub fn init_interrupter(&self) -> Result<IrqNotification, BringUpError> {
        let irq = IrqNotification::subscribe(&DeviceCap(&self.handle), None)
            .map_err(|_| BringUpError::IrqSubscribe)?;
        self.enable_interrupter();
        Ok(irq)
    }

    /// Program interrupter 0's moderation + enable bits. Shared by the fresh
    /// ([`init_interrupter`]) and multiplexed ([`init_interrupter_into`])
    /// subscribe paths. `IMOD = 0` (no moderation) keeps the first event prompt
    /// during bring-up; `IMAN` sets IE and clears any latched IP (write-1-clear).
    /// Steady-state moderation is applied later by [`set_interrupt_moderation`]
    /// once the server loop is reached, so bring-up (which polls, but whose
    /// no-device Enable Slot completion is interrupt-delivered) is unaffected.
    fn enable_interrupter(&self) {
        self.rt_write_u32(rt_reg::IMOD, 0);
        self.rt_write_u32(rt_reg::IMAN, IMAN_IP | IMAN_IE);
    }

    /// Set interrupter 0's moderation interval (the `IMODI` field, the low 16
    /// bits of `IMOD`, in 250 ns increments) — Phase 92d steady-state perf.
    ///
    /// `IMOD = 0` (bring-up) interrupts on *every* event; under sustained bulk
    /// load (a USB NIC / mass-storage stream) that is an interrupt storm. A
    /// non-zero `IMODI` makes the interrupter wait at least `IMODI × 250 ns`
    /// between interrupts, coalescing a burst of completions into one wake.
    ///
    /// This adds at most `IMODI × 250 ns` of *interrupt* latency, but it does
    /// **not** delay delivery to class drivers: they poll their endpoints
    /// (`PollInterruptIn`/`PollBulkIn`), and each poll is an IPC message that
    /// wakes the loop and drains the ring regardless of the interrupt. So
    /// moderation only thins out redundant interrupt wakes (which, with
    /// Optimization B, already do no MMIO when they find an empty ring).
    /// Applied per controller once, when entering the server loop.
    pub fn set_interrupt_moderation(&self, imodi: u16) {
        self.rt_write_u32(rt_reg::IMOD, imodi as u32);
    }

    /// Subscribe the controller IRQ **into an existing notification**
    /// (`notif_cap`) at `bit_index`, then enable interrupter 0 — the Phase 92d
    /// multiplexed-interrupt path for a *secondary* controller.
    ///
    /// `notif_cap` is the primary controller's `IrqNotification` cap handle
    /// (already bound to the server recv loop). This controller's interrupt then
    /// sets `1 << bit_index` in that shared notification word, so the single
    /// recv loop wakes on this controller's interrupt without waiting for
    /// primary traffic. `bit_index` equals the controller's index in the
    /// server's controller table, so a `Notification(bits)` wake names exactly
    /// which controller(s) fired and the loop drains only those (bit-directed
    /// draining). The kernel binds each device its own MSI-X vector; only the
    /// destination notification + bit are shared.
    pub fn init_interrupter_into(
        &self,
        notif_cap: u32,
        bit_index: u8,
    ) -> Result<IrqNotification, BringUpError> {
        let irq = IrqNotification::subscribe_into(&DeviceCap(&self.handle), notif_cap, bit_index)
            .map_err(|_| BringUpError::IrqSubscribe)?;
        self.enable_interrupter();
        Ok(irq)
    }

    /// Set `USBCMD.R/S` (with interrupts enabled) and wait for `USBSTS.HCH`
    /// to clear. Bus Master Enable is a hard precondition, established
    /// kernel-side by `sys_device_claim` (Phase 78a Track B.1) before this
    /// process ever runs; the ring-3 ABI exposes no PCI-config read to assert
    /// it here, so the claim itself is the guarantee.
    pub fn run(&self) -> Result<(), BringUpError> {
        write_str(
            STDOUT_FILENO,
            "[xhci] Bus Master Enable guaranteed by device claim (kernel B.1)\n",
        );
        self.op_write_u32(op_reg::USBCMD, USBCMD_RS | USBCMD_INTE);
        if !poll_yield(POLL_ITERS_1S, || {
            self.op_u32(op_reg::USBSTS) & USBSTS_HCH == 0
        }) {
            return Err(BringUpError::RunTimeout);
        }
        write_str(STDOUT_FILENO, "[xhci] controller running\n");
        Ok(())
    }

    /// Enqueue an `Enable Slot` command at the current producer position and
    /// ring Doorbell 0 (the Command Ring doorbell).
    pub fn enqueue_enable_slot(&mut self) {
        let ring = self.cmd_ring.as_ref().expect("command ring allocated");
        let cycle = self.producer.cycle;
        write_trb(ring, self.producer.enqueue, trb::Trb::enable_slot(0, cycle));
        let cycle_before = self.producer.cycle;
        if self.producer.advance() {
            // Wrapped onto the Link TRB: stamp it with the pre-toggle cycle so
            // the controller follows it. Not exercised by the single 78a
            // command, but kept correct for the 78b+ command stream.
            write_trb(
                ring,
                self.producer.link_index(),
                trb::Trb::link(self.cmd_ring_iova, true, cycle_before),
            );
        }
        write_str(
            STDOUT_FILENO,
            "[xhci] Enable Slot enqueued; ringing Doorbell 0; blocking on MSI-X\n",
        );
        self.ring_doorbell(0, 0);
    }

    // -- 78b synchronous command / transfer machinery ----------------------

    /// Enqueue one TRB on the command ring, ring Doorbell 0, then block until
    /// a Command Completion Event matching this TRB's pointer arrives on the
    /// event ring. Returns the decoded completion event.
    ///
    /// This is the synchronous path used by enumeration. It is distinct from
    /// the fire-and-forget `enqueue_enable_slot` path but shares the same
    /// `producer` cursor and `ring_doorbell` primitive.
    pub fn issue_command_and_wait(
        &mut self,
        _irq: &IrqNotification,
        cmd: trb::Trb,
    ) -> trb::CommandCompletionEvent {
        let ring = self.cmd_ring.as_ref().expect("command ring allocated");
        // Record the IOVA of the slot we are about to write — the completion
        // event's `command_trb_pointer` will point back here.
        let cmd_iova = self.cmd_ring_iova + (self.producer.enqueue as u64) * trb::TRB_SIZE as u64;
        write_trb(ring, self.producer.enqueue, cmd);
        let cycle_before = self.producer.cycle;
        if self.producer.advance() {
            write_trb(
                ring,
                self.producer.link_index(),
                trb::Trb::link(self.cmd_ring_iova, true, cycle_before),
            );
        }
        self.ring_doorbell(0, 0);

        // Drain the event ring on a bounded poll budget (~400 ms at 1 ms/poll)
        // until the matching Command Completion Event appears. We poll rather
        // than block on `irq.wait()`: on real hardware a controller may deliver
        // **zero** MSI/MSI-X interrupts during bring-up (e.g. a Thunderbolt/TCSS
        // xHCI whose vector isn't routed), and a blocking `irq.wait()` would then
        // hang forever on the very first Enable Slot — wedging the whole
        // single-threaded driver so `server::run` is never reached and every
        // class driver (keyboard, NIC) blocks indefinitely. Polling makes a
        // silent controller time out (synthetic failure → enumeration Error)
        // instead, so the bring-up loop always completes. This mirrors
        // `wait_for_transfer_event`'s bounded-poll design.
        const MAX_POLLS: u32 = 400;
        for poll_i in 0..(COMPLETION_SPIN_POLLS + MAX_POLLS) {
            let before = self.consumer.index;
            let mut completed: alloc::vec::Vec<(u8, u8)> = alloc::vec::Vec::new();
            let found = self.drain_for_command_completion(cmd_iova, &mut completed);
            // Only advance ERDP / clear IMAN.IP when the drain actually consumed
            // events — an empty-poll ERDP write disturbs the controller's
            // event-ring bookkeeping (see `wait_for_transfer_event`).
            if self.consumer.index != before {
                let erdp =
                    self.event_ring_iova + (self.consumer.index as u64) * trb::TRB_SIZE as u64;
                self.rt_write_u64(rt_reg::ERDP, erdp | ERDP_EHB);
                self.rt_write_u32(rt_reg::IMAN, IMAN_IP | IMAN_IE);
            }
            // Re-arm any IN endpoint captured while the command was in flight (H.2).
            for (s, d) in completed {
                let len = self
                    .slot(s)
                    .and_then(|sc| sc.interrupt_eps.iter().find(|e| e.dci == d))
                    .map(|e| e.armed_len)
                    .unwrap_or(0);
                if len > 0 {
                    self.arm_ring_in(s, d, len);
                }
            }
            if let Some(ev) = found {
                return ev;
            }
            if poll_i < COMPLETION_SPIN_POLLS {
                core::hint::spin_loop();
            } else {
                let _ = syscall_lib::nanosleep_for(0, 1_000_000);
            }
        }

        write_str(
            STDOUT_FILENO,
            "[xhci] command timed out (no completion event)\n",
        );
        // Return a synthetic failure completion so enumeration can transition to
        // Error/Timeout rather than hanging.
        trb::CommandCompletionEvent {
            command_trb_pointer: cmd_iova,
            completion_code: 0xFF, // synthetic: not COMPLETION_SUCCESS
            slot_id: 0,
            cycle: false,
        }
    }

    /// Drain the event ring looking for a Command Completion Event whose
    /// `command_trb_pointer` matches `cmd_iova`. Side-effects all other
    /// events normally (sentinel emission for Enable Slot, port status
    /// changes). Returns `Some(ev)` when found, `None` if the ring is
    /// exhausted without a match.
    fn drain_for_command_completion(
        &mut self,
        cmd_iova: u64,
        completed: &mut alloc::vec::Vec<(u8, u8)>,
    ) -> Option<trb::CommandCompletionEvent> {
        let mut found: Option<trb::CommandCompletionEvent> = None;
        loop {
            let seg = self.event_ring.as_ref().expect("event ring allocated");
            let candidate = read_trb(seg, self.consumer.index);
            if !self.consumer.owns(&candidate) {
                break;
            }
            match trb::event_trb_type(&candidate) {
                Some(trb::TrbType::CommandCompletion) => {
                    let ev = trb::parse_command_completion(&candidate);
                    // Always run the sentinel-emission side effect.
                    self.on_command_completion(ev);
                    // Check if this is our matching completion.
                    if ev.command_trb_pointer == cmd_iova {
                        found = Some(ev);
                    }
                }
                Some(trb::TrbType::PortStatusChange) => {
                    let ev = trb::parse_port_status_change(&candidate);
                    self.on_port_status_change(ev);
                }
                Some(trb::TrbType::TransferEvent) => {
                    // A command (Configure Endpoint, Disable Slot, …) can run
                    // while a HID/bulk interrupt-IN endpoint is armed. Capture a
                    // non-matching IN completion (odd DCI) rather than drop it
                    // (H.2); the caller re-arms it after the drain.
                    let ev = trb::parse_transfer_event(&candidate);
                    if ev.endpoint_id & 1 == 1 && self.capture_interrupt_report(ev) {
                        completed.push((ev.slot_id, ev.endpoint_id));
                    }
                }
                _ => {}
            }
            self.consumer.dequeue_step();
        }
        found
    }

    /// Perform a control transfer on slot `slot_id`'s EP0 transfer ring.
    ///
    /// Builds: Setup Stage → (optional Data Stage IN) → Status Stage TRBs,
    /// rings Doorbell `slot_id` at DCI 1 (EP0), then blocks for the Transfer
    /// Event. On success returns the received data bytes (or an empty Vec for
    /// OUT-only transfers). Returns `None` on timeout or error completion code.
    pub fn control_transfer(
        &mut self,
        irq: &IrqNotification,
        slot_id: u8,
        setup: trb::SetupPacket,
        len: u16,
        dir_in: bool,
        out_data: Option<&[u8]>,
        drain_others: &mut dyn FnMut(),
    ) -> Option<alloc::vec::Vec<u8>> {
        // Stage the data-stage DMA buffer for any transfer that has one — IN
        // reads receive into it; OUT writes copy the host payload in before the
        // controller drains it (Phase 96 OCP register writes). Reuse the slot's
        // persistent `ep0_data_buf` scratch (grown on demand) instead of a fresh
        // allocation per call: `DmaBuffer::drop` is a no-op, so a per-call
        // allocation leaks an IOVA + DMA cap every control transfer, and the ure
        // NIC driver issues OCP control reads continuously. Returns the staged
        // buffer's IOVA for the Data Stage TRB. `slot_id` is validated here so a
        // bogus id from an untrusted IPC `ControlRequest` fails closed.
        let data_iova = if len > 0 {
            let need_grow = match self.slots.iter().find(|s| s.slot_id == slot_id) {
                Some(sc) => sc.ep0_data_buf.as_ref().map(|b| b.len()).unwrap_or(0) < len as usize,
                None => {
                    write_str(
                        STDOUT_FILENO,
                        "[xhci] ctrl-xfer: unknown slot id — rejecting request\n",
                    );
                    return None;
                }
            };
            if need_grow {
                let buf = match DmaBuffer::<u8>::allocate(&self.handle, len as usize, XHCI_ALIGN) {
                    Ok(b) => b,
                    Err(_) => {
                        write_str(STDOUT_FILENO, "[xhci] ctrl-xfer: data buf alloc failed\n");
                        return None;
                    }
                };
                let Some(sc) = self.slots.iter_mut().find(|s| s.slot_id == slot_id) else {
                    return None;
                };
                sc.ep0_data_buf = Some(buf);
            }
            // Zero the scratch (so a stale IN read returns clean bytes) and copy
            // the host→device payload in for an OUT transfer.
            let Some(sc) = self.slots.iter().find(|s| s.slot_id == slot_id) else {
                return None;
            };
            let buf = sc.ep0_data_buf.as_ref().expect("scratch grown above");
            zero_dma(buf);
            if !dir_in {
                // `out_data.len()` is bounded to `len` (callers validate
                // `wLength == data.len()`); clamp defensively anyway.
                let src = out_data.unwrap_or(&[]);
                let n = src.len().min(len as usize);
                for (i, &b) in src.iter().take(n).enumerate() {
                    // SAFETY: buf was grown to >= `len` bytes, i < n <= len.
                    unsafe { core::ptr::write_volatile(buf.user_ptr().add(i), b) };
                }
            }
            Some(buf.iova())
        } else {
            None
        };

        // `slot_id` arrives from an untrusted `ControlRequest` IPC message
        // (the server decodes it from a class driver's bulk payload), so a
        // bogus or stale id must NOT panic — `panic = abort` would take the
        // whole xHCI USB server down. Return `None` (the server maps it to a
        // `ControlData` error reply) exactly as the poll path does.
        let Some(sc) = self.slots.iter_mut().find(|s| s.slot_id == slot_id) else {
            write_str(
                STDOUT_FILENO,
                "[xhci] ctrl-xfer: unknown slot id — rejecting request\n",
            );
            return None;
        };

        // Build and enqueue the three-stage control transfer on the EP0 ring.
        let tt = if len == 0 {
            trb::SETUP_TT_NO_DATA
        } else if dir_in {
            trb::SETUP_TT_IN
        } else {
            trb::SETUP_TT_OUT
        };

        // Setup Stage TRB
        {
            let cycle = sc.ep0_producer.cycle;
            let setup_trb = trb::Trb::setup_stage(&setup, tt, cycle);
            write_trb(&sc.ep0_ring, sc.ep0_producer.enqueue, setup_trb);
            let cycle_before = sc.ep0_producer.cycle;
            if sc.ep0_producer.advance() {
                let iova = sc.ep0_ring_iova;
                write_trb(
                    &sc.ep0_ring,
                    sc.ep0_producer.link_index(),
                    trb::Trb::link(iova, true, cycle_before),
                );
            }
        }

        // Data Stage TRB (only for transfers with data)
        if let Some(iova) = data_iova {
            let cycle = sc.ep0_producer.cycle;
            let data_trb = trb::Trb::data_stage(iova, len as u32, dir_in, cycle);
            write_trb(&sc.ep0_ring, sc.ep0_producer.enqueue, data_trb);
            let cycle_before = sc.ep0_producer.cycle;
            if sc.ep0_producer.advance() {
                let iova = sc.ep0_ring_iova;
                write_trb(
                    &sc.ep0_ring,
                    sc.ep0_producer.link_index(),
                    trb::Trb::link(iova, true, cycle_before),
                );
            }
        }

        // Status Stage TRB — direction is opposite of data stage for IN,
        // or IN (true) for no-data OUT transfers (xHCI §4.11.2.2).
        // The IOC (Interrupt On Completion) bit is set so the controller
        // generates a Transfer Event when the status phase completes.
        {
            let cycle = sc.ep0_producer.cycle;
            // For IN data transfers status is OUT (dir_in=false); for OUT or
            // no-data, status is IN (dir_in=true).
            let status_dir_in = !dir_in || len == 0;
            // IOC is now set inside Trb::status_stage (kernel-core fix).
            let status_trb = trb::Trb::status_stage(status_dir_in, cycle);
            write_trb(&sc.ep0_ring, sc.ep0_producer.enqueue, status_trb);
            let cycle_before = sc.ep0_producer.cycle;
            if sc.ep0_producer.advance() {
                let iova = sc.ep0_ring_iova;
                write_trb(
                    &sc.ep0_ring,
                    sc.ep0_producer.link_index(),
                    trb::Trb::link(iova, true, cycle_before),
                );
            }
        }

        // Ring EP0 doorbell (DCI = 1).
        self.ring_doorbell(slot_id, 1);

        // Block for Transfer Event(s) — wait for the status stage to complete,
        // interleaving other-controller servicing so they don't starve (92d).
        let result = self.wait_for_transfer_event(irq, slot_id, 1, drain_others);

        match result {
            Some(ev) if ev.completion_code == trb::COMPLETION_SUCCESS => {
                // Read back data from the DMA buffer, length = len minus the
                // event's residual_transfer_length, clamped to [0, len].
                //
                // NOTE: the control TD sets IOC only on the (zero-length)
                // Status Stage TRB and does NOT set ISP on the Data Stage TRB,
                // so the single Transfer Event we receive reports the Status
                // TRB's residual (always 0 on success). `actual` therefore
                // currently equals `len` on every successful read — short-read
                // detection is a no-op until the Data Stage TRB sets ISP and we
                // drain its event. This is fine for 78b enumeration, where every
                // IN control read requests the descriptor's exact length; a
                // future caller needing true short-read accounting must add ISP.
                // Only an IN transfer has device-to-host data to read back; an
                // OUT transfer's scratch holds the payload we just sent, so
                // returning it would echo the request — return an empty Vec.
                if data_iova.is_some() && dir_in {
                    let residual = ev.residual_transfer_length.min(len as u32) as usize;
                    let actual = (len as usize).saturating_sub(residual);
                    let mut out = alloc::vec::Vec::with_capacity(actual);
                    // Re-borrow the slot's scratch buffer (the `wait_for_transfer_event`
                    // above took `&mut self`, ending the earlier borrow).
                    if let Some(sc) = self.slots.iter().find(|s| s.slot_id == slot_id)
                        && let Some(buf) = sc.ep0_data_buf.as_ref()
                    {
                        for i in 0..actual {
                            // SAFETY: buf was grown to >= `len` bytes, i < actual <= len.
                            let byte = unsafe { core::ptr::read_volatile(buf.user_ptr().add(i)) };
                            out.push(byte);
                        }
                    }
                    Some(out)
                } else {
                    Some(alloc::vec::Vec::new())
                }
            }
            Some(ev) => {
                write_str(STDOUT_FILENO, "[xhci] ctrl-xfer: completion code ");
                crate::write_u8_dec(ev.completion_code);
                write_str(STDOUT_FILENO, "\n");
                None
            }
            None => {
                write_str(STDOUT_FILENO, "[xhci] ctrl-xfer: no transfer event\n");
                None
            }
        }
    }

    /// Wait for a Transfer Event targeting `slot_id` / `endpoint_id` (DCI) by
    /// **polling** the event ring (not blocking on `irq.wait()`). `notify_wait`
    /// has no timeout, so a blocking wait deadlocks the single-threaded server
    /// on a never-completing transfer or a coalesced/already-consumed IRQ (a
    /// lost-wakeup race). Polling with a bounded budget (~400 ms) is race-free:
    /// a completion already on the ring is seen on the first drain, and a stuck
    /// transfer returns `None` instead of hanging the whole USB stack.
    fn wait_for_transfer_event(
        &mut self,
        _irq: &IrqNotification,
        slot_id: u8,
        endpoint_id: u8,
        drain_others: &mut dyn FnMut(),
    ) -> Option<trb::TransferEvent> {
        // Phase 92d: bound the dead-device stall. A healthy control transfer
        // completes in microseconds (well within the spin phase); 200 ms is the
        // dead/stuck-device ceiling (was 400 ms). The interleaved `drain_others`
        // below keeps co-resident controllers alive even across this window.
        const MAX_POLLS: u32 = 200;
        for poll_i in 0..(COMPLETION_SPIN_POLLS + MAX_POLLS) {
            let before = self.consumer.index;
            let mut completed: alloc::vec::Vec<(u8, u8)> = alloc::vec::Vec::new();
            let found = self.drain_for_transfer_event(slot_id, endpoint_id, &mut completed);
            // Only advance ERDP / clear IMAN.IP when the drain actually consumed
            // events. Writing ERDP|EHB on every empty poll (e.g. during a dense
            // run of control transfers) disturbs the controller's event-ring
            // bookkeeping and can stall subsequent completions.
            if self.consumer.index != before {
                let erdp =
                    self.event_ring_iova + (self.consumer.index as u64) * trb::TRB_SIZE as u64;
                self.rt_write_u64(rt_reg::ERDP, erdp | ERDP_EHB);
                self.rt_write_u32(rt_reg::IMAN, IMAN_IP | IMAN_IE);
            }
            // Re-arm any IN endpoint whose report we captured mid-transfer (H.2),
            // at its frame-sized `armed_len` so a bulk frame is not truncated.
            for (s, d) in completed {
                let len = self
                    .slot(s)
                    .and_then(|sc| sc.interrupt_eps.iter().find(|e| e.dci == d))
                    .map(|e| e.armed_len)
                    .unwrap_or(0);
                if len > 0 {
                    self.arm_ring_in(s, d, len);
                }
            }
            if let Some(ev) = found {
                return Some(ev);
            }
            if poll_i < COMPLETION_SPIN_POLLS {
                core::hint::spin_loop();
            } else {
                // Phase 92d interleaved drain: this control transfer is blocking
                // the single server loop. Before sleeping, service the OTHER
                // controllers' event rings so a co-resident controller (e.g. a
                // busy NIC) does not overflow its finite event ring / lose
                // completions while we wait here. No-op closure for the
                // single-controller and enumeration paths (no other controllers).
                drain_others();
                let _ = syscall_lib::nanosleep_for(0, 1_000_000);
            }
        }
        None
    }

    /// Drain the event ring looking for a Transfer Event for `slot_id` /
    /// `endpoint_id`. Returns the last matching event, consuming all
    /// produced events up to and including it.
    ///
    /// Phase 92 H.2: a non-matching **IN**-endpoint completion (odd DCI) seen
    /// while waiting for an EP0 control transfer is no longer dropped — it is
    /// routed through `capture_interrupt_report` and its `(slot, dci)` pushed
    /// into `completed` so the caller re-arms the endpoint after the drain
    /// (mirroring `wait_for_bulk_out_event`). Without this, a HID report or
    /// bulk-IN frame completing mid-control-transfer was lost and its endpoint
    /// left armed-but-completed.
    fn drain_for_transfer_event(
        &mut self,
        slot_id: u8,
        endpoint_id: u8,
        completed: &mut alloc::vec::Vec<(u8, u8)>,
    ) -> Option<trb::TransferEvent> {
        let mut found: Option<trb::TransferEvent> = None;
        loop {
            let seg = self.event_ring.as_ref().expect("event ring allocated");
            let candidate = read_trb(seg, self.consumer.index);
            if !self.consumer.owns(&candidate) {
                break;
            }
            match trb::event_trb_type(&candidate) {
                Some(trb::TrbType::TransferEvent) => {
                    let ev = trb::parse_transfer_event(&candidate);
                    if ev.slot_id == slot_id && ev.endpoint_id == endpoint_id {
                        found = Some(ev);
                    } else if ev.endpoint_id & 1 == 1 {
                        // A non-matching IN completion (odd DCI = IN per
                        // trb::dci) — capture it rather than drop it (H.2).
                        if self.capture_interrupt_report(ev) {
                            completed.push((ev.slot_id, ev.endpoint_id));
                        }
                    }
                }
                Some(trb::TrbType::CommandCompletion) => {
                    let ev = trb::parse_command_completion(&candidate);
                    self.on_command_completion(ev);
                }
                Some(trb::TrbType::PortStatusChange) => {
                    let ev = trb::parse_port_status_change(&candidate);
                    self.on_port_status_change(ev);
                }
                _ => {}
            }
            self.consumer.dequeue_step();
        }
        found
    }

    // -- 78b: per-slot context management ----------------------------------

    /// Allocate per-slot DMA: Output Device Context + EP0 transfer ring +
    /// Input Context. Install the Output Device Context IOVA into DCBAA[slot].
    pub fn alloc_slot_context(&mut self, slot_id: u8) -> Result<(), BringUpError> {
        let entry_size = self.context_size;
        let device_ctx_bytes = DEVICE_CONTEXT_ENTRIES * entry_size;
        let input_ctx_bytes = INPUT_CONTEXT_ENTRIES * entry_size;

        // Output Device Context
        let output_ctx = DmaBuffer::<u8>::allocate(&self.handle, device_ctx_bytes, XHCI_ALIGN)
            .map_err(|_| BringUpError::DmaAlloc)?;
        zero_dma(&output_ctx);

        // Install into DCBAA[slot_id]
        let dcbaa = self.dcbaa.as_ref().expect("DCBAA allocated");
        write_u64_at(dcbaa, slot_id as usize * 8, output_ctx.iova());

        // EP0 transfer ring
        let ep0_ring = DmaBuffer::<u8>::allocate(&self.handle, RING_BYTES, XHCI_ALIGN)
            .map_err(|_| BringUpError::DmaAlloc)?;
        zero_dma(&ep0_ring);
        let ep0_ring_iova = ep0_ring.iova();
        let ep0_producer = trb::ProducerRing::new(RING_TRBS);
        // Install trailing Link TRB on the EP0 ring.
        write_trb(
            &ep0_ring,
            ep0_producer.link_index(),
            trb::Trb::link(ep0_ring_iova, true, false),
        );

        // Input Context
        let input_ctx = DmaBuffer::<u8>::allocate(&self.handle, input_ctx_bytes, XHCI_ALIGN)
            .map_err(|_| BringUpError::DmaAlloc)?;
        zero_dma(&input_ctx);

        // Replace any stale context for this slot id (defensive), then append.
        self.slots.retain(|s| s.slot_id != slot_id);
        self.slots.push(SlotContext {
            slot_id,
            output_ctx,
            ep0_ring,
            ep0_producer,
            ep0_ring_iova,
            input_ctx,
            interrupt_eps: alloc::vec::Vec::new(),
            ep0_data_buf: None,
            device_desc: alloc::vec::Vec::new(),
            config_desc: alloc::vec::Vec::new(),
        });
        Ok(())
    }

    /// Cache the raw 18-byte Device Descriptor read during enumeration so a
    /// later `GetDescriptors` IPC can return it without a fresh control read
    /// (Phase 92 H.1). No-op for an unknown slot.
    pub fn cache_device_descriptor(&mut self, slot_id: u8, bytes: &[u8]) {
        if let Some(sc) = self.slot_mut(slot_id) {
            sc.device_desc = bytes.to_vec();
        }
    }

    /// Cache the raw Configuration Descriptor blob (`wTotalLength` bytes) read
    /// during enumeration for a later `GetDescriptors` IPC (Phase 92 H.1).
    pub fn cache_config_descriptor(&mut self, slot_id: u8, bytes: &[u8]) {
        if let Some(sc) = self.slot_mut(slot_id) {
            sc.config_desc = bytes.to_vec();
        }
    }

    /// Return the cached `(device, config)` descriptor blobs for `slot_id`, or
    /// `None` if the slot is unknown or *either* descriptor was never cached
    /// (Phase 92 H.1 — serves `UsbRequest::GetDescriptors`). `GetDescriptors`
    /// promises both the device **and** the full configuration blob, so an empty
    /// `config_desc` is treated as "not cached" rather than handed to the client
    /// as a valid-but-empty config.
    pub fn cached_descriptors(
        &self,
        slot_id: u8,
    ) -> Option<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>)> {
        let sc = self.slot(slot_id)?;
        if sc.device_desc.is_empty() || sc.config_desc.is_empty() {
            return None;
        }
        Some((sc.device_desc.clone(), sc.config_desc.clone()))
    }

    /// Issue a **Disable Slot** command for `slot_id`, drain its completion, and
    /// reclaim the slot's resources: clear `DCBAA[slot_id]` and drop the
    /// `SlotContext` (freeing its EP0 ring / output context / scratch DMA on the
    /// next process-exit reclaim). The matching teardown for Enable Slot — used
    /// on hot-plug detach + re-enumeration so slot IDs are not leaked (Phase 92
    /// H.3). Returns `true` on a successful Command Completion. A bogus
    /// `slot_id` (no live `SlotContext`) is rejected without touching hardware.
    pub fn disable_slot(&mut self, irq: &IrqNotification, slot_id: u8) -> bool {
        if self.slot(slot_id).is_none() {
            return false;
        }
        let cycle = self.producer.cycle;
        let cmd = trb::Trb::disable_slot(slot_id, cycle);
        let ev = self.issue_command_and_wait(irq, cmd);
        let ok = ev.completion_code == trb::COMPLETION_SUCCESS;
        if ok {
            // Clear the DCBAA entry so the controller no longer references the
            // freed Output Device Context, then drop the per-slot state.
            if let Some(dcbaa) = self.dcbaa.as_ref() {
                write_u64_at(dcbaa, slot_id as usize * 8, 0);
            }
            self.slots.retain(|s| s.slot_id != slot_id);
        } else {
            write_str(STDOUT_FILENO, "[xhci] Disable Slot failed: cc=");
            crate::write_u8_dec(ev.completion_code);
            write_str(STDOUT_FILENO, "\n");
        }
        ok
    }

    /// Write a snapshot into the Input Context DMA buffer and return the IOVA.
    /// Layout (xHCI §6.2.5):
    ///   [0]            = Input Control Context (Add Flags at dword1, Drop at dword0=0)
    ///   [entry_size]   = Slot Context (dword0, dword1)
    ///   [(1+dci)*es]   = Endpoint Context for each DCI
    pub fn write_input_context(
        &mut self,
        slot_id: u8,
        snap: &kernel_core::usb::enumerate::InputContextSnapshot,
    ) -> u64 {
        let entry_size = self.context_size;
        let sc = self
            .slots
            .iter_mut()
            .find(|s| s.slot_id == slot_id)
            .expect("slot context allocated");
        zero_dma(&sc.input_ctx);

        // Input Control Context: Drop Flags (dword0) = 0, Add Flags (dword1).
        let icc_base = input_control_offset();
        write_u32_at(&sc.input_ctx, icc_base, 0); // drop flags
        write_u32_at(&sc.input_ctx, icc_base + 4, snap.add_flags); // add flags

        // Slot Context dwords 0 and 1.
        let slot_base = input_slot_offset(entry_size);
        write_u32_at(&sc.input_ctx, slot_base, snap.slot_dword0);
        write_u32_at(&sc.input_ctx, slot_base + 4, snap.slot_dword1);

        // EP0 Context (DCI=1): dword1 at ep_base+4, dequeue ptr at dwords 2-3.
        let ep0_base = input_endpoint_offset(1, entry_size);
        write_u32_at(&sc.input_ctx, ep0_base + 4, snap.ep0_dword1);
        write_u64_at(&sc.input_ctx, ep0_base + 8, snap.ep0_dequeue_ptr);
        // EP0 Average TRB Length (dword4, bits 15:0) = 8 — the conventional
        // value for the default control endpoint (xHCI 1.2 §6.2.3.2 /
        // §4.14.1.1 recommend a non-zero estimate). qemu-xhci tolerates 0, but
        // real controllers use it for bandwidth/scheduling estimation.
        write_u32_at(&sc.input_ctx, ep0_base + 16, 8);

        // Additional endpoint contexts (interrupt/bulk/etc.).
        for ep_snap in &snap.endpoint_contexts {
            let ep_base = input_endpoint_offset(ep_snap.dci, entry_size);
            write_u32_at(&sc.input_ctx, ep_base, ep_snap.ep_dword0);
            write_u32_at(&sc.input_ctx, ep_base + 4, ep_snap.ep_dword1);
            write_u64_at(&sc.input_ctx, ep_base + 8, ep_snap.ep_dequeue_ptr);
            // dword4: Average TRB Length (bits 15:0) = MPS, Max ESIT Payload
            // (bits 31:16) = MPS. For FS/HS interrupt endpoints the controller
            // uses these values for scheduling; setting them to the MPS is the
            // correct value (one full packet per service interval).
            // MPS lives at bits 31:16 of ep_dword1.
            let mps = (ep_snap.ep_dword1 >> 16) as u16;
            let dword4 = (mps as u32) | ((mps as u32) << 16);
            write_u32_at(&sc.input_ctx, ep_base + 16, dword4);
        }

        sc.input_ctx.iova()
    }

    /// Return the IOVA of the EP0 transfer ring for slot `slot_id` (after
    /// `alloc_slot_context`). Used by the enumeration ops to patch the EP0
    /// dequeue pointer of the Input Context snapshot.
    pub fn ep0_ring_iova(&self, slot_id: u8) -> u64 {
        self.slot(slot_id)
            .expect("slot context allocated")
            .ep0_ring_iova
    }

    /// Return the current producer cycle bit — consumed by `enumerate.rs`
    /// before building each command TRB.
    pub fn producer_cycle(&self) -> bool {
        self.producer.cycle
    }

    /// Allocate a transfer ring + report buffer for endpoint `dci` (with
    /// `wMaxPacketSize` `mps`) and return the ring IOVA with DCS=1 ORed in.
    /// Used by `configure_endpoint` in `enumerate.rs` to replace the
    /// placeholder IOVA of 0 the enumeration state machine puts in
    /// `EndpointContextSnapshot.ep_dequeue_ptr`.
    ///
    /// The ring + report buffer are kept alive for the controller's lifetime
    /// (stored in the `slot_ctx`'s `interrupt_eps` Vec). Phase 78c arms the
    /// endpoint with Normal TRBs into the report buffer to receive HID reports.
    pub fn alloc_interrupt_ep_ring(
        &mut self,
        slot_id: u8,
        dci: u8,
        mps: u16,
    ) -> Result<u64, BringUpError> {
        let ring = DmaBuffer::<u8>::allocate(&self.handle, RING_BYTES, XHCI_ALIGN)
            .map_err(|_| BringUpError::DmaAlloc)?;
        zero_dma(&ring);
        let iova = ring.iova();
        // Install trailing Link TRB.
        let producer = trb::ProducerRing::new(RING_TRBS);
        write_trb(
            &ring,
            producer.link_index(),
            trb::Trb::link(iova, true, false),
        );

        // IN endpoints (odd dci) keep `RX_QUEUE_DEPTH` TRBs outstanding, each
        // into its own buffer, so the device always has somewhere to DMA the
        // next frame; OUT endpoints (even dci) only ever submit one transfer at
        // a time, so a single buffer suffices.
        let depth = if dci & 1 == 1 { RX_QUEUE_DEPTH } else { 1 };

        // Persistent buffers the controller writes reports into (at least 8
        // bytes each for a boot keyboard report). Bulk-IN grows them to a
        // frame-sized length on the first arm.
        let data_len = (mps as usize).max(8);
        let mut data_bufs = alloc::vec::Vec::with_capacity(depth);
        for _ in 0..depth {
            let buf = DmaBuffer::<u8>::allocate(&self.handle, data_len, XHCI_ALIGN)
                .map_err(|_| BringUpError::DmaAlloc)?;
            zero_dma(&buf);
            data_bufs.push(buf);
        }

        let sc = self
            .slots
            .iter_mut()
            .find(|s| s.slot_id == slot_id)
            .expect("slot context allocated");
        sc.interrupt_eps.push(InterruptEndpoint {
            dci,
            mps,
            ring,
            ring_iova: iova,
            producer,
            data_bufs,
            reports: alloc::collections::VecDeque::new(),
            in_flight: 0,
            arm_next: 0,
            drain_next: 0,
            depth,
            armed_len: mps as u32,
        });

        // Return IOVA | DCS (DCS=1 means initial cycle state = true).
        Ok(iova | 1)
    }

    // -- 78c: interrupt-IN arming, report capture, and polling -------------

    /// Maximum captured-but-unpolled reports kept per interrupt endpoint.
    /// Bounds memory if a class driver stops polling; oldest reports are
    /// dropped first (a stale boot report is worthless next to a fresh one).
    const MAX_PENDING_REPORTS: usize = 16;

    /// Top the (`slot_id`, `dci`) IN endpoint's queue back up to `depth`
    /// outstanding Normal TRBs of `len` bytes each, one per free buffer in its
    /// cyclic `data_bufs` ring, then ring the slot doorbell once so the
    /// controller (re)starts delivering IN reports/frames. Each buffer is grown
    /// to `len` if smaller (bulk-IN needs a frame-sized buffer, larger than a
    /// HID `mps`) — always safe because only currently-free buffers (`arm_next`
    /// chasing `drain_next`) are touched. Records `len` as `armed_len` so
    /// capture and re-arm agree on the size. Idempotent: a no-op when already at
    /// `depth`. Returns `false` if no such endpoint exists; a buffer-alloc
    /// failure mid-fill stops early (leaving whatever was already armed) and
    /// still rings the doorbell. This is the shared core of `arm_interrupt_in` /
    /// `arm_bulk_in` and every re-arm site.
    fn arm_ring_in(&mut self, slot_id: u8, dci: u8, len: u32) -> bool {
        loop {
            // Probe: locate the endpoint, decide whether there is a free slot to
            // arm and whether the next buffer needs growing. Done in its own
            // borrow scope so `&self.handle` (for allocation) and `&mut self`
            // (for the ring write) don't overlap.
            let (buf_idx, need_grow) = match self.slot(slot_id) {
                Some(sc) => match sc.interrupt_eps.iter().find(|e| e.dci == dci) {
                    Some(ep) if ep.in_flight >= ep.depth => break, // already full
                    Some(ep) => (
                        ep.arm_next,
                        (len as usize) > ep.data_bufs[ep.arm_next].len(),
                    ),
                    None => return false,
                },
                None => return false,
            };
            if need_grow {
                let new_buf =
                    match DmaBuffer::<u8>::allocate(&self.handle, len as usize, XHCI_ALIGN) {
                        Ok(buf) => buf,
                        Err(_) => {
                            write_str(STDOUT_FILENO, "[xhci] arm: bulk rx buf alloc failed\n");
                            break; // keep whatever is already armed
                        }
                    };
                let Some(sc) = self.slot_mut(slot_id) else {
                    return false;
                };
                let Some(ep) = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci) else {
                    return false;
                };
                ep.data_bufs[buf_idx] = new_buf;
            }
            {
                let Some(sc) = self.slot_mut(slot_id) else {
                    return false;
                };
                let Some(ep) = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci) else {
                    return false;
                };
                zero_dma(&ep.data_bufs[buf_idx]);
                let cycle = ep.producer.cycle;
                write_trb(
                    &ep.ring,
                    ep.producer.enqueue,
                    trb::Trb::normal(ep.data_bufs[buf_idx].iova(), len, cycle),
                );
                let cycle_before = ep.producer.cycle;
                if ep.producer.advance() {
                    let iova = ep.ring_iova;
                    write_trb(
                        &ep.ring,
                        ep.producer.link_index(),
                        trb::Trb::link(iova, true, cycle_before),
                    );
                }
                ep.arm_next = (ep.arm_next + 1) % ep.depth;
                ep.in_flight += 1;
                ep.armed_len = len;
            }
        }
        // Doorbell after the mutable borrow ends (ring_doorbell takes &self). A
        // single kick processes every TRB just queued.
        self.ring_doorbell(slot_id, dci);
        true
    }

    /// Top an interrupt-IN endpoint's queue up to `depth` Normal TRBs sized to
    /// its `mps` (Phase 78c HID path). Returns `false` if no such endpoint
    /// exists.
    pub fn arm_interrupt_in(&mut self, slot_id: u8, dci: u8) -> bool {
        let mps = match self
            .slot(slot_id)
            .and_then(|sc| sc.interrupt_eps.iter().find(|e| e.dci == dci))
        {
            Some(ep) => ep.mps as u32,
            None => return false,
        };
        self.arm_ring_in(slot_id, dci, mps)
    }

    /// Top a bulk-IN endpoint's queue up to `depth` Normal TRBs sized to
    /// `rx_len` (frame-sized), growing each free buffer if needed. Phase 96.
    /// Returns `false` on failure.
    pub fn arm_bulk_in(&mut self, slot_id: u8, dci: u8, rx_len: u32) -> bool {
        self.arm_ring_in(slot_id, dci, rx_len)
    }

    /// Drain the event ring (non-blocking): capture interrupt-IN reports into
    /// their endpoint's FIFO, re-arm those endpoints, and dispatch command /
    /// port-status events normally. Called from the server loop on every bound
    /// IRQ-notification wake. Updates ERDP + clears IMAN.IP at the end.
    pub fn service_interrupt_events(&mut self) {
        // Collect (dci, report) pairs first so re-arming (which needs &mut
        // self via ring_doorbell) happens after the drain borrow ends.
        let mut completed: alloc::vec::Vec<(u8, u8)> = alloc::vec::Vec::new();
        // Optimization B (Phase 92d): count consumed events so an *empty* drain
        // skips the ERDP/IMAN write below. Comparing the dequeue index alone is
        // unsound — a drain that consumes exactly one full ring (RING_TRBS = 64
        // events) wraps `consumer.index` back to its starting value (and toggles
        // CCS), so `index == before` despite real consumption, which would
        // wrongly skip the ERDP write and leave IMAN.IP set (a stuck pending
        // interrupt). A direct count is wrap-proof. With multiplexed interrupts
        // the server drains controllers far more often (every IRQ + every client
        // poll re-drains), and an empty-poll ERDP write both wastes an MMIO and,
        // per the command-completion path's own guard, "disturbs the
        // controller's event-ring bookkeeping". Only touch ERDP/IMAN when at
        // least one event was actually consumed.
        let mut drained = 0usize;
        loop {
            let seg = self.event_ring.as_ref().expect("event ring allocated");
            let candidate = read_trb(seg, self.consumer.index);
            if !self.consumer.owns(&candidate) {
                break;
            }
            match trb::event_trb_type(&candidate) {
                Some(trb::TrbType::TransferEvent) => {
                    let ev = trb::parse_transfer_event(&candidate);
                    // DIAGNOSTIC: a non-success / non-short-packet completion on a
                    // transfer endpoint means the endpoint likely HALTED (STALL=6,
                    // transaction-error=4, babble=3, TRB-error=5). The current
                    // re-arm path enqueues a fresh TRB on a halted endpoint, which
                    // never runs — so RX wedges under TX load. Log the code so the
                    // bare-metal failure is attributable; bounded to avoid flooding.
                    if ev.completion_code != trb::COMPLETION_SUCCESS
                        && ev.completion_code != COMPLETION_SHORT_PACKET
                        && self.xfer_err_logged < 12
                    {
                        self.xfer_err_logged += 1;
                        write_str(STDOUT_FILENO, "xhci: xfer ERR cc=");
                        crate::write_u8_dec(ev.completion_code);
                        write_str(STDOUT_FILENO, " slot=");
                        crate::write_u8_dec(ev.slot_id);
                        write_str(STDOUT_FILENO, " ep=");
                        crate::write_u8_dec(ev.endpoint_id);
                        write_str(STDOUT_FILENO, "\n");
                    }
                    if self.capture_interrupt_report(ev) {
                        completed.push((ev.slot_id, ev.endpoint_id));
                    }
                }
                Some(trb::TrbType::CommandCompletion) => {
                    let ev = trb::parse_command_completion(&candidate);
                    self.on_command_completion(ev);
                }
                Some(trb::TrbType::PortStatusChange) => {
                    let ev = trb::parse_port_status_change(&candidate);
                    self.on_port_status_change(ev);
                }
                _ => {}
            }
            self.consumer.dequeue_step();
            drained += 1;
        }
        // Advance the controller's dequeue pointer and clear the pending bit —
        // but only when the drain actually consumed events (Optimization B): an
        // empty-poll ERDP write is wasted MMIO and disturbs the ring bookkeeping
        // (the command-completion path guards the same way).
        if drained > 0 {
            let erdp = self.event_ring_iova + (self.consumer.index as u64) * trb::TRB_SIZE as u64;
            self.rt_write_u64(rt_reg::ERDP, erdp | ERDP_EHB);
            self.rt_write_u32(rt_reg::IMAN, IMAN_IP | IMAN_IE);
        }
        // Re-arm every endpoint that just completed so the next report/frame
        // lands. Re-arm at the endpoint's last `armed_len` (preserving a bulk
        // endpoint's frame-sized buffer) rather than at `mps`.
        for (slot_id, dci) in completed {
            let len = self
                .slot(slot_id)
                .and_then(|sc| sc.interrupt_eps.iter().find(|e| e.dci == dci))
                .map(|e| e.armed_len)
                .unwrap_or(0);
            if len > 0 {
                self.arm_ring_in(slot_id, dci, len);
            }
        }
    }

    /// Read the just-completed report out of `ev`'s endpoint buffer (the one at
    /// `drain_next` — completions arrive in arm order on a single ring) and push
    /// it onto that endpoint's FIFO. Advances the drain cursor and decrements
    /// `in_flight` so the caller's re-arm tops the queue back up to `depth`.
    /// Returns `true` if a matching endpoint with an outstanding TRB was found
    /// (so the caller re-arms it).
    fn capture_interrupt_report(&mut self, ev: trb::TransferEvent) -> bool {
        let Some(sc) = self.slot_mut(ev.slot_id) else {
            return false;
        };
        let Some(ep) = sc
            .interrupt_eps
            .iter_mut()
            .find(|e| e.dci == ev.endpoint_id)
        else {
            return false;
        };
        // Defend against a spurious/duplicate completion with nothing armed —
        // decrementing `in_flight` below 0 (or draining a stale buffer) would
        // desync the cyclic cursors.
        if ep.in_flight == 0 {
            return false;
        }
        let buf_idx = ep.drain_next;
        // Use the length the TRB was actually armed with (= `mps` for interrupt
        // endpoints, a frame-sized value for bulk-IN), not `mps`, or a bulk
        // frame would be truncated to the HID report size.
        let cap = ep.armed_len as usize;
        let residual = (ev.residual_transfer_length as usize).min(cap);
        let len = cap.saturating_sub(residual);
        // This buffer's TRB has completed — advance the drain cursor and free
        // the slot regardless of payload length so the cyclic ring stays in
        // lockstep with the arm cursor.
        ep.drain_next = (ep.drain_next + 1) % ep.depth;
        ep.in_flight -= 1;
        // A zero-length completion (no data) is still a valid wake — capture an
        // empty report only when there is data; otherwise just re-arm.
        if len == 0 {
            return true;
        }
        let mut out = alloc::vec::Vec::with_capacity(len);
        for i in 0..len {
            // SAFETY: data_bufs[buf_idx] was (re)allocated to at least
            // `armed_len` bytes and `i < len <= armed_len`.
            let byte = unsafe { core::ptr::read_volatile(ep.data_bufs[buf_idx].user_ptr().add(i)) };
            out.push(byte);
        }
        if ep.reports.len() >= Self::MAX_PENDING_REPORTS {
            ep.reports.pop_front();
        }
        ep.reports.push_back(out);
        true
    }

    /// Pop the oldest captured report for (`slot_id`, `dci`), topping the
    /// endpoint's TRB queue back up to `depth` if it has fallen below (lazy arm
    /// on first poll, re-fill afterwards). Returns `None` if no report is queued.
    pub fn take_interrupt_report(&mut self, slot_id: u8, dci: u8) -> Option<alloc::vec::Vec<u8>> {
        let (report, need_arm) = {
            let sc = self.slot_mut(slot_id)?;
            let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
            (ep.reports.pop_front(), ep.in_flight < ep.depth)
        };
        if need_arm {
            self.arm_interrupt_in(slot_id, dci);
        }
        report
    }

    /// Pop the oldest captured bulk-IN buffer for (`slot_id`, `dci`), topping
    /// the endpoint's queue back up to `depth` `rx_len`-sized Normal TRBs if it
    /// has fallen below (lazy arm on first poll, re-fill afterwards). Phase 96.
    /// The returned buffer is a raw bulk-IN payload (one or more
    /// Realtek-descriptor-prefixed frames); the class driver splits it. Returns
    /// `None` if nothing is queued.
    pub fn take_bulk_report(
        &mut self,
        slot_id: u8,
        dci: u8,
        rx_len: u32,
    ) -> Option<alloc::vec::Vec<u8>> {
        let (report, need_arm) = {
            let sc = self.slot_mut(slot_id)?;
            let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
            (ep.reports.pop_front(), ep.in_flight < ep.depth)
        };
        if need_arm {
            self.arm_bulk_in(slot_id, dci, rx_len);
        }
        report
    }

    /// Submit a bulk-OUT transfer of `data` on (`slot_id`, `dci`) and block for
    /// its completion. Phase 96 — the TX half of a USB NIC. The endpoint's
    /// `data_bufs[0]` source buffer is grown to `data.len()` if needed (an OUT
    /// endpoint has `depth` = 1 so it only uses buffer 0), the frame is copied
    /// in, a Normal TRB is enqueued (IOC), the doorbell rung, and the Transfer
    /// Event awaited. Returns the number of bytes transferred, or `None` on
    /// timeout/error. Bulk-IN completions seen while waiting are captured (not
    /// dropped) so concurrent RX is not lost.
    pub fn submit_bulk_out(
        &mut self,
        irq: &IrqNotification,
        slot_id: u8,
        dci: u8,
        data: &[u8],
    ) -> Option<usize> {
        let len = data.len() as u32;
        if len == 0 {
            return Some(0);
        }
        // Grow the OUT data buffer if needed (disjoint borrow from self.handle).
        let need_grow = match self.slot(slot_id) {
            Some(sc) => match sc.interrupt_eps.iter().find(|e| e.dci == dci) {
                Some(ep) => data.len() > ep.data_bufs[0].len(),
                None => return None,
            },
            None => return None,
        };
        if need_grow {
            let new_buf = DmaBuffer::<u8>::allocate(&self.handle, data.len(), XHCI_ALIGN).ok()?;
            let sc = self.slot_mut(slot_id)?;
            let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
            ep.data_bufs[0] = new_buf;
        }
        {
            let sc = self.slot_mut(slot_id)?;
            let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
            // Copy the frame into the DMA buffer.
            for (i, &b) in data.iter().enumerate() {
                // SAFETY: data_bufs[0] was (re)allocated to >= data.len() bytes; i < data.len().
                unsafe { core::ptr::write_volatile(ep.data_bufs[0].user_ptr().add(i), b) };
            }
            let cycle = ep.producer.cycle;
            write_trb(
                &ep.ring,
                ep.producer.enqueue,
                trb::Trb::normal(ep.data_bufs[0].iova(), len, cycle),
            );
            let cycle_before = ep.producer.cycle;
            if ep.producer.advance() {
                let iova = ep.ring_iova;
                write_trb(
                    &ep.ring,
                    ep.producer.link_index(),
                    trb::Trb::link(iova, true, cycle_before),
                );
            }
        }
        self.ring_doorbell(slot_id, dci);
        match self.wait_for_bulk_out_event(irq, slot_id, dci) {
            // Accept SHORT_PACKET as well as SUCCESS, mirroring `submit_bulk_in`.
            // The xHC reports a multi-packet bulk-OUT TD whose last packet is
            // short — or whose data buffer was consumed exactly — with a Short
            // Packet completion (cc=13) and a residual, NOT a hard error. The
            // earlier SUCCESS-only filter rejected a perfectly good multi-sector
            // BOT WRITE(10) data-OUT (e.g. 7 × 512 = 3584 B), wedging the write
            // path; the transferred count is `len - residual` either way.
            Some(ev)
                if ev.completion_code == trb::COMPLETION_SUCCESS
                    || ev.completion_code == COMPLETION_SHORT_PACKET =>
            {
                let residual = ev.residual_transfer_length.min(len) as usize;
                Some((len as usize).saturating_sub(residual))
            }
            Some(ev) => {
                // A genuine transport failure (STALL, Babble, transaction error,
                // data-buffer error) — surface the completion code so it is
                // diagnosable rather than an opaque `None`.
                write_str(STDOUT_FILENO, "[xhci] bulk-OUT non-success cc=");
                crate::write_u8_dec(ev.completion_code);
                write_str(STDOUT_FILENO, "\n");
                None
            }
            None => None,
        }
    }

    /// Submit a **synchronous, single-TRB** bulk-IN transfer of exactly `len`
    /// bytes on (`slot_id`, `dci`) and block for its completion, returning the
    /// received bytes (short packets honored via the event residual). Phase 92
    /// Track D — the Bulk-Only Transport (BOT) data + CSW phases.
    ///
    /// This is **distinct** from the streaming `arm_bulk_in`/`take_bulk_report`
    /// path: BOT is a strict request/response protocol (the device sends data
    /// only when commanded and returns to a CBW-wait state afterwards), so the
    /// host must NOT keep surplus IN TRBs queued. The streaming path arms `depth`
    /// (4) outstanding TRBs and auto-re-arms after every completion — perfect for
    /// a NIC that always has another frame, but fatal for BOT: the surplus IN
    /// tokens issued after the device's data + CSW (while it is back in CBW-wait)
    /// make the device STALL the bulk-IN endpoint (`cc=6`), wedging it. This
    /// method instead enqueues exactly one Normal TRB (IOC) of `len` bytes, rings
    /// the doorbell once, waits for that single completion, and never re-arms —
    /// the correct one-transfer-per-phase discipline. The endpoint's `in_flight`
    /// streaming cursors are left at 0 (the BOT daemon never mixes the two paths
    /// on the same endpoint), so the shared `ep.producer` ring stays consistent.
    ///
    /// `data_bufs[0]` is grown to `len` if needed (a multi-sector READ(10) can
    /// exceed the `mps`-sized initial buffer). Returns `None` on timeout or a
    /// non-success/short completion (e.g. a genuine STALL), so the caller can
    /// surface a transport failure.
    pub fn submit_bulk_in(
        &mut self,
        irq: &IrqNotification,
        slot_id: u8,
        dci: u8,
        len: u32,
    ) -> Option<alloc::vec::Vec<u8>> {
        if len == 0 {
            return Some(alloc::vec::Vec::new());
        }
        // Grow the IN data buffer if the requested length exceeds it (disjoint
        // borrow from self.handle), mirroring submit_bulk_out's OUT buffer grow.
        let need_grow = match self.slot(slot_id) {
            Some(sc) => match sc.interrupt_eps.iter().find(|e| e.dci == dci) {
                Some(ep) => (len as usize) > ep.data_bufs[0].len(),
                None => return None,
            },
            None => return None,
        };
        if need_grow {
            let new_buf = DmaBuffer::<u8>::allocate(&self.handle, len as usize, XHCI_ALIGN).ok()?;
            let sc = self.slot_mut(slot_id)?;
            let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
            ep.data_bufs[0] = new_buf;
        }
        {
            let sc = self.slot_mut(slot_id)?;
            let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
            zero_dma(&ep.data_bufs[0]);
            let cycle = ep.producer.cycle;
            write_trb(
                &ep.ring,
                ep.producer.enqueue,
                trb::Trb::normal(ep.data_bufs[0].iova(), len, cycle),
            );
            let cycle_before = ep.producer.cycle;
            if ep.producer.advance() {
                let iova = ep.ring_iova;
                write_trb(
                    &ep.ring,
                    ep.producer.link_index(),
                    trb::Trb::link(iova, true, cycle_before),
                );
            }
        }
        self.ring_doorbell(slot_id, dci);
        match self.wait_for_bulk_out_event(irq, slot_id, dci) {
            Some(ev)
                if ev.completion_code == trb::COMPLETION_SUCCESS
                    || ev.completion_code == COMPLETION_SHORT_PACKET =>
            {
                let residual = (ev.residual_transfer_length.min(len)) as usize;
                let n = (len as usize).saturating_sub(residual);
                let sc = self.slot(slot_id)?;
                let ep = sc.interrupt_eps.iter().find(|e| e.dci == dci)?;
                let mut out = alloc::vec::Vec::with_capacity(n);
                for i in 0..n {
                    // SAFETY: data_bufs[0] is >= len bytes and i < n <= len.
                    let byte =
                        unsafe { core::ptr::read_volatile(ep.data_bufs[0].user_ptr().add(i)) };
                    out.push(byte);
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Phase 92a H.4 — IOMMU-map a shared-memory region into the xHCI device's
    /// domain (zero-copy DMA substrate) and return its device IOVA, or `None`.
    pub fn map_shm(&self, shm_id: u32) -> Option<u64> {
        self.handle.map_shm_dma(shm_id)
    }

    /// Tear down a mapping installed by [`Controller::map_shm`].
    pub fn unmap_shm(&self, iova: u64) {
        self.handle.unmap_shm_dma(iova);
    }

    /// Phase 92a H.4 — submit a **single-TRB, zero-copy** bulk transfer of `len`
    /// bytes whose buffer already lives at device IOVA `iova` (a shared-memory
    /// region mapped via [`Controller::map_shm`]) and block for completion.
    /// Direction (IN vs OUT) is the endpoint's, selected by `dci`; the TRB shape
    /// is identical either way. Unlike `submit_bulk_out`/`submit_bulk_in` there
    /// is **no host-side copy** — the device DMAs straight into/out of the shared
    /// buffer the class driver also maps. Returns the transferred byte count
    /// (`len - residual`), or `None` on timeout / a non-success completion.
    pub fn submit_bulk_iova(
        &mut self,
        irq: &IrqNotification,
        slot_id: u8,
        dci: u8,
        iova: u64,
        len: u32,
    ) -> Option<usize> {
        if len == 0 {
            return Some(0);
        }
        {
            let sc = self.slot_mut(slot_id)?;
            let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
            let cycle = ep.producer.cycle;
            write_trb(
                &ep.ring,
                ep.producer.enqueue,
                trb::Trb::normal(iova, len, cycle),
            );
            let cycle_before = ep.producer.cycle;
            if ep.producer.advance() {
                let ring_iova = ep.ring_iova;
                write_trb(
                    &ep.ring,
                    ep.producer.link_index(),
                    trb::Trb::link(ring_iova, true, cycle_before),
                );
            }
        }
        self.ring_doorbell(slot_id, dci);
        match self.wait_for_bulk_out_event(irq, slot_id, dci) {
            Some(ev)
                if ev.completion_code == trb::COMPLETION_SUCCESS
                    || ev.completion_code == COMPLETION_SHORT_PACKET =>
            {
                let residual = ev.residual_transfer_length.min(len) as usize;
                Some((len as usize).saturating_sub(residual))
            }
            Some(ev) => {
                write_str(STDOUT_FILENO, "[xhci] shm bulk non-success cc=");
                crate::write_u8_dec(ev.completion_code);
                write_str(STDOUT_FILENO, "\n");
                None
            }
            None => None,
        }
    }

    /// Phase 92c (E.1/E.3) — submit an **isochronous OUT** transfer of `data` on
    /// (`slot_id`, `dci`) and block for completion. This is the USB-audio (UAC)
    /// PCM-out path: a class driver dribbles a service-interval's worth of mixed
    /// PCM out to the device.
    ///
    /// `data` is copied into the endpoint's `data_bufs[0]` source buffer (grown
    /// on demand) and split into **one Isoch TRB per `wMaxPacketSize`-sized
    /// frame** — a full-speed isochronous TD carries at most `mps` bytes per
    /// (micro)frame, so a single oversized TD is rejected (STALL/NAK) by the
    /// device. Each TRB is enqueued with **SIA = 1** (Start Isoch ASAP — the
    /// controller schedules it on the next available frame, so the host needs no
    /// Frame-ID bookkeeping). Frames are queued in bounded batches (≤
    /// `ISOCH_BATCH_FRAMES`, well within the transfer + event rings); the
    /// doorbell is rung once per batch and the batch's completions drained.
    ///
    /// Isochronous transfers have **no retry**: a `Ring Underrun` (the periodic
    /// scheduler found the ring empty between batches) or `Missed Service`
    /// completion is the expected steady-state event, not a transport failure —
    /// the audio backend's clock, not a per-frame handshake, governs flow. Such
    /// frames are counted as delivered. Returns the byte count submitted, or
    /// `None` on a genuine transport STALL / timeout.
    pub fn submit_isoch_out(
        &mut self,
        irq: &IrqNotification,
        slot_id: u8,
        dci: u8,
        data: &[u8],
    ) -> Option<usize> {
        /// Frames queued per doorbell — bounded so a batch never laps the 64-TRB
        /// transfer ring nor floods the 64-entry event ring before it is drained.
        const ISOCH_BATCH_FRAMES: usize = 16;

        if data.is_empty() {
            return Some(0);
        }
        // Resolve the endpoint's max-packet size and grow the source buffer to
        // hold the whole payload (disjoint borrow from self.handle).
        let (mps, need_grow) = match self.slot(slot_id) {
            Some(sc) => match sc.interrupt_eps.iter().find(|e| e.dci == dci) {
                Some(ep) => ((ep.mps as usize).max(1), data.len() > ep.data_bufs[0].len()),
                None => return None,
            },
            None => return None,
        };
        if need_grow {
            let new_buf = DmaBuffer::<u8>::allocate(&self.handle, data.len(), XHCI_ALIGN).ok()?;
            let sc = self.slot_mut(slot_id)?;
            let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
            ep.data_bufs[0] = new_buf;
        }
        // Copy the whole payload into the DMA buffer once; TRBs point into it at
        // mps-sized offsets. A single `copy_nonoverlapping` is used rather than a
        // per-byte volatile loop on this periodic audio hot path; the subsequent
        // doorbell MMIO write (a volatile store) orders these buffer stores ahead
        // of the controller being told to read the ring.
        let base_iova = {
            let sc = self.slot_mut(slot_id)?;
            let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
            // SAFETY: data_bufs[0] was (re)allocated to >= data.len() above, the
            // source and destination regions are distinct, and u8 has no
            // alignment requirement.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    ep.data_bufs[0].user_ptr(),
                    data.len(),
                );
            }
            ep.data_bufs[0].iova()
        };

        let len = data.len();
        let mut offset = 0usize;
        while offset < len {
            // Queue a batch of mps-sized isoch TRBs.
            let mut batch = 0usize;
            {
                let sc = self.slot_mut(slot_id)?;
                let ep = sc.interrupt_eps.iter_mut().find(|e| e.dci == dci)?;
                while offset < len && batch < ISOCH_BATCH_FRAMES {
                    let chunk = (len - offset).min(mps) as u32;
                    let cycle = ep.producer.cycle;
                    write_trb(
                        &ep.ring,
                        ep.producer.enqueue,
                        trb::Trb::isoch(base_iova + offset as u64, chunk, 0, true, cycle),
                    );
                    let cycle_before = ep.producer.cycle;
                    if ep.producer.advance() {
                        let ring_iova = ep.ring_iova;
                        write_trb(
                            &ep.ring,
                            ep.producer.link_index(),
                            trb::Trb::link(ring_iova, true, cycle_before),
                        );
                    }
                    offset += chunk as usize;
                    batch += 1;
                }
            }
            if batch == 0 {
                break;
            }
            self.ring_doorbell(slot_id, dci);
            // Drain THIS batch fully before enqueueing the next one. Each isoch
            // TRB carries IOC, so the controller posts one Transfer Event per
            // serviced interval; waiting for the whole batch caps in-flight TRBs
            // at `ISOCH_BATCH_FRAMES` (< `RING_TRBS`), so the producer can never
            // lap the transfer ring. The old code returned after the *first*
            // event, which let back-to-back chunks (usb-audio dribbles a frame
            // window out as adjacent `submit_isoch_out` calls sharing this
            // persistent producer) enqueue far faster than a real controller —
            // pacing one interval per microframe — drains, eventually
            // overwriting still-owned TRBs. QEMU completes isoch near-instantly,
            // which masked the overrun. Ring-Underrun / Missed-Service means the
            // ring drained (non-fatal); a genuine error aborts the stream.
            match self.drain_isoch_batch(irq, slot_id, dci, batch) {
                IsochBatchDrain::Drained => {}
                IsochBatchDrain::Timeout => return None,
                IsochBatchDrain::Error(cc) => {
                    write_str(STDOUT_FILENO, "[xhci] isoch-OUT non-success cc=");
                    crate::write_u8_dec(cc);
                    write_str(STDOUT_FILENO, "\n");
                    return None;
                }
            }
        }
        Some(len)
    }

    /// Block for the bulk-OUT Transfer Event on (`slot_id`, `dci`). Unlike the
    /// EP0 `wait_for_transfer_event`, any **IN** completion (odd DCI) seen while
    /// draining is routed through `capture_interrupt_report` + re-armed, so a
    /// bulk-IN frame (or HID report) completing during a TX is not lost and its
    /// endpoint is not left stuck armed-but-completed. Bounded by IRQ wakes.
    fn wait_for_bulk_out_event(
        &mut self,
        _irq: &IrqNotification,
        slot_id: u8,
        dci: u8,
    ) -> Option<trb::TransferEvent> {
        // Poll the event ring rather than block on `irq.wait()`: a bulk-OUT that
        // never completes (or whose IRQ never fires) must NOT hang the server —
        // `notify_wait` has no timeout, so a blocking wait here would deadlock
        // the whole USB stack. Spin-poll first (catches the common fast
        // completion in µs — see `COMPLETION_SPIN_POLLS`), then a bounded ~400 ms
        // sleep-poll budget (400 × 1 ms) before giving up.
        const MAX_POLLS: u32 = 400;
        for poll_i in 0..(COMPLETION_SPIN_POLLS + MAX_POLLS) {
            let before = self.consumer.index;
            let mut found: Option<trb::TransferEvent> = None;
            let mut completed: alloc::vec::Vec<(u8, u8)> = alloc::vec::Vec::new();
            loop {
                let seg = self.event_ring.as_ref().expect("event ring allocated");
                let candidate = read_trb(seg, self.consumer.index);
                if !self.consumer.owns(&candidate) {
                    break;
                }
                match trb::event_trb_type(&candidate) {
                    Some(trb::TrbType::TransferEvent) => {
                        let ev = trb::parse_transfer_event(&candidate);
                        if ev.slot_id == slot_id && ev.endpoint_id == dci {
                            found = Some(ev);
                        } else if ev.endpoint_id & 1 == 1 {
                            // An IN completion on some other endpoint — capture
                            // it rather than drop it (odd DCI = IN per trb::dci).
                            if self.capture_interrupt_report(ev) {
                                completed.push((ev.slot_id, ev.endpoint_id));
                            }
                        }
                    }
                    Some(trb::TrbType::CommandCompletion) => {
                        let ev = trb::parse_command_completion(&candidate);
                        self.on_command_completion(ev);
                    }
                    Some(trb::TrbType::PortStatusChange) => {
                        let ev = trb::parse_port_status_change(&candidate);
                        self.on_port_status_change(ev);
                    }
                    _ => {}
                }
                self.consumer.dequeue_step();
            }
            // Only advance ERDP / clear IMAN.IP when events were consumed (see
            // wait_for_transfer_event — empty-poll ERDP writes stall the ring).
            if self.consumer.index != before {
                let erdp =
                    self.event_ring_iova + (self.consumer.index as u64) * trb::TRB_SIZE as u64;
                self.rt_write_u64(rt_reg::ERDP, erdp | ERDP_EHB);
                self.rt_write_u32(rt_reg::IMAN, IMAN_IP | IMAN_IE);
            }
            // Re-arm any IN endpoint captured above at its frame-sized length.
            for (s, d) in completed {
                let len = self
                    .slot(s)
                    .and_then(|sc| sc.interrupt_eps.iter().find(|e| e.dci == d))
                    .map(|e| e.armed_len)
                    .unwrap_or(0);
                if len > 0 {
                    self.arm_ring_in(s, d, len);
                }
            }
            if let Some(ev) = found {
                return Some(ev);
            }
            if poll_i < COMPLETION_SPIN_POLLS {
                core::hint::spin_loop();
            } else {
                let _ = syscall_lib::nanosleep_for(0, 1_000_000);
            }
        }
        None
    }

    /// Drain `want` isochronous-OUT Transfer Events for (`slot_id`, `dci`) before
    /// the next batch is enqueued — the ring-overrun guard for `submit_isoch_out`
    /// (see its in-loop comment). Mirrors `wait_for_bulk_out_event`'s poll
    /// skeleton — same ERDP/IMAN advance and IN-completion capture/re-arm so a
    /// concurrent interrupt-IN report is not dropped — but counts matching
    /// completions instead of returning the first. Each isoch TRB carries IOC, so
    /// a fully-serviced batch posts exactly `want` Transfer Events; a Ring-Underrun
    /// / Missed-Service shows the ring drained (in-flight == 0) and ends the wait
    /// early as non-fatal. Bounded by the same spin-then-sleep poll budget.
    fn drain_isoch_batch(
        &mut self,
        _irq: &IrqNotification,
        slot_id: u8,
        dci: u8,
        want: usize,
    ) -> IsochBatchDrain {
        const MAX_POLLS: u32 = 400;
        let mut drained = 0usize;
        for poll_i in 0..(COMPLETION_SPIN_POLLS + MAX_POLLS) {
            let before = self.consumer.index;
            let mut fatal: Option<u8> = None;
            let mut underrun = false;
            let mut completed: alloc::vec::Vec<(u8, u8)> = alloc::vec::Vec::new();
            loop {
                let seg = self.event_ring.as_ref().expect("event ring allocated");
                let candidate = read_trb(seg, self.consumer.index);
                if !self.consumer.owns(&candidate) {
                    break;
                }
                match trb::event_trb_type(&candidate) {
                    Some(trb::TrbType::TransferEvent) => {
                        let ev = trb::parse_transfer_event(&candidate);
                        if ev.slot_id == slot_id && ev.endpoint_id == dci {
                            match ev.completion_code {
                                c if c == trb::COMPLETION_SUCCESS
                                    || c == COMPLETION_SHORT_PACKET =>
                                {
                                    drained += 1;
                                }
                                COMPLETION_RING_UNDERRUN | COMPLETION_MISSED_SERVICE_ERROR => {
                                    underrun = true;
                                }
                                cc => {
                                    fatal = Some(cc);
                                }
                            }
                        } else if ev.endpoint_id & 1 == 1 {
                            // An IN completion on some other endpoint — capture
                            // it rather than drop it (odd DCI = IN per trb::dci).
                            if self.capture_interrupt_report(ev) {
                                completed.push((ev.slot_id, ev.endpoint_id));
                            }
                        }
                    }
                    Some(trb::TrbType::CommandCompletion) => {
                        let ev = trb::parse_command_completion(&candidate);
                        self.on_command_completion(ev);
                    }
                    Some(trb::TrbType::PortStatusChange) => {
                        let ev = trb::parse_port_status_change(&candidate);
                        self.on_port_status_change(ev);
                    }
                    _ => {}
                }
                self.consumer.dequeue_step();
            }
            // Only advance ERDP / clear IMAN.IP when events were consumed (an
            // empty-poll ERDP write stalls the ring — see wait_for_bulk_out_event).
            if self.consumer.index != before {
                let erdp =
                    self.event_ring_iova + (self.consumer.index as u64) * trb::TRB_SIZE as u64;
                self.rt_write_u64(rt_reg::ERDP, erdp | ERDP_EHB);
                self.rt_write_u32(rt_reg::IMAN, IMAN_IP | IMAN_IE);
            }
            // Re-arm any IN endpoint captured above at its frame-sized length.
            for (s, d) in completed {
                let len = self
                    .slot(s)
                    .and_then(|sc| sc.interrupt_eps.iter().find(|e| e.dci == d))
                    .map(|e| e.armed_len)
                    .unwrap_or(0);
                if len > 0 {
                    self.arm_ring_in(s, d, len);
                }
            }
            if let Some(cc) = fatal {
                return IsochBatchDrain::Error(cc);
            }
            if underrun || drained >= want {
                return IsochBatchDrain::Drained;
            }
            if poll_i < COMPLETION_SPIN_POLLS {
                core::hint::spin_loop();
            } else {
                let _ = syscall_lib::nanosleep_for(0, 1_000_000);
            }
        }
        IsochBatchDrain::Timeout
    }

    /// Issue a control transfer for a raw 8-byte SETUP packet on `slot_id`'s
    /// EP0, returning the inline response data (empty for an OUT transfer).
    /// Used by the server to serve a class driver's `ControlRequest` (e.g. the
    /// HID `SET_PROTOCOL(0)` / `SET_IDLE(0)` the `usb-hid` daemon issues). The
    /// transfer direction is taken from `setup[0]` bit 7 (D2H = IN).
    pub fn control_request(
        &mut self,
        irq: &IrqNotification,
        slot_id: u8,
        setup: [u8; 8],
        length: u16,
        drain_others: &mut dyn FnMut(),
    ) -> Option<alloc::vec::Vec<u8>> {
        let packet = trb::SetupPacket {
            bm_request_type: setup[0],
            b_request: setup[1],
            w_value: u16::from_le_bytes([setup[2], setup[3]]),
            w_index: u16::from_le_bytes([setup[4], setup[5]]),
            w_length: u16::from_le_bytes([setup[6], setup[7]]),
        };
        let dir_in = setup[0] & 0x80 != 0;
        // The SETUP packet's `wLength` is the single source of truth for the
        // data-stage size. `length` arrives as a separate IPC argument, so a
        // buggy or hostile peer could supply a value that disagrees with the
        // SETUP it also sent; programming a control TD whose TRB length
        // contradicts the device's `wLength` produces an invalid transfer.
        // Fail closed on any mismatch and drive the transfer from `w_length`.
        if length != packet.w_length {
            return None;
        }
        self.control_transfer(
            irq,
            slot_id,
            packet,
            packet.w_length,
            dir_in,
            None,
            drain_others,
        )
    }

    /// Issue a control transfer with an **OUT data stage** for a raw 8-byte
    /// SETUP packet on `slot_id`'s EP0. Used by the server to serve a class
    /// driver's `ControlWrite` (the `ure` driver's OCP register writes). The
    /// SETUP packet's `wLength` is the source of truth for the data-stage size;
    /// the IPC-supplied `data` must match it and the direction must be OUT, or
    /// the transfer fails closed. Returns an empty Vec on success.
    pub fn control_write(
        &mut self,
        irq: &IrqNotification,
        slot_id: u8,
        setup: [u8; 8],
        data: &[u8],
        drain_others: &mut dyn FnMut(),
    ) -> Option<alloc::vec::Vec<u8>> {
        let packet = trb::SetupPacket {
            bm_request_type: setup[0],
            b_request: setup[1],
            w_value: u16::from_le_bytes([setup[2], setup[3]]),
            w_index: u16::from_le_bytes([setup[4], setup[5]]),
            w_length: u16::from_le_bytes([setup[6], setup[7]]),
        };
        // A ControlWrite must be host-to-device (D2H bit clear) and the inline
        // payload length must agree with the SETUP packet's `wLength`. Reject a
        // mismatched or mis-directed request rather than program an invalid TD.
        if setup[0] & 0x80 != 0 {
            return None;
        }
        if data.len() != packet.w_length as usize {
            return None;
        }
        self.control_transfer(
            irq,
            slot_id,
            packet,
            packet.w_length,
            false,
            Some(data),
            drain_others,
        )
    }

    // -- A.6: single-threaded drain-on-wake event loop ---------------------

    /// The load-bearing milestone: a real `Enable Slot` Command Completion
    /// delivered off the event ring **by interrupt**. Emitted exactly once.
    fn on_command_completion(&mut self, ev: trb::CommandCompletionEvent) {
        if ev.completion_code == trb::COMPLETION_SUCCESS
            && ev.slot_id != 0
            && !self.enable_slot_emitted
        {
            self.enable_slot_emitted = true;
            write_str(STDOUT_FILENO, ENABLE_SLOT_OK_SENTINEL);
        }
    }

    /// Port Status Change handler. A.7 reset/speed detection runs here; Phase 92
    /// Track C adds the live hot-plug surface: a connect resets the port and
    /// **queues** a `PortChange::Connect` (with the decoded speed) for the
    /// server to enumerate; a disconnect queues `PortChange::Disconnect` for the
    /// server to mark detached + Disable Slot. Queuing (rather than enumerating
    /// inline) keeps the heavy work out of the event-ring drain borrow — the
    /// single-threaded server drains the queue via `take_port_events` on each
    /// loop wake. Called from every event-ring drain, so it runs during bring-up
    /// too; the server guards against re-enumerating a port it already serves.
    fn on_port_status_change(&mut self, ev: trb::PortStatusChangeEvent) {
        let portnum = ev.port_id;
        if portnum == 0 || portnum as u16 > self.max_ports as u16 {
            return;
        }
        let off = self.portsc(portnum);
        let raw = self.op_u32(off);
        let p = port::Portsc(raw);
        // Acknowledge the connect-status change (RW1C) so the edge is not
        // re-reported, then classify connect vs disconnect.
        if p.csc() {
            let cleared = port::portsc_clear_change(raw, port::PORTSC_CSC);
            self.op_write_u32(off, cleared);
            if p.ccs() {
                // A real connect: drive the A.7 reset path (decoding speed) and
                // queue the connect for the server's enumeration (Track C.1/C.3).
                if let Some(speed) = self.reset_port_with_speed(portnum) {
                    self.port_events.push(PortChange::Connect {
                        port: portnum,
                        speed,
                    });
                }
            } else {
                // A disconnect: queue it for the server to tear down (Track C.2/C.4).
                self.port_events
                    .push(PortChange::Disconnect { port: portnum });
            }
        }
    }

    /// Drain the queued root-hub port changes for the server to act on
    /// (Phase 92 Track C). Returns and clears the pending list.
    pub fn take_port_events(&mut self) -> alloc::vec::Vec<PortChange> {
        core::mem::take(&mut self.port_events)
    }

    /// Scan every root-hub port and reset any that already report a connected
    /// device (present before the driver started — the common case for a
    /// `usb-kbd` attached at machine creation). Hotplug after this point is
    /// serviced by the Port Status Change path in the event loop. Real xHCI
    /// drivers perform this initial sweep because a device present across the
    /// `HCRST` is not guaranteed to re-post a connect change afterwards.
    ///
    /// Returns `(port_num, speed)` for **every** connected, reset-to-Enabled
    /// port — Phase 78c enumerates all of them (e.g. a keyboard and a mouse on
    /// separate root-hub ports), not just the first.
    pub fn scan_ports(&self) -> alloc::vec::Vec<(u8, port::PortSpeed)> {
        let mut found: alloc::vec::Vec<(u8, port::PortSpeed)> = alloc::vec::Vec::new();
        for portnum in 1..=self.max_ports {
            let off = self.portsc(portnum);
            let mut raw = self.op_u32(off);
            // Ensure Port Power is on before sampling connect status: on real
            // xHCI with software-controlled power `PORTSC.PP` can default to 0,
            // and CCS/CSC are not meaningful until it is set. qemu-xhci powers
            // ports automatically (PP defaults to 1), so this is a no-op there.
            if !port::Portsc(raw).pp() {
                let powered = port::portsc_write_preserving(raw, port::PORTSC_PP);
                self.op_write_u32(off, powered);
                raw = self.op_u32(off);
            }
            if port::Portsc(raw).ccs()
                && let Some(speed) = self.reset_port_with_speed(portnum)
            {
                found.push((portnum, speed));
            }
        }
        found
    }

    /// A.7 port reset + speed detection (RW1C-safe). Returns the detected
    /// `PortSpeed` on success or `None` if the port did not reach Enabled.
    fn reset_port_with_speed(&self, port_num: u8) -> Option<port::PortSpeed> {
        let off = self.portsc(port_num);
        let raw = self.op_u32(off);
        let p = port::Portsc(raw);
        // USB3 ports train to Enabled without a software PR.
        if p.ped()
            && let Some(speed) = port::port_speed_from_psi(p.port_speed())
        {
            self.op_write_u32(
                off,
                port::portsc_clear_change(self.op_u32(off), port::PORTSC_CSC),
            );
            self.report_port_enabled(port_num, speed);
            return Some(speed);
        }
        // USB2: issue a Port Reset, wait for PRC.
        let pr = port::portsc_write_preserving(raw, port::PORTSC_PR);
        self.op_write_u32(off, pr);
        if !poll_yield(POLL_ITERS_250MS, || {
            self.op_u32(off) & port::PORTSC_PRC != 0
        }) {
            return None;
        }
        let after = self.op_u32(off);
        let cleared = port::portsc_clear_change(after, port::PORTSC_PRC | port::PORTSC_CSC);
        self.op_write_u32(off, cleared);
        let final_p = port::Portsc(self.op_u32(off));
        if final_p.ped()
            && let Some(speed) = port::port_speed_from_psi(final_p.port_speed())
        {
            self.report_port_enabled(port_num, speed);
            return Some(speed);
        }
        None
    }

    /// A.7 port reset (legacy path kept for the Port Status Change handler).
    fn reset_port(&self, port_num: u8) {
        self.reset_port_with_speed(port_num);
    }

    fn report_port_enabled(&self, port_num: u8, speed: port::PortSpeed) {
        use port::PortSpeed;
        let label = match speed {
            PortSpeed::Low => "low",
            PortSpeed::Full => "full",
            PortSpeed::High => "high",
            PortSpeed::Super => "super",
        };
        write_str(STDOUT_FILENO, "[xhci] port ");
        crate::write_u8_dec(port_num);
        write_str(STDOUT_FILENO, " enabled (");
        write_str(STDOUT_FILENO, label);
        write_str(STDOUT_FILENO, "-speed)\n");
    }
}

// ---------------------------------------------------------------------------
// DMA region helpers — volatile access at byte offsets into a `DmaBuffer<u8>`.
// ---------------------------------------------------------------------------

fn zero_dma(buf: &DmaBuffer<u8>) {
    // SAFETY: `user_ptr()` is a valid, process-mapped pointer to `len()`
    // writable bytes for the buffer's lifetime.
    unsafe { core::ptr::write_bytes(buf.user_ptr(), 0, buf.len()) };
}

pub(crate) fn write_trb(buf: &DmaBuffer<u8>, index: usize, value: trb::Trb) {
    debug_assert!((index + 1) * trb::TRB_SIZE <= buf.len());
    // SAFETY: index is bounds-checked above; the pointer is 64-byte aligned
    // (allocation alignment) so a 16-byte `Trb` write is well-aligned.
    let ptr = unsafe { buf.user_ptr().add(index * trb::TRB_SIZE) } as *mut trb::Trb;
    unsafe { core::ptr::write_volatile(ptr, value) };
}

fn read_trb(buf: &DmaBuffer<u8>, index: usize) -> trb::Trb {
    debug_assert!((index + 1) * trb::TRB_SIZE <= buf.len());
    // SAFETY: as `write_trb`.
    let ptr = unsafe { buf.user_ptr().add(index * trb::TRB_SIZE) } as *const trb::Trb;
    unsafe { core::ptr::read_volatile(ptr) }
}

fn write_u64_at(buf: &DmaBuffer<u8>, byte_off: usize, val: u64) {
    debug_assert!(byte_off + 8 <= buf.len());
    // SAFETY: bounds-checked; 64-byte allocation alignment covers 8-byte access.
    let ptr = unsafe { buf.user_ptr().add(byte_off) } as *mut u64;
    unsafe { core::ptr::write_volatile(ptr, val) };
}

pub(crate) fn write_u32_at(buf: &DmaBuffer<u8>, byte_off: usize, val: u32) {
    debug_assert!(byte_off + 4 <= buf.len());
    // SAFETY: bounds-checked; 64-byte allocation alignment covers 4-byte access.
    let ptr = unsafe { buf.user_ptr().add(byte_off) } as *mut u32;
    unsafe { core::ptr::write_volatile(ptr, val) };
}
