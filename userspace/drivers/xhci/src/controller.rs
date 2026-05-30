//! xHCI host-controller bring-up — Phase 78a Tracks A.3–A.7 (MMIO/DMA/IRQ
//! glue around the host-tested `kernel_core::usb::xhci` pure logic).
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
use kernel_core::usb::xhci::{port, regs, trb};
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
/// 16 entries × 16 bytes = 256 bytes — ample for the single Enable Slot of
/// the 78a milestone; 78b/78c grow this as transfer traffic appears.
const RING_TRBS: usize = 16;
const RING_BYTES: usize = RING_TRBS * trb::TRB_SIZE;
/// 64-byte alignment is the strictest xHCI requirement for rings, the DCBAA,
/// the ERST, and contexts.
const XHCI_ALIGN: usize = 64;

/// Bounded spin budget for the reset / CNR / halt handshakes. QEMU clears the
/// relevant bit within a handful of reads; a few hundred thousand iterations
/// is a generous ceiling that still terminates if the controller wedges.
const POLL_BUDGET: u32 = 5_000_000;

/// Outcome of the bring-up sequence: either the controller is live (with the
/// IRQ subscription handed to the event loop) or a stage failed.
pub enum BringUpError {
    ResetTimeout,
    RunTimeout,
    DmaAlloc,
    IrqSubscribe,
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
        }
    }

    pub fn max_ports(&self) -> u8 {
        self.max_ports
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
    pub fn release_bios_ownership(&self) {
        let mut off = match self.xecp_off {
            Some(o) => o,
            None => {
                write_str(STDOUT_FILENO, "[xhci] no extended capabilities\n");
                return;
            }
        };
        // Bounded walk: the list is short and `next == 0` terminates it; the
        // cap guards against a malformed self-referential list.
        for _ in 0..64 {
            let dword = self.bar.read_reg::<u32>(off);
            let id = (dword & 0xFF) as u8;
            let next = ((dword >> 8) & 0xFF) as usize;
            if id == XECP_ID_LEGACY {
                let legsup = self.bar.read_reg::<u32>(off);
                if legsup & USBLEGSUP_BIOS_OWNED != 0 {
                    self.bar.write_reg::<u32>(off, legsup | USBLEGSUP_OS_OWNED);
                    let mut budget = POLL_BUDGET;
                    while self.bar.read_reg::<u32>(off) & USBLEGSUP_BIOS_OWNED != 0 && budget > 0 {
                        budget -= 1;
                    }
                }
                write_str(STDOUT_FILENO, "[xhci] BIOS/OS handoff complete\n");
                return;
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
        let mut budget = POLL_BUDGET;
        while self.op_u32(op_reg::USBSTS) & USBSTS_HCH == 0 {
            if budget == 0 {
                return Err(BringUpError::ResetTimeout);
            }
            budget -= 1;
        }

        // Reset: set HCRST, wait until it self-clears AND CNR clears.
        self.op_write_u32(op_reg::USBCMD, USBCMD_HCRST);
        let mut budget = POLL_BUDGET;
        loop {
            let cmd = self.op_u32(op_reg::USBCMD);
            let sts = self.op_u32(op_reg::USBSTS);
            if cmd & USBCMD_HCRST == 0 && sts & USBSTS_CNR == 0 {
                break;
            }
            if budget == 0 {
                return Err(BringUpError::ResetTimeout);
            }
            budget -= 1;
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
        let pagesize = self.op_u32(op_reg::PAGESIZE) & 0xFFFF;
        if pagesize == 0 {
            4096
        } else {
            (pagesize as usize) << 12
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
        // IMOD = 0: no moderation delay, so the first event interrupts
        // promptly during bring-up.
        self.rt_write_u32(rt_reg::IMOD, 0);
        // Enable interrupts and clear any latched IP (write-1-clear).
        self.rt_write_u32(rt_reg::IMAN, IMAN_IP | IMAN_IE);
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
        let mut budget = POLL_BUDGET;
        while self.op_u32(op_reg::USBSTS) & USBSTS_HCH != 0 {
            if budget == 0 {
                return Err(BringUpError::RunTimeout);
            }
            budget -= 1;
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

    // -- A.6: single-threaded drain-on-wake event loop ---------------------

    /// Block on the IRQ notification, drain the event ring on each wake, and
    /// match completion events. Mirrors the NVMe `wait_completion` drain-on-
    /// wake model — no busy-poll, no separate thread. Never returns (the
    /// driver is a supervised daemon).
    pub fn event_loop(&mut self, irq: IrqNotification) -> ! {
        loop {
            let bits = irq.wait();
            if bits == 0 {
                // Spurious / errored wake — re-block rather than spin.
                continue;
            }
            self.drain_event_ring();
            // Update ERDP to the current dequeue pointer and clear EHB.
            let erdp = self.event_ring_iova + (self.consumer.index as u64) * trb::TRB_SIZE as u64;
            self.rt_write_u64(rt_reg::ERDP, erdp | ERDP_EHB);
            // Clear the interrupter's pending bit (write-1-clear), keep IE.
            self.rt_write_u32(rt_reg::IMAN, IMAN_IP | IMAN_IE);
            let _ = irq.ack(bits);
        }
    }

    /// Consume every TRB the controller has produced (cycle bit == CCS),
    /// dispatching each by type.
    fn drain_event_ring(&mut self) {
        loop {
            let seg = self.event_ring.as_ref().expect("event ring allocated");
            let candidate = read_trb(seg, self.consumer.index);
            if !self.consumer.owns(&candidate) {
                break;
            }
            match trb::event_trb_type(&candidate) {
                Some(trb::TrbType::CommandCompletion) => {
                    let ev = trb::parse_command_completion(&candidate);
                    self.on_command_completion(ev);
                }
                Some(trb::TrbType::PortStatusChange) => {
                    let ev = trb::parse_port_status_change(&candidate);
                    self.on_port_status_change(ev);
                }
                Some(trb::TrbType::TransferEvent) => {
                    // No transfers issued in 78a; enumeration is 78b.
                }
                _ => {}
            }
            self.consumer.dequeue_step();
        }
    }

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

    /// Port Status Change handler stub — A.7 reset/speed detection is wired
    /// here; the enumeration consumer is Phase 78b.
    fn on_port_status_change(&self, ev: trb::PortStatusChangeEvent) {
        let portnum = ev.port_id;
        if portnum == 0 || portnum as u16 > self.max_ports as u16 {
            return;
        }
        let off = self.portsc(portnum);
        let raw = self.op_u32(off);
        let p = port::Portsc(raw);
        // Acknowledge the connect-status change (RW1C) so the edge is not
        // re-reported, then, on a real connect, drive the A.7 reset path.
        if p.csc() {
            let cleared = port::portsc_clear_change(raw, port::PORTSC_CSC);
            self.op_write_u32(off, cleared);
            if p.ccs() {
                self.reset_port(portnum);
            }
        }
    }

    /// Scan every root-hub port and reset any that already report a connected
    /// device (present before the driver started — the common case for a
    /// `usb-kbd` attached at machine creation). Hotplug after this point is
    /// serviced by the Port Status Change path in the event loop. Real xHCI
    /// drivers perform this initial sweep because a device present across the
    /// `HCRST` is not guaranteed to re-post a connect change afterwards.
    pub fn scan_ports(&self) {
        for portnum in 1..=self.max_ports {
            let raw = self.op_u32(self.portsc(portnum));
            if port::Portsc(raw).ccs() {
                self.reset_port(portnum);
            }
        }
    }

    /// A.7 port reset + speed detection (RW1C-safe). USB2 ports get an
    /// explicit `PR`; USB3 ports reach Enabled via controller-driven training.
    /// On success the connect-status change (`CSC`) is RW1C-cleared so the
    /// event-loop's Port Status Change handler does not redundantly re-reset
    /// the same port.
    fn reset_port(&self, port_num: u8) {
        let off = self.portsc(port_num);
        let raw = self.op_u32(off);
        let p = port::Portsc(raw);
        // USB3 ports train to Enabled without a software PR — they already
        // report a valid speed and PED.
        if p.ped()
            && let Some(speed) = port::port_speed_from_psi(p.port_speed())
        {
            let _mps = port::ep0_max_packet_for_speed(speed);
            self.op_write_u32(
                off,
                port::portsc_clear_change(self.op_u32(off), port::PORTSC_CSC),
            );
            self.report_port_enabled(port_num, speed);
            return;
        }
        // USB2: issue a Port Reset preserving the RW1C change bits, then wait
        // for PRC, clear PRC + CSC, and confirm PED before decoding speed.
        let pr = port::portsc_write_preserving(raw, port::PORTSC_PR);
        self.op_write_u32(off, pr);
        let mut budget = POLL_BUDGET;
        while self.op_u32(off) & port::PORTSC_PRC == 0 {
            if budget == 0 {
                return;
            }
            budget -= 1;
        }
        let after = self.op_u32(off);
        let cleared = port::portsc_clear_change(after, port::PORTSC_PRC | port::PORTSC_CSC);
        self.op_write_u32(off, cleared);
        let final_p = port::Portsc(self.op_u32(off));
        if final_p.ped()
            && let Some(speed) = port::port_speed_from_psi(final_p.port_speed())
        {
            self.report_port_enabled(port_num, speed);
        }
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

fn write_trb(buf: &DmaBuffer<u8>, index: usize, value: trb::Trb) {
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

fn write_u32_at(buf: &DmaBuffer<u8>, byte_off: usize, val: u32) {
    debug_assert!(byte_off + 4 <= buf.len());
    // SAFETY: bounds-checked; 64-byte allocation alignment covers 4-byte access.
    let ptr = unsafe { buf.user_ptr().add(byte_off) } as *mut u32;
    unsafe { core::ptr::write_volatile(ptr, val) };
}
