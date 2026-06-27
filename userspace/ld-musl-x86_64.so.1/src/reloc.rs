//! x86_64 relocation primitives.
//!
//! Phase 76b applies four relocation types eagerly at load time:
//!
//! * `R_X86_64_RELATIVE` — `*(load_bias + r_offset) = load_bias + r_addend`.
//!   Used for in-image pointers in any PIE/PIC DSO.
//! * `R_X86_64_GLOB_DAT` — `*(load_bias + r_offset) = sym_addr`.
//!   Used for GOT-routed external symbol references.
//! * `R_X86_64_64` — `*(load_bias + r_offset) = sym_addr + r_addend`.
//!   Used for direct 64-bit pointer fields.
//! * `R_X86_64_JUMP_SLOT` — same as `GLOB_DAT`; applied from `DT_JMPREL`.
//!
//! The self-relocation `_dlstart` → `dl_relocate_self` path only ever
//! touches `R_X86_64_RELATIVE` because the linker is a `-pie` ELF
//! linked with `-Bsymbolic` (no external symbols).
//!
//! The host-testable surface is [`apply_relative`]: a pure function
//! that takes a `&Rela`, a load bias, and a mutable byte slice and
//! writes the eight bytes at `r_offset` if all bounds checks pass.
//! The runtime path that walks `DT_RELA` calls into this function via
//! a raw-pointer wrapper.

use crate::elf64::Rela;

/// Errors `apply_relative` can return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocError {
    /// `r_offset` is not 8-byte aligned. x86_64 `R_X86_64_RELATIVE`
    /// always writes a `u64`; an unaligned offset would either crash
    /// the CPU (with `AC=1`) or silently produce a torn write.
    MisalignedOffset(u64),
    /// `r_offset + 8` exceeds the image bounds.
    OutOfBounds { r_offset: u64, image_len: usize },
    /// Relocation type the linker does not understand. The payload is
    /// the raw `r_info` so the log message can include the offending
    /// type number.
    Unsupported(u64),
    /// A relocation named a symbol the linker could not resolve.
    UndefinedSymbol,
}

/// Apply a single `R_X86_64_RELATIVE`-style write to an image buffer.
///
/// Computes `value = load_bias + r_addend` and stores it as an
/// 8-byte little-endian word at `reloc.r_offset` within `image`. Pure
/// logic: never touches memory outside `image`, never reads symbol
/// tables, never invokes a syscall. The caller (`apply_rela_table`
/// runtime wrapper) is responsible for translating a real DSO image
/// pointer into the `&mut [u8]` view used here.
///
/// Returns `RelocError::MisalignedOffset` if `r_offset` is not 8-byte
/// aligned, or `RelocError::OutOfBounds` if the 8-byte store would
/// exceed `image.len()`.
pub fn apply_relative(reloc: &Rela, load_bias: u64, image: &mut [u8]) -> Result<(), RelocError> {
    if !reloc.r_offset.is_multiple_of(8) {
        return Err(RelocError::MisalignedOffset(reloc.r_offset));
    }
    let off = reloc.r_offset as usize;
    let end = off.checked_add(8).ok_or(RelocError::OutOfBounds {
        r_offset: reloc.r_offset,
        image_len: image.len(),
    })?;
    if end > image.len() {
        return Err(RelocError::OutOfBounds {
            r_offset: reloc.r_offset,
            image_len: image.len(),
        });
    }
    // load_bias + r_addend, signed-aware. `r_addend` is `i64`, so a
    // negative addend (rare but legal for `RELATIVE`) wraps via
    // `wrapping_add` on the unsigned bias.
    let value = load_bias.wrapping_add(reloc.r_addend as u64);
    image[off..end].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Relocate one 8-byte slot at byte `offset` within `image`: read the existing
/// value, add `load_bias`, write it back. Alignment- and bounds-checked.
///
/// This is the per-slot primitive both `DT_RELR` ([`apply_relr`]) and a future
/// in-place `R_X86_64_RELATIVE` could share; today only `apply_relr` uses it.
fn relocate_slot(image: &mut [u8], offset: u64, load_bias: u64) -> Result<(), RelocError> {
    if !offset.is_multiple_of(8) {
        return Err(RelocError::MisalignedOffset(offset));
    }
    let off = offset as usize;
    let end = off.checked_add(8).ok_or(RelocError::OutOfBounds {
        r_offset: offset,
        image_len: image.len(),
    })?;
    if end > image.len() {
        return Err(RelocError::OutOfBounds {
            r_offset: offset,
            image_len: image.len(),
        });
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&image[off..end]);
    let value = u64::from_le_bytes(buf).wrapping_add(load_bias);
    image[off..end].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Apply a `DT_RELR` (compact relative-relocation) table against an image
/// buffer. `DT_RELR` is what modern linkers emit *instead of* the equivalent
/// stream of `R_X86_64_RELATIVE` `DT_RELA` entries — e.g. `libhello_fini.so`'s
/// sole relocation (its `DT_FINI_ARRAY` destructor pointer) is RELR-encoded, so
/// a loader that ignores `DT_RELR` leaves that pointer holding its unrelocated
/// in-file value and `dlclose` jumps to a near-NULL address. Each entry is
/// interpreted per the SysV RELR encoding:
///
/// * **Address word** (LSB == 0): the word is the image-relative byte offset of
///   a slot; relocate that slot (`*slot += load_bias`) and set the running
///   cursor to the next slot.
/// * **Bitmap word** (LSB == 1): bits `1..=63` each select a slot starting at
///   the running cursor — bit `k` relocates the slot at `cursor + (k-1)*8`. The
///   cursor then advances by 63 slots regardless of which bits were set.
///
/// `relr` is the raw `.relr.dyn` table (one `u64` per `DT_RELRENT`, always 8).
/// Pure logic: every write is bounds- and alignment-checked against `image`, so
/// a malformed table errors instead of writing out of bounds. Returns the
/// number of slots relocated.
pub fn apply_relr(relr: &[u64], load_bias: u64, image: &mut [u8]) -> Result<usize, RelocError> {
    // Byte offset (into `image`) of the next slot a bitmap word describes.
    let mut cursor: u64 = 0;
    let mut applied = 0usize;
    for &entry in relr {
        if entry & 1 == 0 {
            // Address word: `entry` is the offset of a single slot.
            relocate_slot(image, entry, load_bias)?;
            applied += 1;
            cursor = entry.wrapping_add(8);
        } else {
            // Bitmap word: bits 1..=63 select slots relative to `cursor`.
            let mut bits = entry >> 1;
            let mut k: u64 = 0;
            while bits != 0 {
                if bits & 1 != 0 {
                    relocate_slot(image, cursor.wrapping_add(k.wrapping_mul(8)), load_bias)?;
                    applied += 1;
                }
                bits >>= 1;
                k += 1;
            }
            // A bitmap always covers 63 slots; advance the cursor past them.
            cursor = cursor.wrapping_add(63 * 8);
        }
    }
    Ok(applied)
}

/// Apply a single `R_X86_64_64` write — `*(load_bias + r_offset) =
/// sym_addr + r_addend`.
pub fn apply_abs64(
    reloc: &Rela,
    load_bias: u64,
    sym_addr: u64,
    image: &mut [u8],
) -> Result<(), RelocError> {
    if !reloc.r_offset.is_multiple_of(8) {
        return Err(RelocError::MisalignedOffset(reloc.r_offset));
    }
    let off = reloc.r_offset as usize;
    let end = off.checked_add(8).ok_or(RelocError::OutOfBounds {
        r_offset: reloc.r_offset,
        image_len: image.len(),
    })?;
    if end > image.len() {
        return Err(RelocError::OutOfBounds {
            r_offset: reloc.r_offset,
            image_len: image.len(),
        });
    }
    let _ = load_bias;
    let value = sym_addr.wrapping_add(reloc.r_addend as u64);
    image[off..end].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Apply a single `R_X86_64_GLOB_DAT` / `R_X86_64_JUMP_SLOT` write —
/// `*(load_bias + r_offset) = sym_addr`. The two relocation types
/// have identical semantics on x86_64 in the absence of PLT lazy
/// resolution (which Phase 76b deliberately skips).
pub fn apply_glob_dat(
    reloc: &Rela,
    load_bias: u64,
    sym_addr: u64,
    image: &mut [u8],
) -> Result<(), RelocError> {
    if !reloc.r_offset.is_multiple_of(8) {
        return Err(RelocError::MisalignedOffset(reloc.r_offset));
    }
    let off = reloc.r_offset as usize;
    let end = off.checked_add(8).ok_or(RelocError::OutOfBounds {
        r_offset: reloc.r_offset,
        image_len: image.len(),
    })?;
    if end > image.len() {
        return Err(RelocError::OutOfBounds {
            r_offset: reloc.r_offset,
            image_len: image.len(),
        });
    }
    let _ = load_bias;
    image[off..end].copy_from_slice(&sym_addr.to_le_bytes());
    Ok(())
}

/// Apply a single `R_X86_64_IRELATIVE` (or an `STT_GNU_IFUNC`-routed)
/// write — `*(load_bias + r_offset) = resolved`, where `resolved` is
/// the value the IFUNC resolver returned. Identical store discipline
/// to [`apply_glob_dat`]; the distinct name documents that the value
/// came from *running* a resolver, not from a symbol-table lookup.
pub fn apply_irelative(reloc: &Rela, resolved: u64, image: &mut [u8]) -> Result<(), RelocError> {
    if !reloc.r_offset.is_multiple_of(8) {
        return Err(RelocError::MisalignedOffset(reloc.r_offset));
    }
    let off = reloc.r_offset as usize;
    let end = off.checked_add(8).ok_or(RelocError::OutOfBounds {
        r_offset: reloc.r_offset,
        image_len: image.len(),
    })?;
    if end > image.len() {
        return Err(RelocError::OutOfBounds {
            r_offset: reloc.r_offset,
            image_len: image.len(),
        });
    }
    image[off..end].copy_from_slice(&resolved.to_le_bytes());
    Ok(())
}

/// Apply a single general-dynamic / initial-exec TLS word write —
/// `*(load_bias + r_offset) = value`. Shared by `R_X86_64_DTPMOD64`,
/// `R_X86_64_DTPOFF64`, and `R_X86_64_TPOFF64`; the caller computes the
/// type-specific `value` (module id, in-block offset, or TP-relative
/// offset) and this helper performs the bounds-checked 8-byte store.
pub fn apply_tls_word(reloc: &Rela, value: u64, image: &mut [u8]) -> Result<(), RelocError> {
    if !reloc.r_offset.is_multiple_of(8) {
        return Err(RelocError::MisalignedOffset(reloc.r_offset));
    }
    let off = reloc.r_offset as usize;
    let end = off.checked_add(8).ok_or(RelocError::OutOfBounds {
        r_offset: reloc.r_offset,
        image_len: image.len(),
    })?;
    if end > image.len() {
        return Err(RelocError::OutOfBounds {
            r_offset: reloc.r_offset,
            image_len: image.len(),
        });
    }
    image[off..end].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Apply a single `R_X86_64_COPY` — copy `src.len()` bytes (the
/// defining symbol's `st_size`) from the provider DSO into the relocated
/// image at `r_offset`. Unlike the 8-byte pointer writes, a copy
/// relocation moves an arbitrarily-sized, arbitrarily-aligned data
/// object, so the only constraint enforced is that the whole copy lands
/// inside `image`. `src` must reference the provider's mapped bytes for
/// the symbol; the runtime caller forms it from the resolved provider
/// address + the consumer symbol's `st_size`.
pub fn apply_copy(reloc: &Rela, src: &[u8], image: &mut [u8]) -> Result<(), RelocError> {
    let off = reloc.r_offset as usize;
    let end = off.checked_add(src.len()).ok_or(RelocError::OutOfBounds {
        r_offset: reloc.r_offset,
        image_len: image.len(),
    })?;
    if end > image.len() {
        return Err(RelocError::OutOfBounds {
            r_offset: reloc.r_offset,
            image_len: image.len(),
        });
    }
    image[off..end].copy_from_slice(src);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rela(r_offset: u64, r_addend: i64) -> Rela {
        Rela {
            r_offset,
            r_info: 0,
            r_addend,
        }
    }

    #[test]
    fn zero_addend_writes_load_bias() {
        let mut image = vec![0u8; 16];
        let r = rela(0, 0);
        apply_relative(&r, 0xDEAD_BEEF_CAFE_F00D, &mut image).unwrap();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&image[..8]);
        assert_eq!(u64::from_le_bytes(buf), 0xDEAD_BEEF_CAFE_F00D);
    }

    #[test]
    fn non_zero_addend_adds_to_bias() {
        let mut image = vec![0u8; 32];
        let r = rela(8, 0x100);
        apply_relative(&r, 0x4000_0000, &mut image).unwrap();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&image[8..16]);
        assert_eq!(u64::from_le_bytes(buf), 0x4000_0100);
    }

    #[test]
    fn negative_addend_wraps_via_wrapping_add() {
        let mut image = vec![0u8; 16];
        let r = rela(0, -8);
        apply_relative(&r, 0x4000_0000, &mut image).unwrap();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&image[..8]);
        assert_eq!(u64::from_le_bytes(buf), 0x4000_0000u64.wrapping_sub(8));
    }

    #[test]
    fn misaligned_offset_errors() {
        let mut image = vec![0u8; 32];
        let r = rela(3, 0);
        match apply_relative(&r, 0x4000_0000, &mut image) {
            Err(RelocError::MisalignedOffset(3)) => {}
            other => panic!("expected MisalignedOffset(3), got {other:?}"),
        }
    }

    #[test]
    fn out_of_bounds_offset_errors() {
        let mut image = vec![0u8; 16];
        // r_offset 16 + 8 = 24 > 16
        let r = rela(16, 0);
        match apply_relative(&r, 0x4000_0000, &mut image) {
            Err(RelocError::OutOfBounds {
                r_offset: 16,
                image_len: 16,
            }) => {}
            other => panic!("expected OutOfBounds(16, 16), got {other:?}"),
        }
    }

    #[test]
    fn boundary_write_at_exact_end_succeeds() {
        let mut image = vec![0u8; 16];
        let r = rela(8, 0x42);
        apply_relative(&r, 0, &mut image).unwrap();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&image[8..16]);
        assert_eq!(u64::from_le_bytes(buf), 0x42);
    }

    #[test]
    fn glob_dat_writes_symbol_address_directly() {
        let mut image = vec![0u8; 16];
        let r = rela(0, 0);
        apply_glob_dat(&r, 0, 0x1234_5678_9ABC_DEF0, &mut image).unwrap();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&image[..8]);
        assert_eq!(u64::from_le_bytes(buf), 0x1234_5678_9ABC_DEF0);
    }

    #[test]
    fn abs64_adds_addend_to_symbol() {
        let mut image = vec![0u8; 16];
        let r = rela(0, 0x10);
        apply_abs64(&r, 0, 0x1000, &mut image).unwrap();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&image[..8]);
        assert_eq!(u64::from_le_bytes(buf), 0x1010);
    }

    // -----------------------------------------------------------------
    // Phase 93 B.2 — IFUNC (R_X86_64_IRELATIVE) write.
    // -----------------------------------------------------------------
    #[test]
    fn irelative_writes_resolver_return_value() {
        // The runtime computes `resolved` by calling the resolver at
        // `load_bias + r_addend`; the helper just stores that value.
        let mut image = vec![0u8; 16];
        let r = rela(8, 0 /* addend unused by the helper */);
        apply_irelative(&r, 0xCAFE_F00D_1234_5678, &mut image).unwrap();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&image[8..16]);
        assert_eq!(u64::from_le_bytes(buf), 0xCAFE_F00D_1234_5678);
    }

    #[test]
    fn irelative_misaligned_offset_errors() {
        let mut image = vec![0u8; 16];
        let r = rela(1, 0);
        assert_eq!(
            apply_irelative(&r, 0x1, &mut image),
            Err(RelocError::MisalignedOffset(1))
        );
    }

    // -----------------------------------------------------------------
    // Phase 93 B.3 — TLS word writes (DTPMOD64 / DTPOFF64 / TPOFF64).
    // -----------------------------------------------------------------
    #[test]
    fn tls_word_writes_value_at_offset() {
        // DTPOFF64 value = st_value + addend, computed by the caller.
        let mut image = vec![0u8; 24];
        let r = rela(16, 0);
        apply_tls_word(&r, 0x0000_0000_0000_0048, &mut image).unwrap();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&image[16..24]);
        assert_eq!(u64::from_le_bytes(buf), 0x48);
    }

    #[test]
    fn tls_word_out_of_bounds_errors() {
        let mut image = vec![0u8; 16];
        let r = rela(16, 0); // 16 + 8 = 24 > 16
        assert_eq!(
            apply_tls_word(&r, 1, &mut image),
            Err(RelocError::OutOfBounds {
                r_offset: 16,
                image_len: 16
            })
        );
    }

    // -----------------------------------------------------------------
    // Phase 93 B.1 — copy relocation (R_X86_64_COPY).
    // -----------------------------------------------------------------
    #[test]
    fn copy_reloc_copies_provider_bytes_into_consumer() {
        // Simulate an executable that copy-relocates a libc data symbol:
        // the provider holds the canonical bytes, the consumer's BSS at
        // `r_offset` must end up holding the provider's value.
        let provider: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut consumer = vec![0u8; 16];
        let r = rela(8, 0);
        apply_copy(&r, &provider, &mut consumer).unwrap();
        assert_eq!(&consumer[8..12], &provider);
        // Bytes outside the copied span are untouched.
        assert_eq!(&consumer[0..8], &[0u8; 8]);
        assert_eq!(&consumer[12..16], &[0u8; 4]);
    }

    #[test]
    fn copy_reloc_rejects_out_of_bounds() {
        let provider: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut consumer = vec![0u8; 12];
        let r = rela(8, 0); // 8 + 8 = 16 > 12
        assert_eq!(
            apply_copy(&r, &provider, &mut consumer),
            Err(RelocError::OutOfBounds {
                r_offset: 8,
                image_len: 12
            })
        );
    }

    #[test]
    fn copy_reloc_handles_unaligned_offset() {
        // Copy relocs move data objects of any size/alignment — a
        // 3-byte copy at an odd offset must succeed (no 8-alignment
        // requirement, unlike the pointer writes).
        let provider: [u8; 3] = [0xAA, 0xBB, 0xCC];
        let mut consumer = vec![0u8; 8];
        let r = rela(3, 0);
        apply_copy(&r, &provider, &mut consumer).unwrap();
        assert_eq!(&consumer[3..6], &provider);
    }

    // ---- DT_RELR --------------------------------------------------------

    fn slot(image: &[u8], byte_off: usize) -> u64 {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&image[byte_off..byte_off + 8]);
        u64::from_le_bytes(buf)
    }

    fn put(image: &mut [u8], byte_off: usize, value: u64) {
        image[byte_off..byte_off + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn relr_address_word_relocates_single_slot() {
        // The exact libhello_fini.so shape: one address word naming the
        // .fini_array slot, whose in-file value (the destructor's in-DSO vaddr)
        // must have load_bias added.
        let mut image = vec![0u8; 0x40];
        put(&mut image, 0x10, 0x2a0); // unrelocated destructor vaddr
        let n = apply_relr(&[0x10], 0x4000_0000, &mut image).unwrap();
        assert_eq!(n, 1);
        assert_eq!(slot(&image, 0x10), 0x4000_02a0);
    }

    #[test]
    fn relr_bitmap_word_relocates_selected_slots() {
        // Address word sets the cursor at byte 0; the following bitmap word
        // (LSB=1) selects bits 1 and 3 → the slots at cursor+0 and cursor+16.
        let mut image = vec![0u8; 0x80];
        put(&mut image, 0x00, 0x1000); // address-word target
        put(&mut image, 0x08, 0x10); // cursor+0 (bit 1)
        put(&mut image, 0x18, 0x30); // cursor+16 (bit 3)
        put(&mut image, 0x10, 0x99); // cursor+8 (bit 2, NOT set) — untouched
        // entries: address word 0x00, then bitmap with bits 1 and 3 set.
        // bitmap = (1<<0)tag | (1<<1) | (1<<3) = 0b1011 = 0xB.
        let n = apply_relr(&[0x00, 0xB], 0x4000_0000, &mut image).unwrap();
        assert_eq!(n, 3);
        assert_eq!(slot(&image, 0x00), 0x4000_1000); // address word
        assert_eq!(slot(&image, 0x08), 0x4000_0010); // bit 1
        assert_eq!(slot(&image, 0x18), 0x4000_0030); // bit 3
        assert_eq!(slot(&image, 0x10), 0x99); // bit 2 unset — untouched
    }

    #[test]
    fn relr_rejects_out_of_bounds_offset() {
        let mut image = vec![0u8; 16];
        // Address word naming a slot whose 8-byte write exceeds the image.
        assert!(matches!(
            apply_relr(&[0x10], 0x4000_0000, &mut image),
            Err(RelocError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn relr_rejects_misaligned_offset() {
        let mut image = vec![0u8; 64];
        assert!(matches!(
            apply_relr(&[0x4], 0x4000_0000, &mut image),
            Err(RelocError::MisalignedOffset(4))
        ));
    }
}
