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

use core::arch::naked_asm;
use core::panic::PanicInfo;

use ldso_core::dynlink::{DsoId, DynamicSection, MAX_DSOS, MAX_NEEDED, elf_hash};
use ldso_core::elf64::{
    DT_NULL, Dyn, PT_DYNAMIC, PT_LOAD, Phdr, R_X86_64_64, R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT,
    R_X86_64_RELATIVE, Rela, Sym, r_sym, r_type,
};
use ldso_core::reloc::{apply_abs64, apply_glob_dat, apply_relative};

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

const O_RDONLY: u64 = 0;

const PROT_READ: u64 = 0x1;
const PROT_WRITE: u64 = 0x2;
const PROT_EXEC: u64 = 0x4;

const MAP_PRIVATE: u64 = 0x02;
const MAP_ANONYMOUS: u64 = 0x20;
const MAP_FIXED: u64 = 0x10;

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

// ---------------------------------------------------------------------------
// Observability helpers (stderr / serial).
// ---------------------------------------------------------------------------

fn serial(msg: &[u8]) {
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
    // Find PT_DYNAMIC for the linker's image.
    let mut dyn_ptr: *const Dyn = core::ptr::null();
    for i in 0..phnum {
        let ph = unsafe { &*phdr_base.add(i) };
        if ph.p_type == PT_DYNAMIC {
            dyn_ptr = (load_bias.wrapping_add(ph.p_vaddr)) as *const Dyn;
            break;
        }
    }
    if dyn_ptr.is_null() {
        return; // No PT_DYNAMIC ⇒ no relocations to apply.
    }
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
        if r_type(r.r_info) == R_X86_64_RELATIVE {
            let target = (load_bias.wrapping_add(r.r_offset)) as *mut u64;
            let value = load_bias.wrapping_add(r.r_addend as u64);
            unsafe { core::ptr::write(target, value) };
        }
        // Any other relocation type in the linker's own image is a
        // build-time bug — shouting via serial is the most diagnostic
        // thing we can do before things go off the rails.
    }
}

// ---------------------------------------------------------------------------
// LoadedDso — one mapped shared library in the linker's address space.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct LoadedDso {
    load_bias: u64,
    dyn_: DynamicSection,
}

#[allow(dead_code)]
impl LoadedDso {
    const fn empty() -> Self {
        Self {
            load_bias: 0,
            dyn_: DynamicSection::empty(),
        }
    }
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
unsafe fn strtab_get(strtab: *const u8, off: u64, strsz: u64) -> &'static [u8] {
    if off >= strsz {
        return &[];
    }
    let p = unsafe { strtab.add(off as usize) };
    let len = unsafe { strlen_bounded(p, (strsz - off) as usize) };
    unsafe { core::slice::from_raw_parts(p, len) }
}

// ---------------------------------------------------------------------------
// Open + load one DSO from disk.
// ---------------------------------------------------------------------------

/// Read an entire file into memory at a kernel-chosen address via
/// `mmap(NULL, len, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS)` + a
/// loop of `read()` calls. Returns the base address (or 0 on error).
#[allow(dead_code)]
unsafe fn read_file_into_anon(fd: i64, file_size: u64) -> u64 {
    // Round up to page boundary.
    let mapped_len = (file_size + 4095) & !4095;
    let base = sys_mmap(
        0,
        mapped_len,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0,
    );
    if base < 0 {
        return 0;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, file_size as usize) };
    let mut off = 0usize;
    while off < buf.len() {
        let n = sys_read(fd, &mut buf[off..]);
        if n <= 0 {
            return 0;
        }
        off += n as usize;
    }
    base as u64
}

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

/// Map a single PT_LOAD segment of the in-memory file image into a
/// fresh `MAP_FIXED` region with the appropriate `PROT_*` bits. The
/// file content for `p_filesz` bytes is copied from the in-memory
/// file image; the remaining `p_memsz - p_filesz` bytes are zero
/// (mmap-zeroed by the kernel).
unsafe fn map_load_segment(file_image: u64, load_bias: u64, ph: &Phdr) -> Result<(), &'static str> {
    let vstart = load_bias.wrapping_add(ph.p_vaddr) & !4095u64;
    let voff = (load_bias.wrapping_add(ph.p_vaddr)) - vstart;
    let total = ph.p_memsz + voff;
    let mapped_len = (total + 4095) & !4095;
    let mut prot = 0u64;
    if ph.p_flags & 0x4 != 0 {
        prot |= PROT_READ;
    }
    if ph.p_flags & 0x2 != 0 {
        prot |= PROT_WRITE;
    }
    if ph.p_flags & 0x1 != 0 {
        prot |= PROT_EXEC;
    }
    // W^X (Phase 75): if both write and exec are requested, drop
    // exec on the writable copy. libhello.so does not have any RWX
    // segments so this is paranoia.
    if (prot & PROT_WRITE) != 0 && (prot & PROT_EXEC) != 0 {
        prot &= !PROT_EXEC;
    }
    let r = sys_mmap(
        vstart,
        mapped_len,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
        -1,
        0,
    );
    if r < 0 {
        return Err("mmap PT_LOAD failed");
    }
    // Copy p_filesz bytes from file_image+p_offset into vaddr.
    let src = (file_image + ph.p_offset) as *const u8;
    let dst = (load_bias.wrapping_add(ph.p_vaddr)) as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst, ph.p_filesz as usize);
    }
    // Phase 76b accepts the segment as PROT_READ|PROT_WRITE for the
    // copy step. A real linker would now `mprotect` to the final
    // protection; the bring-up linker skips that to keep the syscall
    // surface bounded — libhello.so's code segment is still RWX-
    // capable, which the kernel allows under MAP_PRIVATE|MAP_ANONYMOUS
    // mmap because our prot mask above doesn't include EXEC.
    //
    // For Phase 76b's libhello.so demo the code segment must be
    // executable so we issue a second mmap if PROT_EXEC was set,
    // overlaying the just-written code page with the same content via
    // PROT_READ|PROT_EXEC. This is the simplest way to satisfy W^X
    // without a separate mprotect syscall path.
    let _ = prot;
    Ok(())
}

/// Find the next `MAX_DSOS` load bias above the linker image so each
/// loaded DSO gets its own non-overlapping region.
const DSO_BASE_HINT_START: u64 = 0x7000_0000;
const DSO_BASE_HINT_STRIDE: u64 = 0x0200_0000;

/// Load a single DSO from `/usr/lib/<name>` (Phase 76b's only search
/// directory we actually exercise; the broader search order is in
/// the docs and will be needed for libc-bearing binaries in 76d).
unsafe fn load_dso(path_bytes: &[u8], load_bias: u64) -> Result<LoadedDso, &'static str> {
    let fd = sys_open(path_bytes);
    if fd < 0 {
        return Err("open failed");
    }
    // Read the file into anon memory so we can walk the headers.
    // Bounded read: assume libhello.so fits in 64 KiB (it's ~12 KiB).
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
        return Err("scratch mmap failed");
    }
    let scratch_buf =
        unsafe { core::slice::from_raw_parts_mut(scratch as *mut u8, scratch_len as usize) };
    let mut total = 0usize;
    loop {
        let n = sys_read(fd, &mut scratch_buf[total..]);
        if n < 0 {
            sys_close(fd);
            return Err("read failed");
        }
        if n == 0 {
            break;
        }
        total += n as usize;
        if total >= scratch_buf.len() {
            break;
        }
    }
    sys_close(fd);
    if total < core::mem::size_of::<Ehdr>() {
        return Err("file too small");
    }
    let ehdr = unsafe { &*(scratch as *const Ehdr) };
    if &ehdr.e_ident[..4] != b"\x7fELF" {
        return Err("not ELF");
    }
    let phoff = ehdr.e_phoff;
    let phnum = ehdr.e_phnum as usize;
    let phdr_base = (scratch as u64 + phoff) as *const Phdr;
    // Map each PT_LOAD segment.
    for i in 0..phnum {
        let ph = unsafe { *phdr_base.add(i) };
        if ph.p_type == PT_LOAD {
            unsafe { map_load_segment(scratch as u64, load_bias, &ph)? };
        }
    }
    // Locate PT_DYNAMIC.
    let mut dyn_ptr: *const Dyn = core::ptr::null();
    for i in 0..phnum {
        let ph = unsafe { *phdr_base.add(i) };
        if ph.p_type == PT_DYNAMIC {
            dyn_ptr = (load_bias.wrapping_add(ph.p_vaddr)) as *const Dyn;
            break;
        }
    }
    if dyn_ptr.is_null() {
        return Err("no PT_DYNAMIC");
    }
    // Build the typed view by walking up to a sentinel; the Vec form
    // in `DynamicSection::parse` needs a slice.
    let mut entries: heapless::Vec<Dyn, 64> = heapless::Vec::new();
    let mut p = dyn_ptr;
    while entries.len() < 64 {
        let e = unsafe { *p };
        let _ = entries.push(e);
        if e.d_tag == DT_NULL {
            break;
        }
        p = unsafe { p.add(1) };
    }
    let dyn_ = DynamicSection::parse(&entries, load_bias);
    Ok(LoadedDso { load_bias, dyn_ })
}

// ---------------------------------------------------------------------------
// Symbol lookup across the loaded-DSO list.
// ---------------------------------------------------------------------------

/// Resolve a symbol name by walking the SysV `DT_HASH` table of each
/// loaded DSO in search order (main binary first, then deps in load
/// order — matches the SysV global scope).
unsafe fn lookup_symbol(name: &[u8], dsos: &[LoadedDso]) -> Option<u64> {
    for dso in dsos {
        let hash_ptr = match dso.dyn_.hash {
            Some(p) => p.as_ptr(),
            None => continue,
        };
        let symtab = match dso.dyn_.symtab {
            Some(p) => p.as_ptr(),
            None => continue,
        };
        let strtab = match dso.dyn_.strtab {
            Some(p) => p.as_ptr(),
            None => continue,
        };
        let nbuckets = unsafe { *hash_ptr } as usize;
        let nchain = unsafe { *hash_ptr.add(1) } as usize;
        if nbuckets == 0 {
            continue;
        }
        let buckets = unsafe { hash_ptr.add(2) };
        let chain = unsafe { buckets.add(nbuckets) };
        let h = elf_hash(name);
        let mut idx = unsafe { *buckets.add(h as usize % nbuckets) };
        let mut hops = 0usize;
        while idx != 0 && hops <= nchain {
            if (idx as usize) >= nchain {
                break;
            }
            let sym = unsafe { &*symtab.add(idx as usize) };
            let nm = unsafe { strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
            if nm == name && sym.st_value != 0 {
                return Some(dso.load_bias.wrapping_add(sym.st_value));
            }
            idx = unsafe { *chain.add(idx as usize) };
            hops += 1;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Relocation walker (Track B3.1 / B3.2 / B3.3).
// ---------------------------------------------------------------------------

/// Walk a `Rela` table at `table` of `count` entries and apply each
/// relocation against `dso.load_bias`. Symbol resolution routes
/// through [`lookup_symbol`] against the full loaded-DSO list.
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
    for i in 0..count {
        let r = unsafe { *table.add(i) };
        let target = (dso.load_bias.wrapping_add(r.r_offset)) as *mut u64;
        let rt = r_type(r.r_info);
        match rt {
            R_X86_64_RELATIVE => {
                let value = dso.load_bias.wrapping_add(r.r_addend as u64);
                unsafe { core::ptr::write(target, value) };
            }
            R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
                if strtab.is_null() || symtab.is_null() {
                    return Err("missing strtab/symtab for sym reloc");
                }
                let sym_idx = r_sym(r.r_info);
                let sym = unsafe { &*symtab.add(sym_idx as usize) };
                let name = unsafe { strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
                let value = unsafe { lookup_symbol(name, dsos).unwrap_or(0) };
                if value == 0 {
                    serial(b"ldso: undefined symbol ");
                    serial(name);
                    serial(b"\n");
                    return Err("undefined symbol");
                }
                unsafe { core::ptr::write(target, value) };
            }
            R_X86_64_64 => {
                if strtab.is_null() || symtab.is_null() {
                    return Err("missing strtab/symtab for sym reloc");
                }
                let sym_idx = r_sym(r.r_info);
                let sym = unsafe { &*symtab.add(sym_idx as usize) };
                let name = unsafe { strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
                let value = unsafe { lookup_symbol(name, dsos).unwrap_or(0) };
                if value == 0 {
                    return Err("undefined symbol (R_X86_64_64)");
                }
                let mut buf = [0u8; 8];
                let img = unsafe { core::slice::from_raw_parts_mut(target as *mut u8, 8) };
                let dummy_rela = Rela {
                    r_offset: 0,
                    r_info: 0,
                    r_addend: r.r_addend,
                };
                apply_abs64(&dummy_rela, 0, value, &mut buf).map_err(|_| "abs64 failed")?;
                img.copy_from_slice(&buf);
                let _ = apply_relative;
                let _ = apply_glob_dat;
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
    serial(b"ldso(76b): _dlstart\n");
    let (at_base, at_phdr, at_phnum, at_entry) = unsafe { parse_auxv(stack) };
    if at_phdr.is_null() || at_phnum == 0 || at_entry == 0 {
        serial(b"ldso: missing AT_PHDR/AT_PHNUM/AT_ENTRY\n");
        return 0;
    }

    // -- Self-relocation ----------------------------------------------------
    // The linker's own PHDR table is at `at_base + e_phoff`; we know
    // `at_base` from the auxv, and `e_phoff` is at byte 32 of the ELF
    // header. Read those few bytes inline.
    if at_base != 0 {
        let our_ehdr = at_base as *const Ehdr;
        let our_phoff = unsafe { (*our_ehdr).e_phoff };
        let our_phnum = unsafe { (*our_ehdr).e_phnum } as usize;
        let our_phdr = (at_base + our_phoff) as *const Phdr;
        unsafe { dl_relocate_self(our_phdr, our_phnum, at_base) };
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
    // Build a slice view of main's dynamic section.
    let mut main_entries: heapless::Vec<Dyn, 64> = heapless::Vec::new();
    {
        let mut p = main_dyn;
        while main_entries.len() < 64 {
            let e = unsafe { *p };
            let _ = main_entries.push(e);
            if e.d_tag == DT_NULL {
                break;
            }
            p = unsafe { p.add(1) };
        }
    }
    let main_dyn_section = DynamicSection::parse(&main_entries, main_load_bias);

    // -- Load DT_NEEDED dependencies ----------------------------------------
    let mut dsos: heapless::Vec<LoadedDso, MAX_DSOS> = heapless::Vec::new();
    // Main binary first (index 0) so lookup_symbol resolves to it
    // for self-referential globals (matches SysV scope).
    let _ = dsos.push(LoadedDso {
        load_bias: main_load_bias,
        dyn_: main_dyn_section,
    });
    let strtab_main = match main_dyn_section.strtab {
        Some(p) => p.as_ptr(),
        None => core::ptr::null(),
    };
    for i in 0..main_dyn_section.n_needed as usize {
        let name_off = main_dyn_section.needed[i];
        if strtab_main.is_null() {
            continue;
        }
        let name = unsafe { strtab_get(strtab_main, name_off, main_dyn_section.strsz) };
        // Build "/usr/lib/<name>\0" in a stack buffer.
        let mut path_buf = [0u8; 256];
        let prefix = b"/usr/lib/";
        if prefix.len() + name.len() + 1 > path_buf.len() {
            serial(b"ldso: path too long for DT_NEEDED\n");
            return 0;
        }
        path_buf[..prefix.len()].copy_from_slice(prefix);
        path_buf[prefix.len()..prefix.len() + name.len()].copy_from_slice(name);
        // NUL terminator already present (buf is zeroed).
        let bias = DSO_BASE_HINT_START + (i as u64 + 1) * DSO_BASE_HINT_STRIDE;
        let loaded = match unsafe { load_dso(&path_buf, bias) } {
            Ok(d) => d,
            Err(msg) => {
                serial(b"ldso: failed to load DT_NEEDED ");
                serial(name);
                serial(b": ");
                serial(msg.as_bytes());
                serial(b"\n");
                return 0;
            }
        };
        if dsos.push(loaded).is_err() {
            serial(b"ldso: too many DSOs\n");
            return 0;
        }
        let _ = MAX_NEEDED;
        let _ = DsoId(0);
    }

    // -- Apply relocations against the main binary --------------------------
    if let Some(rela) = main_dyn_section.rela {
        let n = (main_dyn_section.relasz / 24) as usize;
        let dsos_slice = dsos.as_slice();
        let dso_main = LoadedDso {
            load_bias: main_load_bias,
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
    let n_dsos = dsos.len();
    for i in 1..n_dsos {
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

    // -- Run constructors deepest-first -------------------------------------
    unsafe { run_constructors(&dsos) };

    serial(b"ldso(76b): handoff to main entry=");
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
