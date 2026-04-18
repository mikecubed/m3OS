//! NVMe controller driver — Phase 55 Track D.
//!
//! This commit (D.1) brings the controller up to a known-good state:
//!
//! * PCI discovery via the HAL ([`crate::pci::register_driver`] +
//!   [`crate::pci::probe_all_drivers`]); match rule is "mass storage / NVM
//!   Express programming interface" so we do not need to enumerate vendor
//!   IDs by hand.
//! * BAR0 mapped through [`crate::pci::bar::map_bar`]; memory space + bus
//!   mastering enabled in the PCI command register.
//! * `CAP` is parsed via [`kernel_core::nvme::NvmeCap`]; `CAP.DSTRD` gives
//!   the doorbell stride we stash for later queue programming, `CAP.TO`
//!   bounds the reset polling window so a wedged device does not hang ring
//!   0 forever.
//! * Controller reset: clear `CC.EN`, wait (bounded) for `CSTS.RDY=0`. The
//!   enable side is left for D.2 because it requires valid admin queue
//!   base addresses programmed first.
//!
//! Admin queue + Identify (D.2), I/O queue + block read/write (D.3), and
//! MSI/MSI-X completion path (D.4) layer on top of this driver state.
//!
//! # Ring-0 placement
//!
//! This driver is ring 0 because Phase 55 is about enabling real-hardware
//! paths, not about building a userspace device-driver host. The contracts
//! here (probe, identify, read/write) are written so a future userspace
//! driver host can lift them without changing call sites.

use core::sync::atomic::AtomicBool;
use kernel_core::nvme as knvme;
use spin::Mutex;

use crate::pci::bar::{BarMapping, MmioRegion};
use crate::pci::{self, DriverEntry, DriverProbeResult, PciMatch};

// ===========================================================================
// PCI class / subclass / programming interface
// ===========================================================================

/// NVMe class + subclass + programming interface per PCI Code and ID
/// Assignment Specification §D (0x01 = mass storage, 0x08 = NVM, 0x02 =
/// NVM Express).
const NVME_CLASS: u8 = 0x01;
const NVME_SUBCLASS: u8 = 0x08;
const NVME_PROG_IF: u8 = 0x02;

// ===========================================================================
// Driver constants
// ===========================================================================

/// Extra safety margin on top of `CAP.TO * 500 ms`. Ensures a device that
/// overshoots its advertised timeout by a small amount does not cause us to
/// falsely declare the reset wedged.
const RESET_SAFETY_MARGIN_TICKS: u64 = 1_000;

// ===========================================================================
// Controller state
// ===========================================================================

/// Single bound NVMe controller. One per device — a second matching device
/// is declined rather than bound. The admin queue (D.2) and I/O queue (D.3)
/// hang off this struct once their bring-up sub-tasks land.
pub struct NvmeController {
    /// PCI claim handle — held for the life of the driver.
    #[allow(dead_code)]
    pci: pci::PciDeviceHandle,
    /// BAR0 MMIO region. Size checked at probe time to cover the doorbell
    /// range (>= 0x1000 bytes). Used by D.2 for the enable sequence and by
    /// D.3 for doorbell writes.
    #[allow(dead_code)]
    regs: MmioRegion,
    /// Decoded `CAP.DSTRD` stride in bytes (minimum 4). Used by later
    /// sub-tasks to compute per-queue doorbell offsets.
    #[allow(dead_code)]
    doorbell_stride_bytes: usize,
    /// `CAP.MQES + 1` — maximum queue entries the hardware will accept.
    #[allow(dead_code)]
    max_queue_entries: u16,
    /// Raw `VS` register contents, useful for diagnostic logging and for a
    /// future sysfs-style surface.
    #[allow(dead_code)]
    version: u32,
    /// `CAP.TO`-derived polling window in ticks (ms). Reused by the enable
    /// sequence (D.2) for the same bounded-timeout rule.
    #[allow(dead_code)]
    reset_timeout_ticks: u64,
}

// SAFETY: NvmeController is only accessed under DRIVER.lock(). The MMIO
// region holds raw addresses that stay valid for the driver's lifetime.
unsafe impl Send for NvmeController {}

pub(super) static DRIVER: Mutex<Option<NvmeController>> = Mutex::new(None);

/// Signals to the block dispatch layer that NVMe is ready and bound. Set in
/// D.3 once the I/O queue pair and Identify have succeeded.
#[allow(dead_code)]
pub static NVME_READY: AtomicBool = AtomicBool::new(false);

// ===========================================================================
// Driver registration (D.1)
// ===========================================================================

/// Register the NVMe driver with the PCI HAL. Called from `blk::init()`.
pub fn register() {
    let _ = pci::register_driver(DriverEntry {
        name: "nvme",
        r#match: PciMatch::ClassSubclass {
            class: NVME_CLASS,
            subclass: NVME_SUBCLASS,
        },
        init: nvme_probe,
    });
}

/// Driver init entry invoked by `probe_all_drivers`. Narrows on programming
/// interface `0x02` (NVM Express) since `0x01:0x08` is shared with future
/// NVMe command-set variants (e.g. ZNS, Key-Value).
fn nvme_probe(handle: pci::PciDeviceHandle) -> DriverProbeResult {
    let dev = *handle.device();

    if dev.prog_if != NVME_PROG_IF {
        return DriverProbeResult::Declined("non-NVM-Express programming interface");
    }
    if DRIVER.lock().is_some() {
        return DriverProbeResult::Declined("nvme controller already bound");
    }

    log::info!(
        "[nvme] probing {:04x}:{:04x} at {:02x}:{:02x}.{} (class {:02x}:{:02x}:{:02x})",
        dev.vendor_id,
        dev.device_id,
        dev.bus,
        dev.device,
        dev.function,
        dev.class_code,
        dev.subclass,
        dev.prog_if
    );

    // BAR0 is mandated to be MMIO by the NVMe spec.
    let regs = match pci::bar::map_bar(&handle, 0) {
        Ok(BarMapping::Mmio { region, .. }) => region,
        Ok(_) => return DriverProbeResult::Declined("BAR0 is not MMIO"),
        Err(_) => return DriverProbeResult::Failed("failed to map BAR0"),
    };
    log::info!(
        "[nvme] BAR0 MMIO: virt {:#x} phys {:#x} size {:#x}",
        regs.virt_base(),
        regs.phys_base(),
        regs.size()
    );
    if regs.size() < 0x1000 {
        return DriverProbeResult::Failed("BAR0 too small for doorbell range");
    }

    // Enable memory space + bus mastering on the PCI side.
    let cmd = handle.read_config_u16(0x04);
    if cmd & 0x06 != 0x06 {
        handle.write_config_u16(0x04, cmd | 0x06);
        log::info!("[nvme] PCI command: enabled memory space + bus mastering");
    }

    // Parse CAP / VS so the caller knows queue limits and doorbell stride.
    let cap_raw = regs.read_reg::<u64>(knvme::NvmeRegs::CAP);
    let cap = knvme::NvmeCap(cap_raw);
    let version = regs.read_reg::<u32>(knvme::NvmeRegs::VS);
    let doorbell_stride_bytes = cap.doorbell_stride();
    let max_queue_entries = cap.mqes();
    let reset_timeout_ticks = reset_timeout_ticks(cap.timeout_500ms_units());
    log::info!(
        "[nvme] CAP={:#x} VS={:#x} MQES={} DSTRD={}B TO_budget={}ms CSS.NVM={}",
        cap_raw,
        version,
        max_queue_entries,
        doorbell_stride_bytes,
        reset_timeout_ticks,
        cap.css_nvme()
    );

    if !cap.css_nvme() {
        return DriverProbeResult::Failed("controller does not advertise NVM command set");
    }

    if let Err(e) = reset_controller(&regs, reset_timeout_ticks) {
        return DriverProbeResult::Failed(e);
    }

    *DRIVER.lock() = Some(NvmeController {
        pci: handle,
        regs,
        doorbell_stride_bytes,
        max_queue_entries,
        version,
        reset_timeout_ticks,
    });
    log::info!("[nvme] controller in RESET state; admin queue bring-up pending (D.2)");
    DriverProbeResult::Bound
}

/// Compute the reset / enable polling window in tick_count ms.
///
/// `CAP.TO` is in 500 ms units; zero is interpreted as the implementation
/// default (we treat as one unit). The safety margin prevents false timeouts
/// on devices that overshoot their advertised budget slightly.
fn reset_timeout_ticks(to_500ms_units: u8) -> u64 {
    let units = to_500ms_units.max(1) as u64;
    units.saturating_mul(500) + RESET_SAFETY_MARGIN_TICKS
}

/// Spin on `f()` until it returns true or the tick budget expires.
///
/// Why two budgets:
///   * `budget_ticks` guards against a device that simply never acknowledges
///     its state change. The tick budget starts ticking once the timer IRQ
///     is firing; if we call this before that (during kernel bring-up),
///     `tick_count()` stays at zero and the tick check cannot fire.
///   * `MAX_SPIN_ITERATIONS` bounds the early-boot case so we cannot lock
///     up the BSP if NVMe bring-up runs before the timer is up.
pub(super) fn wait_until<F>(mut f: F, budget_ticks: u64) -> Result<(), &'static str>
where
    F: FnMut() -> bool,
{
    const MAX_SPIN_ITERATIONS: u64 = 200_000_000;
    let start = crate::arch::x86_64::interrupts::tick_count();
    let mut iterations: u64 = 0;
    loop {
        if f() {
            return Ok(());
        }
        iterations = iterations.saturating_add(1);
        if iterations >= MAX_SPIN_ITERATIONS {
            return Err("wait_until: spin budget exceeded");
        }
        let now = crate::arch::x86_64::interrupts::tick_count();
        if now.wrapping_sub(start) >= budget_ticks {
            return Err("wait_until: tick budget exceeded");
        }
        core::hint::spin_loop();
    }
}

/// Disable the controller and wait (bounded) for `CSTS.RDY=0`.
fn reset_controller(regs: &MmioRegion, timeout_ticks: u64) -> Result<(), &'static str> {
    let cc = regs.read_reg::<u32>(knvme::NvmeRegs::CC);
    if cc & knvme::CC_EN != 0 {
        regs.write_reg::<u32>(knvme::NvmeRegs::CC, cc & !knvme::CC_EN);
    }
    wait_until(
        || regs.read_reg::<u32>(knvme::NvmeRegs::CSTS) & knvme::CSTS_RDY == 0,
        timeout_ticks,
    )
    .map_err(|_| "nvme reset timeout waiting for CSTS.RDY=0")?;
    log::info!("[nvme] controller disabled (CSTS.RDY cleared)");
    Ok(())
}

// ===========================================================================
// Public init entry + capability info
// ===========================================================================

/// Register the NVMe driver and run a probe pass. Called from tests or from
/// an alternative boot path that wants to bring up NVMe alone. The normal
/// boot flow uses [`super::init`] which aggregates all block drivers into
/// one probe pass.
#[allow(dead_code)]
pub fn init() {
    register();
    pci::probe_all_drivers();
}

/// Controller version (raw `VS` register) for diagnostic callers. Returns 0
/// when no controller is bound.
#[allow(dead_code)]
pub fn controller_version() -> u32 {
    let drv = DRIVER.lock();
    drv.as_ref().map(|d| d.version).unwrap_or(0)
}
