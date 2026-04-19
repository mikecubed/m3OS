//! Intel 82540EM (classic e1000) network driver — Phase 55 Track E.
//!
//! Covers the task-doc sub-tasks E.1–E.4:
//!
//! * **E.1 — probe + device init.** Claims the PCI function via the HAL's
//!   [`crate::pci::probe_all_drivers`] pass, maps BAR0 as a 128 KiB MMIO
//!   region through [`crate::pci::bar::map_bar`], issues a global reset,
//!   brings the MAC out of PHY reset with auto-speed-detect + link-up, and
//!   reads the MAC from `RAL0`/`RAH0` (QEMU populates those on reset — we do
//!   not need the EEPROM path for the QEMU target).
//! * **E.2 — TX/RX descriptor rings + DMA setup.** Allocates both rings and
//!   per-slot packet buffers through [`crate::mm::dma::DmaBuffer`]; programs
//!   `RDBAL/RDBAH/RDLEN` and `TDBAL/TDBAH/TDLEN` with the ring physical base
//!   and byte length; pre-posts every RX slot and leaves `RDT` one short of
//!   the head per Intel's §13 "Receive Descriptor Ring" guidance.
//! * **E.3 — interrupt handling + receive.** Installs the IRQ via
//!   [`crate::pci::PciDeviceHandle::install_msi_irq`] (preferred) or
//!   [`crate::pci::PciDeviceHandle::install_intx_irq`] (fallback).  The ISR
//!   reads `ICR` to ack the cause, flips the link-state atomic on `LSC`,
//!   raises [`E1000_IRQ_WOKEN`], and wakes the network task so the RX ring
//!   is drained in task context.  Frames go to the same dispatch entry
//!   (`net::dispatch::process_rx_frames`) that virtio-net uses.
//! * **E.4 — transmit + network stack integration.** `e1000_transmit` copies
//!   the packet into the next TX descriptor's DMA buffer, sets `EOP|IFCS|RS`
//!   in `cmd`, advances `TDT`, and returns `NetError::LinkDown` while the
//!   link-state atomic is cleared (the link-down wrap-around is the E.4
//!   "drain in-flight TX on link-up" behaviour).  Upper layers call
//!   [`crate::net::send_frame`] which dispatches through the driver
//!   selector — see `kernel/src/net/mod.rs`.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use kernel_core::e1000::{
    E1000Regs, E1000RxDesc, E1000TxDesc, ctrl, decode_mac_from_ra, irq_cause, rctl,
    rx_descriptor_done, rx_status, status as e_status, tctl, tx_cmd, tx_descriptor_done,
};
use kernel_core::types::{MacAddr, TaskId};
use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::mm::dma::DmaBuffer;
use crate::pci::bar::{BarMapping, MmioRegion};
use crate::pci::{self, DriverEntry, DriverProbeResult, PciMatch};
use crate::task::scheduler::wake_task;

use super::NetError;

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Receive descriptor ring depth.  Must be a multiple of 8 per the spec; 256
/// matches Intel's recommended default for the 82540EM.
pub const RX_RING_SIZE: usize = 256;
/// Transmit descriptor ring depth.  Same constraint as RX.
pub const TX_RING_SIZE: usize = 256;
/// Per-descriptor receive buffer size.  Paired with `rctl::BSIZE_2048`.
pub const RX_BUF_SIZE: usize = 2048;
/// Per-descriptor transmit buffer size.  One MTU-sized buffer per slot.
pub const TX_BUF_SIZE: usize = 2048;

/// Bounded spin count for the self-clearing `CTRL.RST` bit.  Each iteration
/// pauses with `spin_loop`; at typical clock rates this yields well under a
/// second of real time and prevents a dead NIC from hanging ring 0.
const RESET_POLL_LIMIT: u32 = 2_000_000;

/// IDT vector used for the legacy-INTx fallback, picked from the device-IRQ
/// stub bank.  Slot +4 — virtio-blk uses +2, virtio-net uses +3.
const E1000_INTX_VECTOR: u8 = crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE + 4;

// ---------------------------------------------------------------------------
// RX / TX descriptor rings
// ---------------------------------------------------------------------------

/// Receive descriptor ring + the DMA-backed packet buffers the descriptors
/// point at.  Both the ring and the buffers are owned here; dropping the
/// struct returns every page to the buddy allocator.
struct RxDescRing {
    /// Ring of descriptors.  `DmaBuffer<[E1000RxDesc]>` of length RX_RING_SIZE.
    descs: DmaBuffer<[E1000RxDesc]>,
    /// One 4 KiB DMA buffer per ring slot.  Keeping each as its own
    /// `DmaBuffer` rather than one contiguous slab keeps Drop simple and
    /// mirrors the virtio-net approach.
    bufs: Vec<DmaBuffer<[u8]>>,
    /// Cached physical address of the ring's first descriptor.
    ring_phys: u64,
    /// Cached per-slot buffer physical addresses.
    buf_phys: Vec<u64>,
    /// Software tail — the next slot the task will hand back to hardware.
    next_to_read: usize,
}

/// Transmit descriptor ring and its per-slot packet buffers.
struct TxDescRing {
    descs: DmaBuffer<[E1000TxDesc]>,
    bufs: Vec<DmaBuffer<[u8]>>,
    ring_phys: u64,
    buf_phys: Vec<u64>,
    /// Next free software tail.  Hardware's TDT register tracks the same
    /// value (advanced by `e1000_transmit` after filling the slot).
    next_to_write: usize,
}

// SAFETY: Both rings are only accessed under `DRIVER.lock()` with interrupts
// disabled; the unsafe `Send` bound on `DmaBuffer<T>` already covers the
// contained allocations.  The two `Vec`s carry raw `u64` PAs and
// `DmaBuffer`-owned allocations, both Send by construction.
unsafe impl Send for RxDescRing {}
unsafe impl Send for TxDescRing {}

// RxDescRing::descs is DmaBuffer<E1000RxDesc> — we treat it as an array of
// RX_RING_SIZE descriptors by slicing the raw backing allocation.  Same for
// TxDescRing.  Using DmaBuffer::<[T]>::new_bytes with size = N * 16 would
// also work, but new_array keeps the descriptor count implicit.

impl RxDescRing {
    fn new(handle: &pci::PciDeviceHandle) -> Result<Self, &'static str> {
        let descs = DmaBuffer::<E1000RxDesc>::allocate_array(handle, RX_RING_SIZE)
            .map_err(|_| "rx ring DMA alloc failed")?;
        // Acceptance requires ring length in bytes to be multiple of 128;
        // RX_RING_SIZE * 16 is always a multiple of 128 when RX_RING_SIZE is
        // a multiple of 8 (checked at compile time below).
        const _: () = assert!(RX_RING_SIZE.is_multiple_of(8));
        const _: () = assert!(RX_RING_SIZE <= 4096);
        let ring_phys = descs.bus_address();

        let mut bufs: Vec<DmaBuffer<[u8]>> = Vec::with_capacity(RX_RING_SIZE);
        let mut buf_phys: Vec<u64> = Vec::with_capacity(RX_RING_SIZE);
        for _ in 0..RX_RING_SIZE {
            let buf = DmaBuffer::<[u8]>::allocate(handle, RX_BUF_SIZE)
                .map_err(|_| "rx buffer DMA alloc failed")?;
            buf_phys.push(buf.bus_address());
            bufs.push(buf);
        }

        let mut ring = RxDescRing {
            descs,
            bufs,
            ring_phys,
            buf_phys,
            next_to_read: 0,
        };
        // Pre-populate every descriptor to point at its DMA buffer.  The
        // hardware clears `status` after consuming a slot; on the task
        // re-post path we re-write buffer_addr too so that any wild write
        // from a previous lap cannot linger in the ring.
        ring.prepare_all();
        Ok(ring)
    }

    /// Fill every descriptor with its buffer PA and zero status bytes.  Run
    /// once at init and on every reset-after-link-up.
    fn prepare_all(&mut self) {
        for i in 0..RX_RING_SIZE {
            let pa = self.buf_phys[i];
            let desc = &mut self.descs_mut()[i];
            desc.buffer_addr = pa;
            desc.length = 0;
            desc.checksum = 0;
            desc.status = 0;
            desc.errors = 0;
            desc.special = 0;
        }
        self.next_to_read = 0;
    }

    /// Slice the descriptor DmaBuffer as `&mut [E1000RxDesc]`.  Length is
    /// RX_RING_SIZE by construction of `DmaBuffer::allocate_array`.
    fn descs_mut(&mut self) -> &mut [E1000RxDesc] {
        &mut self.descs
    }

    fn descs(&self) -> &[E1000RxDesc] {
        &self.descs
    }
}

impl TxDescRing {
    fn new(handle: &pci::PciDeviceHandle) -> Result<Self, &'static str> {
        let descs = DmaBuffer::<E1000TxDesc>::allocate_array(handle, TX_RING_SIZE)
            .map_err(|_| "tx ring DMA alloc failed")?;
        const _: () = assert!(TX_RING_SIZE.is_multiple_of(8));
        const _: () = assert!(TX_RING_SIZE <= 4096);
        let ring_phys = descs.bus_address();

        let mut bufs: Vec<DmaBuffer<[u8]>> = Vec::with_capacity(TX_RING_SIZE);
        let mut buf_phys: Vec<u64> = Vec::with_capacity(TX_RING_SIZE);
        for _ in 0..TX_RING_SIZE {
            let buf = DmaBuffer::<[u8]>::allocate(handle, TX_BUF_SIZE)
                .map_err(|_| "tx buffer DMA alloc failed")?;
            buf_phys.push(buf.bus_address());
            bufs.push(buf);
        }

        let mut ring = TxDescRing {
            descs,
            bufs,
            ring_phys,
            buf_phys,
            next_to_write: 0,
        };
        for i in 0..TX_RING_SIZE {
            let pa = ring.buf_phys[i];
            let desc = &mut ring.descs_mut()[i];
            *desc = E1000TxDesc::default();
            desc.buffer_addr = pa;
        }
        Ok(ring)
    }

    fn descs_mut(&mut self) -> &mut [E1000TxDesc] {
        &mut self.descs
    }

    fn descs(&self) -> &[E1000TxDesc] {
        &self.descs
    }

    /// Reclaim any in-flight TX slots by waiting for their `DD` bit or
    /// clearing them unconditionally on a link-down flush.  Returns the
    /// number of slots the software pointer rolled past.
    fn drain_in_flight(&mut self) -> usize {
        let mut drained = 0;
        for i in 0..TX_RING_SIZE {
            let d = &mut self.descs_mut()[i];
            // Clear status so the next `e1000_transmit` sees a clean slot.
            d.status = 0;
            d.cmd = 0;
            d.length = 0;
            drained += 1;
        }
        self.next_to_write = 0;
        drained
    }
}

// ---------------------------------------------------------------------------
// Device state
// ---------------------------------------------------------------------------

/// The one-per-machine e1000 driver state.  Accessed under a `Mutex`; task
/// callers wrap the critical section in `without_interrupts` so the ISR,
/// which also takes `DRIVER.lock()`, cannot race on the same CPU.
#[allow(dead_code)]
struct E1000Device {
    /// Claim handle.  Held for the driver's lifetime so no other driver can
    /// re-bind the same PCI function.
    pci: pci::PciDeviceHandle,
    /// BAR0 MMIO region — 128 KiB on the 82540EM.
    mmio: MmioRegion,
    /// MAC address as read from RAL0/RAH0 at init.
    mac: MacAddr,
    rx: RxDescRing,
    tx: TxDescRing,
    /// IRQ registration — must outlive the driver or the stub dispatches to
    /// a stale handler.  Kept alive for the driver's lifetime.
    irq: Option<pci::DeviceIrq>,
}

impl E1000Device {
    /// Read a 32-bit register at `offset`.
    fn read_reg(&self, offset: usize) -> u32 {
        self.mmio.read_reg::<u32>(offset)
    }

    /// Write a 32-bit register at `offset`.
    fn write_reg(&self, offset: usize, value: u32) {
        self.mmio.write_reg::<u32>(offset, value)
    }
}

// ---------------------------------------------------------------------------
// Globals
// ---------------------------------------------------------------------------

static DRIVER: Mutex<Option<E1000Device>> = Mutex::new(None);

/// True once probe + ring setup + IRQ install all succeeded and the device
/// is ready to move frames.  Used by `net::send_frame` to select the driver.
pub static E1000_READY: AtomicBool = AtomicBool::new(false);

/// Set to `true` by the ISR on every RX or LSC IRQ; the network task clears
/// it with `swap(false, Acquire)` and drains the ring.  Mirrors the
/// `NET_IRQ_WOKEN` pattern used by virtio-net's migrated IRQ path.
pub static E1000_IRQ_WOKEN: AtomicBool = AtomicBool::new(false);

/// Link state — `true` when the MAC reports `STATUS.LU=1`.  Flipped by the
/// ISR on LSC events and consulted by `e1000_transmit` so we never silently
/// enqueue on a dead ring.
pub static LINK_UP: AtomicBool = AtomicBool::new(false);

/// Low 32 bits of the most recent `STATUS` snapshot — published as an
/// `AtomicU32` so the shell can inspect link state without taking the lock.
pub static LAST_STATUS: AtomicU32 = AtomicU32::new(0);

/// Edge trigger — set by the ISR when LSC transitions the link from down to
/// up; consumed by the network task in `drain_link_up_edge` to invoke
/// `on_link_up` exactly once per transition.  Acceptance E.4 bullet 5.
pub static LINK_UP_PENDING: AtomicBool = AtomicBool::new(false);

/// `TaskId` of the network processing task, registered once from
/// `kernel_main::net_task` so the ISR can wake the task on every RX IRQ.
/// `0` means "not yet registered" — task IDs are allocated starting at 1 in
/// `Task::new`, so 0 is a safe sentinel. An atomic is used because the ISR
/// (which can fire on the same CPU as the setter) would otherwise deadlock
/// on a spin::Mutex held with interrupts enabled.
static NET_TASK_ID: AtomicU64 = AtomicU64::new(0);

/// Register the network task id with the e1000 driver.
pub fn set_net_task_id(id: TaskId) {
    NET_TASK_ID.store(id.0, Ordering::Release);
}

/// Returns the MAC address, if the e1000 device is initialized.
#[allow(dead_code)]
pub fn mac_address() -> Option<MacAddr> {
    interrupts::without_interrupts(|| DRIVER.lock().as_ref().map(|d| d.mac))
}

/// Returns the legacy PCI interrupt line of the claimed e1000 device, or
/// `None` if the device has not been initialised or has no INTx.  Kept for
/// parity with `virtio_net::pci_interrupt_line` in case bring-up code needs
/// to cross-reference the routed I/O APIC pin from outside.
#[allow(dead_code)]
pub fn pci_interrupt_line() -> Option<u8> {
    interrupts::without_interrupts(|| {
        DRIVER
            .lock()
            .as_ref()
            .map(|d| d.pci.device().interrupt_line)
            .filter(|&line| line != 0xFF)
    })
}

// ---------------------------------------------------------------------------
// IRQ handler (ISR context — no alloc, no blocking, no IPC)
// ---------------------------------------------------------------------------

/// e1000 MSI / INTx handler.
///
/// Reads `ICR` (read-to-clear on the 82540EM), notes any link-state-change,
/// and signals the network task.  The ISR only touches MMIO registers plus
/// two atomics — no allocation, no IPC, and the only lock is the brief
/// `DRIVER.lock()` which task-path callers wrap in `without_interrupts`.
fn e1000_interrupt_handler() {
    // Snapshot the ICR / STATUS registers.  We read under the lock only to
    // locate the MMIO base; no ring manipulation happens here.
    let icr = {
        let driver = DRIVER.lock();
        match driver.as_ref() {
            Some(d) => {
                // ICR on the 82540EM is read-to-clear: the read returns the
                // set cause bits and atomically clears them.  §13.4.17 does
                // not require writing the value back, and QEMU's emulated
                // e1000 does not expect it — removing the echo-write keeps
                // the ISR minimal and rules out any write-after-clear
                // re-latching edge cases.
                let icr = d.read_reg(E1000Regs::ICR);
                // Also refresh the last-status snapshot while we hold the
                // lock so a shell poll doesn't race a writer.
                let status = d.read_reg(E1000Regs::STATUS);
                LAST_STATUS.store(status, Ordering::Relaxed);
                if icr & irq_cause::LSC != 0 {
                    let new_up = status & e_status::LU != 0;
                    let old_up = LINK_UP.swap(new_up, Ordering::AcqRel);
                    if new_up && !old_up {
                        LINK_UP_PENDING.store(true, Ordering::Release);
                    }
                }
                icr
            }
            None => return,
        }
    };

    // Signal the task on any meaningful cause.  We wake on RXT0 (RX timer),
    // RXDMT0 (RX minimum threshold), and LSC so link-up can kick off the
    // drain/prepare path in task context.  The shared `net::NIC_WOKEN` flag
    // is what `net_task` parks on, so set it alongside the driver-specific
    // `E1000_IRQ_WOKEN`.
    let _ = icr;
    E1000_IRQ_WOKEN.store(true, Ordering::Release);
    crate::net::NIC_WOKEN.store(true, Ordering::Release);
    let raw = NET_TASK_ID.load(Ordering::Acquire);
    if raw != 0 {
        let _ = wake_task(TaskId(raw));
    }
}

// ---------------------------------------------------------------------------
// Packet receive (task context)
// ---------------------------------------------------------------------------

/// Drain the RX ring and return a `Vec<Vec<u8>>` of completed Ethernet
/// frames.  Called from the network task after an IRQ wake.
pub fn e1000_receive_packets() -> Vec<Vec<u8>> {
    interrupts::without_interrupts(|| {
        let mut frames = Vec::new();
        let mut driver = DRIVER.lock();
        let driver = match driver.as_mut() {
            Some(d) => d,
            None => return frames,
        };

        let mut last_index: Option<usize> = None;
        loop {
            let idx = driver.rx.next_to_read;
            let done = {
                let desc = &driver.rx.descs()[idx];
                rx_descriptor_done(desc.status)
            };
            if !done {
                break;
            }
            // Copy out the packet.  Hardware has already stripped FCS when
            // `RCTL.SECRC` is set.
            let (len, has_eop) = {
                let desc = &driver.rx.descs()[idx];
                (
                    (desc.length as usize).min(RX_BUF_SIZE),
                    desc.status & rx_status::EOP != 0,
                )
            };
            if has_eop && len > 0 {
                let buf = &driver.rx.bufs[idx];
                // SAFETY: DmaBuffer<[u8]> exposes a slice view of `len`
                // bytes; we bound `len` to RX_BUF_SIZE which equals the
                // buffer capacity.  No aliasing — we have `&mut driver`.
                let data = unsafe { core::slice::from_raw_parts(buf.as_ptr(), len) };
                frames.push(data.to_vec());
            }
            // Recycle the descriptor.
            {
                let pa = driver.rx.buf_phys[idx];
                let desc = &mut driver.rx.descs_mut()[idx];
                desc.status = 0;
                desc.errors = 0;
                desc.length = 0;
                desc.checksum = 0;
                desc.special = 0;
                desc.buffer_addr = pa;
            }
            driver.rx.next_to_read = (idx + 1) % RX_RING_SIZE;
            last_index = Some(idx);
        }

        // Advance RDT to the last consumed slot so hardware can refill.
        // `RDT` points at the last valid descriptor the device may use;
        // since hardware advances up to (but not including) RDT, we set it
        // to the index we just drained.
        if let Some(i) = last_index {
            driver.write_reg(E1000Regs::RDT, i as u32);
        }
        frames
    })
}

// ---------------------------------------------------------------------------
// Packet transmit (task context)
// ---------------------------------------------------------------------------

/// Copy `packet` into the next TX descriptor and hand it to hardware.
///
/// Returns `NetError::LinkDown` if `STATUS.LU` was not set (the task doc
/// E.4 bullet about refusing to enqueue on a ring hardware will not drain).
/// Returns `NetError::TooLarge` if the packet exceeds the per-slot TX buffer.
pub fn e1000_transmit(packet: &[u8]) -> Result<(), NetError> {
    if packet.is_empty() {
        return Err(NetError::TooLarge);
    }
    if packet.len() > TX_BUF_SIZE {
        return Err(NetError::TooLarge);
    }
    if !LINK_UP.load(Ordering::Acquire) {
        return Err(NetError::LinkDown);
    }

    interrupts::without_interrupts(|| {
        let mut driver = DRIVER.lock();
        let driver = match driver.as_mut() {
            Some(d) => d,
            None => return Err(NetError::NotReady),
        };

        let idx = driver.tx.next_to_write;

        // If the slot we are about to overwrite is still owned by hardware
        // (DD bit not yet set), drop the packet rather than scribble over
        // live DMA.  The 82540EM is fast enough on QEMU that this path is
        // effectively cold.
        let status = driver.tx.descs()[idx].status;
        if !tx_descriptor_done(status) && driver.tx.descs()[idx].cmd != 0 {
            // Ring full — drop.  Spec §3.3.3 says software should reclaim
            // descriptors at `TDH` but we only keep a single set of static
            // buffers, so the worst case is a single-packet drop.
            return Err(NetError::TxRingFull);
        }

        // Copy the packet into our per-slot buffer (hot path, one memcpy).
        let dst = driver.tx.bufs[idx].as_mut_ptr();
        // SAFETY: `dst` points to a TX_BUF_SIZE-byte DMA buffer owned by
        // this ring slot; `packet.len()` is bounded by TX_BUF_SIZE above.
        unsafe {
            core::ptr::copy_nonoverlapping(packet.as_ptr(), dst, packet.len());
        }

        // Program the descriptor.  Hardware reads this after the TDT
        // write below; the full-fence + volatile write keeps the memory
        // order observable on SMP.
        {
            let pa = driver.tx.buf_phys[idx];
            let desc = &mut driver.tx.descs_mut()[idx];
            desc.buffer_addr = pa;
            desc.length = packet.len() as u16;
            desc.cso = 0;
            desc.cmd = tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS;
            desc.status = 0;
            desc.css = 0;
            desc.special = 0;
        }

        driver.tx.next_to_write = (idx + 1) % TX_RING_SIZE;

        core::sync::atomic::fence(Ordering::Release);
        // Ring the TX doorbell — writing TDT past the descriptor we just
        // filled hands the slot to the MAC.
        driver.write_reg(E1000Regs::TDT, driver.tx.next_to_write as u32);
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Link-state drain on link-up
// ---------------------------------------------------------------------------

/// Called from the network task after an LSC event when the link has come
/// back up.  Flushes stale TX descriptors so the first post-up transmit
/// doesn't race a partially-drained ring.  Acceptance E.4 bullet 5.
fn on_link_up() {
    interrupts::without_interrupts(|| {
        let mut driver = DRIVER.lock();
        if let Some(d) = driver.as_mut() {
            let drained = d.tx.drain_in_flight();
            log::info!("[e1000] link up — drained {} stale TX slots", drained);
            d.write_reg(E1000Regs::TDT, 0);
        }
    });
}

/// Consume the one-shot "link just came up" edge trigger and, if set, run
/// the on_link_up drain.  Called from the net task between RX drains.
pub fn drain_link_up_edge() {
    if LINK_UP_PENDING.swap(false, Ordering::AcqRel) {
        on_link_up();
    }
}

// ---------------------------------------------------------------------------
// Probe / init
// ---------------------------------------------------------------------------

/// Register the e1000 driver with the PCI discovery framework.
///
/// Uses `PciMatch::Full` so the (0x8086, 0x100E, class 0x02, subclass 0x00)
/// tuple precisely identifies the classic 82540EM — other 8086:100E variants
/// (e.g. integrated network bridges) won't accidentally bind.
pub fn register() {
    let _ = pci::register_driver(DriverEntry {
        name: "e1000",
        r#match: PciMatch::Full {
            vendor: 0x8086,
            device: 0x100E,
            class: 0x02,
            subclass: 0x00,
        },
        init: probe,
    });
}

/// Legacy init — registers and runs one probe pass. Preserved for symmetry
/// with `virtio_net::init`; callers that already drive `pci::probe_all_drivers`
/// separately need only call [`register`].
#[allow(dead_code)]
pub fn init() {
    register();
    pci::probe_all_drivers();
}

/// Driver probe entry point — wraps [`init_with_handle`] so the bring-up
/// body can log and early-return without polluting the result type.
fn probe(handle: pci::PciDeviceHandle) -> DriverProbeResult {
    match init_with_handle(handle) {
        Ok(()) => DriverProbeResult::Bound,
        Err(reason) => DriverProbeResult::Failed(reason),
    }
}

fn init_with_handle(handle: pci::PciDeviceHandle) -> Result<(), &'static str> {
    let dev = *handle.device();
    log::info!(
        "[e1000] probe: {:04x}:{:04x} at {:02x}:{:02x}.{}",
        dev.vendor_id,
        dev.device_id,
        dev.bus,
        dev.device,
        dev.function
    );

    // PCI command register: enable memory space (bit 1) + bus master (bit 2).
    let cmd = handle.read_config_u16(0x04);
    if cmd & 0x06 != 0x06 {
        handle.write_config_u16(0x04, cmd | 0x06);
        log::info!("[e1000] PCI command: enabled memory space + bus mastering");
    }

    // BAR0 must be 32-bit or 64-bit MMIO — the 82540EM exposes a 128 KiB
    // MMIO BAR here.  We reject PIO BARs as a configuration error.
    let mmio = match pci::bar::map_bar(&handle, 0) {
        Ok(BarMapping::Mmio { region, .. }) => region,
        Ok(_) => {
            log::error!("[e1000] BAR0 is not MMIO (expected 128 KiB memory BAR)");
            return Err("e1000 BAR0 is not MMIO");
        }
        Err(e) => {
            log::error!("[e1000] BAR0 map failed: {:?}", e);
            return Err("e1000 BAR0 map failed");
        }
    };
    log::info!(
        "[e1000] BAR0 MMIO phys={:#x} size={} KiB",
        mmio.phys_base(),
        mmio.size() / 1024
    );

    // Mask interrupts before touching the device — we have not yet installed
    // a handler.  Writing all-ones to IMC disables every cause.
    mmio.write_reg::<u32>(E1000Regs::IMC, 0xFFFF_FFFF);

    // Global reset.  The spec says this bit is self-clearing; wait for it.
    let prev_ctrl = mmio.read_reg::<u32>(E1000Regs::CTRL);
    mmio.write_reg::<u32>(E1000Regs::CTRL, prev_ctrl | ctrl::RST);
    let mut cleared = false;
    for _ in 0..RESET_POLL_LIMIT {
        core::hint::spin_loop();
        if mmio.read_reg::<u32>(E1000Regs::CTRL) & ctrl::RST == 0 {
            cleared = true;
            break;
        }
    }
    if !cleared {
        log::error!("[e1000] CTRL.RST did not self-clear within bounded wait");
        return Err("e1000 reset timeout");
    }

    // Mask again after reset (reset leaves IMS implementation-defined).
    mmio.write_reg::<u32>(E1000Regs::IMC, 0xFFFF_FFFF);

    // Configure CTRL: auto-speed-detect + set-link-up; clear LRST and PHY_RST
    // so the PHY can autoneg.  Leave FD cleared — autoneg picks duplex.
    let ctrl_val = (prev_ctrl | ctrl::ASDE | ctrl::SLU) & !(ctrl::LRST | ctrl::PHY_RST);
    mmio.write_reg::<u32>(E1000Regs::CTRL, ctrl_val);

    // Clear the Multicast Table Array: 128 dwords at 0x5200..0x53FC.
    let mut off = E1000Regs::MTA;
    while off <= E1000Regs::MTA_END {
        mmio.write_reg::<u32>(off, 0);
        off += 4;
    }

    // Read MAC from RAL0 / RAH0.  QEMU pre-populates these with the
    // configured NIC MAC on reset.  If RAL0 is zero we fall back to an
    // all-zero MAC and log the anomaly — the EEPROM path is a later phase.
    let ral0 = mmio.read_reg::<u32>(E1000Regs::RAL0);
    let rah0 = mmio.read_reg::<u32>(E1000Regs::RAH0);
    let mac = decode_mac_from_ra(ral0, rah0);
    log::info!(
        "[e1000] MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (RAL0={:#x} RAH0={:#x})",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        ral0,
        rah0
    );

    // ---- E.2: rings ------------------------------------------------------

    let mut rx = RxDescRing::new(&handle)?;
    let tx = TxDescRing::new(&handle)?;

    let rx_ring_bytes = (RX_RING_SIZE * core::mem::size_of::<E1000RxDesc>()) as u32;
    let tx_ring_bytes = (TX_RING_SIZE * core::mem::size_of::<E1000TxDesc>()) as u32;

    mmio.write_reg::<u32>(E1000Regs::RDBAL, (rx.ring_phys & 0xFFFF_FFFF) as u32);
    mmio.write_reg::<u32>(E1000Regs::RDBAH, (rx.ring_phys >> 32) as u32);
    mmio.write_reg::<u32>(E1000Regs::RDLEN, rx_ring_bytes);
    mmio.write_reg::<u32>(E1000Regs::RDH, 0);
    // Hand every slot to hardware: RDT points at the last valid descriptor.
    // The spec says RDH == RDT means "empty", so set RDT to N-1.
    mmio.write_reg::<u32>(E1000Regs::RDT, (RX_RING_SIZE as u32) - 1);
    // Make sure the first pending descriptor is the slot the task will read.
    rx.next_to_read = 0;

    mmio.write_reg::<u32>(E1000Regs::TDBAL, (tx.ring_phys & 0xFFFF_FFFF) as u32);
    mmio.write_reg::<u32>(E1000Regs::TDBAH, (tx.ring_phys >> 32) as u32);
    mmio.write_reg::<u32>(E1000Regs::TDLEN, tx_ring_bytes);
    mmio.write_reg::<u32>(E1000Regs::TDH, 0);
    mmio.write_reg::<u32>(E1000Regs::TDT, 0);

    // Transmit IPG — §13.4.34 recommends 0x0060_200A for 82540EM.
    mmio.write_reg::<u32>(E1000Regs::TIPG, 0x0060_200A);

    // Transmit Control: enable + pad short + CT=0x10 + COLD=0x40.
    let tctl_val =
        tctl::EN | tctl::PSP | (0x10u32 << tctl::CT_SHIFT) | (0x40u32 << tctl::COLD_SHIFT);
    mmio.write_reg::<u32>(E1000Regs::TCTL, tctl_val);

    // Receive Control: enable + broadcast accept + strip CRC + 2 KiB buffers.
    let rctl_val = rctl::EN | rctl::BAM | rctl::SECRC | rctl::BSIZE_2048;
    mmio.write_reg::<u32>(E1000Regs::RCTL, rctl_val);

    // Snapshot link state before arming IRQs.
    let status = mmio.read_reg::<u32>(E1000Regs::STATUS);
    let link_up = status & e_status::LU != 0;
    LAST_STATUS.store(status, Ordering::Relaxed);
    LINK_UP.store(link_up, Ordering::Release);
    log::info!("[e1000] initial STATUS={:#x} link_up={}", status, link_up);

    // Install the RX/LSC IRQ.  Prefer MSI; fall back to legacy INTx on the
    // device-IRQ stub bank slot +4.  The ISR is the same either way.
    let dev_copy = dev;
    let irq = match handle.install_msi_irq(e1000_interrupt_handler) {
        Ok(i) => {
            log::info!("[e1000] MSI IRQ on vector {:#x}", i.vector());
            Some(i)
        }
        Err(_) => {
            let intx = handle.install_intx_irq(E1000_INTX_VECTOR, e1000_interrupt_handler);
            if let Ok(i) = intx {
                if dev_copy.interrupt_line != 0xFF && crate::acpi::io_apic_address().is_some() {
                    crate::arch::x86_64::apic::route_pci_irq(
                        dev_copy.interrupt_line,
                        E1000_INTX_VECTOR,
                    );
                    log::info!(
                        "[e1000] legacy INTx line {} routed to vector {:#x}",
                        dev_copy.interrupt_line,
                        E1000_INTX_VECTOR
                    );
                } else {
                    log::warn!("[e1000] legacy INTx registered but line is 0xFF or no I/O APIC");
                }
                Some(i)
            } else {
                log::warn!("[e1000] failed to install IRQ — relying on periodic polling");
                None
            }
        }
    };

    // Publish the driver **before** arming IMS.  If we armed IMS first, an
    // LSC / RXT0 could fire before `DRIVER` is `Some`, the ISR would return
    // early without reading ICR, and the interrupt would remain asserted on
    // a level-triggered INTx line — producing an interrupt storm that
    // stalls the CPU.  (MSI is edge-triggered and wouldn't storm, but
    // QEMU's classic e1000 has no MSI capability so we always take the INTx
    // path here.)
    let device = E1000Device {
        pci: handle,
        mmio,
        mac,
        rx,
        tx,
        irq,
    };
    *DRIVER.lock() = Some(device);
    E1000_READY.store(true, Ordering::Release);

    // Arm the causes we care about: RX timer (RXT0), RX threshold (RXDMT0),
    // RX overrun (RXO), and link-status change (LSC).  TX completions are
    // handled inline in `e1000_transmit`; we don't need TXDW interrupts.
    // Read back through the stored driver so we use the same MMIO alias
    // the ISR will see.
    {
        let ims = irq_cause::RXT0 | irq_cause::RXDMT0 | irq_cause::RXO | irq_cause::LSC;
        let d = DRIVER.lock();
        if let Some(dev) = d.as_ref() {
            dev.write_reg(E1000Regs::IMS, ims);
        }
    }
    log::info!("[e1000] driver initialized successfully");
    Ok(())
}
