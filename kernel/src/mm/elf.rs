//! ELF64 loader for Phase 11 (P11-T001 through P11-T005, P11-T010).
//!
//! Parses ELF64 headers, maps PT_LOAD segments into an `OffsetPageTable`
//! with correct page permissions, zeros the BSS region, allocates a
//! userspace stack with a guard page, and builds the System V AMD64 ABI
//! initial stack layout.
//!
//! All writes to freshly allocated frames go through the physical-memory
//! offset (`mm::phys_offset()`), so this module works equally for the
//! currently-active CR3 and for a per-process page table that is not yet
//! loaded into CR3.
//!
//! No external ELF parsing crate is used; all structures are defined inline.
#![allow(dead_code)]

use x86_64::{
    structures::paging::{Mapper, OffsetPageTable, Page, PageTableFlags, Size4KiB, Translate},
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
                       // PF_R (0x4) is always assumed present.

/// Virtual address of the top of the user stack (just below 128 TiB).
pub const ELF_STACK_TOP: u64 = 0x0000_7FFF_FFFF_F000;
/// Number of pages to allocate for the user stack (32 KiB).
pub const STACK_PAGES: u64 = 8;
/// Lower bound for valid userspace virtual addresses (4 MiB, matching Linux).
const USER_VADDR_MIN: u64 = 0x0040_0000;
/// Upper bound (exclusive) for valid userspace virtual addresses (128 TiB canonical boundary).
const USER_VADDR_MAX: u64 = 0x0000_8000_0000_0000;

// ---------------------------------------------------------------------------
// Ehdr offsets (byte-level access to avoid repr(C) padding concerns)
// ---------------------------------------------------------------------------

const EI_MAG0: usize = 0;
const EI_CLASS: usize = 4;
const EI_DATA: usize = 5;

const EH_MACHINE: usize = 18; // u16
const EH_ENTRY: usize = 24; // u64
const EH_PHOFF: usize = 32; // u64
const EH_PHENTSIZE: usize = 54; // u16
const EH_PHNUM: usize = 56; // u16

const EHDR_SIZE: usize = 64;

// Phdr offsets
const PH_TYPE: usize = 0; // u32
const PH_FLAGS: usize = 4; // u32
const PH_OFFSET: usize = 8; // u64
const PH_VADDR: usize = 16; // u64
const PH_FILESZ: usize = 32; // u64
const PH_MEMSZ: usize = 40; // u64
const PH_ALIGN: usize = 48; // u64

const PHDR_MIN_SIZE: usize = 56;

// ---------------------------------------------------------------------------
// Little-endian integer helpers
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

    if data[EI_MAG0..EI_MAG0 + 4] != ELFMAG {
        return Err(ElfError::InvalidMagic);
    }
    if data[EI_CLASS] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }
    if data[EI_DATA] != ELFDATA2LSB {
        return Err(ElfError::NotLittleEndian);
    }

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
    let end = base
        .checked_add(size)
        .ok_or(ElfError::TruncatedProgramHeader)?;
    let ph = data
        .get(base..end)
        .ok_or(ElfError::TruncatedProgramHeader)?;

    Ok(Phdr {
        p_type: read_u32_le(ph, PH_TYPE).ok_or(ElfError::TruncatedProgramHeader)?,
        p_flags: read_u32_le(ph, PH_FLAGS).ok_or(ElfError::TruncatedProgramHeader)?,
        p_offset: read_u64_le(ph, PH_OFFSET).ok_or(ElfError::TruncatedProgramHeader)?,
        p_vaddr: read_u64_le(ph, PH_VADDR).ok_or(ElfError::TruncatedProgramHeader)?,
        p_filesz: read_u64_le(ph, PH_FILESZ).ok_or(ElfError::TruncatedProgramHeader)?,
        p_memsz: read_u64_le(ph, PH_MEMSZ).ok_or(ElfError::TruncatedProgramHeader)?,
        p_align: read_u64_le(ph, PH_ALIGN).ok_or(ElfError::TruncatedProgramHeader)?,
    })
}

/// Derive page-table flags from ELF segment flags (P11-T003).
///
/// - Always sets `PRESENT | USER_ACCESSIBLE`.
/// - `PF_W` → adds `WRITABLE`.
/// - No `PF_X` → adds `NO_EXECUTE`.
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

/// Map a single PT_LOAD segment (P11-T002, T003, T004).
///
/// Allocates fresh frames, maps them in `mapper`, zeroes them, then copies
/// the file bytes in.  All writes go through `phys_off + frame.phys_addr`
/// so the function works for any `mapper` — including one for a page table
/// that is **not** currently loaded into CR3.
///
/// # Safety
/// `mapper` must own exclusive access to its PML4. The virtual range
/// `[phdr.p_vaddr, phdr.p_vaddr + phdr.p_memsz)` must not already be mapped.
unsafe fn map_load_segment(
    mapper: &mut OffsetPageTable<'_>,
    phys_off: u64,
    data: &[u8],
    phdr: &Phdr,
) -> Result<(), ElfError> {
    if phdr.p_memsz == 0 {
        return Ok(());
    }

    // Reject malformed segments where the file image claims to be larger than
    // the memory region — would write past the mapped range.
    if phdr.p_filesz > phdr.p_memsz {
        return Err(ElfError::MappingFailed("p_filesz > p_memsz"));
    }
    let file_image_end = phdr
        .p_offset
        .checked_add(phdr.p_filesz)
        .ok_or(ElfError::TruncatedProgramHeader)?;
    if file_image_end > data.len() as u64 {
        return Err(ElfError::TruncatedProgramHeader);
    }

    let vaddr_start = phdr.p_vaddr;
    let vaddr_end = vaddr_start
        .checked_add(phdr.p_memsz)
        .ok_or(ElfError::MappingFailed("segment vaddr overflow"))?;

    // Reject segments outside the canonical userspace range — prevents
    // a malicious ELF from creating USER_ACCESSIBLE mappings in the
    // kernel upper half or at the null page.
    if vaddr_start < USER_VADDR_MIN || vaddr_end > USER_VADDR_MAX {
        return Err(ElfError::MappingFailed("segment vaddr outside user range"));
    }

    let page_start = vaddr_start & !0xFFF;
    // Use checked_add to guard against overflow when vaddr_end is near u64::MAX.
    let page_end = vaddr_end
        .checked_add(0xFFF)
        .ok_or(ElfError::MappingFailed("page_end overflow"))?
        & !0xFFF;
    let num_pages = (page_end - page_start) / 4096;

    let flags = segment_flags(phdr.p_flags);
    let mut frame_alloc = GlobalFrameAlloc;

    for i in 0..num_pages {
        let page_va_start = page_start + i * 4096;
        let vaddr = VirtAddr::new(page_va_start);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        let frame = frame_allocator::allocate_frame().ok_or(ElfError::OutOfFrames)?;

        // Map the page; use ignore() since mapper may not be the current CR3.
        mapper
            .map_to(page, frame, flags, &mut frame_alloc)
            .map_err(|_| ElfError::MappingFailed("map_to failed for PT_LOAD segment"))?
            .ignore();

        // Write to the physical frame via the physical-memory offset.
        // This is valid regardless of which CR3 is active.
        let frame_ptr = (phys_off + frame.start_address().as_u64()) as *mut u8;

        // Zero the entire frame first (covers BSS, P11-T004).
        core::ptr::write_bytes(frame_ptr, 0, 4096);

        // Copy file bytes that fall within this page.
        let page_va_end = page_va_start + 4096;
        let file_end = vaddr_start + phdr.p_filesz;
        let copy_start = page_va_start.max(vaddr_start);
        let copy_end = page_va_end.min(file_end);

        if copy_start < copy_end {
            let copy_len = (copy_end - copy_start) as usize;
            let file_off = usize::try_from(
                phdr.p_offset
                    .checked_add(copy_start - vaddr_start)
                    .ok_or(ElfError::TruncatedProgramHeader)?,
            )
            .map_err(|_| ElfError::TruncatedProgramHeader)?;
            let file_end = file_off
                .checked_add(copy_len)
                .ok_or(ElfError::TruncatedProgramHeader)?;
            let src = data
                .get(file_off..file_end)
                .ok_or(ElfError::TruncatedProgramHeader)?;
            // Offset within the frame.
            let frame_off = (copy_start - page_va_start) as usize;
            let dst = core::slice::from_raw_parts_mut(frame_ptr.add(frame_off), copy_len);
            dst.copy_from_slice(src);
        }
        // BSS portion already zeroed by write_bytes.
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// P11-T005: allocate and map the user stack
// ---------------------------------------------------------------------------

/// Map `STACK_PAGES` pages for the user stack plus one unmapped guard page
/// (P11-T005).
///
/// All frame writes go through `phys_off`, so this works for any `mapper`.
///
/// # Safety
/// `mapper` must have exclusive access to its PML4; the stack range must be
/// unmapped.
unsafe fn map_user_stack(mapper: &mut OffsetPageTable<'_>, phys_off: u64) -> Result<u64, ElfError> {
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    let mut frame_alloc = GlobalFrameAlloc;

    for i in 0..STACK_PAGES {
        let vaddr = VirtAddr::new(ELF_STACK_TOP - STACK_PAGES * 4096 + i * 4096);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        let frame = frame_allocator::allocate_frame().ok_or(ElfError::OutOfFrames)?;

        mapper
            .map_to(page, frame, flags, &mut frame_alloc)
            .map_err(|_| ElfError::MappingFailed("map_to failed for stack page"))?
            .ignore();

        // Zero via physical offset.
        let frame_ptr = (phys_off + frame.start_address().as_u64()) as *mut u8;
        core::ptr::write_bytes(frame_ptr, 0, 4096);
    }

    // Guard page = ELF_STACK_TOP - (STACK_PAGES + 1) * 4096 — intentionally
    // left unmapped; a stack overflow causes a page fault here.

    Ok(ELF_STACK_TOP)
}

// ---------------------------------------------------------------------------
// P11-T010 / P11-T011: System V AMD64 ABI initial stack layout
// ---------------------------------------------------------------------------

/// Write the System V AMD64 ABI initial stack layout into a mapped stack.
///
/// Layout written (growing downward from `stack_top`):
/// ```text
/// [argv strings, null-terminated, packed]
/// 8-byte alignment padding
/// NULL  (end of aux vector)
/// NULL  (end of envp — minimal empty environment, P11-T011)
/// NULL  (end of argv)
/// argv[argc-1] .. argv[0]  (virtual pointers)
/// argc                     ← returned rsp
/// ```
///
/// `mapper` is used to translate the virtual stack addresses to physical
/// frames so writes are performed via the physical-memory offset — valid
/// regardless of the currently-active CR3.
///
/// Returns the new RSP value (virtual address of `argc`) or an error if any
/// stack address is unmapped.
///
/// # Safety
/// The stack pages `[stack_top - STACK_PAGES*4096, stack_top)` must already
/// be mapped in `mapper`.
pub unsafe fn setup_abi_stack(
    stack_top: u64,
    mapper: &OffsetPageTable<'_>,
    phys_off: u64,
    argv: &[&[u8]],
) -> Result<u64, ElfError> {
    // Helper: translate a virtual address in the target page table to a kernel
    // writable pointer via the physical-memory offset.
    let virt_to_kptr = |vaddr: u64| -> Result<*mut u8, ElfError> {
        use x86_64::structures::paging::mapper::TranslateResult;
        match mapper.translate(VirtAddr::new(vaddr)) {
            TranslateResult::Mapped { frame, offset, .. } => {
                Ok((phys_off + frame.start_address().as_u64() + offset) as *mut u8)
            }
            _ => Err(ElfError::MappingFailed(
                "setup_abi_stack: unmapped stack address",
            )),
        }
    };

    // Write strings starting just below stack_top, packing downward.
    let mut cursor: u64 = stack_top;
    let mut arg_ptrs: alloc::vec::Vec<u64> = alloc::vec::Vec::new();

    for arg in argv.iter().rev() {
        // Include the null terminator.
        let len = arg.len() + 1;
        cursor -= len as u64;
        // Write the string bytes (null-terminated).
        for (j, &b) in arg.iter().enumerate() {
            let kptr = virt_to_kptr(cursor + j as u64)?;
            kptr.write(b);
        }
        let kptr = virt_to_kptr(cursor + arg.len() as u64)?;
        kptr.write(0); // null terminator
        arg_ptrs.push(cursor);
    }
    arg_ptrs.reverse(); // put argv[0] first

    // Align cursor down to 8 bytes.
    cursor &= !7;

    // Now build the pointer table growing downward:
    // aux NULL, envp NULL, argv NULLs + pointers, argc

    // Aux vector: two NULLs (AT_NULL = 0, value = 0).
    cursor -= 8;
    let kptr = virt_to_kptr(cursor)?;
    (kptr as *mut u64).write(0); // AT_NULL value
    cursor -= 8;
    let kptr = virt_to_kptr(cursor)?;
    (kptr as *mut u64).write(0); // AT_NULL type

    // envp: NULL terminator only (P11-T011: minimal empty environment).
    cursor -= 8;
    let kptr = virt_to_kptr(cursor)?;
    (kptr as *mut u64).write(0);

    // argv: NULL terminator, then pointers in reverse order.
    cursor -= 8;
    let kptr = virt_to_kptr(cursor)?;
    (kptr as *mut u64).write(0); // argv[argc] = NULL
    for &ptr in arg_ptrs.iter().rev() {
        cursor -= 8;
        let kptr = virt_to_kptr(cursor)?;
        (kptr as *mut u64).write(ptr);
    }

    // SysV AMD64 ABI: RSP at `_start` must be 8 mod 16.
    // After placing argc (–8 bytes), cursor must satisfy `cursor % 16 == 8`.
    // So before the argc write, cursor must be `0 mod 16`.
    if !cursor.is_multiple_of(16) {
        cursor -= 8; // add one 8-byte alignment pad (already zeroed by map_user_stack)
    }

    // argc.
    cursor -= 8;
    let kptr = virt_to_kptr(cursor)?;
    (kptr as *mut u64).write(argv.len() as u64);

    // Return rsp pointing at argc.
    Ok(cursor)
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Load an ELF64 binary from `data` into `mapper`.
///
/// This is the core loader used by both `load_elf` (active CR3) and
/// `execve` / `fork` (new per-process page table).  All physical writes go
/// through `phys_off` so the function works whether `mapper` references the
/// current CR3 or a not-yet-active per-process PML4.
///
/// # Safety
/// `mapper` must have exclusive access to its PML4 and `phys_off` must be
/// the correct physical-memory offset for this machine.
pub unsafe fn load_elf_into(
    mapper: &mut OffsetPageTable<'_>,
    phys_off: u64,
    data: &[u8],
) -> Result<LoadedElf, ElfError> {
    let ehdr = parse_ehdr(data)?;

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
            map_load_segment(mapper, phys_off, data, &phdr)?;
        }
    }

    let stack_top = map_user_stack(mapper, phys_off)?;

    Ok(LoadedElf {
        entry: ehdr.entry,
        stack_top,
    })
}

/// Load an ELF64 binary into the currently-active address space.
///
/// Convenience wrapper around [`load_elf_into`] that obtains the active
/// mapper via `paging::get_mapper()`.
///
/// # Safety
/// No other `OffsetPageTable` over the current CR3 may be alive at the
/// same time.
pub unsafe fn load_elf(data: &[u8]) -> Result<LoadedElf, ElfError> {
    let phys_off = super::phys_offset();
    let mut mapper = super::paging::get_mapper();
    load_elf_into(&mut mapper, phys_off, data)
}
