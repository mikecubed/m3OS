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

    /// Map this (MMIO) BAR into a ring-3 address space — Phase 55b Track B.2.
    ///
    /// Reserves a contiguous user-VA window sized to the BAR, installs 4 KiB
    /// PTEs pointing at the BAR's physical frames with the right cache mode
    /// (`UC` for MMIO, `WC` for prefetchable BARs), and records a VMA so
    /// `sys_linux_munmap` / process-exit teardown skip the frames rather
    /// than return them to the RAM allocator. `BIT_11` marks the leaves
    /// as "device frame — do not free on teardown" in the same convention
    /// established for the UEFI framebuffer.
    ///
    /// # Arguments
    ///
    /// * `pid` — the PID of the driver process to map into. Must already
    ///   own the `AddressSpace` referenced by `addr_space`.
    /// * `addr_space` — cloned `Arc<AddressSpace>` captured under the
    ///   process-table lock so its page-table-mutation lock can be held
    ///   for the duration of the mapping.
    /// * `prefetchable` — the BAR's prefetchable bit, used to pick `UC`
    ///   (uncacheable) vs `WC` (write-combining) cache mode.
    ///
    /// # Returns
    ///
    /// * `Ok(user_va)` — the 4 KiB-aligned base of the new user mapping.
    /// * `Err(UserMapError)` — the mapping could not be installed (no free
    ///   user VA, page-table insert failed, etc.). The method leaves the
    ///   caller's AS exactly as it found it — any partial mapping is
    ///   rolled back before returning.
    ///
    /// Returns [`UserMapError::NotMmio`] on a PIO BAR — I/O port BARs are
    /// not mappable through the paging hardware.
    ///
    /// The production device-host syscall path (`sys_device_mmio_map`)
    /// calls [`map_mmio_region_to_user`] directly with the already-resolved
    /// `(phys_base, size)` tuple; this method is the ergonomic API for
    /// future callers (Track C.2's `driver_runtime::Mmio::map`) that hold
    /// a [`BarMapping`] instead.
    #[allow(dead_code)]
    pub fn map_to_user(
        &self,
        pid: crate::process::Pid,
        addr_space: &alloc::sync::Arc<crate::mm::AddressSpace>,
        prefetchable: bool,
    ) -> Result<u64, UserMapError> {
        let region = match self {
            BarMapping::Mmio { region, .. } => region,
            BarMapping::Pio { .. } => return Err(UserMapError::NotMmio),
        };
        map_mmio_region_to_user(pid, addr_space, region.phys_base, region.size, prefetchable)
    }
}

// ---------------------------------------------------------------------------
// User-space mapping of an MMIO BAR (Phase 55b Track B.2)
// ---------------------------------------------------------------------------

/// Failure surface for [`BarMapping::map_to_user`].
///
/// Kept as a typed enum rather than a `&'static str` so the device-host
/// syscall path can map each variant to a distinct errno deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMapError {
    /// The BAR is an I/O port BAR and cannot be mapped into a user address
    /// space.
    NotMmio,
    /// The caller's AS has no free user-virtual-address range for the BAR.
    NoFreeUserVa,
    /// A page-table insert failed (either frame alloc failed for an
    /// intermediate table, or the target VA was already mapped).
    PageTableInsertFailed,
    /// The BAR's physical base is not page-aligned or its size overflows
    /// `usize::MAX` when expressed in pages.
    InvalidBarGeometry,
    /// The caller's PID has no process-table entry — scheduling race or
    /// bogus PID.
    NoProcess,
}

/// Core user-side mapping routine — used by [`BarMapping::map_to_user`] and
/// by the device-host syscall dispatcher directly when it already holds
/// `(phys_base, size)` and does not need a full [`BarMapping`] handle.
///
/// Holds the target AS's page-table-mutation lock for the duration of the
/// mapping, bumps the generation counter on success, and records a VMA so
/// `sys_linux_munmap` recognises the range. `BIT_11` is set on every leaf
/// so process-teardown's `free_process_page_table` treats the frames as
/// device memory and does not return them to the RAM allocator.
///
/// Rolls back cleanly on failure: any partial mapping is undone and the
/// `mmap_next` cursor is restored to its pre-call position so a retry does
/// not leak VA.
pub(crate) fn map_mmio_region_to_user(
    pid: crate::process::Pid,
    addr_space: &alloc::sync::Arc<crate::mm::AddressSpace>,
    phys_base: u64,
    size: u64,
    prefetchable: bool,
) -> Result<u64, UserMapError> {
    use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};

    // Geometry check — phys_base must be page-aligned and size > 0.
    if phys_base & 0xFFF != 0 || size == 0 {
        return Err(UserMapError::InvalidBarGeometry);
    }
    let page_count = match size.checked_add(0xFFF).map(|v| v >> 12) {
        Some(p) if p > 0 => p as usize,
        _ => return Err(UserMapError::InvalidBarGeometry),
    };
    let total_size = (page_count as u64) * 4096;

    // Cache flags per the task-doc acceptance:
    //   - UC (uncacheable) for MMIO → NO_CACHE | WRITE_THROUGH
    //   - WC (write-combining) for prefetchable → NO_CACHE only
    // Without PAT slots (deferred) this is the best approximation the
    // existing kernel supports; the kernel-core unit tests pin the cache
    // mode selection so a future PAT upgrade is a single-site change.
    let cache_flags = if prefetchable {
        PageTableFlags::NO_CACHE
    } else {
        PageTableFlags::NO_CACHE | PageTableFlags::WRITE_THROUGH
    };
    // BIT_11: "device frame — do not return to the frame allocator on
    // teardown". Same convention as the framebuffer mapping in
    // `sys_framebuffer_mmap`; `free_process_page_table` skips any leaf
    // with this bit set.
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::BIT_11
        | cache_flags;

    // Claim a user-VA range under the AS page-table lock.
    let _page_table_guard = addr_space.lock_page_tables();

    const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;
    // `ANON_MMAP_BASE` is declared in the arch syscall module; inline its
    // value here to avoid a cross-module pub(crate) leak for a single
    // constant. Kept as a local const so a future mismatch is a compile-time
    // error rather than a silent drift.
    const ANON_MMAP_BASE: u64 = 0x0000_0020_0000_0000;

    let base =
        match crate::process::with_shared_mm_mut(pid, |_brk_current, mmap_next, _vma_tree| {
            let current = if *mmap_next == 0 {
                ANON_MMAP_BASE
            } else {
                *mmap_next
            };
            // Align up to 4 KiB — mmap_next is page-aligned in practice but
            // a future caller might leave a sub-page fragment.
            let base = (current + 0xFFF) & !0xFFF;
            let end = base
                .checked_add(total_size)
                .filter(|v| *v <= USER_SPACE_END)?;
            *mmap_next = end;
            Some(base)
        }) {
            Some(Some(base)) => base,
            Some(None) => return Err(UserMapError::NoFreeUserVa),
            None => return Err(UserMapError::NoProcess),
        };
    let reservation_end = base + total_size;

    // Walk the process's PML4 and install each 4 KiB PTE.
    let cr3_phys = x86_64::PhysAddr::new(addr_space.pml4_phys().as_u64());
    let cr3_frame = match PhysFrame::<Size4KiB>::from_start_address(cr3_phys) {
        Ok(f) => f,
        Err(_) => return Err(UserMapError::PageTableInsertFailed),
    };
    // SAFETY: `addr_space.lock_page_tables()` is held, so no concurrent
    // `OffsetPageTable` is alive over this PML4 within the mapping
    // critical section.
    let mut mapper = unsafe { crate::mm::mapper_for_frame(cr3_frame) };
    let mut alloc = crate::mm::paging::GlobalFrameAlloc;

    let mut installed: alloc::vec::Vec<Page<Size4KiB>> = alloc::vec::Vec::new();
    for i in 0..page_count {
        let v = base + (i as u64) * 4096;
        let p = phys_base + (i as u64) * 4096;
        let page: Page<Size4KiB> = Page::containing_address(x86_64::VirtAddr::new(v));
        let frame = match PhysFrame::<Size4KiB>::from_start_address(x86_64::PhysAddr::new(p)) {
            Ok(f) => f,
            Err(_) => {
                rollback_user_mmio_mapping(&mut mapper, &installed);
                rollback_user_mmio_reservation(pid, base, reservation_end);
                return Err(UserMapError::InvalidBarGeometry);
            }
        };
        // SAFETY: mapper and frame are valid; the PTE insert is serialized
        // by the page-table lock; the frame is device MMIO (not in the
        // RAM allocator's pool) so there is no aliasing.
        let insert = unsafe { mapper.map_to(page, frame, flags, &mut alloc) };
        match insert {
            Ok(flush) => {
                flush.flush();
                installed.push(page);
            }
            Err(_) => {
                rollback_user_mmio_mapping(&mut mapper, &installed);
                rollback_user_mmio_reservation(pid, base, reservation_end);
                return Err(UserMapError::PageTableInsertFailed);
            }
        }
    }

    // Record a VMA so `sys_linux_munmap` treats the range sensibly. We
    // don't expect drivers to munmap MMIO (the cap-drop cascade owns
    // teardown), but the VMA keeps diagnostics correct. `flags` carries
    // the MMIO marker bit — reserved in `process::MemoryMapping` flags'
    // low bits; we use `1 << 30` here as a B.2-local marker until a
    // central `MMIO_MAPPING_FLAG` is introduced alongside the framebuffer
    // one in a later phase.
    const MMIO_MAPPING_FLAG: u64 = 1 << 30;
    let _ = crate::process::with_shared_mm_mut(pid, |_brk_current, _mmap_next, vma_tree| {
        vma_tree.insert(crate::process::MemoryMapping {
            start: base,
            len: total_size,
            prot: 0x3, // PROT_READ | PROT_WRITE
            flags: 1 | MMIO_MAPPING_FLAG,
        });
    });

    addr_space.bump_generation();
    crate::smp::tlb::tlb_shootdown_range(addr_space, base, base + total_size);

    Ok(base)
}

/// Tear down a partially-installed user MMIO mapping — used on the
/// map-failure rollback path and by `unmap_mmio_region_from_user`.
fn rollback_user_mmio_mapping(
    mapper: &mut x86_64::structures::paging::OffsetPageTable<'_>,
    pages: &[x86_64::structures::paging::Page<x86_64::structures::paging::Size4KiB>],
) {
    use x86_64::structures::paging::Mapper;
    for page in pages {
        if let Ok((_frame, flush)) = mapper.unmap(*page) {
            flush.flush();
        }
    }
}

/// Reverse the `mmap_next` reservation if no later allocation has moved it.
fn rollback_user_mmio_reservation(pid: crate::process::Pid, base: u64, reservation_end: u64) {
    let _ = crate::process::with_shared_mm_mut(pid, |_brk_current, mmap_next, _vma_tree| {
        if *mmap_next == reservation_end {
            *mmap_next = base;
        }
    });
}

/// Unmap an MMIO range from a ring-3 address space — Phase 55b Track B.2
/// cleanup cascade.
///
/// Called from the device-host registry's cleanup path when a
/// `Capability::Device` is released (either explicitly or via process exit).
/// Removes the PTEs for every 4 KiB page in `[user_va, user_va + len)`,
/// flushes the TLB, and bumps the AS generation. The frames are MMIO and
/// are never returned to the RAM allocator (BIT_11 convention).
///
/// Failures to unmap individual pages are logged but not propagated — the
/// caller is already in the tear-down path and cannot meaningfully recover.
pub(crate) fn unmap_mmio_region_from_user(
    addr_space: &alloc::sync::Arc<crate::mm::AddressSpace>,
    user_va: u64,
    len: usize,
) {
    use x86_64::structures::paging::{Mapper, Page, PhysFrame, Size4KiB};

    if len == 0 || user_va == 0 {
        return;
    }
    let page_count = len.div_ceil(4096);

    let _page_table_guard = addr_space.lock_page_tables();
    let cr3_phys = x86_64::PhysAddr::new(addr_space.pml4_phys().as_u64());
    let cr3_frame = match PhysFrame::<Size4KiB>::from_start_address(cr3_phys) {
        Ok(f) => f,
        Err(_) => {
            log::warn!(
                "[device-host] unmap_mmio: invalid CR3 phys {:#x}",
                cr3_phys.as_u64()
            );
            return;
        }
    };
    // SAFETY: page-table lock held; no aliasing OffsetPageTable.
    let mut mapper = unsafe { crate::mm::mapper_for_frame(cr3_frame) };
    for i in 0..page_count {
        let v = user_va + (i as u64) * 4096;
        let page: Page<Size4KiB> = Page::containing_address(x86_64::VirtAddr::new(v));
        match mapper.unmap(page) {
            Ok((_frame, flush)) => flush.flush(),
            Err(e) => {
                log::warn!(
                    "[device-host] unmap_mmio: page {:#x} unmap failed: {:?}",
                    v,
                    e
                );
            }
        }
    }

    addr_space.bump_generation();
    crate::smp::tlb::tlb_shootdown_range(addr_space, user_va, user_va + (len as u64));
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
