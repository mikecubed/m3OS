//! mt792x driver bring-up: claim, BAR0 map, bus-mastering, MSI-X subscribe,
//! WFDMA soft-reset, chip-ID readback, ring allocation, WFDMA enable.
//! (Task A.3 driver-side.)
//!
//! Register offsets, reset predicates, and bit-field constants all come from
//! `kernel_core::mt792x::regs` so the bit-level logic is host-tested. This
//! module only sequences MMIO writes against the real hardware.
//!
//! ## WFDMA-enable ordering — CRITICAL
//!
//! Set `GLO_CFG` TX_DMA_EN / RX_DMA_EN **only** after rings are programmed and
//! DTX/DRX ring-pointer registers are reset. Enabling DMA before the ring base
//! addresses are written causes the WFDMA engine to DMA to address 0 (or worse,
//! to stale ring pointers from a previous driver instance), corrupting memory
//! and producing spurious NMIs. The ordering is documented in the upstream mt76
//! driver (`mt7921_dma_enable`) and is enforced by this module's `bring_up`
//! sequencing: rings are allocated and programmed before any DMA-enable write.

extern crate alloc;

use driver_runtime::{
    DeviceCapHandle, DeviceCapKey, DeviceHandle, DriverRuntimeError, IrqNotification, Mmio,
    SyscallBackend as IrqSyscallBackend,
};
use kernel_core::mt792x::regs::{
    MT_WFDMA0_GLO_CFG, MT_WFDMA0_RST, MT_WFDMA0_RST_DRX_PTR, MT_WFDMA0_RST_DTX_PTR,
    RST_DMASHDL_ALL_RST, RST_LOGIC_RST, RX_DMA_EN, TX_DMA_EN, reset_complete,
};
use syscall_lib as sys;

use crate::mcu::McuRing;
use crate::rings::DataRings;

/// BAR index for the mt792x register window.
/// MediaTek mt792x exposes all registers including WFDMA on BAR0.
pub const MT792X_BAR_INDEX: u8 = 0;

/// MMIO window length for BAR0 — the mt792x register file spans well beyond
/// the WFDMA region (0xD7000 + a few KiB); map 1 MiB to cover the full range.
pub const MT792X_BAR_LEN: usize = 0x100_000; // 1 MiB

/// Maximum poll iterations for the WFDMA reset-complete predicate.
/// Each iteration includes a short spin-loop; total is O(tens of ms).
const WFDMA_RESET_POLL_MAX: usize = 1000;

/// Reasons mt792x bring-up can fail before any RX/TX path runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpError {
    /// A device-host syscall (claim / mmio_map / dma_alloc / irq_subscribe) failed.
    Runtime(DriverRuntimeError),
    /// The WFDMA busy bits did not clear within the bounded poll.
    ResetTimeout,
    /// The firmware blob was absent — driver continues in degraded mode.
    FirmwareAbsent,
}

impl From<DriverRuntimeError> for BringUpError {
    fn from(e: DriverRuntimeError) -> Self {
        Self::Runtime(e)
    }
}

impl From<crate::mcu::McuError> for BringUpError {
    fn from(e: crate::mcu::McuError) -> Self {
        match e {
            crate::mcu::McuError::Alloc(re) => Self::Runtime(re),
            crate::mcu::McuError::Timeout => Self::Runtime(DriverRuntimeError::Device(
                kernel_core::device_host::DeviceHostError::Internal,
            )),
            crate::mcu::McuError::SequenceMismatch => Self::Runtime(DriverRuntimeError::Device(
                kernel_core::device_host::DeviceHostError::Internal,
            )),
        }
    }
}

/// Orphan-rule-safe local view of a `DeviceHandle` as a `DeviceCapHandle`.
struct DeviceCapView<'a> {
    inner: &'a DeviceHandle,
}

impl<'a> DeviceCapView<'a> {
    fn new(inner: &'a DeviceHandle) -> Self {
        Self { inner }
    }
}

impl DeviceCapHandle for DeviceCapView<'_> {
    fn cap_handle(&self) -> u32 {
        self.inner.cap()
    }
}

/// Typestate marker for the mt792x BAR0 MMIO window.
pub struct Mt792xRegs;

/// The ring-3 mt792x driver state. One per claimed Wi-Fi NIC.
pub struct Mt792x {
    pub pci: DeviceHandle,
    pub mmio: Mmio<Mt792xRegs>,
    pub irq: IrqNotification<IrqSyscallBackend>,
    pub mcu: McuRing,
    pub data: DataRings,
    /// Chip ID read from `MT_HW_CHIPID` after reset.
    pub chip_id: u32,
}

impl Mt792x {
    /// Claim `key`, map BAR0, enable bus-mastering, subscribe the IOMMU fault
    /// ISR, perform WFDMA soft-reset, read the chip ID, allocate MCU + data
    /// rings, and enable WFDMA DMA engines.
    ///
    /// If `fw` is `None` the firmware download is skipped and the driver
    /// degrades gracefully — the hardware shell is still brought up (the WFDMA
    /// engine is reset and rings are allocated) so the binary compiles and
    /// boots, but Wi-Fi data transfers will not function until firmware is
    /// loaded (Wave 3, DRV-net track).
    pub fn bring_up(key: DeviceCapKey, fw: Option<&[u8]>) -> Result<Self, BringUpError> {
        let pci = DeviceHandle::claim(key)?;

        // Map BAR0 — the entire mt792x register file.
        let mmio = Mmio::<Mt792xRegs>::map(&pci, MT792X_BAR_INDEX, MT792X_BAR_LEN)?;

        // Enable PCI bus-mastering (BME) so the WFDMA engine can DMA into
        // host memory. Without this the IOMMU rejects every descriptor fetch.
        // Mirror how the hda driver enables BME via a config-space write to
        // the PCI command register (offset 0x04, bit 2 = Bus Master Enable).
        let _ = driver_runtime::pci_config_write(key, 0x04, 2, 0x0006);

        // Subscribe the device IRQ. The IOMMU fault ISR is routed through the
        // same notification object as the WFDMA interrupt (MSI/MSI-X vector 0).
        // CRITICAL: arm BEFORE the first DMA write so a fault during ring setup
        // is captured rather than silently masked. Mirrors the pattern used by
        // e1000/igb (subscribe_irq before enable).
        // Use the DeviceCapView shim to satisfy the DeviceCapHandle trait bound —
        // mirroring the igb driver's subscribe_irq pattern.
        let irq_view = DeviceCapView::new(&pci);
        let irq = IrqNotification::<IrqSyscallBackend>::subscribe(&irq_view, None)?;
        sys::write_str(sys::STDOUT_FILENO, "mt792x: IOMMU fault ISR armed\n");

        // WFDMA soft-reset: disable DMA engines, reset logic + DMASHDL, then
        // poll until both busy bits clear.
        Self::soft_reset(&mmio)?;

        // Reset DTX/DRX ring pointer registers. These must be cleared before
        // programming the ring base addresses so the hardware sees a clean
        // starting state on next enable.
        mmio.write_reg::<u32>(MT_WFDMA0_RST_DTX_PTR, 0xFFFF_FFFF); // reset all TX pointers
        mmio.write_reg::<u32>(MT_WFDMA0_RST_DRX_PTR, 0xFFFF_FFFF); // reset all RX pointers

        // Read chip-ID register and log it.
        // MT_HW_CHIPID is at BAR0 offset 0x00 (first MMIO word on mt7921+).
        let chip_id = mmio.read_reg::<u32>(0x00);
        write_hex32("mt792x: chip_id=0x", chip_id);

        // Allocate MCU command ring.
        let mut mcu = McuRing::allocate(&pci)?;

        // Download firmware (if blob is present).
        if let Some(blob) = fw {
            // The blob is expected to contain both the ROM-patch and RAM-code
            // sections. For the DRV-shell track we parse the firmware and issue
            // the MCU commands; the actual MCU-send/recv is plumbed through mcu.
            //
            // Use the kernel_core parsers directly: parse_patch_header for the
            // ROM-patch sections, parse_fw_trailer for the RAM-code regions.
            if let Err(e) = crate::fw::download_firmware(&mut mcu, blob, blob) {
                // Firmware parse/send failure is non-fatal at this stage —
                // degrade with a warning rather than aborting bring-up.
                sys::write_str(
                    sys::STDOUT_FILENO,
                    "mt792x: firmware download failed, degraded\n",
                );
                let _ = e;
            }
        } else {
            // No firmware blob — emit the FW_ABSENT_SENTINEL and continue.
            // The caller (main.rs) already emits it; here we just log.
            sys::write_str(sys::STDOUT_FILENO, "mt792x: no firmware blob present\n");
        }

        // Allocate WFDMA data TX/RX rings.
        let data = DataRings::allocate(&pci)?;

        // Program ring base addresses into the WFDMA engine.
        // (Descriptor base registers are written inside DataRings::allocate.)

        // CRITICAL ORDERING: enable TX/RX DMA ONLY after rings are programmed
        // and DTX/DRX pointers reset. See module-level doc comment.
        let glo_cfg = mmio.read_reg::<u32>(MT_WFDMA0_GLO_CFG);
        mmio.write_reg::<u32>(MT_WFDMA0_GLO_CFG, glo_cfg | TX_DMA_EN | RX_DMA_EN);
        sys::write_str(sys::STDOUT_FILENO, "mt792x: WFDMA TX/RX DMA enabled\n");

        Ok(Mt792x {
            pci,
            mmio,
            irq,
            mcu,
            data,
            chip_id,
        })
    }

    /// Issue a WFDMA logic reset and poll until both DMA-busy bits clear.
    fn soft_reset(mmio: &Mmio<Mt792xRegs>) -> Result<(), BringUpError> {
        // Disable DMA engines first so in-flight descriptors drain before reset.
        let glo_cfg = mmio.read_reg::<u32>(MT_WFDMA0_GLO_CFG);
        mmio.write_reg::<u32>(MT_WFDMA0_GLO_CFG, glo_cfg & !(TX_DMA_EN | RX_DMA_EN));

        // Issue logic reset + DMASHDL full reset.
        mmio.write_reg::<u32>(MT_WFDMA0_RST, RST_LOGIC_RST | RST_DMASHDL_ALL_RST);

        // Poll until both TX-busy and RX-busy bits clear.
        for _ in 0..WFDMA_RESET_POLL_MAX {
            let glo = mmio.read_reg::<u32>(MT_WFDMA0_GLO_CFG);
            if reset_complete(glo) {
                return Ok(());
            }
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }
        Err(BringUpError::ResetTimeout)
    }

    /// Log the detected chip version to the serial log.
    pub fn log_chip_id(&self) {
        write_hex32("mt792x: chip_id=0x", self.chip_id);
    }
}

/// Log `label` followed by `val` as 8 hex digits + newline (no alloc/fmt).
fn write_hex32(label: &str, val: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 9];
    for i in 0..8 {
        buf[i] = HEX[((val >> (28 - i * 4)) & 0xF) as usize];
    }
    buf[8] = b'\n';
    let _ = sys::write_str(sys::STDOUT_FILENO, label);
    // SAFETY: buf holds only ASCII hex digits + '\n', valid UTF-8.
    let _ = sys::write_str(sys::STDOUT_FILENO, unsafe {
        core::str::from_utf8_unchecked(&buf)
    });
}
