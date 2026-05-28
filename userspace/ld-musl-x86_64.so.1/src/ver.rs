//! Phase 76d.D2 — symbol versioning parser.
//!
//! glibc-built shared libraries record per-symbol version
//! requirements and definitions in three optional dynamic tags:
//!
//! * `DT_VERSYM` — a parallel array of `Elf64_Half` (u16) values,
//!   one per `DT_SYMTAB` entry. The low 15 bits of each entry index
//!   either a `Verdef` record (for symbols the DSO **defines**) or a
//!   `Vernaux` record (for symbols the DSO **requires** from its
//!   `DT_NEEDED` dependencies). Bit 15 is the "hidden" flag.
//! * `DT_VERDEF` + `DT_VERDEFNUM` — a linked list of `Verdef`
//!   records (one per version the DSO defines, e.g.
//!   `LIBHELLO_1.0`). Each `Verdef` carries the version name (via
//!   `Verdaux::vda_name` into the DSO's `DT_STRTAB`) and a chain of
//!   `Verdaux` records.
//! * `DT_VERNEED` + `DT_VERNEEDNUM` — a linked list of `Verneed`
//!   records (one per `DT_NEEDED` dependency the DSO requires
//!   versions from). Each `Verneed` carries the providing library's
//!   SONAME and a chain of `Vernaux` records (one per required
//!   version).
//!
//! Phase 76d.D2.1 ships the pure-logic decoders so D2.2's runtime
//! lookup path can ask: "which version-name does the requirer's
//! symbol-table index `n` need?" and "does the provider DSO define
//! that version-name?".
//!
//! See the GABI write-up at
//! <https://refspecs.linuxfoundation.org/LSB_3.0.0/LSB-Core-generic/LSB-Core-generic/symversion.html>.

/// One `Elf64_Verdef` record (`sizeof == 20`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdef {
    /// Version revision (always 1 for the format Phase 76d understands).
    pub vd_version: u16,
    /// Flags — bit 0 = `VER_FLG_BASE` (record for the DSO's own SONAME).
    pub vd_flags: u16,
    /// Version index (matches the value `DT_VERSYM` entries carry).
    pub vd_ndx: u16,
    /// Number of `Verdaux` records chained off this `Verdef`.
    pub vd_cnt: u16,
    /// Hash of the version name (for fast string comparison).
    pub vd_hash: u32,
    /// Offset (bytes) from THIS `Verdef` to the first `Verdaux`.
    pub vd_aux: u32,
    /// Offset (bytes) from THIS `Verdef` to the next `Verdef`, or
    /// `0` to terminate the list.
    pub vd_next: u32,
}

/// One `Elf64_Verdaux` record (`sizeof == 8`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdaux {
    /// Offset into `DT_STRTAB` of the version name.
    pub vda_name: u32,
    /// Offset (bytes) from THIS `Verdaux` to the next `Verdaux`, or
    /// `0` to terminate the chain.
    pub vda_next: u32,
}

/// One `Elf64_Verneed` record (`sizeof == 16`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verneed {
    pub vn_version: u16,
    /// Number of `Vernaux` records chained off this `Verneed`.
    pub vn_cnt: u16,
    /// Offset into `DT_STRTAB` of the providing library's SONAME.
    pub vn_file: u32,
    /// Offset (bytes) from THIS `Verneed` to the first `Vernaux`.
    pub vn_aux: u32,
    /// Offset (bytes) from THIS `Verneed` to the next `Verneed`,
    /// or `0` to terminate the list.
    pub vn_next: u32,
}

/// One `Elf64_Vernaux` record (`sizeof == 16`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vernaux {
    /// Hash of the version name.
    pub vna_hash: u32,
    /// Flags.
    pub vna_flags: u16,
    /// Version index — `DT_VERSYM` entries for symbols required from
    /// this `Verneed`'s providing library carry this value.
    pub vna_other: u16,
    /// Offset into `DT_STRTAB` of the required version name.
    pub vna_name: u32,
    /// Offset (bytes) from THIS `Vernaux` to the next `Vernaux`, or
    /// `0` to terminate the chain.
    pub vna_next: u32,
}

/// Bit set on `versym[i]` indicating the symbol is "hidden" (not
/// part of the DSO's default version export). Phase 76d.D2 treats
/// hidden symbols as version-bound — they participate in matching
/// but are not surfaced to unversioned consumers.
pub const VERSYM_HIDDEN: u16 = 0x8000;
/// Mask that extracts the version index from a `versym[i]` entry.
pub const VERSYM_VERSION_MASK: u16 = 0x7FFF;

/// Special version indices (always present, never recorded in
/// `DT_VERDEF` or `DT_VERNEED`).
pub const VER_NDX_LOCAL: u16 = 0;
pub const VER_NDX_GLOBAL: u16 = 1;

/// Decoded version-table view, callable from host tests without
/// `unsafe`. Each input is a borrowed byte slice covering the
/// DSO's mapped image; callers in the runtime construct these
/// from `DT_VERSYM` / `DT_VERDEF` / `DT_VERNEED` pointers + their
/// associated counts.
#[derive(Clone, Copy)]
pub struct VersionTable<'a> {
    pub versym: &'a [u16],
    pub verdef_bytes: &'a [u8],
    pub verdef_num: usize,
    pub verneed_bytes: &'a [u8],
    pub verneed_num: usize,
    pub strtab: &'a [u8],
}

impl<'a> VersionTable<'a> {
    /// Look up the version index for `symbol_idx` from the
    /// `DT_VERSYM` parallel array. Returns `None` if the table is
    /// absent or the index is out of bounds.
    pub fn version_index(&self, symbol_idx: usize) -> Option<u16> {
        self.versym
            .get(symbol_idx)
            .map(|raw| raw & VERSYM_VERSION_MASK)
    }

    /// Look up the version NAME for a defined symbol whose
    /// `version_index` came from `DT_VERSYM`. Walks the `DT_VERDEF`
    /// linked list searching for a record whose `vd_ndx` matches.
    /// Returns `None` when the version table is absent or no
    /// matching record is found.
    pub fn defined_version_name(&self, version_index: u16) -> Option<&'a [u8]> {
        if version_index == VER_NDX_LOCAL || version_index == VER_NDX_GLOBAL {
            return None;
        }
        let mut offset = 0usize;
        for _ in 0..self.verdef_num {
            let verdef = read_verdef(self.verdef_bytes, offset)?;
            if verdef.vd_ndx == version_index && verdef.vd_aux != 0 {
                let aux_offset = offset.checked_add(verdef.vd_aux as usize)?;
                let aux = read_verdaux(self.verdef_bytes, aux_offset)?;
                return strtab_lookup(self.strtab, aux.vda_name);
            }
            if verdef.vd_next == 0 {
                break;
            }
            offset = offset.checked_add(verdef.vd_next as usize)?;
        }
        None
    }

    /// Look up the version NAME a consumer requires for a symbol
    /// whose `version_index` came from `DT_VERSYM`, ignoring the
    /// provider SONAME. Scans every `Verneed` record's `Vernaux`
    /// chain for the first matching `vna_other`. Useful when the
    /// caller doesn't yet know which DSO provides the symbol (the
    /// version index is unique across the consumer's whole
    /// `DT_VERNEED`).
    pub fn required_version_name_by_index(&self, version_index: u16) -> Option<&'a [u8]> {
        if version_index == VER_NDX_LOCAL || version_index == VER_NDX_GLOBAL {
            return None;
        }
        let mut vn_offset = 0usize;
        for _ in 0..self.verneed_num {
            let verneed = read_verneed(self.verneed_bytes, vn_offset)?;
            let mut aux_offset = vn_offset.checked_add(verneed.vn_aux as usize)?;
            for _ in 0..verneed.vn_cnt {
                let aux = read_vernaux(self.verneed_bytes, aux_offset)?;
                if aux.vna_other == version_index {
                    return strtab_lookup(self.strtab, aux.vna_name);
                }
                if aux.vna_next == 0 {
                    break;
                }
                aux_offset = aux_offset.checked_add(aux.vna_next as usize)?;
            }
            if verneed.vn_next == 0 {
                break;
            }
            vn_offset = vn_offset.checked_add(verneed.vn_next as usize)?;
        }
        None
    }

    /// Look up the version NAME a consumer requires for a symbol
    /// whose `version_index` came from `DT_VERSYM` and whose
    /// providing library's SONAME is `provider_soname`. Walks
    /// `DT_VERNEED` for the matching `Verneed`, then its `Vernaux`
    /// chain for the matching `vna_other`.
    pub fn required_version_name(
        &self,
        version_index: u16,
        provider_soname: &[u8],
    ) -> Option<&'a [u8]> {
        if version_index == VER_NDX_LOCAL || version_index == VER_NDX_GLOBAL {
            return None;
        }
        let mut vn_offset = 0usize;
        for _ in 0..self.verneed_num {
            let verneed = read_verneed(self.verneed_bytes, vn_offset)?;
            let soname = strtab_lookup(self.strtab, verneed.vn_file)?;
            if soname == provider_soname {
                let mut aux_offset = vn_offset.checked_add(verneed.vn_aux as usize)?;
                for _ in 0..verneed.vn_cnt {
                    let aux = read_vernaux(self.verneed_bytes, aux_offset)?;
                    if aux.vna_other == version_index {
                        return strtab_lookup(self.strtab, aux.vna_name);
                    }
                    if aux.vna_next == 0 {
                        break;
                    }
                    aux_offset = aux_offset.checked_add(aux.vna_next as usize)?;
                }
            }
            if verneed.vn_next == 0 {
                break;
            }
            vn_offset = vn_offset.checked_add(verneed.vn_next as usize)?;
        }
        None
    }
}

fn read_verdef(bytes: &[u8], offset: usize) -> Option<Verdef> {
    let end = offset.checked_add(core::mem::size_of::<Verdef>())?;
    if end > bytes.len() {
        return None;
    }
    Some(Verdef {
        vd_version: u16::from_le_bytes([bytes[offset], bytes[offset + 1]]),
        vd_flags: u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]),
        vd_ndx: u16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]]),
        vd_cnt: u16::from_le_bytes([bytes[offset + 6], bytes[offset + 7]]),
        vd_hash: u32::from_le_bytes([
            bytes[offset + 8],
            bytes[offset + 9],
            bytes[offset + 10],
            bytes[offset + 11],
        ]),
        vd_aux: u32::from_le_bytes([
            bytes[offset + 12],
            bytes[offset + 13],
            bytes[offset + 14],
            bytes[offset + 15],
        ]),
        vd_next: u32::from_le_bytes([
            bytes[offset + 16],
            bytes[offset + 17],
            bytes[offset + 18],
            bytes[offset + 19],
        ]),
    })
}

fn read_verdaux(bytes: &[u8], offset: usize) -> Option<Verdaux> {
    let end = offset.checked_add(core::mem::size_of::<Verdaux>())?;
    if end > bytes.len() {
        return None;
    }
    Some(Verdaux {
        vda_name: u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]),
        vda_next: u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]),
    })
}

fn read_verneed(bytes: &[u8], offset: usize) -> Option<Verneed> {
    let end = offset.checked_add(core::mem::size_of::<Verneed>())?;
    if end > bytes.len() {
        return None;
    }
    Some(Verneed {
        vn_version: u16::from_le_bytes([bytes[offset], bytes[offset + 1]]),
        vn_cnt: u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]),
        vn_file: u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]),
        vn_aux: u32::from_le_bytes([
            bytes[offset + 8],
            bytes[offset + 9],
            bytes[offset + 10],
            bytes[offset + 11],
        ]),
        vn_next: u32::from_le_bytes([
            bytes[offset + 12],
            bytes[offset + 13],
            bytes[offset + 14],
            bytes[offset + 15],
        ]),
    })
}

fn read_vernaux(bytes: &[u8], offset: usize) -> Option<Vernaux> {
    let end = offset.checked_add(core::mem::size_of::<Vernaux>())?;
    if end > bytes.len() {
        return None;
    }
    Some(Vernaux {
        vna_hash: u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]),
        vna_flags: u16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]]),
        vna_other: u16::from_le_bytes([bytes[offset + 6], bytes[offset + 7]]),
        vna_name: u32::from_le_bytes([
            bytes[offset + 8],
            bytes[offset + 9],
            bytes[offset + 10],
            bytes[offset + 11],
        ]),
        vna_next: u32::from_le_bytes([
            bytes[offset + 12],
            bytes[offset + 13],
            bytes[offset + 14],
            bytes[offset + 15],
        ]),
    })
}

fn strtab_lookup(strtab: &[u8], offset: u32) -> Option<&[u8]> {
    let off = offset as usize;
    if off >= strtab.len() {
        return None;
    }
    let mut end = off;
    while end < strtab.len() && strtab[end] != 0 {
        end += 1;
    }
    Some(&strtab[off..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a hand-rolled `DT_VERDEF` table with two records:
    /// `LIBHELLO_1.0` (vd_ndx=2) and `LIBHELLO_2.0` (vd_ndx=3).
    fn build_verdef_table() -> (Vec<u8>, Vec<u8>) {
        let mut strtab = vec![0u8]; // first byte is the empty string
        let v1_name_off = strtab.len() as u32;
        strtab.extend_from_slice(b"LIBHELLO_1.0\0");
        let v2_name_off = strtab.len() as u32;
        strtab.extend_from_slice(b"LIBHELLO_2.0\0");

        let mut bytes = Vec::new();
        // Verdef #1 (LIBHELLO_1.0):
        //   vd_aux = 20 (Verdef size), vd_next = 28 (Verdef + Verdaux size)
        let verdef1 = Verdef {
            vd_version: 1,
            vd_flags: 0,
            vd_ndx: 2,
            vd_cnt: 1,
            vd_hash: 0x1234,
            vd_aux: 20,
            vd_next: 28,
        };
        let verdaux1 = Verdaux {
            vda_name: v1_name_off,
            vda_next: 0,
        };
        let verdef2 = Verdef {
            vd_version: 1,
            vd_flags: 0,
            vd_ndx: 3,
            vd_cnt: 1,
            vd_hash: 0x5678,
            vd_aux: 20,
            vd_next: 0,
        };
        let verdaux2 = Verdaux {
            vda_name: v2_name_off,
            vda_next: 0,
        };
        bytes.extend_from_slice(&serialize_verdef(&verdef1));
        bytes.extend_from_slice(&serialize_verdaux(&verdaux1));
        bytes.extend_from_slice(&serialize_verdef(&verdef2));
        bytes.extend_from_slice(&serialize_verdaux(&verdaux2));

        (bytes, strtab)
    }

    fn serialize_verdef(v: &Verdef) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[0..2].copy_from_slice(&v.vd_version.to_le_bytes());
        out[2..4].copy_from_slice(&v.vd_flags.to_le_bytes());
        out[4..6].copy_from_slice(&v.vd_ndx.to_le_bytes());
        out[6..8].copy_from_slice(&v.vd_cnt.to_le_bytes());
        out[8..12].copy_from_slice(&v.vd_hash.to_le_bytes());
        out[12..16].copy_from_slice(&v.vd_aux.to_le_bytes());
        out[16..20].copy_from_slice(&v.vd_next.to_le_bytes());
        out
    }

    fn serialize_verdaux(v: &Verdaux) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..4].copy_from_slice(&v.vda_name.to_le_bytes());
        out[4..8].copy_from_slice(&v.vda_next.to_le_bytes());
        out
    }

    fn serialize_verneed(v: &Verneed) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..2].copy_from_slice(&v.vn_version.to_le_bytes());
        out[2..4].copy_from_slice(&v.vn_cnt.to_le_bytes());
        out[4..8].copy_from_slice(&v.vn_file.to_le_bytes());
        out[8..12].copy_from_slice(&v.vn_aux.to_le_bytes());
        out[12..16].copy_from_slice(&v.vn_next.to_le_bytes());
        out
    }

    fn serialize_vernaux(v: &Vernaux) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&v.vna_hash.to_le_bytes());
        out[4..6].copy_from_slice(&v.vna_flags.to_le_bytes());
        out[6..8].copy_from_slice(&v.vna_other.to_le_bytes());
        out[8..12].copy_from_slice(&v.vna_name.to_le_bytes());
        out[12..16].copy_from_slice(&v.vna_next.to_le_bytes());
        out
    }

    #[test]
    fn version_index_masks_hidden_bit() {
        let versym = [0x0002u16, 0x8003u16, 0x0001u16];
        let table = VersionTable {
            versym: &versym,
            verdef_bytes: &[],
            verdef_num: 0,
            verneed_bytes: &[],
            verneed_num: 0,
            strtab: &[],
        };
        assert_eq!(table.version_index(0), Some(2));
        assert_eq!(table.version_index(1), Some(3)); // hidden bit masked
        assert_eq!(table.version_index(2), Some(1));
        assert_eq!(table.version_index(99), None);
    }

    #[test]
    fn defined_version_name_walks_verdef_list() {
        let (verdef_bytes, strtab) = build_verdef_table();
        let table = VersionTable {
            versym: &[],
            verdef_bytes: &verdef_bytes,
            verdef_num: 2,
            verneed_bytes: &[],
            verneed_num: 0,
            strtab: &strtab,
        };
        assert_eq!(table.defined_version_name(2), Some(&b"LIBHELLO_1.0"[..]));
        assert_eq!(table.defined_version_name(3), Some(&b"LIBHELLO_2.0"[..]));
        assert_eq!(table.defined_version_name(99), None);
        // Special indices return None.
        assert_eq!(table.defined_version_name(VER_NDX_LOCAL), None);
        assert_eq!(table.defined_version_name(VER_NDX_GLOBAL), None);
    }

    #[test]
    fn required_version_name_matches_provider_and_index() {
        // Two Verneed records:
        //   * libhello.so wants LIBHELLO_1.0 (vna_other=2)
        //   * libfoo.so wants FOO_3.0 (vna_other=5)
        let mut strtab = vec![0u8];
        let libhello_off = strtab.len() as u32;
        strtab.extend_from_slice(b"libhello.so\0");
        let v1_name_off = strtab.len() as u32;
        strtab.extend_from_slice(b"LIBHELLO_1.0\0");
        let libfoo_off = strtab.len() as u32;
        strtab.extend_from_slice(b"libfoo.so\0");
        let foo_name_off = strtab.len() as u32;
        strtab.extend_from_slice(b"FOO_3.0\0");

        let mut bytes = Vec::new();
        // Verneed 1: libhello.so
        let vn1 = Verneed {
            vn_version: 1,
            vn_cnt: 1,
            vn_file: libhello_off,
            vn_aux: 16,  // Verneed size
            vn_next: 32, // Verneed + Vernaux
        };
        let vna1 = Vernaux {
            vna_hash: 0xAAAA,
            vna_flags: 0,
            vna_other: 2,
            vna_name: v1_name_off,
            vna_next: 0,
        };
        let vn2 = Verneed {
            vn_version: 1,
            vn_cnt: 1,
            vn_file: libfoo_off,
            vn_aux: 16,
            vn_next: 0,
        };
        let vna2 = Vernaux {
            vna_hash: 0xBBBB,
            vna_flags: 0,
            vna_other: 5,
            vna_name: foo_name_off,
            vna_next: 0,
        };
        bytes.extend_from_slice(&serialize_verneed(&vn1));
        bytes.extend_from_slice(&serialize_vernaux(&vna1));
        bytes.extend_from_slice(&serialize_verneed(&vn2));
        bytes.extend_from_slice(&serialize_vernaux(&vna2));

        let table = VersionTable {
            versym: &[],
            verdef_bytes: &[],
            verdef_num: 0,
            verneed_bytes: &bytes,
            verneed_num: 2,
            strtab: &strtab,
        };
        assert_eq!(
            table.required_version_name(2, b"libhello.so"),
            Some(&b"LIBHELLO_1.0"[..])
        );
        assert_eq!(
            table.required_version_name(5, b"libfoo.so"),
            Some(&b"FOO_3.0"[..])
        );
        // Wrong provider — should miss.
        assert_eq!(table.required_version_name(2, b"libfoo.so"), None);
        // Wrong index — should miss.
        assert_eq!(table.required_version_name(99, b"libhello.so"), None);
    }

    #[test]
    fn malformed_verdef_offsets_return_none() {
        // verdef_num claims 1 record, but bytes is too short for even
        // a single Verdef.
        let table = VersionTable {
            versym: &[],
            verdef_bytes: &[0u8; 10], // < 20 bytes
            verdef_num: 1,
            verneed_bytes: &[],
            verneed_num: 0,
            strtab: &[0u8; 4],
        };
        assert_eq!(table.defined_version_name(2), None);
    }
}
