//! PCI bus enumeration and configuration-space access.
//!
//! Scans all 256 buses, 32 devices per bus, up to 8 functions per device,
//! and stores discovered devices in a static list for later use.
//!
//! Configuration space access:
//! * Legacy mechanism #1 (I/O ports `0xCF8` / `0xCFC`) — always available.
//! * PCIe ECAM (MMIO via the MCFG ACPI allocation) — preferred when present,
//!   required for extended config space (offsets >= 256). See Phase 55 B.1.

pub mod bar;

use core::ptr;
use kernel_core::pci as kpci;
use spin::Mutex;
use x86_64::instructions::{interrupts, port::Port};

// ---------------------------------------------------------------------------
// PCI Configuration Space I/O (P15-T033, P15-T034)
// ---------------------------------------------------------------------------

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// Build the 32-bit address for PCI configuration space access.
fn config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    (1u32 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

/// Legacy I/O-port read: 32-bit value from the first 256 bytes of config space.
///
/// Interrupts are disabled for the duration of the two-port transaction
/// to prevent races on the shared CONFIG_ADDRESS/CONFIG_DATA pair.
fn legacy_pci_config_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let addr = config_address(bus, device, function, offset);
    interrupts::without_interrupts(|| {
        // SAFETY: Ports 0xCF8 and 0xCFC are the well-defined PCI configuration
        // space I/O ports on x86. Writing an address and reading data is the
        // standard mechanism #1 access pattern.
        unsafe {
            let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
            let mut data_port = Port::<u32>::new(CONFIG_DATA);
            addr_port.write(addr);
            data_port.read()
        }
    })
}

fn legacy_pci_config_write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let addr = config_address(bus, device, function, offset);
    interrupts::without_interrupts(|| {
        // SAFETY: standard PCI mechanism-#1 write.
        unsafe {
            let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
            let mut data_port = Port::<u32>::new(CONFIG_DATA);
            addr_port.write(addr);
            data_port.write(value);
        }
    });
}

// ---------------------------------------------------------------------------
// PCIe ECAM (MMIO) configuration space — Phase 55 B.1
// ---------------------------------------------------------------------------

/// Translate a segment/bus to a kernel-virtual pointer into the ECAM region,
/// if the bus is covered by an MCFG allocation.
///
/// Returns the base virtual address of the 4 KiB ECAM page for `(bus, device,
/// function)` with the first `offset` bytes already added on.
fn ecam_virt_addr(
    segment_group: u16,
    bus: u8,
    device: u8,
    function: u8,
    offset: u16,
) -> Option<usize> {
    let entries = crate::acpi::mcfg_entries()?;
    let entry = kpci::mcfg_find_base(entries, segment_group, bus)?;
    let phys = entry.ecam_address(bus, device, function, offset);
    Some((crate::mm::phys_offset() + phys) as usize)
}

/// Read a 32-bit value from PCIe MMIO configuration space (ECAM).  Returns
/// `None` if no MCFG allocation covers the target bus.
pub fn pcie_mmio_config_read(bus: u8, device: u8, function: u8, offset: u16) -> Option<u32> {
    debug_assert_eq!(offset & 3, 0, "PCIe MMIO u32 offset must be 4-byte aligned");
    debug_assert!(offset < 4096, "PCIe MMIO offset must be < 4096");
    let virt = ecam_virt_addr(0, bus, device, function, offset)?;
    // SAFETY: ECAM MMIO regions are identity-mapped via phys_offset() and
    // guaranteed aligned to 4 bytes by the debug assertion.
    let value = unsafe { ptr::read_volatile(virt as *const u32) };
    Some(value)
}

/// Write a 32-bit value to PCIe MMIO configuration space (ECAM).  Returns
/// `false` if no MCFG allocation covers the target bus; the caller should
/// fall back to legacy I/O.
pub fn pcie_mmio_config_write(bus: u8, device: u8, function: u8, offset: u16, value: u32) -> bool {
    debug_assert_eq!(offset & 3, 0, "PCIe MMIO u32 offset must be 4-byte aligned");
    debug_assert!(offset < 4096, "PCIe MMIO offset must be < 4096");
    let Some(virt) = ecam_virt_addr(0, bus, device, function, offset) else {
        return false;
    };
    // SAFETY: ECAM MMIO regions are identity-mapped via phys_offset() and
    // guaranteed aligned to 4 bytes by the debug assertion.
    unsafe { ptr::write_volatile(virt as *mut u32, value) };
    true
}

/// Read a 32-bit config value.  Uses PCIe MMIO when available (required for
/// offsets >= 256), otherwise legacy I/O (offsets < 256 only).
pub(crate) fn pci_config_read_u32_any(bus: u8, device: u8, function: u8, offset: u16) -> u32 {
    if let Some(v) = pcie_mmio_config_read(bus, device, function, offset) {
        return v;
    }
    // Legacy fallback: only the first 256 bytes are reachable.
    debug_assert!(
        offset < 256,
        "extended config space requires MCFG/ECAM; bus {} dev {} func {} offset {:#x} unreachable via legacy I/O",
        bus,
        device,
        function,
        offset
    );
    legacy_pci_config_read_u32(bus, device, function, offset as u8)
}

pub(crate) fn pci_config_write_u32_any(bus: u8, device: u8, function: u8, offset: u16, value: u32) {
    if pcie_mmio_config_write(bus, device, function, offset, value) {
        return;
    }
    debug_assert!(
        offset < 256,
        "extended config space requires MCFG/ECAM; bus {} dev {} func {} offset {:#x} unreachable via legacy I/O",
        bus,
        device,
        function,
        offset
    );
    legacy_pci_config_write_u32(bus, device, function, offset as u8, value);
}

/// Legacy-compatible 32-bit config read (first 256 bytes only, u8 offset).
/// Prefers ECAM MMIO when available so that behaviour is uniform.
fn pci_config_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pci_config_read_u32_any(bus, device, function, offset as u16)
}

/// Read a 16-bit value from PCI configuration space.
pub fn pci_config_read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    debug_assert_eq!(
        offset & 1,
        0,
        "PCI config u16 offset must be 2-byte aligned"
    );
    let dword = pci_config_read_u32(bus, device, function, offset);
    // Bit 1 of the offset selects which 16-bit half of the 32-bit dword.
    let shift = ((offset & 2) as u32) * 8;
    ((dword >> shift) & 0xFFFF) as u16
}

/// Read an 8-bit value from PCI configuration space.
#[allow(dead_code)]
pub fn pci_config_read_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let dword = pci_config_read_u32(bus, device, function, offset);
    let shift = ((offset & 3) as u32) * 8;
    ((dword >> shift) & 0xFF) as u8
}

/// Write a 16-bit value to PCI configuration space.  Routes through ECAM
/// MMIO when available, otherwise legacy mechanism #1 ports.
#[allow(dead_code)]
pub fn pci_config_write_u16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    debug_assert_eq!(
        offset & 1,
        0,
        "PCI config u16 offset must be 2-byte aligned"
    );
    let dword_offset = offset & !0x3;
    let dword = pci_config_read_u32_any(bus, device, function, dword_offset as u16);
    let shift = ((offset & 2) as u32) * 8;
    let mask = !(0xFFFFu32 << shift);
    let patched = (dword & mask) | ((value as u32) << shift);
    pci_config_write_u32_any(bus, device, function, dword_offset as u16, patched);
}

/// Read a 16-bit value from PCIe extended configuration space (offsets
/// `0..4096`).  Returns `None` if the bus is not covered by MCFG and the
/// offset is >= 256.
#[allow(dead_code)]
pub fn pcie_config_read_u16(bus: u8, device: u8, function: u8, offset: u16) -> Option<u16> {
    debug_assert_eq!(
        offset & 1,
        0,
        "PCIe config u16 offset must be 2-byte aligned"
    );
    debug_assert!(offset < 4096, "PCIe config offset must be < 4096");
    let dword_offset = offset & !0x3;
    let dword = if offset < 256 {
        pci_config_read_u32_any(bus, device, function, dword_offset)
    } else {
        pcie_mmio_config_read(bus, device, function, dword_offset)?
    };
    let shift = ((offset & 2) as u32) * 8;
    Some(((dword >> shift) & 0xFFFF) as u16)
}

/// Read an 8-bit value from PCIe extended configuration space.
#[allow(dead_code)]
pub fn pcie_config_read_u8(bus: u8, device: u8, function: u8, offset: u16) -> Option<u8> {
    debug_assert!(offset < 4096, "PCIe config offset must be < 4096");
    let dword_offset = offset & !0x3;
    let dword = if offset < 256 {
        pci_config_read_u32_any(bus, device, function, dword_offset)
    } else {
        pcie_mmio_config_read(bus, device, function, dword_offset)?
    };
    let shift = ((offset & 3) as u32) * 8;
    Some(((dword >> shift) & 0xFF) as u8)
}

/// Write a 16-bit value to PCIe extended configuration space.  Returns
/// `false` if the offset is >= 256 and no MCFG allocation covers the bus.
#[allow(dead_code)]
pub fn pcie_config_write_u16(bus: u8, device: u8, function: u8, offset: u16, value: u16) -> bool {
    debug_assert_eq!(
        offset & 1,
        0,
        "PCIe config u16 offset must be 2-byte aligned"
    );
    debug_assert!(offset < 4096, "PCIe config offset must be < 4096");
    let dword_offset = offset & !0x3;
    if offset >= 256 && crate::acpi::mcfg_entries().is_none() {
        return false;
    }
    let dword = if offset < 256 {
        pci_config_read_u32_any(bus, device, function, dword_offset)
    } else {
        match pcie_mmio_config_read(bus, device, function, dword_offset) {
            Some(v) => v,
            None => return false,
        }
    };
    let shift = ((offset & 2) as u32) * 8;
    let mask = !(0xFFFFu32 << shift);
    let patched = (dword & mask) | ((value as u32) << shift);
    if offset < 256 {
        pci_config_write_u32_any(bus, device, function, dword_offset, patched);
        true
    } else {
        pcie_mmio_config_write(bus, device, function, dword_offset, patched)
    }
}

// ---------------------------------------------------------------------------
// Phase 55 (B.3): Device claim / driver binding
// ---------------------------------------------------------------------------

/// Error returned when a claim cannot be granted.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    /// No PCI function with the requested vendor/device IDs was found.
    NotFound,
    /// The device exists but another driver has already claimed it.
    AlreadyClaimed,
}

/// Exclusive handle to a claimed PCI function.  Driver code treats this as
/// the canonical way to read and write its device's configuration space:
/// the handle carries the bus/device/function address, a copy of the
/// discovered metadata, and the registry slot used to release the claim on
/// drop.
///
/// # IOMMU domain lifetime (Phase 55a Track E.1)
///
/// As of Phase 55a every claimed handle owns a [`kernel_core::iommu::contract::DmaDomain`]
/// for the life of the claim. `claim_specific` looks up the owning IOMMU
/// unit (via `iommu::device_to_unit`), asks that unit for a fresh domain
/// via `iommu::registry::create_domain`, and stores the handle on
/// [`PciDeviceHandle::domain`]. Drop tears the domain down through the
/// same registry path.
///
/// The handle is `!Copy` and `!Clone`; ownership represents exclusive
/// access for the claim's lifetime. Every [`crate::mm::DmaBuffer`]
/// allocated against the handle is wired into a single consistent IOMMU
/// context.
///
/// ## Caller requirement: buffer lifetime must not outlive the handle
///
/// [`crate::mm::DmaBuffer::allocate`] takes a shared reference to a
/// `PciDeviceHandle` but does **not** tie the returned buffer's lifetime
/// to the handle in the type system. Each buffer records a
/// [`DomainSnapshot`] by value so Drop can unmap without touching the
/// (potentially-gone) handle, which means nothing at compile time
/// prevents a caller from letting a buffer outlive the handle that
/// created it. If that happens, Drop will try to unmap against a
/// domain that `destroy_domain` has already torn down.
///
/// Drivers must therefore uphold this ordering themselves:
///
/// 1. Drop every `DmaBuffer` the driver owns **before** dropping its
///    `PciDeviceHandle`.
/// 2. Equivalently, never `mem::forget` a handle whose buffers are
///    still live, and never hand a buffer to a pool that outlives the
///    claim.
///
/// The in-kernel drivers (NVMe, virtio-blk, virtio-net) satisfy this by
/// keeping both the handle and the rings on the same driver struct — Drop
/// ordering on struct fields tears buffers down before the handle, which
/// destroys the domain. The ring-3 e1000 driver (`userspace/drivers/e1000`)
/// owns its own `PciDeviceHandle` in userspace. Future lifetime-parameterised
/// or refcount-based API work (tracked as follow-up) would turn this
/// requirement into a compile-time guarantee.
pub struct PciDeviceHandle {
    dev: PciDevice,
    /// Index into `PCI_DEVICE_REGISTRY`. Used on drop to free the claim.
    slot: usize,
    /// The IOMMU domain assigned to this device for the claim's life.
    /// Populated on successful claim; cleared by Drop before the handle
    /// goes away so `destroy_domain` consumes the handle.
    ///
    /// `Option` because constructing a `PciDeviceHandle` before the
    /// IOMMU subsystem is initialised (early bring-up) must not panic —
    /// in that case domain creation is silently skipped and DMA
    /// allocations flow through the identity-fallback path.
    domain: Option<kernel_core::iommu::contract::DmaDomain>,
    /// Which unit in the registry owns `domain`. Copied out of the
    /// device-to-unit map at claim time so Drop does not need to
    /// re-lookup (and so the registry lock is not held across Drop).
    domain_unit_index: Option<usize>,
}

/// A snapshot of the domain attached to a handle, used by
/// `DmaBuffer::allocate` to record where an IOVA mapping lives without
/// taking a reference to the full `DmaDomain`.
#[derive(Clone, Copy, Debug)]
pub struct DomainSnapshot {
    pub unit_index: usize,
    pub domain: kernel_core::iommu::contract::DomainId,
}

#[allow(dead_code)]
impl PciDeviceHandle {
    /// The underlying device descriptor (bus/device/function + cached
    /// vendor/device/class/BARs).
    pub fn device(&self) -> &PciDevice {
        &self.dev
    }

    pub fn bus(&self) -> u8 {
        self.dev.bus
    }

    pub fn device_number(&self) -> u8 {
        self.dev.device
    }

    pub fn function(&self) -> u8 {
        self.dev.function
    }

    pub fn vendor_id(&self) -> u16 {
        self.dev.vendor_id
    }

    pub fn device_id(&self) -> u16 {
        self.dev.device_id
    }

    pub fn bars(&self) -> [u32; 6] {
        self.dev.bars
    }

    /// Read a 16-bit value from this device's configuration space.
    pub fn read_config_u16(&self, offset: u8) -> u16 {
        pci_config_read_u16(self.dev.bus, self.dev.device, self.dev.function, offset)
    }

    /// Write a 16-bit value to this device's configuration space.
    pub fn write_config_u16(&self, offset: u8, value: u16) {
        pci_config_write_u16(
            self.dev.bus,
            self.dev.device,
            self.dev.function,
            offset,
            value,
        )
    }

    /// Read an 8-bit value from this device's configuration space.
    #[allow(dead_code)]
    pub fn read_config_u8(&self, offset: u8) -> u8 {
        pci_config_read_u8(self.dev.bus, self.dev.device, self.dev.function, offset)
    }

    /// Allocate MSI or MSI-X vectors for this device (Phase 55 B.2).
    #[allow(dead_code)]
    pub fn allocate_msi_vectors(&self, count: u8) -> Option<AllocatedMsi> {
        allocate_msi_vectors(&self.dev, count)
    }

    /// Pure-data identifier for the device this handle owns.
    ///
    /// Phase 55b Track B.1 shim: the device-host syscall path uses this to
    /// key entries in `DeviceHostRegistry` and to stamp the `Capability::Device`
    /// it hands back to the driver. Callers that only need the BDF do not
    /// need the full [`PciDeviceHandle`].
    pub fn device_cap_key(&self) -> kernel_core::device_host::DeviceCapKey {
        kernel_core::device_host::DeviceCapKey::new(
            0,
            self.dev.bus,
            self.dev.device,
            self.dev.function,
        )
    }

    /// Create the inert `Capability::Device` descriptor for this handle.
    ///
    /// Phase 55b Track B.1: the syscall dispatcher stores the underlying
    /// handle in `DeviceHostRegistry` (keyed by PID + `DeviceCapKey`) so the
    /// claim, its IOMMU domain, and its PCI-registry slot all stay alive
    /// for the life of the driver process. The returned `Capability::Device`
    /// is the descriptor the driver receives via `CapabilityTable::insert`.
    ///
    /// The method borrows `self` and only constructs the capability
    /// descriptor; ownership of the handle remains with the registry path
    /// that tracks the claim (the name echoes the B.1 task-doc symbol
    /// `PciDeviceHandle::into_capability`, but no `self` is consumed here).
    pub fn as_capability(&self) -> kernel_core::ipc::Capability {
        kernel_core::ipc::Capability::Device {
            key: self.device_cap_key(),
        }
    }

    /// Snapshot of the IOMMU domain attached to this handle, if any.
    ///
    /// Returned as an `Option` because `claim_specific` may be called
    /// before `iommu::init` has run, in which case the handle carries no
    /// domain and DMA buffers fall through the identity path.
    ///
    /// `DmaBuffer::allocate` is the sole consumer — it needs just
    /// enough information to route `map` / `unmap` calls to the right
    /// unit without taking a reference to the underlying `DmaDomain`
    /// (which must remain uniquely owned by this handle).
    pub fn domain_snapshot(&self) -> Option<DomainSnapshot> {
        let unit = self.domain_unit_index?;
        let d = self.domain.as_ref()?;
        Some(DomainSnapshot {
            unit_index: unit,
            domain: d.id(),
        })
    }
}

impl Drop for PciDeviceHandle {
    fn drop(&mut self) {
        // 1. Destroy the IOMMU domain before releasing the claim. Live
        //    DMA buffers allocated against this handle **must** have
        //    been dropped before the handle itself — they carry IOVA
        //    mappings into this domain and releasing the domain first
        //    would leak the IOVA space. Track E.1's ordering contract
        //    enforces this at the driver layer.
        if let (Some(domain), Some(unit_index)) = (self.domain.take(), self.domain_unit_index) {
            match crate::iommu::registry::destroy_domain(unit_index, domain) {
                Ok(()) => {
                    log::debug!(
                        "[pci] drop: destroyed domain for {:02x}:{:02x}.{}",
                        self.dev.bus,
                        self.dev.device,
                        self.dev.function,
                    );
                }
                Err(e) => {
                    log::warn!(
                        "[pci] drop: destroy_domain failed on unit {} for {:02x}:{:02x}.{}: {}",
                        unit_index,
                        self.dev.bus,
                        self.dev.device,
                        self.dev.function,
                        e,
                    );
                }
            }
        }

        // 2. Return the registry slot to the free pool. Drivers currently
        //    never unload, so this runs only in tests or teardown paths.
        let mut reg = PCI_DEVICE_REGISTRY.lock();
        if let Some(slot) = reg.slots.get_mut(self.slot) {
            *slot = None;
        }
    }
}

/// One entry in the claim registry.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
struct ClaimSlot {
    /// bus/device/function of the claimed PCI function.
    bus: u8,
    device: u8,
    function: u8,
    /// Driver name for diagnostic logging.
    driver: &'static str,
}

const MAX_PCI_CLAIMS: usize = 64;

pub(crate) struct PciDeviceRegistry {
    slots: [Option<ClaimSlot>; MAX_PCI_CLAIMS],
}

impl PciDeviceRegistry {
    const fn new() -> Self {
        Self {
            slots: [None; MAX_PCI_CLAIMS],
        }
    }

    /// Returns true if `(bus, device, function)` is already in the table.
    fn is_claimed(&self, bus: u8, device: u8, function: u8) -> bool {
        self.slots.iter().any(|s| {
            s.map(|c| c.bus == bus && c.device == device && c.function == function)
                .unwrap_or(false)
        })
    }

    /// Reserve a slot for `(bus, device, function)`; returns the slot index.
    fn reserve(
        &mut self,
        bus: u8,
        device: u8,
        function: u8,
        driver: &'static str,
    ) -> Option<usize> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(ClaimSlot {
                    bus,
                    device,
                    function,
                    driver,
                });
                return Some(i);
            }
        }
        None
    }
}

pub(crate) static PCI_DEVICE_REGISTRY: Mutex<PciDeviceRegistry> =
    Mutex::new(PciDeviceRegistry::new());

/// Claim a PCI function matching `(vendor_id, device_id)`.  Returns an
/// exclusive [`PciDeviceHandle`], or an error if no match is found or the
/// device is already claimed.
///
/// `driver` is a short name like `"virtio-blk"` used for diagnostic logging.
///
/// Only the first matching function is considered — callers that need to
/// disambiguate multi-function devices should use
/// [`claim_pci_device_by_bdf`].
#[allow(dead_code)]
pub fn claim_pci_device(
    vendor_id: u16,
    device_id: u16,
    driver: &'static str,
) -> Result<PciDeviceHandle, ClaimError> {
    let mut found: Option<PciDevice> = None;
    let mut idx = 0;
    while let Some(dev) = pci_device(idx) {
        if dev.vendor_id == vendor_id && dev.device_id == device_id {
            found = Some(dev);
            break;
        }
        idx += 1;
    }
    let dev = found.ok_or(ClaimError::NotFound)?;
    claim_specific(dev, driver)
}

/// Claim a specific `PciDevice` discovered during enumeration.  Useful when
/// a driver matches on class code or walks the device list itself.
///
/// # IOMMU domain setup (Phase 55a Track E.1)
///
/// After the slot is reserved, this path:
///
/// 1. Looks up the IOMMU unit that owns the device via
///    `iommu::device_to_unit`. If none is returned (e.g. boot has not
///    yet reached `iommu::init`), the returned handle carries
///    `domain == None`; DMA buffers allocated against it flow through
///    the identity-fallback path.
/// 2. Requests a fresh domain from `iommu::registry::create_domain`.
/// 3. Pre-maps every firmware-declared reserved region in the new
///    domain as an identity mapping, so firmware-owned devices
///    (GPU framebuffers, ACPI reclaim) keep working.
///
/// On any IOMMU-side failure the claim is still granted (the device is
/// still usable); we log the failure and attach `None` so the driver
/// sees the identity-fallback path. This deliberately avoids making
/// IOMMU bring-up a hard requirement for device discovery — a regressed
/// IOMMU should degrade gracefully, not brick the system.
pub fn claim_specific(dev: PciDevice, driver: &'static str) -> Result<PciDeviceHandle, ClaimError> {
    let mut reg = PCI_DEVICE_REGISTRY.lock();
    if reg.is_claimed(dev.bus, dev.device, dev.function) {
        return Err(ClaimError::AlreadyClaimed);
    }
    let slot = reg
        .reserve(dev.bus, dev.device, dev.function, driver)
        .ok_or(ClaimError::AlreadyClaimed)?;
    drop(reg);
    log::info!(
        "[pci] claim: {} -> {:04x}:{:04x} {:02x}:{:02x}.{} (slot {})",
        driver,
        dev.vendor_id,
        dev.device_id,
        dev.bus,
        dev.device,
        dev.function,
        slot
    );

    let (domain, domain_unit_index) = attach_domain(&dev);

    Ok(PciDeviceHandle {
        dev,
        slot,
        domain,
        domain_unit_index,
    })
}

/// Claim a PCI function by segment / bus / device / function.
///
/// Phase 55b Track B.1: the device-host syscall dispatcher (`sys_device_claim`)
/// uses this to bind a ring-3 driver process to a specific BDF. Returns
/// [`ClaimError::NotFound`] when no enumerated device matches, or
/// [`ClaimError::AlreadyClaimed`] when an in-kernel driver or another
/// ring-3 driver already holds the slot.
///
/// `segment` is currently required to be zero — multi-segment PCIe is a
/// future extension; any non-zero segment returns `NotFound`.
pub fn claim_pci_device_by_bdf(
    segment: u16,
    bus: u8,
    device: u8,
    function: u8,
    driver: &'static str,
) -> Result<PciDeviceHandle, ClaimError> {
    if segment != 0 {
        return Err(ClaimError::NotFound);
    }
    let mut idx = 0;
    while let Some(dev) = pci_device(idx) {
        if dev.bus == bus && dev.device == device && dev.function == function {
            return claim_specific(dev, driver);
        }
        idx += 1;
    }
    Err(ClaimError::NotFound)
}

/// Look up the IOMMU unit for `dev`, request a domain, and pre-map any
/// reserved regions. Returns `(None, None)` on any failure — the caller
/// proceeds with an identity-fallback handle.
fn attach_domain(
    dev: &PciDevice,
) -> (
    Option<kernel_core::iommu::contract::DmaDomain>,
    Option<usize>,
) {
    // Segment group is always 0 on current platforms; multi-segment
    // PCI setups are a future extension.
    let unit_index = match crate::iommu::device_to_unit(0, dev.bus, dev.device, dev.function) {
        Some(i) => i,
        None => {
            // Either iommu::init hasn't run yet, or the device is not
            // covered by any IOMMU unit (rare but valid — e.g. legacy
            // devices scoped out of the DMAR). Fall through to no-domain.
            return (None, None);
        }
    };
    match crate::iommu::registry::create_domain(unit_index) {
        Ok(domain) => {
            let id = domain.id();
            // Pre-map firmware reserved regions so RMRR-owned devices
            // keep working (E.4).
            crate::iommu::registry::pre_map_reserved(unit_index, id);
            log::debug!(
                "[pci] iommu: claim {:02x}:{:02x}.{} bound to unit {} domain {:?}",
                dev.bus,
                dev.device,
                dev.function,
                unit_index,
                id,
            );
            (Some(domain), Some(unit_index))
        }
        Err(e) => {
            log::warn!(
                "[pci] iommu: create_domain on unit {} failed for {:02x}:{:02x}.{}: {} — falling back to identity",
                unit_index,
                dev.bus,
                dev.device,
                dev.function,
                e,
            );
            (None, None)
        }
    }
}

// ---------------------------------------------------------------------------
// PciDevice (P15-T035)
// ---------------------------------------------------------------------------

/// Describes a single PCI function discovered during bus enumeration.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub header_type: u8,
    pub bars: [u32; 6],
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
}

// ---------------------------------------------------------------------------
// Static storage (P15-T038)
// ---------------------------------------------------------------------------

const MAX_PCI_DEVICES: usize = 64;

struct PciDeviceList {
    devices: [Option<PciDevice>; MAX_PCI_DEVICES],
    count: usize,
}

impl PciDeviceList {
    const fn new() -> Self {
        Self {
            devices: [None; MAX_PCI_DEVICES],
            count: 0,
        }
    }

    fn push(&mut self, dev: PciDevice) -> bool {
        if self.count < MAX_PCI_DEVICES {
            self.devices[self.count] = Some(dev);
            self.count += 1;
            true
        } else {
            false
        }
    }
}

static PCI_DEVICES: Mutex<PciDeviceList> = Mutex::new(PciDeviceList::new());

// ---------------------------------------------------------------------------
// Read-only accessors (P15-T039)
// ---------------------------------------------------------------------------

/// Returns the number of PCI devices discovered during the last scan.
#[allow(dead_code)]
pub fn pci_device_count() -> usize {
    PCI_DEVICES.lock().count
}

/// Returns a copy of the PCI device at the given index, or `None`.
pub fn pci_device(index: usize) -> Option<PciDevice> {
    let list = PCI_DEVICES.lock();
    if index < list.count {
        list.devices[index]
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Device probing (P15-T037)
// ---------------------------------------------------------------------------

/// Read all relevant fields for a single PCI function and return a `PciDevice`.
fn probe_function(bus: u8, device: u8, function: u8) -> PciDevice {
    // Offset 0x00: vendor_id (low 16), device_id (high 16)
    let id_reg = pci_config_read_u32(bus, device, function, 0x00);
    let vendor_id = id_reg as u16;
    let device_id = (id_reg >> 16) as u16;

    // Offset 0x08: revision (byte 0), prog_if (byte 1), subclass (byte 2), class (byte 3)
    let class_reg = pci_config_read_u32(bus, device, function, 0x08);
    let prog_if = ((class_reg >> 8) & 0xFF) as u8;
    let subclass = ((class_reg >> 16) & 0xFF) as u8;
    let class_code = ((class_reg >> 24) & 0xFF) as u8;

    // Offset 0x0C: header_type is byte 2
    let hdr_reg = pci_config_read_u32(bus, device, function, 0x0C);
    let header_type = ((hdr_reg >> 16) & 0xFF) as u8;

    // BARs: only for header type 0 (general device). Header type 1 (PCI-PCI bridge)
    // and type 2 (CardBus) have different layouts.
    let mut bars = [0u32; 6];
    if header_type & 0x7F == 0x00 {
        for (i, bar) in bars.iter_mut().enumerate() {
            *bar = pci_config_read_u32(bus, device, function, 0x10 + (i as u8) * 4);
        }
    }

    // Offset 0x3C: interrupt_line (byte 0), interrupt_pin (byte 1)
    let int_reg = pci_config_read_u32(bus, device, function, 0x3C);
    let interrupt_line = (int_reg & 0xFF) as u8;
    let interrupt_pin = ((int_reg >> 8) & 0xFF) as u8;

    PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id,
        class_code,
        subclass,
        prog_if,
        header_type,
        bars,
        interrupt_line,
        interrupt_pin,
    }
}

// ---------------------------------------------------------------------------
// Bus scan (P15-T036)
// ---------------------------------------------------------------------------

/// Scan all PCI buses and populate the global device list.
fn pci_scan() {
    let mut list = PCI_DEVICES.lock();
    list.count = 0;
    for i in 0..MAX_PCI_DEVICES {
        list.devices[i] = None;
    }
    let mut overflow_logged = false;

    for bus in 0..=255u16 {
        let bus = bus as u8;
        for device in 0..32u8 {
            // Check if function 0 exists.
            let vendor = pci_config_read_u16(bus, device, 0, 0x00);
            if vendor == 0xFFFF {
                continue;
            }

            // Probe function 0.
            let dev0 = probe_function(bus, device, 0);
            if !list.push(dev0) && !overflow_logged {
                log::warn!(
                    "[pci] device list full ({} devices); additional devices not stored",
                    MAX_PCI_DEVICES
                );
                overflow_logged = true;
            }

            // Check multi-function bit (bit 7 of header_type at function 0).
            let multi_function = dev0.header_type & 0x80 != 0;
            if !multi_function {
                continue;
            }

            // Scan remaining functions 1..7.
            for function in 1..8u8 {
                let vendor = pci_config_read_u16(bus, device, function, 0x00);
                if vendor == 0xFFFF {
                    continue;
                }
                let dev = probe_function(bus, device, function);
                let _ = list.push(dev);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Logging (P15-T040)
// ---------------------------------------------------------------------------

/// Return a human-readable description for common PCI class/subclass pairs.
fn class_description(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x00, 0x00) => "Unclassified",
        (0x00, _) => "Unclassified",
        (0x01, 0x00) => "SCSI Bus Controller",
        (0x01, 0x01) => "IDE Controller",
        (0x01, 0x06) => "SATA Controller",
        (0x01, _) => "Mass Storage",
        (0x02, 0x00) => "Ethernet Controller",
        (0x02, _) => "Network",
        (0x03, 0x00) => "VGA Controller",
        (0x03, _) => "Display",
        (0x04, _) => "Multimedia",
        (0x05, _) => "Memory Controller",
        (0x06, 0x00) => "Host Bridge",
        (0x06, 0x01) => "ISA Bridge",
        (0x06, 0x04) => "PCI-to-PCI Bridge",
        (0x06, 0x80) => "Other Bridge",
        (0x06, _) => "Bridge",
        (0x07, _) => "Communication Controller",
        (0x08, _) => "System Peripheral",
        (0x0C, 0x03) => "USB Controller",
        (0x0C, _) => "Serial Bus Controller",
        _ => "Unknown",
    }
}

/// Scan PCI buses and log all discovered devices.
pub fn pci_scan_and_log() {
    pci_scan();

    let count = {
        let list = PCI_DEVICES.lock();
        list.count
    };

    log::info!("[pci] discovered {} device(s)", count);

    for i in 0..count {
        if let Some(dev) = pci_device(i) {
            log::info!(
                "[pci] {:02x}:{:02x}.{} {:04x}:{:04x} {:02x}/{:02x} ({})",
                dev.bus,
                dev.device,
                dev.function,
                dev.vendor_id,
                dev.device_id,
                dev.class_code,
                dev.subclass,
                class_description(dev.class_code, dev.subclass),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Module init (called from kernel_main)
// ---------------------------------------------------------------------------

/// Initialize PCI subsystem: enumerate buses and log devices.
pub fn init() {
    pci_scan_and_log();
}

// ===========================================================================
// PCI capability walking + MSI / MSI-X parsing (Phase 55 B.2)
//
// The MSI / MSI-X surface is forward-compatible: Track C's `DeviceIrq`
// contract and Track D/E NVMe / e1000 drivers are the first callers.  For
// the B.2 landing commit nothing in the kernel yet calls `allocate_msi_vectors`,
// so we `#[allow(dead_code)]` each item individually below.
// ===========================================================================

/// Maximum capability-list entries we will traverse before giving up — guards
/// against circular capability pointers on malformed hardware.
const MAX_CAP_WALK: usize = 48;

/// Walk the PCI capability list for `(bus, device, function)` and pass each
/// `(cap_id, cap_offset)` pair to `visit` until it returns `Some`.
///
/// `cap_offset` is the byte offset of the capability structure within PCI
/// configuration space. `cap_id` is read from offset 0 of the capability;
/// `next_ptr` is read from offset 1 and traversed until zero or we hit the
/// guard limit.
#[allow(dead_code)]
pub fn walk_capabilities<F, R>(bus: u8, device: u8, function: u8, mut visit: F) -> Option<R>
where
    F: FnMut(u8, u8) -> Option<R>,
{
    // Status register bit 4 indicates a capabilities list is present.
    let status = pci_config_read_u16(bus, device, function, kpci::PCI_STATUS);
    if status & kpci::PCI_STATUS_CAP_LIST == 0 {
        return None;
    }
    let mut next = pci_config_read_u8(bus, device, function, kpci::PCI_CAPABILITIES_POINTER) & 0xFC;
    let mut steps = 0;
    while next != 0 && steps < MAX_CAP_WALK {
        let cap_id = pci_config_read_u8(bus, device, function, next);
        let next_ptr = pci_config_read_u8(bus, device, function, next.wrapping_add(1)) & 0xFC;
        if let Some(result) = visit(cap_id, next) {
            return Some(result);
        }
        // Guard against a cap pointing at itself.
        if next_ptr == next {
            break;
        }
        next = next_ptr;
        steps += 1;
    }
    None
}

/// Find a capability with the given ID.  Returns the offset of the capability
/// header in config space, or `None` if absent.
#[allow(dead_code)]
pub fn find_capability(bus: u8, device: u8, function: u8, cap_id: u8) -> Option<u8> {
    walk_capabilities(bus, device, function, |id, off| {
        if id == cap_id { Some(off) } else { None }
    })
}

// ---------------------------------------------------------------------------
// MSI (legacy) capability
// ---------------------------------------------------------------------------

/// A parsed MSI capability.  Describes what the device supports and where in
/// config space to program the message address/data.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct MsiCapability {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    /// Offset in config space of the capability header.
    pub cap_offset: u8,
    /// Value of the Message Control register at discovery time.
    pub control: u16,
    pub is_64bit: bool,
    pub per_vector_mask: bool,
    /// Number of vectors the device can request (power of two, 1..=32).
    pub multi_message_capable: u8,
}

#[allow(dead_code)]
impl MsiCapability {
    fn read(bus: u8, device: u8, function: u8, cap_offset: u8) -> Self {
        let control = pci_config_read_u16(
            bus,
            device,
            function,
            cap_offset + kpci::MSI_MESSAGE_CONTROL,
        );
        let is_64bit = control & kpci::MSI_CTRL_64BIT != 0;
        let per_vector_mask = control & kpci::MSI_CTRL_PER_VECTOR_MASK != 0;
        let multi_message_capable = kpci::msi_decode_mmc_count(control);
        Self {
            bus,
            device,
            function,
            cap_offset,
            control,
            is_64bit,
            per_vector_mask,
            multi_message_capable,
        }
    }

    /// Program Message Address/Data for a single delivered vector and enable
    /// MSI.  `apic_lapic_id` is the target LAPIC ID (bits 19:12 of the MSI
    /// address), `vector` is the IDT vector (bits 7:0 of the MSI data).
    fn program_single(&self, apic_lapic_id: u8, vector: u8, count: u8) {
        // MSI address: FEEx_xxxx with target LAPIC ID in bits 19:12.
        let addr_low: u32 = 0xFEE0_0000 | ((apic_lapic_id as u32) << 12);
        let addr_high: u32 = 0;
        pci_config_write_u32_any(
            self.bus,
            self.device,
            self.function,
            (self.cap_offset + kpci::MSI_MESSAGE_ADDRESS) as u16,
            addr_low,
        );
        if self.is_64bit {
            pci_config_write_u32_any(
                self.bus,
                self.device,
                self.function,
                (self.cap_offset + kpci::MSI_MESSAGE_ADDRESS_HIGH) as u16,
                addr_high,
            );
        }
        // MSI data: delivery mode 000 (fixed), trigger edge, vector in bits 7:0.
        let data_off = kpci::msi_data_offset(self.is_64bit);
        pci_config_write_u16(
            self.bus,
            self.device,
            self.function,
            self.cap_offset + data_off,
            vector as u16,
        );

        // Update Message Control: enable + MME = log2(count).
        let new_mc = kpci::msi_encode_mme(self.control, count) | kpci::MSI_CTRL_ENABLE;
        pci_config_write_u16(
            self.bus,
            self.device,
            self.function,
            self.cap_offset + kpci::MSI_MESSAGE_CONTROL,
            new_mc,
        );
    }
}

// ---------------------------------------------------------------------------
// MSI-X capability
// ---------------------------------------------------------------------------

/// A parsed MSI-X capability.  The table itself lives in BAR-mapped MMIO at
/// `(table_bar, table_offset)`; the pending-bit array at `(pba_bar,
/// pba_offset)`.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct MsixCapability {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub cap_offset: u8,
    pub control: u16,
    /// Number of entries in the MSI-X table (decoded from control field).
    pub table_size: u16,
    pub table_bar: u8,
    pub table_offset: u32,
    pub pba_bar: u8,
    pub pba_offset: u32,
}

#[allow(dead_code)]
impl MsixCapability {
    fn read(bus: u8, device: u8, function: u8, cap_offset: u8) -> Self {
        let control = pci_config_read_u16(
            bus,
            device,
            function,
            cap_offset + kpci::MSIX_MESSAGE_CONTROL,
        );
        let table_size = kpci::msix_decode_table_size(control);
        let raw_table =
            pci_config_read_u32(bus, device, function, cap_offset + kpci::MSIX_TABLE_OFFSET);
        let raw_pba =
            pci_config_read_u32(bus, device, function, cap_offset + kpci::MSIX_PBA_OFFSET);
        let (table_bar, table_offset) = kpci::msix_decode_offset_bir(raw_table);
        let (pba_bar, pba_offset) = kpci::msix_decode_offset_bir(raw_pba);
        Self {
            bus,
            device,
            function,
            cap_offset,
            control,
            table_size,
            table_bar,
            table_offset,
            pba_bar,
            pba_offset,
        }
    }

    /// Compute the kernel-virtual pointer to the MSI-X table.
    ///
    /// Delegates to the [`bar`] module's shared BAR decoder so that MSI-X
    /// table mapping and driver BAR mapping (Phase 55 C.1) go through the
    /// same codepath. Returns `None` if the table BAR is an I/O BAR (not
    /// valid for MSI-X per spec), uses a reserved encoding, or a 64-bit BAR
    /// claims a non-existent partner slot.
    fn table_virt_addr(&self, bars: [u32; 6]) -> Option<usize> {
        bar::bar_mmio_virt_offset(bars, self.table_bar, self.table_offset)
    }

    /// Program table entry `index` to deliver `vector` to `apic_lapic_id`.
    /// Each MSI-X table entry is 16 bytes: addr_low, addr_high, data, vector
    /// control (bit 0 = masked).
    fn program_entry(&self, bars: [u32; 6], index: u16, apic_lapic_id: u8, vector: u8) -> bool {
        let Some(table_virt) = self.table_virt_addr(bars) else {
            return false;
        };
        let entry_base = table_virt + (index as usize) * 16;
        let addr_low: u32 = 0xFEE0_0000 | ((apic_lapic_id as u32) << 12);
        let addr_high: u32 = 0;
        let data: u32 = vector as u32;
        // SAFETY: MSI-X table is MMIO mapped via phys_offset; each 4-byte
        // field is aligned within a 16-byte entry boundary.
        unsafe {
            ptr::write_volatile(entry_base as *mut u32, addr_low);
            ptr::write_volatile((entry_base + 4) as *mut u32, addr_high);
            ptr::write_volatile((entry_base + 8) as *mut u32, data);
            // Clear the vector mask bit to enable delivery.
            ptr::write_volatile((entry_base + 12) as *mut u32, 0);
        }
        true
    }

    /// Enable the MSI-X function and clear the function-mask bit.
    fn enable(&self) {
        let new_mc = (self.control & !kpci::MSIX_CTRL_FN_MASK) | kpci::MSIX_CTRL_ENABLE;
        pci_config_write_u16(
            self.bus,
            self.device,
            self.function,
            self.cap_offset + kpci::MSIX_MESSAGE_CONTROL,
            new_mc,
        );
    }
}

/// Find the MSI capability for a device, if any.
#[allow(dead_code)]
pub fn find_msi(bus: u8, device: u8, function: u8) -> Option<MsiCapability> {
    let off = find_capability(bus, device, function, kpci::CAP_ID_MSI)?;
    Some(MsiCapability::read(bus, device, function, off))
}

/// Find the MSI-X capability for a device, if any.
#[allow(dead_code)]
pub fn find_msix(bus: u8, device: u8, function: u8) -> Option<MsixCapability> {
    let off = find_capability(bus, device, function, kpci::CAP_ID_MSIX)?;
    Some(MsixCapability::read(bus, device, function, off))
}

// ---------------------------------------------------------------------------
// MSI vector allocation pool (Phase 55 B.2)
// ---------------------------------------------------------------------------

/// Lowest IDT vector we hand out for device MSI / MSI-X interrupts.  Vectors
/// 32..=47 are reserved for legacy PIC/APIC IRQs and the SMP IPI block.
///
/// Kept in lockstep with
/// [`crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE`] (and the
/// assertion below): the MSI pool must not advertise vectors that the IDT
/// stub bank cannot dispatch.
#[allow(dead_code)]
pub const MSI_VECTOR_BASE: u8 = crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE;
/// One past the highest IDT vector we will hand out (exclusive upper bound
/// used by [`kpci::MsiVectorAllocator`]).
///
/// Derived from the device IRQ stub bank in `arch::x86_64::interrupts` so an
/// allocation from this pool always lands on a registered dispatcher.
/// Before Phase 55 review, this was a hand-picked `0xEF` which advertised
/// 143 vectors while the stub bank only had 16 — `allocate_msi_vectors`
/// would hand out e.g. `0x90`, `register_device_irq` would return "vector
/// out of device IRQ range", and driver init would fail late.
///
/// 16 vectors is comfortably enough for Phase 55b: NVMe needs 1 admin + 1
/// I/O = 2, VirtIO-blk needs 1, VirtIO-net needs 1 — total 4 in-kernel
/// vectors. The ring-3 e1000 driver (`userspace/drivers/e1000`) claims its
/// own MSI vector in userspace via `sys_device_irq_subscribe`. If that ever
/// tightens, grow the stub bank in `arch::x86_64::interrupts` first;
/// `MSI_VECTOR_TOP` will follow automatically.
#[allow(dead_code)]
pub const MSI_VECTOR_TOP: u8 = crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE
    + crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_COUNT;

// Compile-time guard: catch future drift if either side is tweaked in
// isolation. See `DEVICE_IRQ_VECTOR_BASE` / `DEVICE_IRQ_VECTOR_COUNT` in
// `kernel::arch::x86_64::interrupts`.
const _: () = assert!(MSI_VECTOR_BASE == crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE);
const _: () = assert!(
    MSI_VECTOR_TOP
        == crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE
            + crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_COUNT
);

static MSI_POOL: Mutex<kpci::MsiVectorAllocator> = Mutex::new(kpci::MsiVectorAllocator::new(
    MSI_VECTOR_BASE,
    MSI_VECTOR_TOP,
));

/// Reserve `count` consecutive MSI vectors.  `count` must be a power of two.
#[allow(dead_code)]
pub fn reserve_msi_vectors(count: u8) -> Option<u8> {
    MSI_POOL.lock().allocate(count)
}

// ---------------------------------------------------------------------------
// Allocated vector record
// ---------------------------------------------------------------------------

/// Result of a successful MSI or MSI-X vector allocation for a device.
/// Vectors are returned as an inclusive range starting at `first_vector`.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct AllocatedMsi {
    /// First IDT vector (subsequent ones are `first_vector + i`).
    pub first_vector: u8,
    pub count: u8,
    pub kind: MsiKind,
}

/// Which capability was programmed.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsiKind {
    Msi,
    MsiX,
}

/// Allocate `count` MSI or MSI-X vectors for a device and program the
/// capability.  Prefers MSI-X when available, falls back to MSI, returns
/// `None` if neither capability is present or no vectors are free.
///
/// `count` must be a power of two.  The caller is expected to register
/// handlers for the returned vectors through the IDT path (Track C will make
/// this uniform via the `DeviceIrq` contract).
#[allow(dead_code)]
pub fn allocate_msi_vectors(dev: &PciDevice, count: u8) -> Option<AllocatedMsi> {
    if !count.is_power_of_two() {
        return None;
    }
    let apic_id = crate::arch::x86_64::apic::current_lapic_id();

    if let Some(msix) = find_msix(dev.bus, dev.device, dev.function)
        && msix.table_size as u32 >= count as u32
    {
        let first_vector = reserve_msi_vectors(count)?;
        for i in 0..count {
            if !msix.program_entry(dev.bars, i as u16, apic_id, first_vector + i) {
                log::warn!(
                    "[pci-msi] device {:04x}:{:04x} MSI-X table BAR unmappable; aborting",
                    dev.vendor_id,
                    dev.device_id
                );
                return None;
            }
        }
        msix.enable();
        log::info!(
            "[pci-msi] {:04x}:{:04x}: MSI-X vectors {:#x}..{:#x} (lapic {})",
            dev.vendor_id,
            dev.device_id,
            first_vector,
            first_vector + count - 1,
            apic_id
        );
        return Some(AllocatedMsi {
            first_vector,
            count,
            kind: MsiKind::MsiX,
        });
    }

    if let Some(msi) = find_msi(dev.bus, dev.device, dev.function)
        && msi.multi_message_capable >= count
    {
        let first_vector = reserve_msi_vectors(count)?;
        msi.program_single(apic_id, first_vector, count);
        log::info!(
            "[pci-msi] {:04x}:{:04x}: MSI vectors {:#x}..{:#x} (lapic {})",
            dev.vendor_id,
            dev.device_id,
            first_vector,
            first_vector + count - 1,
            apic_id
        );
        return Some(AllocatedMsi {
            first_vector,
            count,
            kind: MsiKind::Msi,
        });
    }

    // No MSI/MSI-X available — caller should fall back to legacy INTx.
    log::info!(
        "[pci-msi] {:04x}:{:04x}: no MSI/MSI-X capability — fall back to INTx",
        dev.vendor_id,
        dev.device_id
    );
    None
}

// ---------------------------------------------------------------------------
// Phase 55 C.3 — driver-facing IRQ contract
// ---------------------------------------------------------------------------
//
// High-level shape:
//
//   let irq = handle.install_msi_irq(my_isr)?;       // prefers MSI/MSI-X.
//   let irq = handle.install_intx_irq(my_isr)?;      // legacy INTx fallback.
//
// In both cases `my_isr: fn()` is called in ISR context. It must:
//
//   * Read/ack the device register (e.g. virtio ISR status, NVMe
//     completion doorbell).
//   * NOT allocate, NOT block, NOT take IPC locks.
//   * Signal a wait queue / Notification / AtomicBool so a task context
//     can do the real work.
//
// The returned [`DeviceIrq`] records the allocated vector and kind; drivers
// hold it alongside their device state so the vector stays live for the
// driver's lifetime. No current caller unloads, so `Drop` is a no-op.

/// An allocated device IRQ vector plus its registered handler.
#[allow(dead_code)]
#[derive(Debug)]
pub struct DeviceIrq {
    vector: u8,
    kind: crate::arch::x86_64::interrupts::DeviceIrqKind,
    /// For MSI/MSI-X kinds, which specific capability was programmed.
    /// `None` for LegacyIntx. Drivers that need to know the difference
    /// (e.g. legacy virtio shifts its register layout only when MSI-X
    /// is enabled, not plain MSI) check this.
    msi_kind: Option<MsiKind>,
}

#[allow(dead_code)]
impl DeviceIrq {
    /// IDT vector assigned to this interrupt.
    pub fn vector(&self) -> u8 {
        self.vector
    }

    /// IRQ kind — `Msi` for MSI/MSI-X, `LegacyIntx` for shared INTx.
    pub fn kind(&self) -> crate::arch::x86_64::interrupts::DeviceIrqKind {
        self.kind
    }

    /// For MSI-family interrupts, which capability (MSI or MSI-X) was
    /// actually programmed. `None` for `LegacyIntx`.
    pub fn msi_kind(&self) -> Option<MsiKind> {
        self.msi_kind
    }
}

#[allow(dead_code)]
impl PciDeviceHandle {
    /// Install `handler` for this device's MSI / MSI-X vector.
    ///
    /// Returns `Err` if the device has no MSI / MSI-X capability, no free
    /// vector is available in the device IRQ bank, or registration fails.
    /// The caller is expected to fall back to
    /// [`Self::install_intx_irq`] when this returns `Err`.
    ///
    /// Only a single vector is supported here; multi-vector MSI-X (e.g.
    /// NVMe with multiple I/O queues) is a future extension.
    pub fn install_msi_irq(&self, handler: fn()) -> Result<DeviceIrq, &'static str> {
        let dev = self.device();
        let alloc = allocate_msi_vectors(dev, 1).ok_or("no MSI/MSI-X capability available")?;
        let entry = crate::arch::x86_64::interrupts::DeviceIrqEntry {
            handler,
            kind: crate::arch::x86_64::interrupts::DeviceIrqKind::Msi,
        };
        crate::arch::x86_64::interrupts::register_device_irq(alloc.first_vector, entry)?;
        Ok(DeviceIrq {
            vector: alloc.first_vector,
            kind: crate::arch::x86_64::interrupts::DeviceIrqKind::Msi,
            msi_kind: Some(alloc.kind),
        })
    }

    /// Install `handler` as a legacy-INTx shared interrupt on the vector
    /// given by `idt_vector`. The caller is responsible for routing the
    /// device's PCI interrupt line through the I/O APIC (see
    /// `arch::x86_64::apic::route_pci_irq`).
    ///
    /// Legacy INTx handlers must check the device's ISR status register
    /// before doing any work — the interrupt line is potentially shared
    /// with other devices.
    pub fn install_intx_irq(
        &self,
        idt_vector: u8,
        handler: fn(),
    ) -> Result<DeviceIrq, &'static str> {
        let entry = crate::arch::x86_64::interrupts::DeviceIrqEntry {
            handler,
            kind: crate::arch::x86_64::interrupts::DeviceIrqKind::LegacyIntx,
        };
        crate::arch::x86_64::interrupts::register_device_irq(idt_vector, entry)?;
        Ok(DeviceIrq {
            vector: idt_vector,
            kind: crate::arch::x86_64::interrupts::DeviceIrqKind::LegacyIntx,
            msi_kind: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Phase 55 C.4 — driver registration and discovery
// ---------------------------------------------------------------------------
//
// Drivers describe themselves with a [`DriverEntry`] that pairs a
// [`PciMatch`] rule with an init function. [`register_driver`] adds the entry
// to a global table; [`probe_all_drivers`] walks discovered PCI devices in
// deterministic (bus, device, function) order and invokes matching init
// functions for any unclaimed device.
//
// Driver init takes a [`PciDeviceHandle`] (the claim is already taken) and
// returns success/failure. Failure drops the handle so another driver can
// try the same device if desired (but typically a `NotFound` match doesn't
// consume a claim).

/// Match rule for a driver's device discovery. Either match a specific
/// vendor/device pair, or a class/subclass pair (e.g. class 0x01 subclass
/// 0x08 for NVMe).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub enum PciMatch {
    /// Match by PCI vendor and device IDs.
    VendorDevice { vendor: u16, device: u16 },
    /// Match by PCI class + subclass. Useful for NVMe (0x01:0x08:0x02) or
    /// Ethernet (0x02:0x00).
    ClassSubclass { class: u8, subclass: u8 },
    /// Match by vendor/device **and** class/subclass — disambiguates
    /// multi-function vendor IDs. Example: virtio-net uses vendor 0x1AF4,
    /// device 0x1000, class 0x02 subclass 0x00 to separate from other
    /// 0x1AF4:0x1000 variants.
    Full {
        vendor: u16,
        device: u16,
        class: u8,
        subclass: u8,
    },
}

impl PciMatch {
    #[allow(dead_code)]
    fn matches(&self, dev: &PciDevice) -> bool {
        match *self {
            PciMatch::VendorDevice { vendor, device } => {
                dev.vendor_id == vendor && dev.device_id == device
            }
            PciMatch::ClassSubclass { class, subclass } => {
                dev.class_code == class && dev.subclass == subclass
            }
            PciMatch::Full {
                vendor,
                device,
                class,
                subclass,
            } => {
                dev.vendor_id == vendor
                    && dev.device_id == device
                    && dev.class_code == class
                    && dev.subclass == subclass
            }
        }
    }
}

/// Outcome of a driver init attempt.
#[allow(dead_code)]
#[derive(Debug)]
pub enum DriverProbeResult {
    /// Driver bound and initialized successfully.
    Bound,
    /// Driver declined this device (e.g. unsupported variant).  The PCI
    /// claim is dropped and another driver may be tried.
    Declined(&'static str),
    /// Driver attempted to bind but failed (bring-up error). Logged.
    Failed(&'static str),
}

/// Init function signature. Receives a claimed PCI handle, returns an outcome.
pub type DriverInitFn = fn(handle: PciDeviceHandle) -> DriverProbeResult;

/// A single driver registered for discovery.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct DriverEntry {
    /// Human-readable name for log output.
    pub name: &'static str,
    /// Match rule for selecting devices.
    pub r#match: PciMatch,
    /// Init function.
    pub init: DriverInitFn,
}

const MAX_DRIVERS: usize = 16;

struct DriverRegistry {
    drivers: [Option<DriverEntry>; MAX_DRIVERS],
    count: usize,
}

impl DriverRegistry {
    const fn new() -> Self {
        Self {
            drivers: [None; MAX_DRIVERS],
            count: 0,
        }
    }
}

static DRIVER_REGISTRY: Mutex<DriverRegistry> = Mutex::new(DriverRegistry::new());

/// Register a driver for PCI discovery. Returns `Err` if the registry is full.
#[allow(dead_code)]
pub fn register_driver(entry: DriverEntry) -> Result<(), &'static str> {
    let mut reg = DRIVER_REGISTRY.lock();
    if reg.count >= MAX_DRIVERS {
        return Err("driver registry full");
    }
    let idx = reg.count;
    reg.drivers[idx] = Some(entry);
    reg.count += 1;
    log::info!(
        "[pci-drv] registered driver `{}` (count now {})",
        entry.name,
        reg.count
    );
    Ok(())
}

/// Probe every discovered PCI device in (bus, device, function) order and
/// invoke the init function of the first registered driver whose match rule
/// matches an unclaimed device. Drivers that match but [`DriverProbeResult::Declined`]
/// or [`DriverProbeResult::Failed`] release the claim so a later-registered
/// driver can try.
#[allow(dead_code)]
pub fn probe_all_drivers() {
    // Snapshot the driver list so the global lock is not held across init
    // (drivers may themselves call into pci:: functions).
    let drivers: alloc::vec::Vec<DriverEntry> = {
        let reg = DRIVER_REGISTRY.lock();
        reg.drivers
            .iter()
            .take(reg.count)
            .filter_map(|e| *e)
            .collect()
    };
    let device_count = pci_device_count();
    for idx in 0..device_count {
        let Some(dev) = pci_device(idx) else { continue };
        // Skip devices already claimed (e.g. driver registered itself outside
        // the probe path, or the user manually claimed in bring-up).
        {
            let reg = PCI_DEVICE_REGISTRY.lock();
            if reg.is_claimed(dev.bus, dev.device, dev.function) {
                continue;
            }
        }
        for entry in &drivers {
            if !entry.r#match.matches(&dev) {
                continue;
            }
            // Try to claim. If another driver raced us and already claimed,
            // skip and move on.
            let handle = match claim_specific(dev, entry.name) {
                Ok(h) => h,
                Err(ClaimError::AlreadyClaimed) => break,
                Err(ClaimError::NotFound) => continue,
            };
            log::info!(
                "[pci-drv] probing `{}` for {:04x}:{:04x} at {:02x}:{:02x}.{}",
                entry.name,
                dev.vendor_id,
                dev.device_id,
                dev.bus,
                dev.device,
                dev.function
            );
            match (entry.init)(handle) {
                DriverProbeResult::Bound => {
                    log::info!("[pci-drv] `{}` bound successfully", entry.name);
                    break;
                }
                DriverProbeResult::Declined(reason) => {
                    log::info!("[pci-drv] `{}` declined: {}", entry.name, reason);
                    // `handle` was moved into init and dropped on its return
                    // path; the claim is already released.
                    continue;
                }
                DriverProbeResult::Failed(reason) => {
                    log::warn!("[pci-drv] `{}` failed: {}", entry.name, reason);
                    break;
                }
            }
        }
    }
}
