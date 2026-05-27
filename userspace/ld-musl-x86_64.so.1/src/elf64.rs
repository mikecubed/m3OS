//! Minimal ELF64 type stubs and dynamic-tag constants shared by the
//! dynamic linker's parser and relocation engine.
//!
//! Only the fields and tags the bring-up linker (Phase 76b) reads are
//! defined here. `DT_GNU_HASH` and the PLT lazy-resolve `DT_DEBUG`
//! plumbing are intentionally omitted — they land in Phase 76d.
//!
//! All structures are `#[repr(C)]` and laid out to match the SysV
//! AMD64 ABI ELF64 LSB encoding so a raw byte cast through
//! `from_bytes` produces a valid view on a little-endian host.

/// One `Elf64_Rela` entry (`sizeof == 24`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rela {
    pub r_offset: u64,
    pub r_info: u64,
    pub r_addend: i64,
}

/// One `Elf64_Dyn` entry (`sizeof == 16`). `d_val` and `d_ptr` share
/// the same 8-byte slot in the ABI; we expose it as `d_val` and let
/// the caller cast when treating it as a pointer.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dyn {
    pub d_tag: i64,
    pub d_val: u64,
}

/// One `Elf64_Sym` entry (`sizeof == 24`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sym {
    pub st_name: u32,
    pub st_info: u8,
    pub st_other: u8,
    pub st_shndx: u16,
    pub st_value: u64,
    pub st_size: u64,
}

/// One `Elf64_Phdr` entry (`sizeof == 56`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

// ---------------------------------------------------------------------------
// Dynamic tags (subset Phase 76b reads).
// ---------------------------------------------------------------------------

pub const DT_NULL: i64 = 0;
pub const DT_NEEDED: i64 = 1;
pub const DT_PLTRELSZ: i64 = 2;
/// `DT_PLTGOT` — address of the Procedure Linkage Table's GOT region.
/// Phase 76d.B4 reads this to install `&_dl_runtime_resolve` at
/// `GOT[2]` and the link-map at `GOT[1]` for each DSO.
pub const DT_PLTGOT: i64 = 3;
pub const DT_HASH: i64 = 4;
pub const DT_STRTAB: i64 = 5;
pub const DT_SYMTAB: i64 = 6;
pub const DT_RELA: i64 = 7;
pub const DT_RELASZ: i64 = 8;
pub const DT_RELAENT: i64 = 9;
pub const DT_STRSZ: i64 = 10;
pub const DT_SYMENT: i64 = 11;
pub const DT_INIT: i64 = 12;
pub const DT_FINI: i64 = 13;
pub const DT_SONAME: i64 = 14;
pub const DT_PLTREL: i64 = 20;
pub const DT_JMPREL: i64 = 23;
pub const DT_INIT_ARRAY: i64 = 25;
pub const DT_FINI_ARRAY: i64 = 26;
pub const DT_INIT_ARRAYSZ: i64 = 27;
pub const DT_FINI_ARRAYSZ: i64 = 28;

// ---------------------------------------------------------------------------
// Phase 76d.D2 — symbol versioning tags.
// ---------------------------------------------------------------------------
pub const DT_VERSYM: i64 = 0x6FFFFFF0;
pub const DT_VERDEF: i64 = 0x6FFFFFFC;
pub const DT_VERDEFNUM: i64 = 0x6FFFFFFD;
pub const DT_VERNEED: i64 = 0x6FFFFFFE;
pub const DT_VERNEEDNUM: i64 = 0x6FFFFFFF;

// ---------------------------------------------------------------------------
// Phase 76d.D1 — GNU hash table tag.
// ---------------------------------------------------------------------------
pub const DT_GNU_HASH: i64 = 0x6FFFFEF5;

// ---------------------------------------------------------------------------
// Program-header types.
// ---------------------------------------------------------------------------

pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_PHDR: u32 = 6;

// ---------------------------------------------------------------------------
// x86_64 relocation types (subset Phase 76b applies).
// ---------------------------------------------------------------------------

pub const R_X86_64_NONE: u32 = 0;
pub const R_X86_64_64: u32 = 1;
pub const R_X86_64_GLOB_DAT: u32 = 6;
pub const R_X86_64_JUMP_SLOT: u32 = 7;
pub const R_X86_64_RELATIVE: u32 = 8;

/// Extract the relocation type from `r_info`. The lower 32 bits of
/// `r_info` are the type; the upper 32 are the symbol-table index.
#[inline]
pub const fn r_type(r_info: u64) -> u32 {
    (r_info & 0xFFFF_FFFF) as u32
}

/// Extract the symbol-table index from `r_info`.
#[inline]
pub const fn r_sym(r_info: u64) -> u32 {
    (r_info >> 32) as u32
}

// ---------------------------------------------------------------------------
// `DT_PLTREL` values (telling the linker whether `DT_JMPREL` is Rel or Rela).
// ---------------------------------------------------------------------------

pub const DT_REL: i64 = 17;
pub const DT_RELA_TYPE: i64 = 7; // `Rela`; matches `DT_RELA`'s d_tag value
