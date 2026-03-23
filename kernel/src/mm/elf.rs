//! ELF64 loader for Phase 11 (P11-T001 through P11-T005).
//!
//! Parses ELF64 headers, maps PT_LOAD segments into the current address space
//! with correct page permissions, zeros the BSS region, and allocates a
//! userspace stack with a guard page.
//!
//! No external ELF parsing crate is used; all structures are defined inline.
//!
//! This module is public API for Phase 11 integration; callers are added in a
//! later track (process wiring).  Suppress dead-code lints without weakening
//! -D warnings.
#![allow(dead_code)]

use x86_64::{
    structures::paging::{Mapper, Page, PageTableFlags, Size4KiB},
    VirtAddr,
};

use super::{frame_allocator, paging::GlobalFrameAlloc};

// ---------------------------------------------------------------------------
// ELF64 constants
// ---------------------------------------------------------------------------

const ELFMAG: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1; // little-endian
const EM_X86_64: u16 = 0x3E;

const PT_LOAD: u32 = 1;

// ELF segment flags
const PF_X: u32 = 0x1; // Execute
const PF_W: u32 = 0x2; // Write
                       // PF_R (0x4) is always assumed present; we don't need a named constant.

// Stack layout (P11-T005)
/// Virtual address of the top of the user stack (just below 128 TiB).
pub const ELF_STACK_TOP: u64 = 0x0000_7FFF_FFFF_F000;
/// Number of pages to allocate for the user stack (32 KiB).
const STACK_PAGES: u64 = 8;

// ---------------------------------------------------------------------------
// Raw ELF64 header layout (52+ bytes in little-endian)
// ---------------------------------------------------------------------------
//
// We read fields by byte offset to avoid repr(C) alignment/padding concerns
// in a no_std environment.

// e_ident offsets
const EI_MAG0: usize = 0;
const EI_CLASS: usize = 4;
const EI_DATA: usize = 5;

// Ehdr offsets
const EH_MACHINE: usize = 18; // u16
const EH_ENTRY: usize = 24; // u64
const EH_PHOFF: usize = 32; // u64 — offset of program header table
const EH_PHENTSIZE: usize = 54; // u16
const EH_PHNUM: usize = 56; // u16

const EHDR_SIZE: usize = 64;

// Phdr offsets within a single program header entry
const PH_TYPE: usize = 0; // u32
const PH_FLAGS: usize = 4; // u32
const PH_OFFSET: usize = 8; // u64 — file offset of segment data
const PH_VADDR: usize = 16; // u64
const PH_FILESZ: usize = 32; // u64
const PH_MEMSZ: usize = 40; // u64
const PH_ALIGN: usize = 48; // u64

const PHDR_MIN_SIZE: usize = 56;

// ---------------------------------------------------------------------------
// Helper: read little-endian integers from a byte slice
// ---------------------------------------------------------------------------

fn read_u16_le(data: &[u8], off: usize) -> Option<u16> {
    let b = data.get(off..off + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u64_le(data: &[u8], off: usize) -> Option<u64> {
    let b = data.get(off..off + 8)?;
    Some(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

// ---------------------------------------------------------------------------
// Public error / result types
// ---------------------------------------------------------------------------

/// Error type for ELF loading failures.
#[derive(Debug)]
pub enum ElfError {
    InvalidMagic,
    Not64Bit,
    NotLittleEndian,
    NotX86_64,
    TruncatedHeader,
    TruncatedProgramHeader,
    MappingFailed(&'static str),
    OutOfFrames,
}

/// Result of a successful ELF load.
pub struct LoadedElf {
    /// Virtual address of the entry point.
    pub entry: u64,
    /// Virtual address of the top of the allocated user stack.
    pub stack_top: u64,
}

// ---------------------------------------------------------------------------
// P11-T001: validate and parse the ELF64 Ehdr
// ---------------------------------------------------------------------------

struct Ehdr {
    entry: u64,
    phoff: u64,
    phentsize: u16,
    phnum: u16,
}

fn parse_ehdr(data: &[u8]) -> Result<Ehdr, ElfError> {
    if data.len() < EHDR_SIZE {
        return Err(ElfError::TruncatedHeader);
    }

    // Magic
    if data[EI_MAG0..EI_MAG0 + 4] != ELFMAG {
        return Err(ElfError::InvalidMagic);
    }

    // Class: 64-bit
    if data[EI_CLASS] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }

    // Data encoding: little-endian
    if data[EI_DATA] != ELFDATA2LSB {
        return Err(ElfError::NotLittleEndian);
    }

    // Machine: x86-64
    let machine = read_u16_le(data, EH_MACHINE).ok_or(ElfError::TruncatedHeader)?;
    if machine != EM_X86_64 {
        return Err(ElfError::NotX86_64);
    }

    let entry = read_u64_le(data, EH_ENTRY).ok_or(ElfError::TruncatedHeader)?;
    let phoff = read_u64_le(data, EH_PHOFF).ok_or(ElfError::TruncatedHeader)?;
    let phentsize = read_u16_le(data, EH_PHENTSIZE).ok_or(ElfError::TruncatedHeader)?;
    let phnum = read_u16_le(data, EH_PHNUM).ok_or(ElfError::TruncatedHeader)?;

    Ok(Ehdr {
        entry,
        phoff,
        phentsize,
        phnum,
    })
}

// ---------------------------------------------------------------------------
// P11-T002 / P11-T003 / P11-T004: iterate PT_LOAD segments and map them
// ---------------------------------------------------------------------------

struct Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    #[allow(dead_code)]
    p_align: u64,
}

fn parse_phdr(data: &[u8], base: usize, size: usize) -> Result<Phdr, ElfError> {
    if size < PHDR_MIN_SIZE {
        return Err(ElfError::TruncatedProgramHeader);
    }
    let ph = data
        .get(base..base + size)
        .ok_or(ElfError::TruncatedProgramHeader)?;

    let p_type = read_u32_le(ph, PH_TYPE).ok_or(ElfError::TruncatedProgramHeader)?;
    let p_flags = read_u32_le(ph, PH_FLAGS).ok_or(ElfError::TruncatedProgramHeader)?;
    let p_offset = read_u64_le(ph, PH_OFFSET).ok_or(ElfError::TruncatedProgramHeader)?;
    let p_vaddr = read_u64_le(ph, PH_VADDR).ok_or(ElfError::TruncatedProgramHeader)?;
    let p_filesz = read_u64_le(ph, PH_FILESZ).ok_or(ElfError::TruncatedProgramHeader)?;
    let p_memsz = read_u64_le(ph, PH_MEMSZ).ok_or(ElfError::TruncatedProgramHeader)?;
    let p_align = read_u64_le(ph, PH_ALIGN).ok_or(ElfError::TruncatedProgramHeader)?;

    Ok(Phdr {
        p_type,
        p_flags,
        p_offset,
        p_vaddr,
        p_filesz,
        p_memsz,
        p_align,
    })
}

/// Derive page-table flags from ELF segment flags.
///
/// P11-T003:
/// - Always set `PRESENT | USER_ACCESSIBLE`.
/// - PF_W → add `WRITABLE`.
/// - No PF_X → add `NO_EXECUTE`.
fn segment_flags(p_flags: u32) -> PageTableFlags {
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if p_flags & PF_W != 0 {
        flags |= PageTableFlags::WRITABLE;
    }
    if p_flags & PF_X == 0 {
        flags |= PageTableFlags::NO_EXECUTE;
    }
    flags
}

/// Map a single PT_LOAD segment into the active page table.
///
/// Allocates fresh 4 KiB frames for every page in [p_vaddr, p_vaddr+p_memsz).
/// Copies p_filesz bytes from `data[p_offset..]`, then zeroes the remainder
/// (the BSS region, P11-T004).
///
/// # Safety
/// The caller must hold an exclusive reference to the active page table via
/// `mapper`.  `virt_base` must not already be mapped.
unsafe fn map_load_segment(
    mapper: &mut x86_64::structures::paging::OffsetPageTable<'_>,
    data: &[u8],
    phdr: &Phdr,
) -> Result<(), ElfError> {
    if phdr.p_memsz == 0 {
        return Ok(());
    }

    let vaddr_start = phdr.p_vaddr;
    let vaddr_end = vaddr_start
        .checked_add(phdr.p_memsz)
        .ok_or(ElfError::MappingFailed("segment vaddr overflow"))?;

    // Page-align start and end to determine the set of pages to map.
    let page_start = vaddr_start & !0xFFF;
    let page_end = (vaddr_end + 0xFFF) & !0xFFF;
    let num_pages = (page_end - page_start) / 4096;

    let flags = segment_flags(phdr.p_flags);
    let mut frame_alloc = GlobalFrameAlloc;

    for i in 0..num_pages {
        let vaddr = VirtAddr::new(page_start + i * 4096);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        let frame = frame_allocator::allocate_frame().ok_or(ElfError::OutOfFrames)?;

        // Map the page.
        mapper
            .map_to(page, frame, flags, &mut frame_alloc)
            .map_err(|_| ElfError::MappingFailed("map_to failed for PT_LOAD segment"))?
            .flush();

        // Determine byte range within this page that needs to be initialised.
        // The mapped virtual address is identity-accessible once mapped.
        let page_va_start = page_start + i * 4096;
        let page_va_end = page_va_start + 4096;

        // Byte range within the segment's memory image that this page covers.
        // (relative to vaddr_start of the segment)
        // We need to zero the whole page first, then copy file bytes if any.
        let page_ptr = page_va_start as *mut u8;
        // Zero the entire page.
        core::ptr::write_bytes(page_ptr, 0, 4096);

        // Copy file bytes that fall within this page.
        // File bytes occupy [vaddr_start, vaddr_start + p_filesz).
        let file_end = vaddr_start + phdr.p_filesz;
        // Overlap of [page_va_start, page_va_end) with [vaddr_start, file_end)
        let copy_start = page_va_start.max(vaddr_start);
        let copy_end = page_va_end.min(file_end);

        if copy_start < copy_end {
            let copy_len = (copy_end - copy_start) as usize;
            let file_off = (phdr.p_offset + (copy_start - vaddr_start)) as usize;
            let src = data
                .get(file_off..file_off + copy_len)
                .ok_or(ElfError::TruncatedProgramHeader)?;
            let dst = core::slice::from_raw_parts_mut(copy_start as *mut u8, copy_len);
            dst.copy_from_slice(src);
        }
        // BSS (p_filesz..p_memsz) is already zeroed from the write_bytes above.
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// P11-T005: allocate and map the user stack
// ---------------------------------------------------------------------------

/// Map STACK_PAGES pages for the user stack, leaving one unmapped guard page
/// immediately below the bottom of the stack.
///
/// Stack layout (high addresses at top):
///
/// ```text
/// ELF_STACK_TOP  (0x0000_7FFF_FFFF_F000)   ← stack pointer starts here
/// ...  (STACK_PAGES × 4 KiB mapped pages)
/// stack_bottom  = ELF_STACK_TOP - STACK_PAGES * 4096
/// guard page    = stack_bottom - 4096         ← unmapped, causes #PF on overflow
/// ```
///
/// Returns `ELF_STACK_TOP` as the initial stack pointer.
///
/// # Safety
/// `mapper` must be the active page table and the stack virtual range must not
/// already be mapped.
unsafe fn map_user_stack(
    mapper: &mut x86_64::structures::paging::OffsetPageTable<'_>,
) -> Result<u64, ElfError> {
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    let mut frame_alloc = GlobalFrameAlloc;

    for i in 0..STACK_PAGES {
        // Pages are mapped from (ELF_STACK_TOP - STACK_PAGES*4096) upward.
        let vaddr = VirtAddr::new(ELF_STACK_TOP - STACK_PAGES * 4096 + i * 4096);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        let frame = frame_allocator::allocate_frame().ok_or(ElfError::OutOfFrames)?;

        mapper
            .map_to(page, frame, flags, &mut frame_alloc)
            .map_err(|_| ElfError::MappingFailed("map_to failed for stack page"))?
            .flush();

        // Zero the stack page.
        core::ptr::write_bytes(vaddr.as_mut_ptr::<u8>(), 0, 4096);
    }

    // Guard page is at ELF_STACK_TOP - (STACK_PAGES + 1) * 4096 — intentionally
    // not mapped.  A stack overflow will hit it and cause a page fault.

    Ok(ELF_STACK_TOP)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Load an ELF64 binary from `data` into the current address space.
///
/// Maps all PT_LOAD segments with correct permissions, zeros BSS,
/// allocates a user stack with a guard page, and returns the entry
/// point and stack top.
///
/// # Safety
/// The caller must ensure the current page table is the one that will
/// be active when the loaded process runs. Calling this while another
/// mapper is live causes undefined behavior.
pub unsafe fn load_elf(data: &[u8]) -> Result<LoadedElf, ElfError> {
    // P11-T001: parse and validate the ELF header.
    let ehdr = parse_ehdr(data)?;

    // P11-T002–T004: map all PT_LOAD segments.
    let mut mapper = super::paging::get_mapper();

    let phoff = ehdr.phoff as usize;
    let phentsize = ehdr.phentsize as usize;
    let phnum = ehdr.phnum as usize;

    for i in 0..phnum {
        let base = phoff
            .checked_add(
                i.checked_mul(phentsize)
                    .ok_or(ElfError::TruncatedProgramHeader)?,
            )
            .ok_or(ElfError::TruncatedProgramHeader)?;

        let phdr = parse_phdr(data, base, phentsize)?;

        if phdr.p_type == PT_LOAD {
            map_load_segment(&mut mapper, data, &phdr)?;
        }
    }

    // P11-T005: allocate userspace stack.
    let stack_top = map_user_stack(&mut mapper)?;

    Ok(LoadedElf {
        entry: ehdr.entry,
        stack_top,
    })
}
