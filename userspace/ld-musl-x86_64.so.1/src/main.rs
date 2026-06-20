//! m3OS dynamic linker (`ld-musl-x86_64.so.1`) — Phase 76b bring-up.
//!
//! The kernel loads this PIE ELF when a binary carries `PT_INTERP`
//! and hands control to `_start` with the SysV-ABI initial stack at
//! `rsp`. From there:
//!
//! 1. [`_start`] is naked-asm. It zeroes `rbp`, passes the stack
//!    pointer in `rdi`, and calls into [`dl_entry`]. Naked entry is
//!    mandatory because the function returns the resolved entry
//!    point in `rax`; the asm `jmp rax` after the call transfers
//!    control to the main binary's `_start` without touching the
//!    initial stack.
//! 2. [`dl_entry`] walks the auxv for `AT_BASE` / `AT_PHDR` /
//!    `AT_PHNUM` / `AT_ENTRY`, runs [`dl_relocate_self`] against the
//!    linker's own image (Phase 76b's transfer-only stub has zero
//!    `R_X86_64_RELATIVE` entries today; the call is still made so
//!    future linker growth is correctness-bounded), parses the main
//!    binary's `PT_DYNAMIC`, loads each `DT_NEEDED` dependency from
//!    `/usr/lib/`, applies the four core relocations, runs any
//!    `DT_INIT` / `DT_INIT_ARRAY` constructors, and returns the main
//!    binary's `AT_ENTRY` value.
//! 3. The naked-asm caller `jmp rax`s to that entry point, leaving
//!    the kernel-built initial stack intact for the main binary's
//!    `_start` to consume.
//!
//! ## Why `dl_entry` is `extern "C"` and not `dlstart_rust`
//!
//! The Phase 76 stub used the name `dlstart_rust`. Phase 76b grows
//! that function into the full bring-up linker entry; the new name
//! `dl_entry` reflects that it is no longer "just the stub". The
//! musl reference linker calls the conceptually equivalent function
//! `__dls2` (after `__dls1` self-relocates); we collapse the two
//! phases because Phase 76b's linker is small enough that the
//! separation has no payoff yet.
//!
//! ## Safety constraints
//!
//! - Until `dl_relocate_self` returns, no code on this path may
//!   touch a Rust global (`static`, `&str` literal in `.rodata` via
//!   a GOT-routed reference, etc.). The Phase 76b ld.so emits zero
//!   `R_X86_64_RELATIVE` entries today because every reference goes
//!   through stack-local data or `static` constants the compiler
//!   resolves at link time as PC-relative (no GOT round-trip). The
//!   `apply_rela_table_self` call is still made so adding new
//!   globals later does not silently break the bring-up.
//! - All inter-function calls in this file are intra-crate, so the
//!   PIE-default code generation emits PC-relative `call rel32`s
//!   instead of GOT-routed `call qword ptr [rip+offset]`s. This is
//!   the property that lets the bring-up linker call into Rust
//!   helpers without first self-relocating those very call sites.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]

mod dl;
pub(crate) mod plt;
pub(crate) mod sym;

use core::arch::naked_asm;
use core::panic::PanicInfo;

use ldso_core::dynlink::{
    DsoId, DynamicSection, LoadedDso, MAX_DSOS, MAX_NEEDED, TopoError, topo_sort,
};
use ldso_core::elf64::{
    DT_NULL, Dyn, PT_DYNAMIC, PT_LOAD, Phdr, R_X86_64_64, R_X86_64_COPY, R_X86_64_DTPMOD64,
    R_X86_64_DTPOFF64, R_X86_64_GLOB_DAT, R_X86_64_IRELATIVE, R_X86_64_JUMP_SLOT,
    R_X86_64_RELATIVE, R_X86_64_TPOFF64, Rela, STT_GNU_IFUNC, Sym, r_sym, r_type, st_type,
};
use ldso_core::reloc::{
    apply_abs64, apply_copy, apply_glob_dat, apply_irelative, apply_relative, apply_tls_word,
};

// ---------------------------------------------------------------------------
// AT_* constants (subset we read).
// ---------------------------------------------------------------------------

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_BASE: u64 = 7;
const AT_ENTRY: u64 = 9;

// ---------------------------------------------------------------------------
// Raw syscalls — Phase 76b's linker cannot link `syscall_lib` because
// `BrkAllocator` would touch the heap before the main binary's
// `_start` has had a chance to set the brk pointer. Inline-asm
// `syscall` keeps the surface to exactly the calls we use.
// ---------------------------------------------------------------------------

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_MMAP: u64 = 9;
const SYS_MPROTECT: u64 = 10;
const SYS_MUNMAP: u64 = 11;
const SYS_EXIT: u64 = 60;

/// `errno` codes the bring-up linker uses as `exit(2)` codes when a
/// DT_NEEDED dependency fails to load. The negative gates (Track
/// F1.4) wait for these specific codes via `WEXITSTATUS(status)`.
const ENOENT_CODE: u64 = 2;
const ELIBBAD_CODE: u64 = 80;

/// Basename used to dedup a `DT_NEEDED` reference to the linker itself.
/// Programs that link `-l:ld-musl-x86_64.so.1` (or that link against a
/// `libdl.so` stub that itself DT_NEEDEDs the linker) end up with this
/// name in `DT_NEEDED`; the bring-up driver recognises it and skips
/// loading the linker a second time (it is already mapped by the
/// kernel via `PT_INTERP` and self-injected into the DSO scope).
const LDSO_BASENAME: &[u8] = b"ld-musl-x86_64.so.1";

const O_RDONLY: u64 = 0;

const PROT_READ: u64 = 0x1;
const PROT_WRITE: u64 = 0x2;
const PROT_EXEC: u64 = 0x4;

const MAP_PRIVATE: u64 = 0x02;
const MAP_ANONYMOUS: u64 = 0x20;

#[inline(always)]
unsafe fn syscall1(num: u64, a1: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") num => ret,
            in("rdi") a1,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall3(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") num => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall6(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") num => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            in("r9") a6,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    ret
}

fn sys_write(fd: i32, buf: &[u8]) -> i64 {
    unsafe { syscall3(SYS_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64) }
}

fn sys_open(path: &[u8]) -> i64 {
    // `path` must be NUL-terminated by the caller.
    unsafe { syscall3(SYS_OPEN, path.as_ptr() as u64, O_RDONLY, 0) }
}

fn sys_close(fd: i64) -> i64 {
    unsafe { syscall1(SYS_CLOSE, fd as u64) }
}

fn sys_read(fd: i64, buf: &mut [u8]) -> i64 {
    unsafe {
        syscall3(
            SYS_READ,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }
}

fn sys_mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: i64, offset: u64) -> i64 {
    unsafe { syscall6(SYS_MMAP, addr, len, prot, flags, fd as u64, offset) }
}

fn sys_mprotect(addr: u64, len: u64, prot: u64) -> i64 {
    unsafe { syscall3(SYS_MPROTECT, addr, len, prot) }
}

pub(crate) fn sys_munmap(addr: u64, len: u64) -> i64 {
    // `addr` must be page-aligned (sys_linux_munmap rejects with
    // -EINVAL otherwise). All mappings unmapped by this file are
    // created by `sys_mmap(addr=0, …)` so the kernel-chosen base is
    // already page-aligned — callers can pass the raw mmap return
    // value. Linux munmap takes only `(addr, len)`; the unused third
    // syscall3 register is ignored by the kernel.
    unsafe { syscall3(SYS_MUNMAP, addr, len, 0) }
}

fn sys_exit(code: u64) -> ! {
    unsafe {
        let _ = syscall1(SYS_EXIT, code);
        core::hint::unreachable_unchecked()
    }
}

// ---------------------------------------------------------------------------
// Observability helpers (stderr / serial).
// ---------------------------------------------------------------------------

pub(crate) fn serial(msg: &[u8]) {
    let _ = sys_write(2, msg);
}

fn serial_hex(mut value: u64) {
    let mut buf = [0u8; 16];
    let mut i = buf.len();
    if value == 0 {
        serial(b"0");
        return;
    }
    while value > 0 {
        let nibble = (value & 0xF) as u8;
        i -= 1;
        buf[i] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        };
        value >>= 4;
    }
    serial(&buf[i..]);
}

// ---------------------------------------------------------------------------
// Self-relocation (Track B1.2).
// ---------------------------------------------------------------------------

/// Walk the linker's own `PT_DYNAMIC`, find its `DT_RELA` table, and
/// apply every `R_X86_64_RELATIVE` against the linker's load image.
/// Other relocation types in the linker's own image are unexpected
/// (the bring-up linker is built `-Bsymbolic`-style with no external
/// symbols) so they trigger a hard fail.
///
/// # Safety
/// `phdr_base`, `phnum`, `load_bias` must all describe the linker's
/// own load image, and the call must occur before any Rust global is
/// dereferenced through a GOT-routed read.
unsafe fn dl_relocate_self(phdr_base: *const Phdr, phnum: usize, load_bias: u64) {
    // Find PT_DYNAMIC for the linker's image. Simultaneously compute
    // the image span so the per-RELA slice view passed into
    // `apply_relative` carries a real bound — Phase 76d.S1.3 routes
    // every write-site through the `ldso_core::reloc` slice helpers
    // so the runtime hits the same alignment + bounds path that the
    // host tests pin.
    let mut dyn_ptr: *const Dyn = core::ptr::null();
    let mut image_end: u64 = 0;
    for i in 0..phnum {
        let ph = unsafe { &*phdr_base.add(i) };
        if ph.p_type == PT_LOAD {
            let end = ph.p_vaddr.wrapping_add(ph.p_memsz);
            if end > image_end {
                image_end = end;
            }
        }
        if ph.p_type == PT_DYNAMIC {
            dyn_ptr = (load_bias.wrapping_add(ph.p_vaddr)) as *const Dyn;
        }
    }
    if dyn_ptr.is_null() {
        return; // No PT_DYNAMIC ⇒ no relocations to apply.
    }
    let image_len = if image_end == 0 {
        0
    } else {
        ((image_end + 4095) & !4095) as usize
    };
    // Walk DT_RELA / DT_RELASZ inline (no allocation, no helper).
    let mut rela: *const Rela = core::ptr::null();
    let mut relasz: u64 = 0;
    let mut p = dyn_ptr;
    loop {
        let entry = unsafe { *p };
        if entry.d_tag == DT_NULL {
            break;
        }
        match entry.d_tag {
            ldso_core::elf64::DT_RELA => {
                rela = (load_bias.wrapping_add(entry.d_val)) as *const Rela;
            }
            ldso_core::elf64::DT_RELASZ => relasz = entry.d_val,
            _ => {}
        }
        p = unsafe { p.add(1) };
    }
    if rela.is_null() || relasz == 0 {
        return;
    }
    let n = (relasz / 24) as usize; // sizeof(Rela) == 24
    for i in 0..n {
        let r = unsafe { *rela.add(i) };
        let t = r_type(r.r_info);
        if t == R_X86_64_RELATIVE {
            // Route through the host-tested `apply_relative` slice
            // helper. The slice covers the whole image; the helper
            // writes 8 bytes at `r.r_offset` and bounds-checks the
            // range internally. The slice is re-borrowed per iteration
            // so it does not overlap with the raw-pointer read of the
            // next RELA above.
            //
            // SAFETY: the linker's own image is mapped RW (text was
            // not yet mprotected R-X — the kernel maps the linker
            // with the same load flags as any PIE), and we are
            // single-threaded. No other code holds a slice into the
            // image while this loop runs.
            let image: &mut [u8] =
                unsafe { core::slice::from_raw_parts_mut(load_bias as *mut u8, image_len) };
            if let Err(_e) = apply_relative(&r, load_bias, image) {
                serial(b"ldso: dl_relocate_self: apply_relative failed\n");
                sys_exit(ELIBBAD_CODE);
            }
        } else {
            // Any other relocation type in the linker's own image is
            // a build-time bug — the bring-up linker is built with
            // `-Bsymbolic`-style flags so only R_X86_64_RELATIVE
            // should appear in its own DT_RELA. Shout via serial and
            // halt: if we kept going we would later read through an
            // unrelocated GOT slot and crash with no diagnostic.
            serial(b"ldso: dl_relocate_self: unexpected relocation type in linker image\n");
            sys_exit(ELIBBAD_CODE);
        }
    }
}

/// Parse the linker's own PT_DYNAMIC and compute its image span so
/// it can be injected into the bring-up DSO scope. Without this, the
/// libdl entry points (`dlopen` / `dlsym` / `dlclose` / `dlerror`)
/// would never resolve through the relocation walker — a stub
/// `libdl.so` exporting the same names would shadow them and break
/// `dlopen`.
///
/// # Safety
/// `phdr_base` / `phnum` / `load_bias` must describe the linker's
/// own mapped image (the kernel hands them off via `AT_BASE` +
/// `e_phoff`). Caller must have run [`dl_relocate_self`] first so
/// any Rust global referenced from this function is already valid.
unsafe fn parse_linker_dso(phdr_base: *const Phdr, phnum: usize, load_bias: u64) -> LoadedDso {
    // Image extent: max(p_vaddr + p_memsz) over PT_LOAD, page-aligned up.
    let mut image_end: u64 = 0;
    let mut dyn_ptr: *const Dyn = core::ptr::null();
    for i in 0..phnum {
        let ph = unsafe { *phdr_base.add(i) };
        if ph.p_type == PT_LOAD {
            let end = ph.p_vaddr.wrapping_add(ph.p_memsz);
            if end > image_end {
                image_end = end;
            }
        }
        if ph.p_type == PT_DYNAMIC {
            dyn_ptr = (load_bias.wrapping_add(ph.p_vaddr)) as *const Dyn;
        }
    }
    let image_len = if image_end == 0 {
        0
    } else {
        (image_end + 4095) & !4095
    };
    if dyn_ptr.is_null() {
        return LoadedDso {
            load_bias,
            image_len,
            dyn_: DynamicSection::empty(),
        };
    }
    let mut entries: heapless::Vec<Dyn, 64> = heapless::Vec::new();
    let mut saw_null = false;
    let mut p = dyn_ptr;
    while entries.len() < 64 {
        let e = unsafe { *p };
        let _ = entries.push(e);
        if e.d_tag == DT_NULL {
            saw_null = true;
            break;
        }
        p = unsafe { p.add(1) };
    }
    if !saw_null {
        serial(b"ldso: linker PT_DYNAMIC > 64 entries\n");
        // Continue with whatever we have; the linker's own dynamic
        // section is always small enough to fit so this branch is
        // a future-proofing failsafe.
    }
    let dyn_ = DynamicSection::parse(&entries, load_bias);
    LoadedDso {
        load_bias,
        image_len,
        dyn_,
    }
}

// ---------------------------------------------------------------------------
// `LoadedDso` lives in `ldso_core::dynlink` so the host harness can
// drive the pure-logic `unmap_dso` helper without invoking real
// syscalls. Phase 76c moved the type out of this binary.
// ---------------------------------------------------------------------------

/// Validate the entry-size and PLT-flavour invariants Phase 76b
/// silently relied on when dividing `relasz`/`pltrelsz` by 24.
/// All three tags are optional — when absent (`0`), the matching
/// table is also absent, so no division occurs. When present, they
/// must match the canonical x86_64 values (`Rela`/`Sym` are 24 bytes
/// and Phase 76b only resolves `DT_RELA`-flavoured PLT entries).
/// Returns a static error string identifying which invariant failed.
fn validate_dyn_invariants(d: &DynamicSection) -> Result<(), &'static str> {
    if d.relaent != 0 && d.relaent != 24 {
        return Err("DT_RELAENT != 24");
    }
    if d.syment != 0 && d.syment != 24 {
        return Err("DT_SYMENT != 24");
    }
    if d.pltrel != 0 && d.pltrel != ldso_core::elf64::DT_RELA {
        return Err("DT_PLTREL != DT_RELA");
    }
    Ok(())
}

/// Validate that every pointer carried by `PT_DYNAMIC` lies inside
/// the DSO's mapped image span `[load_bias, load_bias + image_len)`.
///
/// A malformed (or attacker-controlled) DSO can place arbitrary
/// values in `DT_STRTAB` / `DT_SYMTAB` / `DT_HASH` / `DT_RELA` /
/// `DT_JMPREL` / `DT_INIT` / `DT_INIT_ARRAY`.  Without this check,
/// `strtab_get` / `strlen_bounded` / `lookup_symbol` / `apply_rela` /
/// `run_constructors` would happily dereference pointers pointing
/// into unrelated process memory — leaking data via the serial log
/// or corrupting the heap.
///
/// For sized tags (`strtab + strsz`, `rela + relasz`, `jmprel +
/// pltrelsz`, `init_array + init_arraysz`) the full range must fit.
/// For pointer-only tags (`symtab`, `hash`, `init`) the base must lie
/// inside the span and (for `hash`) at least the 8-byte header
/// (`nbuckets`/`nchain`) must fit.  `init_arraysz`-clear is treated
/// the same way: no upper bound to enforce.
///
/// `image_len == 0` is the placeholder-`LoadedDso` shape — the check
/// is skipped (no bounds known).  Every real-runtime construction
/// site populates `image_len`.
fn validate_dyn_pointers(
    d: &DynamicSection,
    load_bias: u64,
    image_len: u64,
) -> Result<(), &'static str> {
    if image_len == 0 {
        return Ok(());
    }
    let image_end = load_bias.wrapping_add(image_len);
    // Inclusive-base, exclusive-end: the address must satisfy
    // `load_bias <= ptr < image_end`.
    let in_image = |ptr: u64| ptr >= load_bias && ptr < image_end;
    // Sized range: `[base, base + size)` must lie within the image.
    let range_in_image = |base: u64, size: u64| {
        if !in_image(base) {
            return false;
        }
        match base.checked_add(size) {
            Some(end) => end <= image_end,
            None => false,
        }
    };
    if let Some(p) = d.strtab
        && !range_in_image(p.as_ptr() as u64, d.strsz)
    {
        return Err("DT_STRTAB + DT_STRSZ outside image");
    }
    if let Some(p) = d.symtab
        && !in_image(p.as_ptr() as u64)
    {
        return Err("DT_SYMTAB outside image");
    }
    if let Some(p) = d.hash
        && !range_in_image(p.as_ptr() as u64, 8)
    {
        // Need at least the 8-byte (nbuckets,nchain) header in range
        // before lookup_symbol dereferences it.
        return Err("DT_HASH header outside image");
    }
    if let Some(p) = d.gnu_hash
        && !range_in_image(p.as_ptr() as u64, 16)
    {
        // Phase 76d.D1 — need at least the 16-byte (nbuckets,
        // symoffset, bloom_size, bloom_shift) header in range before
        // `sym::lookup_gnu` reads it.
        return Err("DT_GNU_HASH header outside image");
    }
    if let Some(p) = d.pltgot
        && !range_in_image(p.as_ptr() as u64, 24)
    {
        // Phase 76d.B4 — `plt::install_trampoline` writes GOT[1] and
        // GOT[2] via this pointer (offsets +8 and +16 bytes), so the
        // first three `u64` slots must lie inside the DSO image.
        // Without this check a malformed `DT_PLTGOT` could redirect
        // those writes into unrelated process memory.
        return Err("DT_PLTGOT (first 3 slots) outside image");
    }
    // Phase 76d round-7 — alignment hardening. The range checks above
    // prove each pointer lands inside the image but NOT that it carries
    // the natural alignment of the type the lookup/relocation paths read
    // through it. A malformed DSO can supply an in-range yet misaligned
    // pointer; the typed `*const u16` / `*const u32` / `*const u64` /
    // `*const Rela` dereferences in `sym::lookup_gnu` / `lookup_sysv`,
    // `versym_entry`, `consumer_required_version`, `crate::sym_entry`,
    // `plt::install_trampoline`, `plt::resolve_pltrel`, and `apply_rela`
    // would then be Undefined Behaviour even on x86_64 (which tolerates
    // unaligned scalar loads in hardware). Reject misaligned tables here,
    // before any typed pointer is formed. Required alignment is the read
    // width: `DT_VERSYM` reads `u16` (2); `DT_HASH` reads `u32` (4);
    // `DT_GNU_HASH` needs 8 because its bloom array is `u64`; `DT_SYMTAB`
    // (`Elf64_Sym`), `DT_PLTGOT` (`u64` GOT slots), and `DT_RELA` /
    // `DT_JMPREL` (`Elf64_Rela`) all read 8-byte fields, so 8.
    if let Some(p) = d.hash
        && !ldso_core::bounds::is_aligned(p.as_ptr() as u64, 4)
    {
        return Err("DT_HASH misaligned");
    }
    if let Some(p) = d.gnu_hash
        && !ldso_core::bounds::is_aligned(p.as_ptr() as u64, 8)
    {
        return Err("DT_GNU_HASH misaligned");
    }
    if let Some(p) = d.symtab
        && !ldso_core::bounds::is_aligned(p.as_ptr() as u64, 8)
    {
        return Err("DT_SYMTAB misaligned");
    }
    if let Some(p) = d.pltgot
        && !ldso_core::bounds::is_aligned(p.as_ptr() as u64, 8)
    {
        return Err("DT_PLTGOT misaligned");
    }
    if let Some(p) = d.versym
        && !ldso_core::bounds::is_aligned(p.as_ptr() as u64, 2)
    {
        return Err("DT_VERSYM misaligned");
    }
    if let Some(p) = d.rela
        && !ldso_core::bounds::is_aligned(p.as_ptr() as u64, 8)
    {
        return Err("DT_RELA misaligned");
    }
    if let Some(p) = d.jmprel
        && !ldso_core::bounds::is_aligned(p.as_ptr() as u64, 8)
    {
        return Err("DT_JMPREL misaligned");
    }
    if let Some(p) = d.versym
        && !in_image(p.as_ptr() as u64)
    {
        return Err("DT_VERSYM outside image");
    }
    if let Some(p) = d.verdef
        && !in_image(p.as_ptr() as u64)
    {
        return Err("DT_VERDEF outside image");
    }
    if let Some(p) = d.verneed
        && !in_image(p.as_ptr() as u64)
    {
        return Err("DT_VERNEED outside image");
    }
    if let Some(p) = d.rela
        && !range_in_image(p.as_ptr() as u64, d.relasz)
    {
        return Err("DT_RELA + DT_RELASZ outside image");
    }
    if let Some(p) = d.jmprel
        && !range_in_image(p.as_ptr() as u64, d.pltrelsz)
    {
        return Err("DT_JMPREL + DT_PLTRELSZ outside image");
    }
    if let Some(p) = d.init
        && !in_image(p.as_ptr() as u64)
    {
        return Err("DT_INIT outside image");
    }
    if let Some(p) = d.init_array
        && !range_in_image(p.as_ptr() as u64, d.init_arraysz)
    {
        return Err("DT_INIT_ARRAY + DT_INIT_ARRAYSZ outside image");
    }
    // Phase 76c parsed DT_FINI / DT_FINI_ARRAY but the original
    // validation only covered the init side; `run_destructors_for`
    // walks these at dlclose, so they need the same range guard.
    if let Some(p) = d.fini
        && !in_image(p.as_ptr() as u64)
    {
        return Err("DT_FINI outside image");
    }
    if let Some(p) = d.fini_array
        && !range_in_image(p.as_ptr() as u64, d.fini_arraysz)
    {
        return Err("DT_FINI_ARRAY + DT_FINI_ARRAYSZ outside image");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// String-table helpers (raw pointer reads inside the loaded image).
// ---------------------------------------------------------------------------

/// Compute the byte length of a C string. Bounded by `max` to keep
/// the linker from running off into unmapped pages if the string
/// table is corrupt.
unsafe fn strlen_bounded(p: *const u8, max: usize) -> usize {
    let mut i = 0usize;
    while i < max {
        let b = unsafe { *p.add(i) };
        if b == 0 {
            return i;
        }
        i += 1;
    }
    max
}

/// Borrow a string from `DT_STRTAB` at byte offset `off`. The
/// returned slice borrows from the loaded image so the caller must
/// not free or unmap it.
pub(crate) unsafe fn strtab_get(strtab: *const u8, off: u64, strsz: u64) -> &'static [u8] {
    if off >= strsz {
        return &[];
    }
    let p = unsafe { strtab.add(off as usize) };
    let len = unsafe { strlen_bounded(p, (strsz - off) as usize) };
    unsafe { core::slice::from_raw_parts(p, len) }
}

/// Read `DT_SYMTAB[sym_idx]` (a 24-byte `Elf64_Sym`) only when the
/// entry lies inside the DSO image. ELF carries no symbol *count* in
/// `PT_DYNAMIC`, so `validate_dyn_pointers` can only check the symtab
/// *base*; this per-index guard is what keeps reads driven by an
/// untrusted `sym_idx` (from a hash-chain walk or a relocation's
/// `r_info`) inside `load_bias + image_len`. Returns `None` when the
/// computed entry would run past the image; reads unconditionally when
/// `image_len == 0` (placeholder DSO, no bound known). `Sym` is `Copy`
/// (24 bytes) so the entry is returned by value.
pub(crate) unsafe fn sym_entry(
    symtab: *const Sym,
    sym_idx: u32,
    load_bias: u64,
    image_len: u64,
) -> Option<Sym> {
    let entry_size = core::mem::size_of::<Sym>() as u64;
    if image_len > 0
        && !ldso_core::bounds::elem_in_image(
            symtab as u64,
            sym_idx as u64,
            entry_size,
            load_bias,
            image_len,
        )
    {
        return None;
    }
    Some(unsafe { *symtab.add(sym_idx as usize) })
}

// ---------------------------------------------------------------------------
// Open + load one DSO from disk.
// ---------------------------------------------------------------------------

/// Minimal ELF64 header subset.
#[repr(C)]
#[derive(Clone, Copy)]
struct Ehdr {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    _rest: [u8; 6],
}

/// Load a single DSO from `/usr/lib/<name>`.
///
/// Strategy (matched to the m3OS kernel's anonymous-mmap behavior,
/// which ignores `MAP_FIXED` and always allocates linearly from
/// `mmap_next` — see `kernel/src/arch/x86_64/syscall/mod.rs::sys_linux_mmap`):
///
/// 1. Open the file, mmap a scratch buffer, read the entire file in.
/// 2. Parse the ELF header to find `phoff` / `phnum` / the highest
///    `p_vaddr + p_memsz` across all `PT_LOAD` segments.
/// 3. mmap one contiguous anonymous region of `total_image_size`
///    bytes as `PROT_READ | PROT_WRITE`. The kernel picks the address;
///    we use it as the `load_bias`.
/// 4. For each `PT_LOAD`, copy `p_filesz` bytes from the scratch
///    buffer at `p_offset` to `load_bias + p_vaddr`. The
///    `p_memsz - p_filesz` tail is already zero (mmap-zeroed).
/// 5. For each segment whose `p_flags` has `PF_X` set, `mprotect` its
///    page range to `PROT_READ | PROT_EXEC` (Phase 75 W^X requires
///    separate W and X mappings).
///
/// (continues — see [`load_dso`] below.)
const _LOAD_DSO_DOC_BREAK: () = ();

/// Distinguishable error from `load_dso`: `NotFound` lets the
/// caller map a missing DSO to `exit(ENOENT)` while `Other` covers
/// any other failure shape.
#[derive(Clone, Copy)]
enum LoadError {
    NotFound,
    Other(&'static str),
}

/// Phase 93 B.4 — resolve a `DT_NEEDED` soname by searching the standard
/// library directories in order: `/usr/lib` then `/lib`. The shipped
/// `libc.so` lives in `/usr/lib`, but searching `/lib` as a fallback
/// matches the conventional loader search and lets a bare soname resolve
/// predictably. Returns the first successful load; if every directory
/// reports `NotFound`, returns `NotFound`; a non-NotFound error from any
/// attempt (malformed image) is returned immediately.
unsafe fn load_dso_search(name: &[u8]) -> Result<LoadedDso, LoadError> {
    const PREFIXES: [&[u8]; 2] = [b"/usr/lib/", b"/lib/"];
    for prefix in PREFIXES {
        let mut path_buf = [0u8; 256];
        if prefix.len() + name.len() + 1 > path_buf.len() {
            return Err(LoadError::Other("path too long for DT_NEEDED"));
        }
        path_buf[..prefix.len()].copy_from_slice(prefix);
        path_buf[prefix.len()..prefix.len() + name.len()].copy_from_slice(name);
        match unsafe { load_dso(&path_buf) } {
            Ok(d) => return Ok(d),
            Err(LoadError::NotFound) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(LoadError::NotFound)
}

unsafe fn load_dso(path_bytes: &[u8]) -> Result<LoadedDso, LoadError> {
    let fd = sys_open(path_bytes);
    if fd < 0 {
        // m3OS `sys_open` follows the Linux ABI and returns `-errno`
        // on failure. Distinguish ENOENT (the DT_NEEDED file genuinely
        // doesn't exist — caller maps this to `exit(ENOENT)`) from
        // every other open failure (EACCES, EINVAL, ENFILE, etc.) so
        // diagnostics stay accurate. The caller currently exits with
        // ENOENT_CODE on both variants, but a richer error string for
        // `Other` makes serial-log triage tractable.
        if fd == -(ENOENT_CODE as i64) {
            return Err(LoadError::NotFound);
        }
        return Err(LoadError::Other("open failed"));
    }
    let scratch_len: u64 = 64 * 1024;
    let scratch = sys_mmap(
        0,
        scratch_len,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0,
    );
    if scratch < 0 {
        sys_close(fd);
        return Err(LoadError::Other("scratch mmap failed"));
    }
    // The scratch buffer is only needed for header parsing + PT_LOAD
    // copy.  Once `dyn_` is built, every pointer it carries lives in
    // the freshly-mmap'd image, not in scratch.  Delegating to an
    // inner helper lets us munmap the scratch buffer on EVERY return
    // path (success + every early-error path) without scattering the
    // teardown across half a dozen `return Err(…)` sites.
    let result = unsafe { load_dso_impl(fd, scratch as u64, scratch_len) };
    let _ = sys_munmap(scratch as u64, scratch_len);
    result
}

unsafe fn load_dso_impl(fd: i64, scratch: u64, scratch_len: u64) -> Result<LoadedDso, LoadError> {
    let scratch_buf =
        unsafe { core::slice::from_raw_parts_mut(scratch as *mut u8, scratch_len as usize) };
    let mut total = 0usize;
    let mut truncated = false;
    loop {
        let n = sys_read(fd, &mut scratch_buf[total..]);
        if n < 0 {
            sys_close(fd);
            return Err(LoadError::Other("read failed"));
        }
        if n == 0 {
            break;
        }
        total += n as usize;
        if total >= scratch_buf.len() {
            // Buffer is full. Probe for one more byte to distinguish
            // "file is exactly scratch_len" from "file is larger and
            // would be silently truncated". A non-zero return from
            // this read means the file kept going past our buffer —
            // refuse to parse a truncated image.
            let mut probe = [0u8; 1];
            let extra = sys_read(fd, &mut probe);
            truncated = extra > 0;
            break;
        }
    }
    sys_close(fd);
    if truncated {
        return Err(LoadError::Other("DSO larger than 64 KiB scratch"));
    }
    if total < core::mem::size_of::<Ehdr>() {
        return Err(LoadError::Other("file too small"));
    }
    let ehdr = unsafe { &*(scratch as *const Ehdr) };
    if &ehdr.e_ident[..4] != b"\x7fELF" {
        return Err(LoadError::Other("not ELF"));
    }
    // Reject ELFs whose header-size or program-header entry-size
    // don't match what this loader was compiled against.  Without
    // this check a malformed ELF with an unexpected `e_phentsize`
    // would let the loop below treat the PHDR table as an array of
    // `Phdr` even though each entry would actually be wider/narrower,
    // producing mis-parsed PT_LOAD records and out-of-bounds reads.
    if ehdr.e_ehsize as usize != core::mem::size_of::<Ehdr>() {
        return Err(LoadError::Other("e_ehsize != sizeof(Ehdr)"));
    }
    if ehdr.e_phentsize as usize != core::mem::size_of::<Phdr>() {
        return Err(LoadError::Other("e_phentsize != sizeof(Phdr)"));
    }
    let phoff = ehdr.e_phoff;
    let phnum = ehdr.e_phnum as usize;
    // Bounds-check the program-header table against the bytes we
    // actually read. A malformed or truncated ELF must not cause
    // out-of-bounds reads while iterating PHDRs below.
    let phdr_bytes = phnum
        .checked_mul(core::mem::size_of::<Phdr>())
        .ok_or(LoadError::Other("phnum*sizeof(Phdr) overflow"))?;
    let phdr_end = (phoff as usize)
        .checked_add(phdr_bytes)
        .ok_or(LoadError::Other("phoff+phdr_bytes overflow"))?;
    if phdr_end > total {
        return Err(LoadError::Other("PHDR table outside scratch"));
    }
    let phdr_base = (scratch + phoff) as *const Phdr;

    // Pass 1: compute total in-memory image span.
    let mut image_end: u64 = 0;
    for i in 0..phnum {
        let ph = unsafe { *phdr_base.add(i) };
        if ph.p_type == PT_LOAD {
            let end = ph.p_vaddr + ph.p_memsz;
            if end > image_end {
                image_end = end;
            }
        }
    }
    if image_end == 0 {
        return Err(LoadError::Other("no PT_LOAD"));
    }
    let image_len = (image_end + 4095) & !4095;

    // One anonymous mmap for the whole image. The kernel picks the
    // address; the returned value IS our load bias.
    let image_base = sys_mmap(
        0,
        image_len,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0,
    );
    if image_base < 0 {
        return Err(LoadError::Other("image mmap failed"));
    }
    let load_bias = image_base as u64;

    // Pass 2: copy each PT_LOAD into the image, then mprotect text
    // pages to R-X (W^X requires separate W and X mappings).
    for i in 0..phnum {
        let ph = unsafe { *phdr_base.add(i) };
        if ph.p_type != PT_LOAD {
            continue;
        }
        // Bounds-check the file range against the bytes we actually
        // read. A malformed ELF whose PT_LOAD references data past
        // the scratch buffer would otherwise cause an out-of-bounds
        // read during copy_nonoverlapping.
        let seg_end = (ph.p_offset as usize)
            .checked_add(ph.p_filesz as usize)
            .ok_or(LoadError::Other("p_offset+p_filesz overflow"))?;
        if seg_end > total {
            return Err(LoadError::Other("PT_LOAD file range outside scratch"));
        }
        let src = (scratch + ph.p_offset) as *const u8;
        let dst = (load_bias + ph.p_vaddr) as *mut u8;
        unsafe { core::ptr::copy_nonoverlapping(src, dst, ph.p_filesz as usize) };
    }
    for i in 0..phnum {
        let ph = unsafe { *phdr_base.add(i) };
        if ph.p_type != PT_LOAD {
            continue;
        }
        // PF_X = 0x1.
        if ph.p_flags & 0x1 == 0 {
            continue;
        }
        // Page-align the segment.
        let seg_start = (load_bias + ph.p_vaddr) & !4095u64;
        let seg_end = (load_bias + ph.p_vaddr + ph.p_memsz + 4095) & !4095u64;
        let seg_len = seg_end - seg_start;
        // PROT_READ | PROT_EXEC — no PROT_WRITE so Phase 75 W^X is
        // satisfied; the kernel splits the VMA if needed.
        let r = sys_mprotect(seg_start, seg_len, PROT_READ | PROT_EXEC);
        if r < 0 {
            return Err(LoadError::Other("mprotect PT_LOAD R-X failed"));
        }
    }

    // Locate PT_DYNAMIC.
    let mut dyn_ptr: *const Dyn = core::ptr::null();
    for i in 0..phnum {
        let ph = unsafe { *phdr_base.add(i) };
        if ph.p_type == PT_DYNAMIC {
            dyn_ptr = (load_bias + ph.p_vaddr) as *const Dyn;
            break;
        }
    }
    if dyn_ptr.is_null() {
        return Err(LoadError::Other("no PT_DYNAMIC"));
    }
    let mut entries: heapless::Vec<Dyn, 64> = heapless::Vec::new();
    let mut p = dyn_ptr;
    // Track whether we actually observed DT_NULL inside the 64-entry
    // cap. If the loop fills without seeing DT_NULL, the dynamic
    // section is larger than this loader supports and parsing
    // whatever we have would silently drop tags — return an error
    // instead so the caller can fail with ELIBBAD.
    let mut saw_null = false;
    while entries.len() < 64 {
        let e = unsafe { *p };
        let _ = entries.push(e);
        if e.d_tag == DT_NULL {
            saw_null = true;
            break;
        }
        p = unsafe { p.add(1) };
    }
    if !saw_null {
        return Err(LoadError::Other("PT_DYNAMIC too large (>64 entries)"));
    }
    let dyn_ = DynamicSection::parse(&entries, load_bias);
    // Reject DSOs whose dynamic-section pointer tags reference memory
    // outside the mapped image span.  Without this, downstream
    // `strtab_get` / `lookup_symbol` / `apply_rela` /
    // `run_constructors` would dereference attacker-controlled
    // pointers into unrelated process memory.
    if let Err(why) = validate_dyn_pointers(&dyn_, load_bias, image_len) {
        serial(b"ldso: DSO dynamic-pointer bounds check failed: ");
        serial(why.as_bytes());
        serial(b"\n");
        return Err(LoadError::Other("dynamic pointer outside image"));
    }
    Ok(LoadedDso {
        load_bias,
        image_len,
        dyn_,
    })
}

// ---------------------------------------------------------------------------
// Relocation walker (Track B3.1 / B3.2 / B3.3).
// ---------------------------------------------------------------------------

/// Phase 76d.D2.2 — translate the consumer's `versym[sym_idx]` into a
/// required version-name byte slice the per-DSO version-matcher
/// (`sym::dso_version_matches`) can compare against the provider's
/// `DT_VERDEF`.
///
/// Returns:
///   * `None` when the consumer has no `DT_VERSYM` (unversioned —
///     Phase 76b/c semantics, any provider satisfies any request).
///   * `None` when the symbol's version index is the special LOCAL
///     (0) or GLOBAL (1) — those are the unversioned default and any
///     provider satisfies them.
///   * `Some(name)` when the consumer's `DT_VERNEED` records carry a
///     `Vernaux` with matching `vna_other`.
///
/// `load_bias` / `image_len` bound the `versym[sym_idx]` read to the
/// consumer's mapped image: `validate_dyn_pointers` only checks the
/// `DT_VERSYM` base, so a malformed relocation or symbol index could
/// otherwise drive an out-of-image read here. An out-of-range index
/// (or `image_len == 0`, the placeholder shape) yields `None` (no
/// version requirement) — mirrors `sym::versym_entry`.
///
/// # Safety
/// If `versym_ptr` is `Some`, the bytes for `versym[sym_idx]` are read
/// only after the per-index range check against `load_bias +
/// image_len` passes (or unconditionally when `image_len == 0`). The
/// `ver_table.verneed_bytes` slice must reference the consumer's
/// mapped image.
pub(crate) fn consumer_required_version<'a>(
    versym_ptr: Option<*mut u16>,
    sym_idx: u32,
    load_bias: u64,
    image_len: u64,
    ver_table: &ldso_core::ver::VersionTable<'a>,
) -> Option<&'a [u8]> {
    let p = versym_ptr?;
    if image_len > 0
        && !ldso_core::bounds::elem_in_image(p as u64, sym_idx as u64, 2, load_bias, image_len)
    {
        return None;
    }
    let raw = unsafe { *p.add(sym_idx as usize) };
    let version_index = raw & ldso_core::ver::VERSYM_VERSION_MASK;
    if version_index == ldso_core::ver::VER_NDX_LOCAL
        || version_index == ldso_core::ver::VER_NDX_GLOBAL
    {
        return None;
    }
    ver_table.required_version_name_by_index(version_index)
}

/// Phase 93 B.1 — resolve a copy-relocation's *provider*: the first
/// DSO in `dsos` (other than the consumer being relocated) that defines
/// `name`. A `R_X86_64_COPY` symbol is defined in both the consumer (the
/// copy target) and the provider, so a plain `sym::lookup` over the full
/// scope could return the consumer's own definition (a self-copy);
/// excluding the consumer by `load_bias` yields the real provider.
///
/// # Safety
/// Same contract as [`sym::lookup`] — every `LoadedDso`'s populated
/// `dyn_` pointers must reference its mapped image.
unsafe fn lookup_copy_provider(
    dsos: &[LoadedDso],
    consumer_bias: u64,
    name: &[u8],
    version: Option<&[u8]>,
) -> Option<u64> {
    for d in dsos {
        if d.load_bias == consumer_bias {
            continue;
        }
        if let Some(addr) = unsafe { sym::lookup(core::slice::from_ref(d), name, version) } {
            return Some(addr);
        }
    }
    None
}

/// Phase 93 B.3 — emit a one-time note when a `DTPMOD64`/`TPOFF64` TLS
/// relocation is encountered. These are deferred to musl's own
/// `static_init_tls` (see the relocation arm); the note documents that
/// without flooding the serial log if many appear.
fn warn_tls_reloc_once() {
    use core::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        serial(
            b"ldso: note: DTPMOD64/TPOFF64 TLS reloc deferred to libc static_init_tls (loader does not own runtime TLS module assignment)\n",
        );
    }
}

/// Phase 93 B.2 — if `sym` is an `STT_GNU_IFUNC` *definition* (resolved
/// within scope, so its `st_shndx` is non-zero), the resolved `value` is
/// a resolver function whose return value is the real implementation;
/// call it. Otherwise return `value` unchanged. This handles the common
/// in-DSO IFUNC reference reached via `GLOB_DAT`/`JUMP_SLOT`; cross-DSO
/// `STT_GNU_IFUNC` (where the consumer's symbol is `UND`) is the rare
/// case the `R_X86_64_IRELATIVE` path covers directly.
unsafe fn maybe_ifunc_resolve(sym: &Sym, value: u64) -> u64 {
    if st_type(sym.st_info) == STT_GNU_IFUNC && sym.st_shndx != 0 && value != 0 {
        // SAFETY: an STT_GNU_IFUNC definition's address is an
        // executable resolver `extern "C" fn() -> u64`.
        let resolver: extern "C" fn() -> u64 =
            unsafe { core::mem::transmute::<u64, extern "C" fn() -> u64>(value) };
        resolver()
    } else {
        value
    }
}

/// Walk a `Rela` table at `table` of `count` entries and apply each
/// relocation against `dso.load_bias`. Symbol resolution routes
/// through [`sym::lookup`] (Phase 76d.S1.1's unified dispatch) against
/// the full loaded-DSO list.
unsafe fn apply_rela(
    dso: &LoadedDso,
    table: *const Rela,
    count: usize,
    dsos: &[LoadedDso],
) -> Result<(), &'static str> {
    let strtab = match dso.dyn_.strtab {
        Some(p) => p.as_ptr(),
        None => core::ptr::null(),
    };
    let symtab = match dso.dyn_.symtab {
        Some(p) => p.as_ptr(),
        None => core::ptr::null(),
    };
    // Phase 76d.D2.2 — consumer-side version metadata. For each
    // symbol-relocation we read `versym[sym_idx]` and translate via
    // the consumer's `DT_VERNEED` to a required version-name string,
    // which `sym::lookup` then matches against the provider's
    // `DT_VERSYM` + `DT_VERDEF`. DSOs with no `DT_VERSYM` (unversioned
    // consumers) pass `None` and Phase 76b/c behaviour holds.
    let versym_ptr = dso.dyn_.versym.map(|p| p.as_ptr());
    let verneed_bytes: &[u8] = match (dso.dyn_.verneed, dso.dyn_.verneednum) {
        (Some(p), n) if n > 0 && dso.image_len != 0 => {
            let base = p.as_ptr() as u64;
            let image_end = dso.load_bias.saturating_add(dso.image_len);
            let len = image_end.saturating_sub(base) as usize;
            unsafe { core::slice::from_raw_parts(p.as_ptr(), len) }
        }
        _ => &[],
    };
    let strtab_bytes: &[u8] = if strtab.is_null() {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(strtab, dso.dyn_.strsz as usize) }
    };
    let ver_table = ldso_core::ver::VersionTable {
        versym: &[],
        verdef_bytes: &[],
        verdef_num: 0,
        verneed_bytes,
        verneed_num: dso.dyn_.verneednum as usize,
        strtab: strtab_bytes,
    };
    for i in 0..count {
        let r = unsafe { *table.add(i) };
        // Bounds-check `r_offset` against the image span recorded by
        // `load_dso` (or the main-binary loader).  A malformed (or
        // attacker-controlled) DSO whose RELA carries `r_offset >=
        // image_len - 8` would otherwise write 8 bytes outside the
        // mapped image and corrupt unrelated memory.  `image_len == 0`
        // is the placeholder-LoadedDso shape; treating it as "no
        // bounds known" preserves backwards compatibility but the
        // standard runtime construction sites all populate it.
        if dso.image_len != 0 && (r.r_offset > dso.image_len || dso.image_len - r.r_offset < 8) {
            serial(b"ldso: relocation r_offset outside image\n");
            return Err("relocation outside image");
        }
        let rt = r_type(r.r_info);
        match rt {
            R_X86_64_RELATIVE => {
                // Phase 76d.S1.3: route through the host-tested
                // `apply_relative` slice helper so the runtime hits
                // the same alignment + bounds path the host tests
                // pin. The slice covers the whole image; the helper
                // writes 8 bytes at `r.r_offset` and bounds-checks
                // the range internally.
                //
                // SAFETY: `dso.load_bias` and `dso.image_len` came
                // from `load_dso_impl` (or the main-binary loader)
                // and reference a live mmap. We are single-threaded.
                let image: &mut [u8] = unsafe {
                    core::slice::from_raw_parts_mut(
                        dso.load_bias as *mut u8,
                        dso.image_len as usize,
                    )
                };
                if let Err(_e) = apply_relative(&r, dso.load_bias, image) {
                    serial(b"ldso: apply_relative failed\n");
                    return Err("apply_relative failed");
                }
            }
            R_X86_64_GLOB_DAT => {
                if strtab.is_null() || symtab.is_null() {
                    return Err("missing strtab/symtab for sym reloc");
                }
                let sym_idx = r_sym(r.r_info);
                let sym = match unsafe { sym_entry(symtab, sym_idx, dso.load_bias, dso.image_len) }
                {
                    Some(s) => s,
                    None => return Err("symbol index outside image"),
                };
                let name = unsafe { strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
                let version = consumer_required_version(
                    versym_ptr,
                    sym_idx,
                    dso.load_bias,
                    dso.image_len,
                    &ver_table,
                );
                let value = unsafe { sym::lookup(dsos, name, version).unwrap_or(0) };
                if value == 0 {
                    serial(b"ldso: undefined symbol ");
                    serial(name);
                    serial(b"\n");
                    return Err("undefined symbol");
                }
                // Phase 93 B.2 — route an in-DSO STT_GNU_IFUNC reference
                // through its resolver before storing.
                let value = unsafe { maybe_ifunc_resolve(&sym, value) };
                // Phase 76d.S1.3: route through `apply_glob_dat` slice
                // helper. The helper writes 8 bytes at `r.r_offset`
                // and bounds-checks the range internally.
                let image: &mut [u8] = unsafe {
                    core::slice::from_raw_parts_mut(
                        dso.load_bias as *mut u8,
                        dso.image_len as usize,
                    )
                };
                if let Err(_e) = apply_glob_dat(&r, dso.load_bias, value, image) {
                    serial(b"ldso: apply_glob_dat failed\n");
                    return Err("apply_glob_dat failed");
                }
            }
            R_X86_64_JUMP_SLOT => {
                // Phase 76d.B4.4 — JUMP_SLOT is lazy by default once
                // the trampoline is installed. The dispatcher path:
                //
                //   * BIND_NOW=true (Phase 76b/c behaviour, kept as
                //     the initial default) — resolve immediately,
                //     write the absolute address into the GOT slot.
                //     Identical to the GLOB_DAT path above.
                //   * BIND_NOW=false (E4 default) — leave the static
                //     linker's image-relative offset alone but add
                //     `load_bias` so the first call lands on the
                //     PLT's plt0 stub, which jumps to
                //     `_dl_runtime_resolve` via `GOT[2]`.
                //
                // The trampoline target at `GOT[2]` and the link-map
                // at `GOT[1]` are installed by
                // `plt::install_trampoline` after relocations and
                // before constructors run.
                if plt::bind_now_set() {
                    if strtab.is_null() || symtab.is_null() {
                        return Err("missing strtab/symtab for sym reloc");
                    }
                    let sym_idx = r_sym(r.r_info);
                    let sym =
                        match unsafe { sym_entry(symtab, sym_idx, dso.load_bias, dso.image_len) } {
                            Some(s) => s,
                            None => return Err("symbol index outside image"),
                        };
                    let name = unsafe { strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
                    let version = consumer_required_version(
                        versym_ptr,
                        sym_idx,
                        dso.load_bias,
                        dso.image_len,
                        &ver_table,
                    );
                    let value = unsafe { sym::lookup(dsos, name, version).unwrap_or(0) };
                    if value == 0 {
                        serial(b"ldso: undefined symbol ");
                        serial(name);
                        serial(b"\n");
                        return Err("undefined symbol");
                    }
                    // Phase 93 B.2 — resolve an in-DSO STT_GNU_IFUNC.
                    let value = unsafe { maybe_ifunc_resolve(&sym, value) };
                    let image: &mut [u8] = unsafe {
                        core::slice::from_raw_parts_mut(
                            dso.load_bias as *mut u8,
                            dso.image_len as usize,
                        )
                    };
                    if let Err(_e) = apply_glob_dat(&r, dso.load_bias, value, image) {
                        serial(b"ldso: apply_glob_dat (JUMP_SLOT eager) failed\n");
                        return Err("apply_glob_dat failed");
                    }
                } else {
                    // Lazy path — rebase the image-relative offset.
                    unsafe { plt::apply_jmprel_lazy(dso, &r) };
                }
            }
            R_X86_64_64 => {
                if strtab.is_null() || symtab.is_null() {
                    return Err("missing strtab/symtab for sym reloc");
                }
                let sym_idx = r_sym(r.r_info);
                let sym = match unsafe { sym_entry(symtab, sym_idx, dso.load_bias, dso.image_len) }
                {
                    Some(s) => s,
                    None => return Err("symbol index outside image"),
                };
                let name = unsafe { strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
                let version = consumer_required_version(
                    versym_ptr,
                    sym_idx,
                    dso.load_bias,
                    dso.image_len,
                    &ver_table,
                );
                let value = unsafe { sym::lookup(dsos, name, version).unwrap_or(0) };
                if value == 0 {
                    return Err("undefined symbol (R_X86_64_64)");
                }
                // Route through `apply_abs64` against the FULL image
                // slice (matching the RELATIVE / GLOB_DAT arms) so the
                // helper bounds-checks `r.r_offset` against the image
                // before writing. The earlier narrow-8-byte-slice shape
                // applied apply_abs64's bound to a throwaway buffer and
                // then blind-copied to `load_bias + r_offset`, leaving
                // the real write target unbounded on a corrupt reloc.
                let image: &mut [u8] = unsafe {
                    core::slice::from_raw_parts_mut(
                        dso.load_bias as *mut u8,
                        dso.image_len as usize,
                    )
                };
                if let Err(_e) = apply_abs64(&r, dso.load_bias, value, image) {
                    serial(b"ldso: apply_abs64 failed\n");
                    return Err("apply_abs64 failed");
                }
            }
            R_X86_64_COPY => {
                // Phase 93 B.1 — copy `st_size` bytes of a data symbol
                // from its *defining* DSO into this image's BSS. The
                // consumer carries its own definition of the symbol as
                // the copy target, so the provider lookup must skip the
                // consumer's own image (a COPY symbol is defined twice).
                if strtab.is_null() || symtab.is_null() {
                    return Err("missing strtab/symtab for sym reloc");
                }
                let sym_idx = r_sym(r.r_info);
                let sym = match unsafe { sym_entry(symtab, sym_idx, dso.load_bias, dso.image_len) }
                {
                    Some(s) => s,
                    None => return Err("symbol index outside image"),
                };
                let name = unsafe { strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
                let version = consumer_required_version(
                    versym_ptr,
                    sym_idx,
                    dso.load_bias,
                    dso.image_len,
                    &ver_table,
                );
                let src_addr =
                    match unsafe { lookup_copy_provider(dsos, dso.load_bias, name, version) } {
                        Some(a) => a,
                        None => {
                            serial(b"ldso: copy-reloc undefined symbol ");
                            serial(name);
                            serial(b"\n");
                            return Err("copy-reloc undefined symbol");
                        }
                    };
                let size = sym.st_size as usize;
                // SAFETY: `src_addr` is the resolved provider symbol's
                // address inside a mapped DSO; `size` is the symbol's
                // own `st_size`. `apply_copy` bounds-checks the write
                // target against the consumer image.
                let src: &[u8] =
                    unsafe { core::slice::from_raw_parts(src_addr as *const u8, size) };
                let image: &mut [u8] = unsafe {
                    core::slice::from_raw_parts_mut(
                        dso.load_bias as *mut u8,
                        dso.image_len as usize,
                    )
                };
                if let Err(_e) = apply_copy(&r, src, image) {
                    serial(b"ldso: apply_copy failed\n");
                    return Err("apply_copy failed");
                }
            }
            R_X86_64_IRELATIVE => {
                // Phase 93 B.2 — IFUNC. The value at `load_bias +
                // r_addend` is a zero-argument resolver returning the
                // real implementation address; call it and store the
                // result. (musl 1.2.x emits none, but a from-source
                // libc / a lib-dynload `.so` may, and aborting the load
                // on an unrecognized type would break the interpreter.)
                let resolver_addr = dso.load_bias.wrapping_add(r.r_addend as u64);
                // SAFETY: `resolver_addr` is load_bias + an in-image
                // addend, i.e. an executable address inside the DSO; an
                // IFUNC resolver is `extern "C" fn() -> u64`.
                let resolver: extern "C" fn() -> u64 =
                    unsafe { core::mem::transmute::<u64, extern "C" fn() -> u64>(resolver_addr) };
                let resolved = resolver();
                let image: &mut [u8] = unsafe {
                    core::slice::from_raw_parts_mut(
                        dso.load_bias as *mut u8,
                        dso.image_len as usize,
                    )
                };
                if let Err(_e) = apply_irelative(&r, resolved, image) {
                    serial(b"ldso: apply_irelative failed\n");
                    return Err("apply_irelative failed");
                }
            }
            R_X86_64_DTPOFF64 => {
                // Phase 93 B.3 — general-dynamic TLS offset within the
                // module's block. This is `st_value + addend`, which is
                // independent of the runtime module id and thread
                // pointer, so a foreign loader can always write it.
                let sym_idx = r_sym(r.r_info);
                let st_value = if sym_idx == 0 || symtab.is_null() {
                    0
                } else {
                    match unsafe { sym_entry(symtab, sym_idx, dso.load_bias, dso.image_len) } {
                        Some(s) => s.st_value,
                        None => 0,
                    }
                };
                let value = st_value.wrapping_add(r.r_addend as u64);
                let image: &mut [u8] = unsafe {
                    core::slice::from_raw_parts_mut(
                        dso.load_bias as *mut u8,
                        dso.image_len as usize,
                    )
                };
                if let Err(_e) = apply_tls_word(&r, value, image) {
                    serial(b"ldso: apply_tls_word (DTPOFF64) failed\n");
                    return Err("apply_tls_word failed");
                }
            }
            R_X86_64_DTPMOD64 | R_X86_64_TPOFF64 => {
                // Phase 93 B.3 — these need musl's *runtime* TLS module-id
                // / static-TLS-offset assignment, which musl's own
                // libc.so owns via `static_init_tls` (the kernel + loader
                // hand it the auxv; it builds the TCB+DTV and sets the
                // thread pointer itself — the weak `__init_tls` is NOT
                // overridden by m3OS's foreign loader). A foreign loader
                // cannot compute these values, and they do NOT appear in
                // the Phase 93 target artifacts (libc.so has no PT_TLS;
                // the main executable uses local-exec `%fs:` offsets
                // baked at static-link time — verified empirically).
                // Recognize the type so the load does not abort, leaving
                // the slot at its load-time content. Loader-owned
                // general-dynamic TLS across dlopen'd TLS libraries is
                // deferred (would require replacing static_init_tls).
                warn_tls_reloc_once();
            }
            _ => {
                serial(b"ldso: unsupported reloc type ");
                serial_hex(rt as u64);
                serial(b"\n");
                return Err("unsupported reloc");
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Constructors (Track B5.1).
// ---------------------------------------------------------------------------

unsafe fn run_constructors(dsos: &[LoadedDso]) {
    // dsos[0] is the main binary. Deepest-first ⇒ reverse iteration
    // (so deps run before the main binary).
    for dso in dsos.iter().rev() {
        // SAFETY: each DSO's `dyn_` came from a `validate_dyn_pointers`
        // pass at load time, so the function pointers lie inside the
        // mapped image. The destructor convention is `extern "C" fn()`.
        unsafe { run_constructors_for(dso) };
    }
}

// ---------------------------------------------------------------------------
// dl-runtime façade — `pub(crate)` wrappers consumed by `crate::dl`.
// ---------------------------------------------------------------------------

/// Run `DT_INIT` then iterate `DT_INIT_ARRAY` in array order for a
/// single DSO. Used by `dlopen` after a fresh load.
///
/// # Safety
/// `dso` must have been produced by `load_dso` or the bring-up
/// linker — i.e. its `init` / `init_array` pointers lie inside the
/// mapped image. Constructors are called as `extern "C" fn()`.
pub(crate) unsafe fn run_constructors_for(dso: &LoadedDso) {
    if let Some(init) = dso.dyn_.init {
        let f: extern "C" fn() = unsafe { core::mem::transmute(init.as_ptr()) };
        f();
    }
    if let Some(arr) = dso.dyn_.init_array
        && dso.dyn_.init_arraysz >= 8
    {
        let n = (dso.dyn_.init_arraysz / 8) as usize;
        let base = arr.as_ptr() as *const u64;
        for i in 0..n {
            let fnptr = unsafe { *base.add(i) };
            if fnptr != 0 {
                let f: extern "C" fn() = unsafe { core::mem::transmute(fnptr) };
                f();
            }
        }
    }
}

/// Run `DT_FINI_ARRAY` in reverse-array order then `DT_FINI` for a
/// single DSO. Mirrors the SysV ELF destructor pipeline; matches the
/// inverse of `run_constructors_for`.
///
/// The destructor is invoked through a register-loaded function
/// pointer (the `transmute` returns a value-typed `extern "C" fn()`
/// the compiler emits as `call rax`) — not via a GOT slot — because
/// the DSO's GOT may be about to be unmapped by `dlclose`; a
/// GOT-routed indirect call would page-fault on the next dispatch.
///
/// # Safety
/// Same invariant as `run_constructors_for`. The caller must NOT
/// unmap the DSO until after this function returns.
pub(crate) unsafe fn run_destructors_for(dso: &LoadedDso) {
    if let Some(arr) = dso.dyn_.fini_array
        && dso.dyn_.fini_arraysz >= 8
    {
        let n = (dso.dyn_.fini_arraysz / 8) as usize;
        let base = arr.as_ptr() as *const u64;
        // Reverse-array order: index `n - 1` first.
        for i in (0..n).rev() {
            let fnptr = unsafe { *base.add(i) };
            if fnptr != 0 {
                let f: extern "C" fn() = unsafe { core::mem::transmute(fnptr) };
                f();
            }
        }
    }
    if let Some(fini) = dso.dyn_.fini {
        let f: extern "C" fn() = unsafe { core::mem::transmute(fini.as_ptr()) };
        f();
    }
}

/// `dlopen`-flavoured load-from-disk. Mirrors `load_dso` but returns
/// a typed error the caller can map to a `dlerror` slot string.
///
/// # Safety
/// Same invariant as `load_dso` — `path_bytes` must be a
/// NUL-terminated C string.
pub(crate) unsafe fn load_dso_for_dl(path_bytes: &[u8]) -> Result<LoadedDso, dl::DlLoadError> {
    match unsafe { load_dso(path_bytes) } {
        Ok(d) => Ok(d),
        Err(LoadError::NotFound) => Err(dl::DlLoadError::NotFound),
        Err(LoadError::Other(_msg)) => Err(dl::DlLoadError::Other),
    }
}

/// Apply `DT_RELA` and `DT_JMPREL` on `state.dsos[id]` using the
/// state's complete DSO list as the symbol search scope. Used by
/// `dlopen` after a fresh load.
///
/// # Safety
/// `state.dsos[id]` must be a fully-populated `LoadedDso` produced by
/// `load_dso_for_dl`. Symbol resolution dereferences other DSO
/// images through the state's dsos slice — every entry must be valid
/// for the lifetime of this call.
pub(crate) unsafe fn apply_relocations_for(
    id: DsoId,
    state: &dl::DlState,
) -> Result<(), &'static str> {
    let idx = id.0 as usize;
    let dso = state.dsos[idx];
    if let Some(rela) = dso.dyn_.rela {
        let n = (dso.dyn_.relasz / 24) as usize;
        let scope = &state.dsos[..state.n_slots_used];
        unsafe {
            apply_rela(
                &dso,
                rela.as_ptr() as *const ldso_core::elf64::Rela,
                n,
                scope,
            )?
        };
    }
    if let Some(jmprel) = dso.dyn_.jmprel {
        let n = (dso.dyn_.pltrelsz / 24) as usize;
        let scope = &state.dsos[..state.n_slots_used];
        unsafe {
            apply_rela(
                &dso,
                jmprel.as_ptr() as *const ldso_core::elf64::Rela,
                n,
                scope,
            )?
        };
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main bring-up driver.
// ---------------------------------------------------------------------------

/// Walk argc / argv / envp / auxv to extract `AT_BASE`, `AT_PHDR`,
/// `AT_PHNUM`, `AT_ENTRY`. Returns the four values in order.
///
/// # Safety
/// `stack` must be the genuine kernel-built SysV stack.
unsafe fn parse_auxv(stack: *const u64) -> (u64, *const Phdr, usize, u64) {
    let argc = unsafe { *stack } as usize;
    let mut p = unsafe { stack.add(1).add(argc).add(1) };
    while unsafe { *p } != 0 {
        p = unsafe { p.add(1) };
    }
    p = unsafe { p.add(1) };
    let mut at_base = 0u64;
    let mut at_phdr: *const Phdr = core::ptr::null();
    let mut at_phnum = 0usize;
    let mut at_entry = 0u64;
    loop {
        let a_type = unsafe { *p };
        let a_val = unsafe { *p.add(1) };
        if a_type == AT_NULL {
            break;
        }
        match a_type {
            AT_BASE => at_base = a_val,
            AT_PHDR => at_phdr = a_val as *const Phdr,
            AT_PHNUM => at_phnum = a_val as usize,
            AT_ENTRY => at_entry = a_val,
            _ => {}
        }
        p = unsafe { p.add(2) };
    }
    let _ = AT_PHENT; // accepted in input, not yet acted on
    (at_base, at_phdr, at_phnum, at_entry)
}

/// Phase 76d.E4.1 — Walk `envp` for `LD_BIND_NOW`. Returns `true` when
/// the variable is present and its value is neither empty
/// (`LD_BIND_NOW=`) nor the single character `0` (`LD_BIND_NOW=0`);
/// every other non-empty value enables eager binding. So
/// `LD_BIND_NOW=1`, `LD_BIND_NOW=true`, and `LD_BIND_NOW=anything` all
/// enable it, while `LD_BIND_NOW=0` and an empty value disable it.
/// (Note: multi-character forms like `00` are NOT special-cased — only
/// the exact one-byte `0` disables; this matches the "set unless empty
/// or exactly 0" rule, not a full numeric parse.)
///
/// The `=0` carve-out exists because the conventional shell idiom for
/// disabling the flag is `unset LD_BIND_NOW` (which removes it from
/// envp), but a developer who set it once and wants to opt out without
/// clearing the env spelling expects `LD_BIND_NOW=0` to disable.
///
/// # Safety
/// `stack` must be the genuine kernel-built SysV stack.
unsafe fn read_ld_bind_now(stack: *const u64) -> bool {
    // envp starts at `stack + 1 + argc + 1`.
    let argc = unsafe { *stack } as usize;
    let mut p = unsafe { stack.add(1).add(argc).add(1) } as *const *const u8;
    loop {
        let entry = unsafe { *p };
        if entry.is_null() {
            return false;
        }
        // Match "LD_BIND_NOW=" prefix; tolerate truncation. Stop at
        // the first NUL terminator so a shorter env string can never
        // drag the loop past its mapped length, even if a future
        // PREFIX edit ever added embedded NUL bytes.
        const PREFIX: &[u8] = b"LD_BIND_NOW=";
        let mut ok = true;
        for (i, want) in PREFIX.iter().enumerate() {
            let b = unsafe { *entry.add(i) };
            if b == 0 || b != *want {
                ok = false;
                break;
            }
        }
        if ok {
            let val_start = unsafe { entry.add(PREFIX.len()) };
            let first = unsafe { *val_start };
            // Empty value → not set.
            if first == 0 {
                return false;
            }
            // Single-char "0" → off (also "0\0", regardless of trailing).
            if first == b'0' && unsafe { *val_start.add(1) } == 0 {
                return false;
            }
            return true;
        }
        p = unsafe { p.add(1) };
    }
}

/// The bring-up driver. Returns the main binary's `AT_ENTRY` value
/// for the asm caller to `jmp` to. Returns 0 on any unrecoverable
/// error (the asm caller will then `jmp 0` and the kernel reports a
/// page-fault on user code — visible failure mode).
/// Bring-up driver invoked from the naked-asm `_start`.
///
/// # Safety
/// `stack` must be the genuine kernel-built SysV-ABI initial stack
/// passed in `rsp` at process entry. Caller is the linker's own
/// `_start` and never invokes this function from any other context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_entry(stack: *const u64) -> u64 {
    serial(b"ldso(76d): _dlstart\n");
    let (at_base, at_phdr, at_phnum, at_entry) = unsafe { parse_auxv(stack) };
    if at_phdr.is_null() || at_phnum == 0 || at_entry == 0 {
        serial(b"ldso: missing AT_PHDR/AT_PHNUM/AT_ENTRY\n");
        return 0;
    }

    // -- Phase 76d.E4.1 — Read `LD_BIND_NOW` from envp BEFORE any
    // relocation pass runs, so apply_rela's JUMP_SLOT arm sees the
    // correct mode. The flag flips `plt::BIND_NOW` from POSIX-default
    // lazy (false) to eager (true) when the variable is non-empty
    // and non-zero. Track D2.3 also reads `plt::BIND_NOW` for the
    // strict-mode version-mismatch handler.
    let bind_now = unsafe { read_ld_bind_now(stack) };
    if bind_now {
        plt::BIND_NOW.store(true, core::sync::atomic::Ordering::Release);
        serial(b"ldso(76d): LD_BIND_NOW=1 (eager resolve)\n");
    }

    // -- Self-relocation ----------------------------------------------------
    // The linker's own PHDR table is at `at_base + e_phoff`; we know
    // `at_base` from the auxv, and `e_phoff` is at byte 32 of the ELF
    // header. Read those few bytes inline.
    let mut linker_dso = LoadedDso::empty();
    if at_base != 0 {
        let our_ehdr = at_base as *const Ehdr;
        let our_phoff = unsafe { (*our_ehdr).e_phoff };
        let our_phnum = unsafe { (*our_ehdr).e_phnum } as usize;
        let our_phdr = (at_base + our_phoff) as *const Phdr;
        unsafe { dl_relocate_self(our_phdr, our_phnum, at_base) };
        // After self-relocation it is safe to dereference Rust
        // globals. Parse the linker's own PT_DYNAMIC so we can
        // inject the linker into the DSO scope below — that lets
        // `dlopen` / `dlsym` / `dlclose` / `dlerror` resolve to the
        // linker's implementations rather than to any stub library
        // (e.g. `libdl.so`) the consumer linked.
        linker_dso = unsafe { parse_linker_dso(our_phdr, our_phnum, at_base) };
    }

    // -- Walk main binary's PT_DYNAMIC --------------------------------------
    // The kernel sets at_base for the main binary by passing the main
    // binary's load bias (or 0 for non-PIE).  For PIE binaries the
    // load bias must be computed from PT_PHDR vs at_phdr.
    let mut main_phdr_vaddr = 0u64;
    for i in 0..at_phnum {
        let ph = unsafe { *at_phdr.add(i) };
        if ph.p_type == ldso_core::elf64::PT_PHDR {
            main_phdr_vaddr = ph.p_vaddr;
            break;
        }
    }
    let main_load_bias = if main_phdr_vaddr != 0 {
        (at_phdr as u64).wrapping_sub(main_phdr_vaddr)
    } else {
        0
    };

    // Compute the main binary's in-memory image span the same way
    // `load_dso` does for DT_NEEDED libraries — `max(p_vaddr +
    // p_memsz)` over PT_LOAD segments, rounded up to a page.  This
    // populates `LoadedDso.image_len` so `apply_rela`'s bounds check
    // is symmetric for the main binary and its dependencies.  A
    // zero result (no PT_LOAD found) leaves the check disabled for
    // the main binary, which would only happen on a degenerate
    // static binary that this function has already returned through.
    let mut main_image_end: u64 = 0;
    for i in 0..at_phnum {
        let ph = unsafe { *at_phdr.add(i) };
        if ph.p_type == PT_LOAD {
            let end = ph.p_vaddr.wrapping_add(ph.p_memsz);
            if end > main_image_end {
                main_image_end = end;
            }
        }
    }
    let main_image_len = if main_image_end == 0 {
        0
    } else {
        (main_image_end + 4095) & !4095
    };

    // Locate main's PT_DYNAMIC.
    let mut main_dyn: *const Dyn = core::ptr::null();
    for i in 0..at_phnum {
        let ph = unsafe { *at_phdr.add(i) };
        if ph.p_type == PT_DYNAMIC {
            main_dyn = (main_load_bias.wrapping_add(ph.p_vaddr)) as *const Dyn;
            break;
        }
    }
    if main_dyn.is_null() {
        return at_entry; // static binary — just transfer through
    }
    // Build a slice view of main's dynamic section. Same truncation
    // guard as `load_dso_impl`: if the loop fills 64 entries without
    // observing DT_NULL, the binary's dynamic section exceeds what
    // this loader supports and silently truncating would drop tags
    // (causing confusing downstream failures).  Hard-fail with
    // ELIBBAD instead.
    let mut main_entries: heapless::Vec<Dyn, 64> = heapless::Vec::new();
    let mut main_saw_null = false;
    {
        let mut p = main_dyn;
        while main_entries.len() < 64 {
            let e = unsafe { *p };
            let _ = main_entries.push(e);
            if e.d_tag == DT_NULL {
                main_saw_null = true;
                break;
            }
            p = unsafe { p.add(1) };
        }
    }
    if !main_saw_null {
        serial(b"ldso: main binary PT_DYNAMIC > 64 entries\n");
        sys_exit(ELIBBAD_CODE);
    }
    let main_dyn_section = DynamicSection::parse(&main_entries, main_load_bias);
    // Same dynamic-pointer bounds check as `load_dso_impl` runs for
    // every loaded DSO — keep the main binary on the same protection
    // floor so a corrupted main binary's `DT_STRTAB` / `DT_HASH` /
    // etc. cannot trick the linker into reading unrelated memory.
    if let Err(why) = validate_dyn_pointers(&main_dyn_section, main_load_bias, main_image_len) {
        serial(b"ldso: main binary dynamic-pointer bounds check failed: ");
        serial(why.as_bytes());
        serial(b"\n");
        sys_exit(ELIBBAD_CODE);
    }

    // -- Load DT_NEEDED dependencies ----------------------------------------
    let mut dsos: heapless::Vec<LoadedDso, MAX_DSOS> = heapless::Vec::new();
    // Main binary first (index 0) so lookup_symbol resolves to it
    // for self-referential globals (matches SysV scope).
    if dsos
        .push(LoadedDso {
            load_bias: main_load_bias,
            image_len: main_image_len,
            dyn_: main_dyn_section,
        })
        .is_err()
    {
        serial(b"ldso: bring-up: dsos.push(main) failed (MAX_DSOS exhausted)\n");
        sys_exit(ELIBBAD_CODE);
    }
    let strtab_main = match main_dyn_section.strtab {
        Some(p) => p.as_ptr(),
        None => core::ptr::null(),
    };
    // SONAME (or DT_NEEDED name when DT_SONAME is absent) of every
    // loaded DSO, parallel to `dsos`. The main binary's slot is
    // empty (it has no SONAME).
    let mut loaded_names: heapless::Vec<&[u8], MAX_DSOS> = heapless::Vec::new();
    if loaded_names.push(&[]).is_err() {
        serial(b"ldso: bring-up: loaded_names.push(main) failed\n");
        sys_exit(ELIBBAD_CODE);
    }

    // Phase 76c — inject the linker itself as slot 1 so the libdl
    // entry points (`dlopen` / `dlsym` / `dlclose` / `dlerror`)
    // resolve through SysV symbol search rather than via a stub
    // library. The dedup loop below recognises `ld-musl-x86_64.so.1`
    // as the linker's basename so a `DT_NEEDED` carrying that name
    // does not trigger a second load of the linker file from disk.
    if at_base != 0 {
        // Both pushes must succeed atomically; otherwise the parallel
        // `dsos` / `loaded_names` arrays go out of sync and downstream
        // dedup + the bring-up publication loop (which interns the
        // names into `DL_STATE`) silently corrupt the linker's view
        // of the DSO graph.
        if dsos.push(linker_dso).is_err() || loaded_names.push(LDSO_BASENAME).is_err() {
            serial(b"ldso: bring-up: linker self-injection push failed\n");
            sys_exit(ELIBBAD_CODE);
        }
    }
    // Pending DT_NEEDED queue: name + index of the DSO that asked
    // for it (so the graph edge can be recorded).
    let mut queue: heapless::Vec<(&[u8], usize), 64> = heapless::Vec::new();
    if !strtab_main.is_null() {
        for i in 0..main_dyn_section.n_needed as usize {
            let name_off = main_dyn_section.needed[i];
            let name = unsafe { strtab_get(strtab_main, name_off, main_dyn_section.strsz) };
            let _ = queue.push((name, 0));
        }
    }
    // Per-DSO DT_NEEDED → child-index lists, used for cycle detection.
    let mut dep_lists: heapless::Vec<heapless::Vec<DsoId, MAX_DSOS>, MAX_DSOS> =
        heapless::Vec::new();
    for _ in 0..MAX_DSOS {
        let _ = dep_lists.push(heapless::Vec::new());
    }
    let mut qhead = 0usize;
    while qhead < queue.len() {
        let (name, parent_idx) = queue[qhead];
        qhead += 1;
        // Dedup against already-loaded SONAMEs.
        let mut existing = None;
        for (idx, n) in loaded_names.iter().enumerate() {
            if *n == name {
                existing = Some(idx);
                break;
            }
        }
        if let Some(idx) = existing {
            if let Some(slot) = dep_lists.get_mut(parent_idx) {
                let _ = slot.push(DsoId(idx as u32));
            }
            continue;
        }
        // Phase 93 B.4 — search `/usr/lib` then `/lib` for the soname.
        let loaded = match unsafe { load_dso_search(name) } {
            Ok(d) => d,
            Err(LoadError::NotFound) => {
                serial(b"ldso: DT_NEEDED not found: ");
                serial(name);
                serial(b"\n");
                sys_exit(ENOENT_CODE);
            }
            Err(LoadError::Other(msg)) => {
                serial(b"ldso: failed to load DT_NEEDED ");
                serial(name);
                serial(b": ");
                serial(msg.as_bytes());
                serial(b"\n");
                sys_exit(ENOENT_CODE);
            }
        };
        // Resolve the loaded DSO's actual DT_SONAME (if present) so
        // dedup is keyed on the library's self-identified name, not
        // the DT_NEEDED string the parent happened to use. Falls back
        // to the requested DT_NEEDED string when DT_SONAME is absent.
        let display_name: &[u8] = if loaded.dyn_.soname != u64::MAX {
            let strtab_p = match loaded.dyn_.strtab {
                Some(p) => p.as_ptr(),
                None => core::ptr::null(),
            };
            if !strtab_p.is_null() {
                unsafe { strtab_get(strtab_p, loaded.dyn_.soname, loaded.dyn_.strsz) }
            } else {
                name
            }
        } else {
            name
        };
        let new_idx = dsos.len();
        if dsos.push(loaded).is_err() || loaded_names.push(display_name).is_err() {
            serial(b"ldso: too many DSOs\n");
            sys_exit(ELIBBAD_CODE);
        }
        if let Some(slot) = dep_lists.get_mut(parent_idx) {
            let _ = slot.push(DsoId(new_idx as u32));
        }
        // Enqueue this DSO's own DT_NEEDED for transitive resolution.
        let strtab_p = match loaded.dyn_.strtab {
            Some(p) => p.as_ptr(),
            None => core::ptr::null(),
        };
        if !strtab_p.is_null() {
            for k in 0..loaded.dyn_.n_needed as usize {
                let dep_off = loaded.dyn_.needed[k];
                let dep_name = unsafe { strtab_get(strtab_p, dep_off, loaded.dyn_.strsz) };
                if queue.push((dep_name, new_idx)).is_err() {
                    serial(b"ldso: queue overflow\n");
                    sys_exit(ELIBBAD_CODE);
                }
            }
        }
    }
    // Cycle detection via topo_sort on the dep graph.
    {
        let mut slices: heapless::Vec<&[DsoId], MAX_DSOS> = heapless::Vec::new();
        for i in 0..dsos.len() {
            let _ = slices.push(dep_lists[i].as_slice());
        }
        match topo_sort(slices.as_slice()) {
            Ok(_) => {}
            Err(TopoError::CircularDependency(a, b)) => {
                serial(b"ldso: circular DT_NEEDED between ");
                if let Some(n) = loaded_names.get(a.0 as usize) {
                    serial(n);
                }
                serial(b" and ");
                if let Some(n) = loaded_names.get(b.0 as usize) {
                    serial(n);
                }
                serial(b"\n");
                sys_exit(ELIBBAD_CODE);
            }
            Err(_) => {
                serial(b"ldso: dependency-graph overflow\n");
                sys_exit(ELIBBAD_CODE);
            }
        }
    }
    let _ = MAX_NEEDED;
    let _ = DsoId(0);

    // -- Validate entry-size invariants across main + every loaded DSO.
    // The reloc passes below divide `relasz` / `pltrelsz` by 24 and
    // assume DT_PLTREL == DT_RELA. Catch any DSO that breaks those
    // invariants before we trust the resulting entry counts.
    if let Err(why) = validate_dyn_invariants(&main_dyn_section) {
        serial(b"ldso: main DT_* invariant failed: ");
        serial(why.as_bytes());
        serial(b"\n");
        sys_exit(ELIBBAD_CODE);
    }
    for i in 1..dsos.len() {
        if let Err(why) = validate_dyn_invariants(&dsos[i].dyn_) {
            serial(b"ldso: DSO DT_* invariant failed: ");
            serial(why.as_bytes());
            serial(b"\n");
            sys_exit(ELIBBAD_CODE);
        }
    }

    // -- Apply relocations against the main binary --------------------------
    if let Some(rela) = main_dyn_section.rela {
        let n = (main_dyn_section.relasz / 24) as usize;
        let dsos_slice = dsos.as_slice();
        let dso_main = LoadedDso {
            load_bias: main_load_bias,
            image_len: main_image_len,
            dyn_: main_dyn_section,
        };
        if let Err(e) =
            unsafe { apply_rela(&dso_main, rela.as_ptr() as *const Rela, n, dsos_slice) }
        {
            serial(b"ldso: apply_rela (main DT_RELA) failed: ");
            serial(e.as_bytes());
            serial(b"\n");
            return 0;
        }
    }
    if let Some(jmprel) = main_dyn_section.jmprel {
        let n = (main_dyn_section.pltrelsz / 24) as usize;
        let dsos_slice = dsos.as_slice();
        let dso_main = LoadedDso {
            load_bias: main_load_bias,
            image_len: main_image_len,
            dyn_: main_dyn_section,
        };
        if let Err(e) =
            unsafe { apply_rela(&dso_main, jmprel.as_ptr() as *const Rela, n, dsos_slice) }
        {
            serial(b"ldso: apply_rela (main DT_JMPREL) failed: ");
            serial(e.as_bytes());
            serial(b"\n");
            return 0;
        }
    }

    // -- Apply relocations against each loaded DSO --------------------------
    // Slot 1 (when present) is the self-injected linker; its image
    // was already self-relocated by `dl_relocate_self` and applying
    // another reloc pass would double-process every entry. Skip it.
    let n_dsos = dsos.len();
    let linker_slot: Option<usize> = if at_base != 0 { Some(1) } else { None };
    for i in 1..n_dsos {
        if linker_slot == Some(i) {
            continue;
        }
        let dso = dsos[i];
        if let Some(rela) = dso.dyn_.rela {
            let n = (dso.dyn_.relasz / 24) as usize;
            let dsos_slice = dsos.as_slice();
            if let Err(_e) =
                unsafe { apply_rela(&dso, rela.as_ptr() as *const Rela, n, dsos_slice) }
            {
                serial(b"ldso: apply_rela on DSO failed\n");
                return 0;
            }
        }
        if let Some(jmprel) = dso.dyn_.jmprel {
            let n = (dso.dyn_.pltrelsz / 24) as usize;
            let dsos_slice = dsos.as_slice();
            if let Err(_e) =
                unsafe { apply_rela(&dso, jmprel.as_ptr() as *const Rela, n, dsos_slice) }
            {
                serial(b"ldso: apply_rela jmprel on DSO failed\n");
                return 0;
            }
        }
    }

    // -- Phase 76d.B4.3 — Install the PLT lazy-resolve trampoline
    // addresses into each DSO's GOT, BEFORE publication so the
    // post-publication path can read `link_map` from `DL_STATE`.
    //
    // Wait — install_trampoline needs a stable link_map pointer.
    // `DL_STATE.dsos[i]` is the only stable address we have, but
    // publication moves the dsos into that array. So we must install
    // AFTER publication. See the second `install_trampoline` loop
    // below, after the publication block.

    // -- Publish bring-up state into `DL_STATE` so the libdl entry
    // points have something to operate on after handoff.
    //
    // Refcounts on every bring-up DSO are set to `REFCOUNT_PERMANENT`
    // so `dlclose` can never drop them to zero — the main binary,
    // the linker, and any `DT_NEEDED` library loaded at process
    // start are non-evictable for the life of the process.
    {
        let state = dl::dl_state_mut();
        for i in 0..n_dsos {
            state.dsos[i] = dsos[i];
            // Intern bring-up names into linker-owned per-slot storage.
            // The source bytes come from each DSO's mapped strtab,
            // which is permanent for bring-up DSOs, so we could read
            // them directly — but going through `intern_name` keeps
            // every `state.names[i]` slice rooted in `DlState` and
            // removes the implicit-`'static` claim entirely.
            let name = loaded_names.get(i).copied().unwrap_or(&[]);
            state.intern_name(i, name);
            state.refcounts[i] = dl::REFCOUNT_PERMANENT;
            state.in_global_scope[i] = true;
            state.dep_lists[i].clear();
            for &child in dep_lists[i].iter() {
                let _ = state.dep_lists[i].push(child);
            }
        }
        state.n_slots_used = n_dsos;
        state.initialized = true;
    }

    // -- Phase 76d.B4.3 — Install the PLT lazy-resolve trampoline
    // for every DSO that carries a `DT_PLTGOT` (which is every DSO
    // with a PLT). The link-map pointer references the DSO's
    // canonical slot inside `DL_STATE.dsos`, which is now populated
    // and lives for the program's lifetime.
    //
    // The linker itself (slot 1, when self-injected) is skipped: it
    // is built with `-Bsymbolic`-style flags so it has no JUMP_SLOTs
    // and would otherwise install the trampoline against its own
    // GOT, which the kernel mapped via `PT_INTERP` and which we do
    // not re-walk.
    {
        let state = dl::dl_state_mut();
        for i in 0..n_dsos {
            if linker_slot == Some(i) {
                continue;
            }
            let link_map = &state.dsos[i] as *const LoadedDso;
            // SAFETY: `state.dsos[i]` was populated above and lives
            // for the program lifetime; `DT_PLTGOT` was bounds-checked
            // by `validate_dyn_pointers` at load time.
            unsafe { plt::install_trampoline(&state.dsos[i], link_map) };
        }
    }

    // -- Run constructors deepest-first -------------------------------------
    unsafe { run_constructors(&dsos) };

    serial(b"ldso(76c): handoff to main entry=");
    serial_hex(at_entry);
    serial(b"\n");
    at_entry
}

// ---------------------------------------------------------------------------
// Naked entry point. Mirrors the Phase 76 stub's shape but calls
// `dl_entry` (the full bring-up driver) instead of the old
// `dlstart_rust`.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    naked_asm!(
        "xor rbp, rbp",
        "mov rdi, rsp",
        "call {dl_entry}",
        "jmp rax",
        dl_entry = sym dl_entry,
    );
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _dlstart() -> ! {
    naked_asm!("jmp _start");
}

// ---------------------------------------------------------------------------
// Panic handler.
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    serial(b"ldso: PANIC\n");
    let _ = info;
    unsafe {
        core::arch::asm!("ud2", options(noreturn));
    }
}

// Pull a Sym reference so the type is not unused (rustc would warn).
#[allow(dead_code)]
const _SYM_SIZE: usize = core::mem::size_of::<Sym>();
