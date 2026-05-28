//! Phase 76d.B4 — PLT lazy-resolution trampoline.
//!
//! ## Why lazy resolution exists
//!
//! The Phase 76b/c bring-up linker resolves every `R_X86_64_JUMP_SLOT`
//! at load time (the "eager" path). That works for the smoke gates
//! but does not scale: a real program linking against a multi-MiB
//! `libc.so` would pay the symbol-resolution cost for hundreds of
//! functions it never calls. The SysV ELF PLT/GOT design exists to
//! defer that cost — each function call goes through a tiny stub
//! that the loader patches on first invocation, so resolution
//! amortises across the program's lifetime.
//!
//! ## Trampoline ABI contract
//!
//! On first call to a PLT-routed function `f`:
//!
//! 1. caller does `call f@plt`, pushing a return address.
//! 2. `f@plt` reads its GOT slot. On first call it still points back
//!    to `f@plt`'s `push reloc_index` instruction (set up by the
//!    static linker, rebased by `apply_jmprel_lazy` below). On
//!    subsequent calls the GOT slot is the resolved address and the
//!    trampoline is bypassed.
//! 3. `f@plt` pushes `reloc_index` (an immediate that uniquely
//!    identifies which `DT_JMPREL` entry this PLT entry corresponds to)
//!    and `jmp plt0`.
//! 4. `plt0` pushes `link_map` (from `GOT[1]`, set by
//!    [`install_trampoline`] to point at a stable `LoadedDso`) and
//!    `jmp *GOT[2]` (which holds `&_dl_runtime_resolve`, also set by
//!    [`install_trampoline`]).
//!
//! On entry to `_dl_runtime_resolve`, the stack is:
//!
//! ```text
//! [rsp + 16]: caller's return address
//! [rsp +  8]: reloc_index   (from plt entry — pushed first)
//! [rsp +  0]: link_map      (from plt0 — pushed last, on top)
//! ```
//!
//! (plt0 pushes link_map AFTER the plt entry pushed reloc_index, so
//! link_map is closer to `rsp` than reloc_index.)
//!
//! and `rsp` is offset `+8 (mod 16)` from 16-byte alignment because:
//!
//! ```text
//! caller: rsp == 16-aligned
//! call f@plt   ; rsp -= 8 → rsp == +8 mod 16
//! push reloc_index ; rsp -= 8 → rsp == 0 mod 16
//! push link_map    ; rsp -= 8 → rsp == +8 mod 16
//! jmp _dl_runtime_resolve
//! ```
//!
//! [`_dl_runtime_resolve`] saves the caller-saved **general-purpose**
//! argument registers (`rax`, `rcx`, `rdx`, `rsi`, `rdi`, `r8`–`r11`)
//! so the original caller's arguments survive, passes `link_map` in
//! `rdi` and `reloc_index` in `rsi`, calls [`resolve_pltrel`], stores
//! the resolved address over the link-map slot on the stack, restores
//! those registers, discards the now-unused `reloc_index` slot, and
//! `ret`s — which pops the resolved address into `rip`. The caller's
//! return address remains on the stack so the resolved function's own
//! `ret` returns to the caller normally.
//!
//! XMM registers are also caller-saved on the SysV x86_64 ABI and
//! would normally need preserving across the resolver call, but m3OS
//! builds the whole tree with `-mmx,-sse` (see CLAUDE.md "Target
//! flags") so no float/vector arguments are ever passed in XMM. The
//! trampoline therefore deliberately preserves only the GPR argument
//! set; if SIMD is ever re-enabled this must be extended to spill
//! `xmm0`–`xmm7`.
//!
//! ## Why naked
//!
//! The asm must NEVER let the compiler insert a function prologue
//! (the caller's argument registers must survive verbatim) or pad
//! the stack (the `[rsp + N]` offsets are exact). `#[naked]` plus
//! `naked_asm!` enforces both invariants.

use core::sync::atomic::{AtomicBool, Ordering};

use ldso_core::dynlink::LoadedDso;
use ldso_core::elf64::{R_X86_64_JUMP_SLOT, Rela, r_sym, r_type};

use crate::dl;
use crate::sym;
use crate::{serial, sys_exit};

/// `BIND_NOW` master flag. POSIX default is **lazy** (the trampoline
/// path resolves each PLT entry on first call); the smoke gates
/// validate that both lazy and eager paths produce the same observable
/// behaviour. E4 reads `LD_BIND_NOW` from `envp` at linker startup
/// and flips this to `true` when the variable is set to a non-zero
/// value, restoring Phase 76b/c eager behaviour.
///
/// The flag uses `AtomicBool` rather than `static mut bool` so the
/// load/store sites are explicit and we do not rely on the
/// single-threaded invariant of `DlState` (the trampoline path
/// runs on every PLT-routed call site and is hotter than the libdl
/// surface).
pub static BIND_NOW: AtomicBool = AtomicBool::new(false);

/// `true` if the active mode is "resolve every JUMP_SLOT eagerly at
/// load time". The trampoline can still be installed even in eager
/// mode (it is just never reached); the cost is two stores per DSO.
pub fn bind_now_set() -> bool {
    BIND_NOW.load(Ordering::Acquire)
}

/// Naked-asm trampoline reached on first call to a lazily-resolved
/// PLT-routed symbol. See the module-level docs for the ABI.
///
/// # Safety
/// Only reachable from a PLT entry's plt0 stub. Direct invocation
/// from Rust would skip the stack-state contract and corrupt the
/// caller's frame.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _dl_runtime_resolve() -> ! {
    core::arch::naked_asm!(
        // Save all caller-saved registers so the original caller's
        // arguments survive the resolver call. Order matters for the
        // [rsp + N] math below.
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        // Load PLT-pushed args from above the saved-reg block.
        // plt0's `push [GOT[1]]` (link_map) was the LAST push before
        // the trampoline ran, so at entry link_map was at [rsp+0]
        // and reloc_index was at [rsp+8]. After 9 register pushes
        // (72 bytes), link_map is at [rsp+72] and reloc_index at
        // [rsp+80].
        "mov rdi, [rsp + 72]",        // link_map → arg 1
        "mov rsi, [rsp + 80]",        // reloc_index → arg 2
        // Alignment check: entry rsp was +8 mod 16; after 9 pushes
        // (72 bytes) rsp is 0 mod 16, which is exactly what the SysV
        // call instruction expects (call pushes 8 bytes to give the
        // callee +8 mod 16 at entry). No padding push needed.
        "call {resolve_pltrel}",
        // rax = resolved function address. Overwrite the
        // reloc_index slot (at [rsp+80] in the saved-state) with it.
        // After the 9 pops below the resolved address ends up at
        // [rsp+8]; the subsequent `add rsp, 8` collapses the stale
        // link_map slot off the top, leaving resolved_addr at
        // [rsp+0] for the final `ret` to consume.
        "mov [rsp + 80], rax",
        // Restore caller-saved registers in reverse order.
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",
        // Discard the (now stale) link_map slot. After this, the
        // resolved address is at [rsp+0] (it overwrote reloc_index)
        // and the caller's return address is at [rsp+8].
        "add rsp, 8",
        // `ret` pops the resolved address into rip, leaving the
        // caller's return address on top so the resolved function's
        // own `ret` returns to the caller.
        "ret",
        resolve_pltrel = sym resolve_pltrel,
    );
}

/// Rust callback the trampoline invokes to compute one symbol's
/// resolved address and patch the GOT slot.
///
/// # Safety
/// `link_map` must be a pointer the linker installed at load time
/// (via [`install_trampoline`]) pointing at a stable `LoadedDso`
/// inside `DL_STATE.dsos`. `reloc_index` must be an index into the
/// DSO's `DT_JMPREL` table.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn resolve_pltrel(link_map: *const LoadedDso, reloc_index: usize) -> u64 {
    // Bounds + non-null check before any pointer arithmetic. A
    // corrupted GOT[1] would otherwise dereference into unmapped
    // memory.
    if link_map.is_null() {
        serial(b"plt: resolve_pltrel: null link_map\n");
        sys_exit(127);
    }
    let dso = unsafe { &*link_map };
    let jmprel = match dso.dyn_.jmprel {
        Some(p) => p.as_ptr() as *const Rela,
        None => {
            serial(b"plt: resolve_pltrel: DSO has no DT_JMPREL\n");
            sys_exit(127);
        }
    };
    let n = (dso.dyn_.pltrelsz / 24) as usize;
    if reloc_index >= n {
        serial(b"plt: resolve_pltrel: reloc_index out of range\n");
        sys_exit(127);
    }
    let r = unsafe { *jmprel.add(reloc_index) };
    let rt = r_type(r.r_info);
    if rt != R_X86_64_JUMP_SLOT {
        serial(b"plt: resolve_pltrel: not a JUMP_SLOT\n");
        sys_exit(127);
    }
    let symtab = match dso.dyn_.symtab {
        Some(p) => p.as_ptr(),
        None => {
            serial(b"plt: resolve_pltrel: DSO has no DT_SYMTAB\n");
            sys_exit(127);
        }
    };
    let strtab = match dso.dyn_.strtab {
        Some(p) => p.as_ptr(),
        None => {
            serial(b"plt: resolve_pltrel: DSO has no DT_STRTAB\n");
            sys_exit(127);
        }
    };
    let sym_idx = r_sym(r.r_info);
    let sym = match unsafe { crate::sym_entry(symtab, sym_idx, dso.load_bias, dso.image_len) } {
        Some(s) => s,
        None => {
            serial(b"plt: resolve_pltrel: symbol index outside image\n");
            sys_exit(127);
        }
    };
    let name = unsafe { crate::strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
    // Phase 76d.D2.2 — read the consumer's `DT_VERSYM` /
    // `DT_VERNEED` to derive the required version name for this
    // symbol. `consumer_required_version` returns `None` for
    // unversioned consumers.
    let strtab_bytes: &[u8] =
        unsafe { core::slice::from_raw_parts(strtab, dso.dyn_.strsz as usize) };
    let verneed_bytes: &[u8] = match (dso.dyn_.verneed, dso.dyn_.verneednum) {
        (Some(p), n) if n > 0 && dso.image_len != 0 => {
            let base = p.as_ptr() as u64;
            let image_end = dso.load_bias.saturating_add(dso.image_len);
            let len = image_end.saturating_sub(base) as usize;
            unsafe { core::slice::from_raw_parts(p.as_ptr(), len) }
        }
        _ => &[],
    };
    let ver_table = ldso_core::ver::VersionTable {
        versym: &[],
        verdef_bytes: &[],
        verdef_num: 0,
        verneed_bytes,
        verneed_num: dso.dyn_.verneednum as usize,
        strtab: strtab_bytes,
    };
    let version = crate::consumer_required_version(
        dso.dyn_.versym.map(|p| p.as_ptr()),
        sym_idx,
        dso.load_bias,
        dso.image_len,
        &ver_table,
    );
    // Search the full process scope via `sym::lookup`. The bring-up
    // publication into `DL_STATE.dsos` makes the entire dependency
    // graph the lookup scope (matches SysV global semantics).
    let state = dl::dl_state_mut();
    let scope = &state.dsos[..state.n_slots_used];
    let resolved = unsafe { sym::lookup(scope, name, version) };
    let addr = match resolved {
        Some(a) => a,
        None => {
            serial(b"plt: resolve_pltrel: undefined symbol ");
            serial(name);
            serial(b"\n");
            sys_exit(127);
        }
    };
    // Patch the GOT slot so subsequent calls skip the trampoline.
    // `r.r_offset` is the in-image offset of the slot; bound the 8-byte
    // write to the image (`r_offset + 8 <= image_len`) before writing,
    // so a corrupt JMPREL entry cannot redirect the write out of image.
    if dso.image_len != 0 {
        match r.r_offset.checked_add(8) {
            Some(end) if end <= dso.image_len => {}
            _ => {
                serial(b"plt: resolve_pltrel: GOT slot outside image\n");
                sys_exit(127);
            }
        }
    }
    let got_slot = (dso.load_bias.wrapping_add(r.r_offset)) as *mut u64;
    unsafe { core::ptr::write_unaligned(got_slot, addr) };
    addr
}

/// Install the lazy-resolve trampoline addresses into a DSO's GOT.
///
/// Writes `link_map` (a pointer to the DSO's slot in `DL_STATE.dsos`,
/// chosen so the pointer stays valid for the life of the DSO) at
/// `GOT[1]` and `&_dl_runtime_resolve` at `GOT[2]`. The PLT's
/// `plt0` stub reads both slots to drive every first-call dispatch.
///
/// `GOT[0]` is the static linker's `DT_DYNAMIC` back-pointer and is
/// left untouched.
///
/// # Safety
/// `dso` must be a fully-loaded DSO whose `dyn_.pltgot` points at
/// the DSO's GOT region inside its mapped image. `link_map` must
/// remain valid for the life of the DSO.
pub unsafe fn install_trampoline(dso: &LoadedDso, link_map: *const LoadedDso) {
    let pltgot = match dso.dyn_.pltgot {
        Some(p) => p.as_ptr(),
        None => return, // No PLT → nothing to install.
    };
    // GOT[1] = link_map, GOT[2] = &_dl_runtime_resolve.
    let trampoline_addr = _dl_runtime_resolve as *const () as u64;
    unsafe {
        core::ptr::write_unaligned(pltgot.add(1), link_map as u64);
        core::ptr::write_unaligned(pltgot.add(2), trampoline_addr);
    }
}

/// Rebase a single lazy `R_X86_64_JUMP_SLOT` GOT slot. The static
/// linker pre-populates the GOT slot with an image-relative offset
/// to the PLT entry's `push reloc_index` instruction; we add
/// `load_bias` to make it absolute so the first call lands on the
/// PLT trampoline path.
///
/// # Safety
/// `dso.load_bias` + `r.r_offset` must point inside the DSO's
/// writable GOT region. The slot must currently hold an
/// image-relative offset (the static linker's default).
pub unsafe fn apply_jmprel_lazy(dso: &LoadedDso, r: &Rela) {
    // `r.r_offset` is the in-image offset of the GOT slot; bound the
    // 8-byte read+write to the image before touching it so a corrupt
    // JMPREL entry cannot rebase memory outside the DSO.
    if dso.image_len != 0 {
        match r.r_offset.checked_add(8) {
            Some(end) if end <= dso.image_len => {}
            _ => return,
        }
    }
    let target = (dso.load_bias.wrapping_add(r.r_offset)) as *mut u64;
    let cur = unsafe { core::ptr::read_unaligned(target) };
    unsafe { core::ptr::write_unaligned(target, cur.wrapping_add(dso.load_bias)) };
}
