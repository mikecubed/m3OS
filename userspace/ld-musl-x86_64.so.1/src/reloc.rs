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
}
