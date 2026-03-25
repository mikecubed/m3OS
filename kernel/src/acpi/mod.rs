//! ACPI table discovery and parsing.
//!
//! Parses the RSDP, RSDT/XSDT, MADT, and FADT tables from firmware-provided
//! ACPI structures.  Physical addresses are converted to virtual pointers via
//! the identity-mapped physical memory region (`mm::phys_offset()`).

use core::ptr;
use spin::Once;
use x86_64::PhysAddr;

// ---------------------------------------------------------------------------
// P15-T001: Global RSDP address
// ---------------------------------------------------------------------------

static RSDP_ADDR: Once<PhysAddr> = Once::new();

// ---------------------------------------------------------------------------
// P15-T002: RSDP structures
// ---------------------------------------------------------------------------

/// ACPI 1.0 RSDP (Root System Description Pointer), 20 bytes.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RsdpDescriptor {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
}

/// ACPI 2.0+ RSDP extension, 36 bytes total.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RsdpDescriptorV2 {
    v1: RsdpDescriptor,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

// ---------------------------------------------------------------------------
// P15-T004: SDT header
// ---------------------------------------------------------------------------

/// Common header for all ACPI System Description Tables.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AcpiSdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id: u32,
    pub creator_revision: u32,
}

// ---------------------------------------------------------------------------
// P15-T007: MADT structures
// ---------------------------------------------------------------------------

/// A Local APIC entry from the MADT (type 0).
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct LocalApicEntry {
    pub acpi_processor_id: u8,
    pub apic_id: u8,
    pub flags: u32,
}

/// An I/O APIC entry from the MADT (type 1).
#[derive(Clone, Copy, Debug)]
pub struct IoApicEntry {
    pub io_apic_id: u8,
    pub io_apic_address: u32,
    pub global_system_interrupt_base: u32,
}

/// An Interrupt Source Override entry from the MADT (type 2).
#[derive(Clone, Copy, Debug)]
pub struct IrqSourceOverride {
    pub bus: u8,
    pub source: u8,
    pub global_system_interrupt: u32,
    pub flags: u16,
}

// ---------------------------------------------------------------------------
// P15-T008: Parsed MADT information
// ---------------------------------------------------------------------------

/// Collected MADT parse results.  Fixed-size arrays avoid heap allocation
/// during early init (max 16 CPUs, max 16 IRQ overrides).
#[derive(Clone)]
pub struct MadtInfo {
    pub local_apic_address: u32,
    pub local_apics: [Option<LocalApicEntry>; 16],
    pub local_apic_count: usize,
    pub io_apic: Option<IoApicEntry>,
    pub irq_overrides: [Option<IrqSourceOverride>; 16],
    pub irq_override_count: usize,
}

static MADT_INFO: Once<MadtInfo> = Once::new();

// ---------------------------------------------------------------------------
// P15-T003: RSDP validation
// ---------------------------------------------------------------------------

/// Validate the RSDP checksum and signature.  Returns the ACPI revision
/// (0 = ACPI 1.0 / RSDT only, >=2 = ACPI 2.0+ / XSDT available).
fn validate_rsdp(addr: PhysAddr) -> Option<u8> {
    let virt = phys_to_virt(addr);

    // SAFETY: the bootloader guarantees `rsdp_addr` points to a valid RSDP
    // in identity-mapped physical memory that outlives the kernel.
    let v1: RsdpDescriptor = unsafe { ptr::read_unaligned(virt as *const RsdpDescriptor) };

    // Check signature: "RSD PTR " (8 bytes, trailing space).
    if &v1.signature != b"RSD PTR " {
        log::warn!("[acpi] RSDP signature mismatch");
        return None;
    }

    // v1 checksum: sum of first 20 bytes must be 0 mod 256.
    let v1_bytes = unsafe {
        core::slice::from_raw_parts(virt as *const u8, core::mem::size_of::<RsdpDescriptor>())
    };
    let sum: u8 = v1_bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    if sum != 0 {
        log::warn!("[acpi] RSDP v1 checksum invalid");
        return None;
    }

    let revision = v1.revision;

    if revision >= 2 {
        // v2 checksum: sum of all 36 bytes must be 0 mod 256.
        let v2_bytes = unsafe {
            core::slice::from_raw_parts(virt as *const u8, core::mem::size_of::<RsdpDescriptorV2>())
        };
        let sum2: u8 = v2_bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        if sum2 != 0 {
            log::warn!("[acpi] RSDP v2 checksum invalid");
            return None;
        }
    }

    Some(revision)
}

// ---------------------------------------------------------------------------
// P15-T005: RSDT / XSDT parsing
// ---------------------------------------------------------------------------

/// Validate an SDT's checksum (sum of all bytes in the table == 0 mod 256).
fn validate_sdt(header_virt: usize) -> bool {
    const MIN_SDT_LENGTH: usize = core::mem::size_of::<AcpiSdtHeader>();
    const MAX_SDT_LENGTH: usize = 1024 * 1024; // 1 MiB upper bound

    // SAFETY: reading the `length` field from a packed SDT header at a
    // potentially unaligned address via raw pointer arithmetic.
    let length = unsafe {
        let hdr_ptr = header_virt as *const u8;
        ptr::read_unaligned(hdr_ptr.add(4) as *const u32)
    } as usize;

    if !(MIN_SDT_LENGTH..=MAX_SDT_LENGTH).contains(&length) {
        log::warn!(
            "[acpi] SDT length out of bounds: {} (expected {}..={})",
            length,
            MIN_SDT_LENGTH,
            MAX_SDT_LENGTH
        );
        return false;
    }

    // SAFETY: length is bounded above; the ACPI region is identity-mapped.
    let bytes = unsafe { core::slice::from_raw_parts(header_virt as *const u8, length) };
    let sum: u8 = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    sum == 0
}

/// Collected SDT entry pointers from the RSDT or XSDT.
/// Fixed-size: most systems have < 32 tables.
struct SdtEntries {
    entries: [usize; 32], // virtual addresses of SDT headers
    count: usize,
}

/// Parse the RSDT (32-bit pointers).
fn parse_rsdt(phys: u32) -> Option<SdtEntries> {
    let virt = phys_to_virt(PhysAddr::new(phys as u64));
    if !validate_sdt(virt) {
        log::warn!("[acpi] RSDT checksum invalid");
        return None;
    }

    // SAFETY: virt points to a valid RSDT in identity-mapped memory.
    let header: AcpiSdtHeader = unsafe { ptr::read_unaligned(virt as *const AcpiSdtHeader) };
    let header_size = core::mem::size_of::<AcpiSdtHeader>();
    let header_len = header.length as usize;
    if header_len < header_size {
        log::warn!(
            "[acpi] RSDT length too small: length={} header_size={}",
            header_len,
            header_size
        );
        return None;
    }
    let entry_count = (header_len - header_size) / 4;

    let mut result = SdtEntries {
        entries: [0; 32],
        count: 0,
    };

    for i in 0..entry_count.min(32) {
        // SAFETY: reading u32 entries from the RSDT array region.
        let entry_phys = unsafe { ptr::read_unaligned((virt + header_size + i * 4) as *const u32) };
        result.entries[result.count] = phys_to_virt(PhysAddr::new(entry_phys as u64));
        result.count += 1;
    }

    Some(result)
}

/// Parse the XSDT (64-bit pointers).
fn parse_xsdt(phys: u64) -> Option<SdtEntries> {
    let virt = phys_to_virt(PhysAddr::new(phys));
    if !validate_sdt(virt) {
        log::warn!("[acpi] XSDT checksum invalid");
        return None;
    }

    // SAFETY: virt points to a valid XSDT in identity-mapped memory.
    let header: AcpiSdtHeader = unsafe { ptr::read_unaligned(virt as *const AcpiSdtHeader) };
    let header_size = core::mem::size_of::<AcpiSdtHeader>();
    let header_len = header.length as usize;
    if header_len < header_size {
        log::warn!(
            "[acpi] XSDT length too small: length={} header_size={}",
            header_len,
            header_size
        );
        return None;
    }
    let entry_count = (header_len - header_size) / 8;

    let mut result = SdtEntries {
        entries: [0; 32],
        count: 0,
    };

    for i in 0..entry_count.min(32) {
        // SAFETY: reading u64 entries from the XSDT array region.
        let entry_phys = unsafe { ptr::read_unaligned((virt + header_size + i * 8) as *const u64) };
        result.entries[result.count] = phys_to_virt(PhysAddr::new(entry_phys));
        result.count += 1;
    }

    Some(result)
}

// ---------------------------------------------------------------------------
// Cached SDT entries (set once during init)
// ---------------------------------------------------------------------------

static SDT_ENTRIES: Once<SdtEntries> = Once::new();

// ---------------------------------------------------------------------------
// P15-T006: SDT signature lookup
// ---------------------------------------------------------------------------

/// Find an ACPI table by its 4-byte signature.  Returns a pointer to the
/// table's `AcpiSdtHeader` in kernel virtual memory, or `None` if not found.
pub fn find_table(signature: &[u8; 4]) -> Option<*const AcpiSdtHeader> {
    let entries = SDT_ENTRIES.get()?;
    for i in 0..entries.count {
        let hdr_virt = entries.entries[i];
        // SAFETY: hdr_virt points to an identity-mapped ACPI SDT header.
        // Use addr_of! to avoid forming a reference to a packed field (UB).
        let sig = unsafe {
            let sig_ptr = core::ptr::addr_of!((*(hdr_virt as *const AcpiSdtHeader)).signature);
            ptr::read_unaligned(sig_ptr)
        };
        if sig == *signature {
            return Some(hdr_virt as *const AcpiSdtHeader);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// P15-T008: MADT parsing
// ---------------------------------------------------------------------------

fn parse_madt() {
    let hdr_ptr = match find_table(b"APIC") {
        Some(p) => p,
        None => {
            log::info!("[acpi] MADT (APIC) table not found");
            return;
        }
    };

    let hdr_virt = hdr_ptr as usize;
    if !validate_sdt(hdr_virt) {
        log::warn!("[acpi] MADT checksum invalid");
        return;
    }

    // SAFETY: hdr_ptr points to a valid MADT in identity-mapped memory.
    let header: AcpiSdtHeader = unsafe { ptr::read_unaligned(hdr_ptr) };
    let table_length = header.length as usize;

    // After the SDT header: local_apic_addr (u32) + flags (u32).
    let sdt_size = core::mem::size_of::<AcpiSdtHeader>();
    // SAFETY: reading MADT-specific fields following the SDT header.
    let local_apic_addr = unsafe { ptr::read_unaligned((hdr_virt + sdt_size) as *const u32) };
    let _madt_flags = unsafe { ptr::read_unaligned((hdr_virt + sdt_size + 4) as *const u32) };

    let mut info = MadtInfo {
        local_apic_address: local_apic_addr,
        local_apics: [None; 16],
        local_apic_count: 0,
        io_apic: None,
        irq_overrides: [None; 16],
        irq_override_count: 0,
    };

    // Variable-length entries start at offset sdt_size + 8.
    let entries_start = hdr_virt + sdt_size + 8;
    let entries_end = hdr_virt + table_length;
    let mut offset = entries_start;

    while offset + 2 <= entries_end {
        // SAFETY: reading entry type and length from MADT entry region.
        let entry_type = unsafe { ptr::read_unaligned(offset as *const u8) };
        let entry_len = unsafe { ptr::read_unaligned((offset + 1) as *const u8) } as usize;

        if entry_len < 2 || offset + entry_len > entries_end {
            break;
        }

        match entry_type {
            // Type 0: Processor Local APIC
            0 if entry_len >= 8 && info.local_apic_count < 16 => {
                let entry = LocalApicEntry {
                    acpi_processor_id: unsafe { ptr::read_unaligned((offset + 2) as *const u8) },
                    apic_id: unsafe { ptr::read_unaligned((offset + 3) as *const u8) },
                    flags: unsafe { ptr::read_unaligned((offset + 4) as *const u32) },
                };
                info.local_apics[info.local_apic_count] = Some(entry);
                info.local_apic_count += 1;
            }
            // Type 1: I/O APIC
            1 if entry_len >= 12 => {
                let entry = IoApicEntry {
                    io_apic_id: unsafe { ptr::read_unaligned((offset + 2) as *const u8) },
                    io_apic_address: unsafe { ptr::read_unaligned((offset + 4) as *const u32) },
                    global_system_interrupt_base: unsafe {
                        ptr::read_unaligned((offset + 8) as *const u32)
                    },
                };
                info.io_apic = Some(entry);
            }
            // Type 2: Interrupt Source Override
            2 if entry_len >= 10 && info.irq_override_count < 16 => {
                let entry = IrqSourceOverride {
                    bus: unsafe { ptr::read_unaligned((offset + 2) as *const u8) },
                    source: unsafe { ptr::read_unaligned((offset + 3) as *const u8) },
                    global_system_interrupt: unsafe {
                        ptr::read_unaligned((offset + 4) as *const u32)
                    },
                    flags: unsafe { ptr::read_unaligned((offset + 8) as *const u16) },
                };
                info.irq_overrides[info.irq_override_count] = Some(entry);
                info.irq_override_count += 1;
            }
            _ => { /* skip unknown or too-short entries */ }
        }

        offset += entry_len;
    }

    MADT_INFO.call_once(|| info);
}

// ---------------------------------------------------------------------------
// P15-T009: Minimal FADT parsing
// ---------------------------------------------------------------------------

fn parse_fadt() {
    let hdr_ptr = match find_table(b"FACP") {
        Some(p) => p,
        None => {
            log::info!("[acpi] FADT (FACP) table not found");
            return;
        }
    };

    let hdr_virt = hdr_ptr as usize;
    if !validate_sdt(hdr_virt) {
        log::warn!("[acpi] FADT checksum invalid");
        return;
    }

    // SAFETY: hdr_ptr points to a valid FADT in identity-mapped memory.
    let header: AcpiSdtHeader = unsafe { ptr::read_unaligned(hdr_ptr) };
    let table_length = header.length as usize;

    // IAPC_BOOT_ARCH is at offset 109 from the start of the table (2 bytes).
    const IAPC_BOOT_ARCH_OFFSET: usize = 109;
    if table_length >= IAPC_BOOT_ARCH_OFFSET + 2 {
        // SAFETY: reading IAPC_BOOT_ARCH field within bounds of the FADT.
        let iapc_boot_arch =
            unsafe { ptr::read_unaligned((hdr_virt + IAPC_BOOT_ARCH_OFFSET) as *const u16) };

        let legacy_pic = iapc_boot_arch & 1 != 0;
        if legacy_pic {
            log::info!("[acpi] FADT: legacy 8259 PIC present (IAPC_BOOT_ARCH bit 0 set)");
        } else {
            log::info!("[acpi] FADT: no legacy 8259 PIC indicated");
        }
    } else {
        log::warn!("[acpi] FADT too short for IAPC_BOOT_ARCH field");
    }
}

// ---------------------------------------------------------------------------
// P15-T010: Log discovery results
// ---------------------------------------------------------------------------

fn log_discovery() {
    if let Some(info) = MADT_INFO.get() {
        log::info!(
            "[acpi] CPU count: {}, Local APIC address: {:#x}",
            info.local_apic_count,
            info.local_apic_address
        );

        for i in 0..info.local_apic_count {
            if let Some(ref apic) = info.local_apics[i] {
                log::info!(
                    "[acpi]   CPU {}: APIC ID={}, flags={:#x}",
                    i,
                    apic.apic_id,
                    apic.flags
                );
            }
        }

        if let Some(ref io) = info.io_apic {
            log::info!(
                "[acpi] I/O APIC: id={}, address={:#x}, GSI base={}",
                io.io_apic_id,
                io.io_apic_address,
                io.global_system_interrupt_base
            );
        } else {
            log::warn!("[acpi] No I/O APIC found in MADT");
        }

        for i in 0..info.irq_override_count {
            if let Some(ref ovr) = info.irq_overrides[i] {
                log::info!(
                    "[acpi] IRQ override: bus={}, source={} -> GSI={}, flags={:#x}",
                    ovr.bus,
                    ovr.source,
                    ovr.global_system_interrupt,
                    ovr.flags
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// P15-T001: Public init entry point
// ---------------------------------------------------------------------------

/// Initialize the ACPI subsystem.  Must be called after `mm::init` so that
/// the physical memory offset is available for address translation.
///
/// `rsdp_addr` is the physical address of the RSDP as provided by the
/// bootloader (`boot_info.rsdp_addr`).
pub fn init(rsdp_addr: Option<u64>) {
    let addr = match rsdp_addr {
        Some(a) => PhysAddr::new(a),
        None => {
            log::warn!("[acpi] No RSDP address provided by bootloader");
            return;
        }
    };

    RSDP_ADDR.call_once(|| addr);
    log::info!("[acpi] RSDP at physical address {:#x}", addr.as_u64());

    // Validate RSDP and determine ACPI revision.
    let revision = match validate_rsdp(addr) {
        Some(r) => r,
        None => {
            log::error!("[acpi] RSDP validation failed");
            return;
        }
    };

    log::info!("[acpi] ACPI revision: {}", revision);

    // Parse RSDT or XSDT depending on revision.
    let entries = if revision >= 2 {
        let rsdp_virt = phys_to_virt(addr);
        // SAFETY: addr points to a validated RSDP v2 in identity-mapped memory.
        let v2: RsdpDescriptorV2 =
            unsafe { ptr::read_unaligned(rsdp_virt as *const RsdpDescriptorV2) };
        let xsdt_addr = v2.xsdt_address;
        if xsdt_addr != 0 {
            log::info!("[acpi] Using XSDT at {:#x}", xsdt_addr);
            parse_xsdt(xsdt_addr)
        } else {
            log::info!("[acpi] XSDT address is 0, falling back to RSDT");
            let rsdt_addr = v2.v1.rsdt_address;
            parse_rsdt(rsdt_addr)
        }
    } else {
        let rsdp_virt = phys_to_virt(addr);
        // SAFETY: addr points to a validated RSDP v1 in identity-mapped memory.
        let v1: RsdpDescriptor = unsafe { ptr::read_unaligned(rsdp_virt as *const RsdpDescriptor) };
        let rsdt_addr = v1.rsdt_address;
        log::info!("[acpi] Using RSDT at {:#x}", rsdt_addr);
        parse_rsdt(rsdt_addr)
    };

    match entries {
        Some(e) => {
            log::info!("[acpi] Found {} ACPI tables", e.count);
            // Log each table signature for debugging.
            for i in 0..e.count {
                let sig = unsafe {
                    let hdr = e.entries[i] as *const AcpiSdtHeader;
                    let sig_ptr = core::ptr::addr_of!((*hdr).signature);
                    ptr::read_unaligned(sig_ptr)
                };
                if let Ok(s) = core::str::from_utf8(&sig) {
                    log::info!("[acpi]   table {}: {}", i, s);
                }
            }
            SDT_ENTRIES.call_once(|| e);
        }
        None => {
            log::error!("[acpi] Failed to parse RSDT/XSDT");
            return;
        }
    }

    // Parse individual tables.
    parse_madt();
    parse_fadt();
    log_discovery();
}

// ---------------------------------------------------------------------------
// Public API for later tracks
// ---------------------------------------------------------------------------

/// Returns a reference to the parsed MADT information.
///
/// # Panics
///
/// Panics if `acpi::init` has not been called or the MADT was not found.
#[allow(dead_code)]
pub fn madt_info() -> &'static MadtInfo {
    MADT_INFO.get().expect("MADT not initialized")
}

/// Convenience accessor for the Local APIC base address from the MADT.
///
/// # Panics
///
/// Panics if the MADT has not been parsed.
#[allow(dead_code)]
pub fn local_apic_address() -> u32 {
    madt_info().local_apic_address
}

/// Returns the I/O APIC MMIO base address, if one was found in the MADT.
#[allow(dead_code)]
pub fn io_apic_address() -> Option<u32> {
    MADT_INFO
        .get()
        .and_then(|m| m.io_apic.map(|io| io.io_apic_address))
}

/// Returns the I/O APIC's Global System Interrupt base, or 0 if unknown.
pub fn ioapic_gsi_base() -> u32 {
    MADT_INFO
        .get()
        .and_then(|m| m.io_apic.map(|io| io.global_system_interrupt_base))
        .unwrap_or(0)
}

/// Look up an IRQ source override by its ISA source IRQ number.
#[allow(dead_code)]
pub fn irq_override(irq: u8) -> Option<&'static IrqSourceOverride> {
    let info = MADT_INFO.get()?;
    for i in 0..info.irq_override_count {
        if let Some(ref ovr) = info.irq_overrides[i] {
            // Only match ISA bus (bus 0) overrides — callers assume ISA IRQs.
            if ovr.bus == 0 && ovr.source == irq {
                return Some(ovr);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a physical address to a virtual address using the identity-mapped
/// physical memory offset.
fn phys_to_virt(phys: PhysAddr) -> usize {
    let offset = crate::mm::phys_offset();
    (offset + phys.as_u64()) as usize
}
