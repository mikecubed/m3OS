//! BAR (Base Address Register) mapping abstraction — Phase 55 C.1.
//!
//! Every PCI driver needs access to its device registers through either
//! memory-mapped I/O (MMIO BARs) or I/O port space (PIO BARs). The virtio
//! drivers used to hardcode BAR0 port-extraction logic inline; NVMe and e1000
//! need MMIO BARs mapped into kernel virtual memory with uncacheable attributes.
//!
//! The pure-logic BAR decoding (type + size math) lives in
//! [`kernel_core::pci`] so it can be unit-tested on the host. This module
//! wraps it with real config-space reads, MMIO page mapping, and typed
//! accessors.
//!
//! # API shape
//!
//! * [`map_bar`] — reads the BAR registers for a claimed PCI device, performs
//!   the write-ones/read-back sizing dance, and returns a [`BarMapping`]
//!   describing the region.
//! * [`BarMapping::Mmio`] wraps an [`MmioRegion`] with `read_reg<T>` /
//!   `write_reg<T>` accessors that use `core::ptr::{read,write}_volatile`.
//! * [`BarMapping::Pio`] wraps a [`PortRegion`] with equivalent typed port
//!   accessors.
//!
//! The returned [`BarMapping`] holds no ownership of the underlying region —
//! the PCI device still owns it. Dropping the mapping does not unmap memory
//! (BARs are identity-mapped via the physical-memory offset the kernel
//! established at boot, which lives for the full process lifetime).

use core::marker::PhantomData;

use kernel_core::pci as kpci;
use x86_64::instructions::port::{PortRead, PortWrite};
use x86_64::structures::paging::PageTableFlags;

use super::{PciDeviceHandle, pci_config_read_u32_any, pci_config_write_u32_any};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Reason a BAR mapping could not be built.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarError {
    /// BAR index out of range (valid indices are 0..=5 on a type-0 header).
    IndexOutOfRange,
    /// BAR reads as zero / unimplemented.
    Unimplemented,
    /// BAR uses a reserved width encoding (legacy "below 1 MiB" or unused).
    Reserved,
    /// The partner slot of a 64-bit BAR is out of range (e.g. BAR5 claiming
    /// to be 64-bit would require a non-existent BAR6).
    InvalidPair,
    /// The BAR size could not be determined (readback was invalid).
    InvalidSize,
}

// ---------------------------------------------------------------------------
// Typed MMIO + PIO wrappers
// ---------------------------------------------------------------------------

/// An MMIO register region mapped at a kernel-virtual base address.
///
/// Accesses use `core::ptr::{read_volatile, write_volatile}` so the compiler
/// cannot elide or reorder them. The `T` type parameter is restricted to
/// machine-word-size primitives via the private [`MmioRegType`] trait so
/// callers cannot accidentally alias misaligned reads on MMIO.
#[derive(Debug, Clone, Copy)]
pub struct MmioRegion {
    /// Kernel-virtual base address of the region.
    virt_base: usize,
    /// Physical (bus) base address — useful for IOMMU programming or DMA.
    #[allow(dead_code)]
    phys_base: u64,
    /// Size of the region in bytes.
    size: u64,
}

impl MmioRegion {
    /// Kernel-virtual base address of the region.
    #[inline]
    #[allow(dead_code)]
    pub fn virt_base(&self) -> usize {
        self.virt_base
    }

    /// Physical (bus) base address.
    #[inline]
    #[allow(dead_code)]
    pub fn phys_base(&self) -> u64 {
        self.phys_base
    }

    /// Region size in bytes (from the sizing readback).
    #[inline]
    #[allow(dead_code)]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Read a register at `offset` as a `T`.
    ///
    /// `T` must implement the private [`MmioRegType`] marker, which is only
    /// implemented for `u8`, `u16`, `u32`, `u64` — the full machine-word set
    /// that x86 MMIO guarantees atomic single-transaction semantics for.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `offset + size_of::<T>() <= self.size`.
    #[inline]
    #[allow(dead_code)]
    pub fn read_reg<T: MmioRegType>(&self, offset: usize) -> T {
        debug_assert!(
            (offset + core::mem::size_of::<T>()) as u64 <= self.size,
            "MmioRegion::read_reg: offset {:#x} + size {} > region size {:#x}",
            offset,
            core::mem::size_of::<T>(),
            self.size
        );
        let ptr = (self.virt_base + offset) as *const T;
        // SAFETY: `ptr` is within the mapped region (checked above). The
        // physical memory backing the BAR is live for the lifetime of the
        // device; the kernel's phys-offset mapping is valid post-init.
        unsafe { core::ptr::read_volatile(ptr) }
    }

    /// Write a register at `offset` with a `T`.
    ///
    /// See [`Self::read_reg`] for constraints.
    #[inline]
    #[allow(dead_code)]
    pub fn write_reg<T: MmioRegType>(&self, offset: usize, value: T) {
        debug_assert!(
            (offset + core::mem::size_of::<T>()) as u64 <= self.size,
            "MmioRegion::write_reg: offset {:#x} + size {} > region size {:#x}",
            offset,
            core::mem::size_of::<T>(),
            self.size
        );
        let ptr = (self.virt_base + offset) as *mut T;
        // SAFETY: same as `read_reg`.
        unsafe { core::ptr::write_volatile(ptr, value) }
    }
}

/// Marker trait for MMIO register widths we support.
///
/// Sealed — only `u8`, `u16`, `u32`, `u64` implement it. Sealing prevents
/// callers from providing their own implementations that might violate the
/// "must be a single MMIO transaction" expectation.
pub trait MmioRegType: Copy + sealed::Sealed {}

mod sealed {
    pub trait Sealed {}
    impl Sealed for u8 {}
    impl Sealed for u16 {}
    impl Sealed for u32 {}
    impl Sealed for u64 {}
}

impl MmioRegType for u8 {}
impl MmioRegType for u16 {}
impl MmioRegType for u32 {}
impl MmioRegType for u64 {}

/// An I/O port region.
#[derive(Debug, Clone, Copy)]
pub struct PortRegion {
    /// Base port number.
    port_base: u16,
    /// Region size in bytes.
    size: u32,
}

impl PortRegion {
    /// Base port number.
    #[inline]
    #[allow(dead_code)]
    pub fn port_base(&self) -> u16 {
        self.port_base
    }

    /// Region size in bytes.
    #[inline]
    #[allow(dead_code)]
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Read a `T` from `port_base + offset`.
    ///
    /// `T` must implement [`PortRegType`].
    #[inline]
    #[allow(dead_code)]
    pub fn read_reg<T: PortRegType>(&self, offset: u16) -> T {
        debug_assert!(
            (offset as u32 + core::mem::size_of::<T>() as u32) <= self.size,
            "PortRegion::read_reg: offset {:#x} + size {} > region size {:#x}",
            offset,
            core::mem::size_of::<T>(),
            self.size
        );
        T::port_read(self.port_base + offset)
    }

    /// Write a `T` to `port_base + offset`.
    #[inline]
    #[allow(dead_code)]
    pub fn write_reg<T: PortRegType>(&self, offset: u16, value: T) {
        debug_assert!(
            (offset as u32 + core::mem::size_of::<T>() as u32) <= self.size,
            "PortRegion::write_reg: offset {:#x} + size {} > region size {:#x}",
            offset,
            core::mem::size_of::<T>(),
            self.size
        );
        T::port_write(self.port_base + offset, value);
    }
}

/// Marker trait for port I/O register widths we support (u8/u16/u32).
pub trait PortRegType: Copy + sealed::Sealed {
    #[doc(hidden)]
    fn port_read(port: u16) -> Self;
    #[doc(hidden)]
    fn port_write(port: u16, value: Self);
}

impl PortRegType for u8 {
    #[inline]
    fn port_read(port: u16) -> Self {
        // SAFETY: PortRegion restricts accesses to the mapped range; all I/O
        // BARs in QEMU's virtio device class are port-safe reads.
        unsafe { u8::read_from_port(port) }
    }
    #[inline]
    fn port_write(port: u16, value: Self) {
        unsafe { u8::write_to_port(port, value) }
    }
}

impl PortRegType for u16 {
    #[inline]
    fn port_read(port: u16) -> Self {
        unsafe { u16::read_from_port(port) }
    }
    #[inline]
    fn port_write(port: u16, value: Self) {
        unsafe { u16::write_to_port(port, value) }
    }
}

impl PortRegType for u32 {
    #[inline]
    fn port_read(port: u16) -> Self {
        unsafe { u32::read_from_port(port) }
    }
    #[inline]
    fn port_write(port: u16, value: Self) {
        unsafe { u32::write_to_port(port, value) }
    }
}

// ---------------------------------------------------------------------------
// BarMapping
// ---------------------------------------------------------------------------

/// A mapped PCI BAR — either MMIO (memory-mapped) or PIO (port I/O).
///
/// The mapping records the underlying type (32-bit MMIO, 64-bit MMIO, or I/O
/// port) so drivers that care (e.g. for 64-bit descriptor programming) can
/// dispatch on it.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum BarMapping {
    /// Memory-mapped BAR.
    Mmio {
        region: MmioRegion,
        bar_type: kpci::BarType,
    },
    /// I/O-port BAR.
    Pio { region: PortRegion },
}

impl BarMapping {
    /// Expose the MMIO region if this is an MMIO BAR.
    #[inline]
    #[allow(dead_code)]
    pub fn as_mmio(&self) -> Option<&MmioRegion> {
        match self {
            BarMapping::Mmio { region, .. } => Some(region),
            _ => None,
        }
    }

    /// Expose the port region if this is a PIO BAR.
    #[inline]
    #[allow(dead_code)]
    pub fn as_pio(&self) -> Option<&PortRegion> {
        match self {
            BarMapping::Pio { region } => Some(region),
            _ => None,
        }
    }

    /// BAR type (MMIO 32/64, PIO) — useful for drivers that need to pick
    /// between descriptor formats.
    #[inline]
    #[allow(dead_code)]
    pub fn bar_type(&self) -> kpci::BarType {
        match self {
            BarMapping::Mmio { bar_type, .. } => *bar_type,
            BarMapping::Pio { .. } => kpci::BarType::Io,
        }
    }
}

// ---------------------------------------------------------------------------
// map_bar
// ---------------------------------------------------------------------------

/// Map the BAR at `bar_index` for a claimed PCI device.
///
/// Reads the raw BAR value, performs the write-ones/read-back sizing dance,
/// restores the BAR, and returns a [`BarMapping`]. MMIO BARs are exposed via
/// the kernel's physical-memory offset identity mapping; [`ensure_uncacheable`]
/// then marks the 4 KiB leaf PTEs covering the region so stores reach the
/// device on every write.
///
/// # Memory type note
///
/// The bootloader establishes the physical-memory offset map with writeback
/// caching because it spans normal RAM. Rather than reshape the boot-time
/// map for every device range, [`ensure_uncacheable`] patches each 4 KiB PTE
/// that backs the BAR by OR'ing in `NO_CACHE | WRITE_THROUGH` — see that
/// function's docs for the exact algorithm, limits, and failure handling.
///
/// Where a BAR happens to fall under a huge-page (2 MiB / 1 GiB) mapping, the
/// per-page PTE patch is skipped — we do not currently promote huge pages to
/// 4 KiB leaves. On QEMU this is not an issue in practice because MMIO sits
/// above the boot-visible RAM range, outside any huge-page RAM mapping; in
/// the residual case the CPU falls back to the default UC memory type for
/// high MMIO (x86 MTRR default behaviour), keeping device writes correct.
/// Dedicated MMIO PAT slots and real-hardware IOMMU mapping with privately
/// allocated PTEs are deferred to a later phase.
///
/// # Returns
///
/// * `Ok(BarMapping::Mmio { ... })` for memory BARs.
/// * `Ok(BarMapping::Pio { ... })` for I/O port BARs.
/// * `Err(BarError)` on invalid BAR index, reserved encoding, or unimplemented
///   BAR.
#[allow(dead_code)]
pub fn map_bar(handle: &PciDeviceHandle, bar_index: u8) -> Result<BarMapping, BarError> {
    if bar_index >= 6 {
        return Err(BarError::IndexOutOfRange);
    }

    let bus = handle.bus();
    let device = handle.device_number();
    let function = handle.function();
    let bar_offset: u16 = 0x10 + (bar_index as u16) * 4;

    // Read the original low-slot value, then size it.
    let raw_low = pci_config_read_u32_any(bus, device, function, bar_offset);

    let bar_type = match kpci::BarType::decode(raw_low) {
        Some(t) => t,
        None => return Err(BarError::Reserved),
    };

    match bar_type {
        kpci::BarType::Memory32 { .. } => {
            // Size dance: write 0xFFFFFFFF, read back, restore.
            pci_config_write_u32_any(bus, device, function, bar_offset, 0xFFFF_FFFF);
            let readback = pci_config_read_u32_any(bus, device, function, bar_offset);
            pci_config_write_u32_any(bus, device, function, bar_offset, raw_low);

            let size32 = kpci::decode_bar_size_32(raw_low, readback);
            if size32 == 0 {
                return Err(BarError::Unimplemented);
            }

            let phys_base = (kpci::bar_base_low(raw_low, bar_type)) as u64;
            if phys_base == 0 {
                return Err(BarError::Unimplemented);
            }
            let virt_base = (crate::mm::phys_offset() + phys_base) as usize;
            ensure_uncacheable(virt_base, size32 as u64);
            Ok(BarMapping::Mmio {
                region: MmioRegion {
                    virt_base,
                    phys_base,
                    size: size32 as u64,
                },
                bar_type,
            })
        }
        kpci::BarType::Memory64 { .. } => {
            if bar_index >= 5 {
                return Err(BarError::InvalidPair);
            }
            let high_offset = bar_offset + 4;
            let raw_high = pci_config_read_u32_any(bus, device, function, high_offset);

            // Size dance across both slots.
            pci_config_write_u32_any(bus, device, function, bar_offset, 0xFFFF_FFFF);
            pci_config_write_u32_any(bus, device, function, high_offset, 0xFFFF_FFFF);
            let readback_low = pci_config_read_u32_any(bus, device, function, bar_offset);
            let readback_high = pci_config_read_u32_any(bus, device, function, high_offset);
            pci_config_write_u32_any(bus, device, function, bar_offset, raw_low);
            pci_config_write_u32_any(bus, device, function, high_offset, raw_high);

            let size64 = kpci::decode_bar_size_64(raw_low, readback_low, readback_high);
            if size64 == 0 {
                return Err(BarError::Unimplemented);
            }

            let phys_base = kpci::combine_bar_64(raw_low, raw_high);
            if phys_base == 0 {
                return Err(BarError::Unimplemented);
            }
            let virt_base = (crate::mm::phys_offset() + phys_base) as usize;
            ensure_uncacheable(virt_base, size64);
            Ok(BarMapping::Mmio {
                region: MmioRegion {
                    virt_base,
                    phys_base,
                    size: size64,
                },
                bar_type,
            })
        }
        kpci::BarType::Io => {
            pci_config_write_u32_any(bus, device, function, bar_offset, 0xFFFF_FFFF);
            let readback = pci_config_read_u32_any(bus, device, function, bar_offset);
            pci_config_write_u32_any(bus, device, function, bar_offset, raw_low);

            let size = kpci::decode_bar_size_32(raw_low, readback);
            if size == 0 {
                return Err(BarError::Unimplemented);
            }
            let port_base = (kpci::bar_base_low(raw_low, bar_type)) as u16;
            if port_base == 0 {
                return Err(BarError::Unimplemented);
            }
            Ok(BarMapping::Pio {
                region: PortRegion { port_base, size },
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Cache-disable hint
// ---------------------------------------------------------------------------

/// Mark the PTE(s) covering `[virt_base, virt_base + size)` as uncacheable
/// (`NO_CACHE | WRITE_THROUGH`).
///
/// On boot the physical-memory offset mapping uses writeback caching because
/// it spans normal RAM. For MMIO BAR regions we want uncacheable access so
/// that stores reach the device on every write. We walk the active page
/// table with `OffsetPageTable` and, for each 4 KiB leaf in range, OR in
/// `PageTableFlags::NO_CACHE | PageTableFlags::WRITE_THROUGH` while
/// preserving every other bit (`NO_EXECUTE`, `GLOBAL`, custom bits, etc.).
/// If a range is covered by a huge page (2 MiB / 1 GiB), we leave it alone —
/// no huge-page promotion. In practice QEMU puts MMIO above the boot-visible
/// RAM range where the kernel's huge-page RAM mapping does not reach, so the
/// leaf patch covers every BAR we care about; if a BAR did fall under a huge
/// page, the CPU would fall back to its default UC memory type for high MMIO
/// (x86 MTRR default behaviour).
///
/// Failures here are downgraded to a warning — `map_bar` still returns a
/// valid `BarMapping` and the driver can fall back to the shared mapping.
/// Dedicated MMIO PAT slots and real-hardware IOMMU-mapped private PTEs are
/// deferred to a later phase.
#[allow(dead_code)]
fn ensure_uncacheable(virt_base: usize, size: u64) {
    use x86_64::VirtAddr;
    use x86_64::structures::paging::{Page, Size4KiB, Translate};

    // Skip the empty/zero case — nothing to do.
    if size == 0 {
        return;
    }

    // Bound the loop so that a pathological size never ties up the CPU.
    const MAX_PAGES: usize = 4096; // 16 MiB max at page granularity.
    let page_count = (size.div_ceil(4096)) as usize;
    let page_count = page_count.min(MAX_PAGES);

    // SAFETY: the caller runs during device init; no other `OffsetPageTable`
    // is alive in the same scope. get_mapper() produces a fresh mapper.
    let mut mapper = unsafe { crate::mm::paging::get_mapper() };

    use x86_64::structures::paging::Mapper;
    use x86_64::structures::paging::mapper::TranslateResult;
    for i in 0..page_count {
        let vaddr = VirtAddr::new((virt_base + i * 4096) as u64);
        // If the translation succeeds and yields a 4 KiB leaf, patch NO_CACHE.
        // We do not promote huge pages to 4 KiB — MMIO typically sits above
        // RAM in QEMU and those pages aren't covered by huge-page mappings.
        if let TranslateResult::Mapped {
            flags: existing, ..
        } = mapper.translate(vaddr)
        {
            let page = Page::<Size4KiB>::containing_address(vaddr);
            // Preserve every bit the kernel already chose for this leaf
            // (e.g. GLOBAL, NO_EXECUTE, custom BIT_11 flags) and only OR in
            // the cache-strength bits required for MMIO. Overwriting the
            // full flag set here would silently clear NO_EXECUTE on the
            // phys-offset map and change memory semantics for this range.
            let flags = existing | PageTableFlags::NO_CACHE | PageTableFlags::WRITE_THROUGH;
            // Ignore errors — the mapping may be a huge page (PageSizeNotSupported)
            // or not present. In either case the driver's init will report the
            // real failure when it touches MMIO.
            let _ = unsafe { mapper.update_flags(page, flags) }.map(|f| f.flush());
        }
    }
}

// ---------------------------------------------------------------------------
// Typed helpers used by MSI-X — replaces the inline BAR decoder previously
// in MsixCapability::table_virt_addr.
// ---------------------------------------------------------------------------

/// Compute a kernel-virtual pointer into an MMIO BAR at `bar_index + offset`.
///
/// Returns `None` if the BAR is not a memory BAR or the index is invalid.
/// Used by the MSI-X table programming path to replace its hand-rolled BAR
/// decoder.
pub(super) fn bar_mmio_virt_offset(bars: [u32; 6], bar_index: u8, offset: u32) -> Option<usize> {
    let idx = bar_index as usize;
    if idx >= bars.len() {
        return None;
    }
    let raw_low = bars[idx];
    let bar_type = kpci::BarType::decode(raw_low)?;
    match bar_type {
        kpci::BarType::Memory32 { .. } => {
            let phys = kpci::bar_base_low(raw_low, bar_type) as u64 + offset as u64;
            Some((crate::mm::phys_offset() + phys) as usize)
        }
        kpci::BarType::Memory64 { .. } => {
            if idx + 1 >= bars.len() {
                return None;
            }
            let raw_high = bars[idx + 1];
            let phys = kpci::combine_bar_64(raw_low, raw_high) + offset as u64;
            Some((crate::mm::phys_offset() + phys) as usize)
        }
        kpci::BarType::Io => None,
    }
}

// Provide the phantom lifetime annotation on BarMapping so future extensions
// can thread a device lifetime without breaking the API. PhantomData is
// behind `#[allow(dead_code)]` since BarMapping currently does not carry it.
#[allow(dead_code)]
struct _BarMappingLifetime<'a>(PhantomData<&'a ()>);
