//! SysV AMD64 ABI auxiliary-vector layout helpers (Phase 76).
//!
//! The auxv sits on the initial user stack just above `envp NULL` and
//! below the string region. Its byte-exact layout is part of the
//! contract between the kernel and the dynamic linker — musl's
//! `arch/x86_64/crt_arch.h::_dlstart` walks it and reads specific
//! a_type values. Get the order wrong or omit an entry and the linker
//! either crashes on first global access or jumps to a bogus address.
//!
//! This module is pure-logic: it returns a fixed-size list of
//! `AuxEntry { a_type, a_val }` records that the caller (`mm::elf`
//! in the kernel, or future per-binary tooling) writes onto a target
//! stack via the physical-memory offset.

use heapless::Vec as HeaplessVec;

// ---------------------------------------------------------------------------
// AT_* constants (subset Phase 76 emits)
// ---------------------------------------------------------------------------

/// Sentinel: end of auxv.
pub const AT_NULL: u64 = 0;
/// Address of program-header table in the loaded main binary.
pub const AT_PHDR: u64 = 3;
/// Size of one program-header entry.
pub const AT_PHENT: u64 = 4;
/// Number of program-header entries.
pub const AT_PHNUM: u64 = 5;
/// Page size (always 4096 on x86_64).
pub const AT_PAGESZ: u64 = 6;
/// Interpreter (dynamic linker) load base. Only emitted when `PT_INTERP`
/// was honored — the linker reads this to know where its own segments
/// landed so it can compute internal addresses correctly.
pub const AT_BASE: u64 = 7;
/// Main binary entry point. With `PT_INTERP` the kernel transfers
/// control to the interpreter, so the interpreter needs `AT_ENTRY` to
/// know where to jump once it finishes its bring-up work.
pub const AT_ENTRY: u64 = 9;
/// Pointer to a 16-byte random seed in the string region. musl seeds
/// its stack canary from this.
pub const AT_RANDOM: u64 = 25;

/// Hard upper bound on entries this module can emit. Eight slots cover
/// every entry Phase 76 produces (PHDR, PHENT, PHNUM, PAGESZ, BASE,
/// ENTRY, RANDOM, NULL); subsequent phases (76d versioned-symbol
/// fast-path, future TLS work) may add more — the cap is a safety
/// belt, not a limit.
pub const MAX_AUX_ENTRIES: usize = 16;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One auxv entry as it lands on the stack: 16 bytes (`a_type`, `a_val`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuxEntry {
    pub a_type: u64,
    pub a_val: u64,
}

/// Main-binary program-header info every auxv carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhdrInfo {
    pub phdr_vaddr: u64,
    pub phentsize: u16,
    pub phnum: u16,
}

/// Extra entries the kernel emits only when `PT_INTERP` was honored.
/// `at_base` is the interpreter load bias; `at_entry` is the main
/// binary's entry vaddr (the kernel's `entry` field is the
/// interpreter's entry, since that is what control transfers to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuxExtras {
    pub at_base: u64,
    pub at_entry: u64,
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Build the list of auxv entries the kernel writes onto the user
/// stack (low → high addresses).
///
/// `at_random_ptr` must be the user-space virtual address of the
/// 16-byte random seed in the string region. The caller is
/// responsible for writing those 16 bytes; this function just
/// references them via `AT_RANDOM`.
///
/// When `extras` is `Some`, `AT_BASE` and `AT_ENTRY` are emitted in
/// the canonical musl-`_dlstart`-friendly order (BASE before ENTRY).
/// When `extras` is `None`, neither is emitted — keeping the
/// pre-Phase-76 static-binary auxv shape bit-identical so existing
/// binaries are unaffected.
pub fn build_layout(
    phdr: PhdrInfo,
    extras: Option<AuxExtras>,
    at_random_ptr: u64,
) -> HeaplessVec<AuxEntry, MAX_AUX_ENTRIES> {
    let mut out = HeaplessVec::new();

    // Pushes that cannot fail unless MAX_AUX_ENTRIES is too low.
    // Use unwrap() because exceeding the cap is a programmer error
    // (raise MAX_AUX_ENTRIES) — not a runtime failure mode.
    out.push(AuxEntry {
        a_type: AT_PHDR,
        a_val: phdr.phdr_vaddr,
    })
    .unwrap();
    out.push(AuxEntry {
        a_type: AT_PHENT,
        a_val: phdr.phentsize as u64,
    })
    .unwrap();
    out.push(AuxEntry {
        a_type: AT_PHNUM,
        a_val: phdr.phnum as u64,
    })
    .unwrap();
    out.push(AuxEntry {
        a_type: AT_PAGESZ,
        a_val: 4096,
    })
    .unwrap();

    if let Some(extras) = extras {
        out.push(AuxEntry {
            a_type: AT_BASE,
            a_val: extras.at_base,
        })
        .unwrap();
        out.push(AuxEntry {
            a_type: AT_ENTRY,
            a_val: extras.at_entry,
        })
        .unwrap();
    }

    out.push(AuxEntry {
        a_type: AT_RANDOM,
        a_val: at_random_ptr,
    })
    .unwrap();
    out.push(AuxEntry {
        a_type: AT_NULL,
        a_val: 0,
    })
    .unwrap();

    out
}

/// Size in bytes the auxv will occupy on the stack (`entries * 16`).
pub fn bytes_for(entries: usize) -> usize {
    entries * 16
}

// ---------------------------------------------------------------------------
// Host-side tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn phdr() -> PhdrInfo {
        PhdrInfo {
            phdr_vaddr: 0x40_0040,
            phentsize: 56,
            phnum: 8,
        }
    }

    #[test]
    fn static_binary_layout_matches_phase_11_shape() {
        let v = build_layout(phdr(), None, 0xDEAD_BEEF);
        // 6 entries: PHDR, PHENT, PHNUM, PAGESZ, RANDOM, NULL — same
        // as before Phase 76 so existing static binaries are unaffected.
        assert_eq!(v.len(), 6);
        assert_eq!(v[0].a_type, AT_PHDR);
        assert_eq!(v[1].a_type, AT_PHENT);
        assert_eq!(v[2].a_type, AT_PHNUM);
        assert_eq!(v[3].a_type, AT_PAGESZ);
        assert_eq!(v[4].a_type, AT_RANDOM);
        assert_eq!(v[5].a_type, AT_NULL);
        assert_eq!(v[5].a_val, 0);
    }

    #[test]
    fn dynamic_binary_layout_emits_base_and_entry_in_musl_order() {
        let extras = AuxExtras {
            at_base: 0x4000_0000,
            at_entry: 0x40_1000,
        };
        let v = build_layout(phdr(), Some(extras), 0xCAFE);
        // 8 entries — PAGESZ before BASE; BASE before ENTRY; ENTRY before
        // RANDOM; NULL last. This is the exact order musl _dlstart's
        // walker consumes.
        assert_eq!(v.len(), 8);
        assert_eq!(v[0].a_type, AT_PHDR);
        assert_eq!(v[1].a_type, AT_PHENT);
        assert_eq!(v[2].a_type, AT_PHNUM);
        assert_eq!(v[3].a_type, AT_PAGESZ);
        assert_eq!(v[4].a_type, AT_BASE);
        assert_eq!(v[4].a_val, 0x4000_0000);
        assert_eq!(v[5].a_type, AT_ENTRY);
        assert_eq!(v[5].a_val, 0x40_1000);
        assert_eq!(v[6].a_type, AT_RANDOM);
        assert_eq!(v[6].a_val, 0xCAFE);
        assert_eq!(v[7].a_type, AT_NULL);
    }

    #[test]
    fn at_null_is_always_last() {
        let v_static = build_layout(phdr(), None, 0);
        assert_eq!(v_static.last().map(|e| e.a_type), Some(AT_NULL));

        let v_dyn = build_layout(
            phdr(),
            Some(AuxExtras {
                at_base: 0x1000,
                at_entry: 0x2000,
            }),
            0,
        );
        assert_eq!(v_dyn.last().map(|e| e.a_type), Some(AT_NULL));
    }

    #[test]
    fn bytes_for_is_16_times_entries() {
        assert_eq!(bytes_for(6), 96);
        assert_eq!(bytes_for(8), 128);
    }

    /// The static-binary layout must remain bit-identical to the
    /// pre-Phase-76 shape so existing static binaries observe no
    /// difference. This test pins the exact 6-tuple of a_type values
    /// so the regression is visible the moment someone reorders them.
    #[test]
    fn static_layout_byte_sequence_pinned() {
        let v = build_layout(
            PhdrInfo {
                phdr_vaddr: 0x10,
                phentsize: 56,
                phnum: 1,
            },
            None,
            0x20,
        );
        let kinds: heapless::Vec<u64, MAX_AUX_ENTRIES> = v.iter().map(|e| e.a_type).collect();
        let expect = [AT_PHDR, AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_RANDOM, AT_NULL];
        assert_eq!(&kinds[..], &expect[..]);
    }

    /// Phase 86f Track B.2 — verify the AMD64 SysV ABI process-entry RSP
    /// alignment contract: given the auxv size from `build_layout`, the
    /// pointer-table computation in `setup_abi_stack_with_envp` must
    /// produce RSP ≡ 0 (mod 16) at `_start`.
    ///
    /// The m3OS `_start` stubs do `mov rdi, rsp; call entry` — for the callee
    /// to see RSP ≡ 8 (mod 16) after the `call` push, `_start` must be entered
    /// with RSP ≡ 0 (mod 16).  This is the SysV AMD64 process-entry contract
    /// (psABI §3.4.1) required for SSE `movaps` stack spills.
    ///
    /// This is a pure-math mirror of the `debug_assert_eq!` added in
    /// `kernel/src/mm/elf.rs::setup_abi_stack_with_envp`.  We model the
    /// same arithmetic here so the host-test suite catches a misalignment
    /// regression without a QEMU boot.
    #[test]
    fn rsp_at_start_is_0_mod_16() {
        // Two representative cursor values after the AT_RANDOM write:
        // one 16-byte aligned, one 8-mod-16.
        for &start_cursor in &[0x7fff_fff0_u64, 0x7fff_fff8_u64] {
            for extras in [
                None,
                Some(AuxExtras {
                    at_base: 1,
                    at_entry: 2,
                }),
            ] {
                let auxv = build_layout(
                    PhdrInfo {
                        phdr_vaddr: 0x400040,
                        phentsize: 56,
                        phnum: 8,
                    },
                    extras,
                    0xdead_beef,
                );
                let nenv: usize = 2;
                let narg: usize = 1;

                // Reproduce the arithmetic from setup_abi_stack_with_envp.
                let auxv_slots = auxv.len() * 2;
                let envp_slots = nenv + 1;
                let argv_slots = narg + 1;
                let argc_slot = 1_usize;
                let total_slots = auxv_slots + envp_slots + argv_slots + argc_slot;
                let table_bytes = total_slots * 8;

                let mut cursor = start_cursor;
                let target = cursor - table_bytes as u64;
                if target % 16 != 0 {
                    cursor -= 8;
                }
                // Subtract the full table to reach argc position.
                cursor -= table_bytes as u64;

                assert_eq!(
                    cursor % 16,
                    0,
                    "RSP not 0 mod 16: start_cursor={:#x} has_extras={} auxv_len={}",
                    start_cursor,
                    extras.is_some(),
                    auxv.len(),
                );
            }
        }
    }

    /// Equivalent pin for the dynamic-binary case.
    #[test]
    fn dynamic_layout_byte_sequence_pinned() {
        let v = build_layout(
            PhdrInfo {
                phdr_vaddr: 0x10,
                phentsize: 56,
                phnum: 1,
            },
            Some(AuxExtras {
                at_base: 0x4000_0000,
                at_entry: 0x40_1000,
            }),
            0x20,
        );
        let kinds: heapless::Vec<u64, MAX_AUX_ENTRIES> = v.iter().map(|e| e.a_type).collect();
        let expect = [
            AT_PHDR, AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_BASE, AT_ENTRY, AT_RANDOM, AT_NULL,
        ];
        assert_eq!(&kinds[..], &expect[..]);
    }
}
