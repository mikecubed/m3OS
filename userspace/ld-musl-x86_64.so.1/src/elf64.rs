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
/// `DT_RPATH` (legacy) / `DT_RUNPATH` — colon-separated library search paths
/// (offsets into `DT_STRTAB`), supporting `$ORIGIN` expansion. Phase 95b reads
/// these so a binary like rust-lld (`RUNPATH=$ORIGIN/../lib`) can locate its
/// bundled `libLLVM.so` outside the default `/usr/lib`+`/lib` search.
pub const DT_RPATH: i64 = 15;
pub const DT_RUNPATH: i64 = 29;
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
/// `PT_TLS` — the thread-local-storage template segment. Phase 93 B.3 reads
/// the main executable's `PT_TLS` to lay out the static TLS block beneath the
/// thread pointer (x86_64 variant II).
pub const PT_TLS: u32 = 7;

// ---------------------------------------------------------------------------
// x86_64 relocation types (subset Phase 76b applies).
// ---------------------------------------------------------------------------

pub const R_X86_64_NONE: u32 = 0;
pub const R_X86_64_64: u32 = 1;
/// `R_X86_64_COPY` (Phase 93 B.1) — copy `st_size` bytes of a data
/// symbol from its defining DSO into the relocated image's BSS. Used
/// when an executable copy-relocates a libc data object for legacy
/// interposition.
pub const R_X86_64_COPY: u32 = 5;
pub const R_X86_64_GLOB_DAT: u32 = 6;
pub const R_X86_64_JUMP_SLOT: u32 = 7;
pub const R_X86_64_RELATIVE: u32 = 8;
/// `R_X86_64_DTPMOD64` (Phase 93 B.3) — general-dynamic TLS module id.
pub const R_X86_64_DTPMOD64: u32 = 16;
/// `R_X86_64_DTPOFF64` (Phase 93 B.3) — general-dynamic TLS offset
/// within the module's TLS block (`st_value + addend`; module-id and
/// thread-pointer independent, so a foreign loader can always write it).
pub const R_X86_64_DTPOFF64: u32 = 17;
/// `R_X86_64_TPOFF64` (Phase 93 B.3) — initial-exec TLS offset relative
/// to the thread pointer.
pub const R_X86_64_TPOFF64: u32 = 18;
/// `R_X86_64_IRELATIVE` (Phase 93 B.2) — IFUNC: the value at
/// `load_bias + r_addend` is a resolver function; its return value is
/// written into the relocated slot.
pub const R_X86_64_IRELATIVE: u32 = 37;

/// Symbol type `STT_GNU_IFUNC` — stored in the low nibble of `st_info`.
/// A symbol of this type is resolved by *calling* it (it returns the
/// real implementation address) rather than using its `st_value`.
pub const STT_GNU_IFUNC: u8 = 10;

/// Symbol binding `STB_WEAK` — stored in the high nibble of `st_info`.
/// A relocation against a **weak** undefined symbol that resolves nowhere
/// is satisfied by writing 0 (the consumer guards `if (sym) sym();`), not
/// a hard undefined-symbol error. GCC's crt objects emit weak refs like
/// `_ITM_registerTMCloneTable` / `__gmon_start__` that real libc never
/// provides.
pub const STB_WEAK: u8 = 2;

/// Extract the symbol *type* (low nibble) from an `Elf64_Sym::st_info`.
#[inline]
pub const fn st_type(st_info: u8) -> u8 {
    st_info & 0x0F
}

/// Extract the symbol *binding* (high nibble) from an `Elf64_Sym::st_info`.
#[inline]
pub const fn st_bind(st_info: u8) -> u8 {
    st_info >> 4
}

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

// ---------------------------------------------------------------------------
// `DT_RELR` — compact relative-relocation table (the `.relr.dyn` section).
// Modern linkers (lld, recent binutils with `-z pack-relative-relocs`) emit
// every `R_X86_64_RELATIVE` here instead of `DT_RELA` to shrink the relocation
// table. Each `DT_RELRENT`-sized (8) entry is either an address word (LSB=0)
// or a bitmap word (LSB=1) per the SysV RELR encoding; the apply loop lives in
// `crate::reloc::apply_relr`.
// ---------------------------------------------------------------------------

pub const DT_RELRSZ: i64 = 35;
pub const DT_RELR: i64 = 36;
pub const DT_RELRENT: i64 = 37;
