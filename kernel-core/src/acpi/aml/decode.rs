//! AML lexical layer: the byte-stream cursor, `PkgLength`, `NameString`,
//! and the opcode constants (ACPI 6.5 §20.2).
//!
//! Everything here is bounds-checked: running off the end of the stream
//! is [`AmlError::Truncated`], never a panic or a wrap.

use alloc::string::String;
use alloc::vec::Vec;

use super::object::AmlError;

// ---------------------------------------------------------------------------
// Opcodes (§20.2.5). Only the enumeration subset is listed; anything not
// named here surfaces as `AmlError::UnsupportedOpcode`.
// ---------------------------------------------------------------------------

pub const ZERO_OP: u8 = 0x00;
pub const ONE_OP: u8 = 0x01;
pub const ALIAS_OP: u8 = 0x06;
pub const NAME_OP: u8 = 0x08;
pub const BYTE_PREFIX: u8 = 0x0A;
pub const WORD_PREFIX: u8 = 0x0B;
pub const DWORD_PREFIX: u8 = 0x0C;
pub const STRING_PREFIX: u8 = 0x0D;
pub const QWORD_PREFIX: u8 = 0x0E;
pub const SCOPE_OP: u8 = 0x10;
pub const BUFFER_OP: u8 = 0x11;
pub const PACKAGE_OP: u8 = 0x12;
pub const VAR_PACKAGE_OP: u8 = 0x13;
pub const METHOD_OP: u8 = 0x14;
pub const EXTERNAL_OP: u8 = 0x15;
pub const DUAL_NAME_PREFIX: u8 = 0x2E;
pub const MULTI_NAME_PREFIX: u8 = 0x2F;
pub const EXT_OP_PREFIX: u8 = 0x5B;
pub const ROOT_CHAR: u8 = 0x5C; // '\'
pub const PARENT_PREFIX_CHAR: u8 = 0x5E; // '^'
pub const LOCAL0_OP: u8 = 0x60;
pub const LOCAL7_OP: u8 = 0x67;
pub const ARG0_OP: u8 = 0x68;
pub const ARG6_OP: u8 = 0x6E;
pub const STORE_OP: u8 = 0x70;
pub const REF_OF_OP: u8 = 0x71;
pub const ADD_OP: u8 = 0x72;
pub const CONCAT_OP: u8 = 0x73;
pub const SUBTRACT_OP: u8 = 0x74;
pub const INCREMENT_OP: u8 = 0x75;
pub const DECREMENT_OP: u8 = 0x76;
pub const MULTIPLY_OP: u8 = 0x77;
pub const DIVIDE_OP: u8 = 0x78;
pub const SHIFT_LEFT_OP: u8 = 0x79;
pub const SHIFT_RIGHT_OP: u8 = 0x7A;
pub const AND_OP: u8 = 0x7B;
pub const NAND_OP: u8 = 0x7C;
pub const OR_OP: u8 = 0x7D;
pub const NOR_OP: u8 = 0x7E;
pub const XOR_OP: u8 = 0x7F;
pub const NOT_OP: u8 = 0x80;
pub const FIND_SET_LEFT_BIT_OP: u8 = 0x81;
pub const FIND_SET_RIGHT_BIT_OP: u8 = 0x82;
pub const DEREF_OF_OP: u8 = 0x83;
pub const CONCAT_RES_OP: u8 = 0x84;
pub const MOD_OP: u8 = 0x85;
pub const NOTIFY_OP: u8 = 0x86;
pub const SIZE_OF_OP: u8 = 0x87;
pub const INDEX_OP: u8 = 0x88;
pub const MATCH_OP: u8 = 0x89;
pub const CREATE_DWORD_FIELD_OP: u8 = 0x8A;
pub const CREATE_WORD_FIELD_OP: u8 = 0x8B;
pub const CREATE_BYTE_FIELD_OP: u8 = 0x8C;
pub const CREATE_BIT_FIELD_OP: u8 = 0x8D;
pub const OBJECT_TYPE_OP: u8 = 0x8E;
pub const CREATE_QWORD_FIELD_OP: u8 = 0x8F;
pub const LAND_OP: u8 = 0x90;
pub const LOR_OP: u8 = 0x91;
pub const LNOT_OP: u8 = 0x92;
pub const LEQUAL_OP: u8 = 0x93;
pub const LGREATER_OP: u8 = 0x94;
pub const LLESS_OP: u8 = 0x95;
pub const TO_BUFFER_OP: u8 = 0x96;
pub const TO_DECIMAL_STRING_OP: u8 = 0x97;
pub const TO_HEX_STRING_OP: u8 = 0x98;
pub const TO_INTEGER_OP: u8 = 0x99;
pub const TO_STRING_OP: u8 = 0x9C;
pub const COPY_OBJECT_OP: u8 = 0x9D;
pub const MID_OP: u8 = 0x9E;
pub const CONTINUE_OP: u8 = 0x9F;
pub const IF_OP: u8 = 0xA0;
pub const ELSE_OP: u8 = 0xA1;
pub const WHILE_OP: u8 = 0xA2;
pub const NOOP_OP: u8 = 0xA3;
pub const RETURN_OP: u8 = 0xA4;
pub const BREAK_OP: u8 = 0xA5;
pub const BREAK_POINT_OP: u8 = 0xCC;
pub const ONES_OP: u8 = 0xFF;

// Extended opcodes: the byte following EXT_OP_PREFIX (0x5B).
pub const EXT_MUTEX_OP: u8 = 0x01;
pub const EXT_EVENT_OP: u8 = 0x02;
pub const EXT_COND_REF_OF_OP: u8 = 0x12;
pub const EXT_CREATE_FIELD_OP: u8 = 0x13;
pub const EXT_STALL_OP: u8 = 0x21;
pub const EXT_SLEEP_OP: u8 = 0x22;
pub const EXT_ACQUIRE_OP: u8 = 0x23;
pub const EXT_SIGNAL_OP: u8 = 0x24;
pub const EXT_WAIT_OP: u8 = 0x25;
pub const EXT_RESET_OP: u8 = 0x26;
pub const EXT_RELEASE_OP: u8 = 0x27;
pub const EXT_FROM_BCD_OP: u8 = 0x28;
pub const EXT_TO_BCD_OP: u8 = 0x29;
pub const EXT_REVISION_OP: u8 = 0x30;
pub const EXT_DEBUG_OP: u8 = 0x31;
pub const EXT_FATAL_OP: u8 = 0x32;
pub const EXT_TIMER_OP: u8 = 0x33;
pub const EXT_OP_REGION_OP: u8 = 0x80;
pub const EXT_FIELD_OP: u8 = 0x81;
pub const EXT_DEVICE_OP: u8 = 0x82;
pub const EXT_PROCESSOR_OP: u8 = 0x83;
pub const EXT_POWER_RES_OP: u8 = 0x84;
pub const EXT_THERMAL_ZONE_OP: u8 = 0x85;
pub const EXT_INDEX_FIELD_OP: u8 = 0x86;
pub const EXT_BANK_FIELD_OP: u8 = 0x87;

/// Size of the standard ACPI System Descriptor Table header that prefixes
/// every definition block (DSDT/SSDT); the AML byte stream starts after it.
pub const ACPI_SDT_HEADER_LEN: usize = 36;

/// A parsed `NameString`: optional root anchor or `^` hops, then 0..N
/// four-byte name segments. `segs.is_empty()` with neither anchor is the
/// grammar's `NullName`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamePath {
    /// Leading `\` — resolve from the namespace root.
    pub root: bool,
    /// Number of leading `^` — resolve from the Nth ancestor scope.
    pub parent_hops: u8,
    pub segs: Vec<[u8; 4]>,
}

impl NamePath {
    /// A single absolute segment (`\SEG_`), for tests and built-ins.
    pub fn absolute(seg: [u8; 4]) -> NamePath {
        NamePath {
            root: true,
            parent_hops: 0,
            segs: alloc::vec![seg],
        }
    }

    /// Render as ASL-style text (`\_SB.PCI0.I2C1`), for diagnostics and
    /// [`super::object::AmlValue::NamePath`] storage.
    pub fn display(&self) -> String {
        let mut s = String::new();
        if self.root {
            s.push('\\');
        }
        for _ in 0..self.parent_hops {
            s.push('^');
        }
        for (i, seg) in self.segs.iter().enumerate() {
            if i > 0 {
                s.push('.');
            }
            // Render the ASL convention: trim the `_` padding NameSegs
            // are stored with (`_SB_` displays as `_SB`).
            let len = seg.iter().rposition(|&b| b != b'_').map_or(1, |p| p + 1);
            for &b in &seg[..len] {
                s.push(b as char);
            }
        }
        s
    }
}

/// Is `b` valid as a NameSeg lead character (`A`-`Z` or `_`)?
pub fn is_lead_name_char(b: u8) -> bool {
    b == b'_' || b.is_ascii_uppercase()
}

/// Is `b` valid as a NameSeg trailing character (lead chars plus `0`-`9`)?
pub fn is_name_char(b: u8) -> bool {
    is_lead_name_char(b) || b.is_ascii_digit()
}

/// Bounds-checked cursor over an AML byte slice.
#[derive(Clone)]
pub struct Stream<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Stream<'a> {
    pub fn new(data: &'a [u8]) -> Stream<'a> {
        Stream { data, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Reposition the cursor (used to skip to a known `PkgLength` end).
    /// `pos` may equal `len` (exhausted stream) but not exceed it.
    pub fn seek(&mut self, pos: usize) -> Result<(), AmlError> {
        if pos > self.data.len() {
            return Err(AmlError::Truncated);
        }
        self.pos = pos;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.data.get(self.pos + ahead).copied()
    }

    pub fn next_u8(&mut self) -> Result<u8, AmlError> {
        let b = *self.data.get(self.pos).ok_or(AmlError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], AmlError> {
        let end = self.pos.checked_add(n).ok_or(AmlError::Truncated)?;
        if end > self.data.len() {
            return Err(AmlError::Truncated);
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn next_u16(&mut self) -> Result<u16, AmlError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn next_u32(&mut self) -> Result<u32, AmlError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn next_u64(&mut self) -> Result<u64, AmlError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Decode a `PkgLength` (§20.2.4) and return the **absolute end
    /// offset** of the package within this stream. The encoded length
    /// counts from the first `PkgLength` byte, so the end is
    /// `start + length`; it must land at or after the cursor and within
    /// the stream.
    pub fn pkg_end(&mut self) -> Result<usize, AmlError> {
        let start = self.pos;
        let length = self.pkg_length_value()?;
        let end = start.checked_add(length).ok_or(AmlError::BadPkgLength)?;
        if end < self.pos || end > self.data.len() {
            return Err(AmlError::BadPkgLength);
        }
        Ok(end)
    }

    /// Decode a `PkgLength` encoding as a raw *number* — the `FieldList`
    /// grammar reuses the encoding for bit counts (`NamedField` widths,
    /// `ReservedField` padding), where it is not an extent and must not
    /// be bounds-checked against the stream.
    pub fn pkg_length_value(&mut self) -> Result<usize, AmlError> {
        let lead = self.next_u8()?;
        let follow = (lead >> 6) as usize;
        if follow == 0 {
            return Ok((lead & 0x3F) as usize);
        }
        // Multi-byte form: lead bits 5:4 must be zero; lead bits 3:0
        // are the least-significant nibble.
        if lead & 0x30 != 0 {
            return Err(AmlError::BadPkgLength);
        }
        let mut v = (lead & 0x0F) as usize;
        for i in 0..follow {
            v |= (self.next_u8()? as usize) << (4 + 8 * i);
        }
        Ok(v)
    }

    /// Decode a `NameString` (§20.2.2).
    pub fn name_string(&mut self) -> Result<NamePath, AmlError> {
        let mut path = NamePath {
            root: false,
            parent_hops: 0,
            segs: Vec::new(),
        };
        match self.peek().ok_or(AmlError::Truncated)? {
            ROOT_CHAR => {
                self.pos += 1;
                path.root = true;
            }
            PARENT_PREFIX_CHAR => {
                while self.peek() == Some(PARENT_PREFIX_CHAR) {
                    self.pos += 1;
                    path.parent_hops = path
                        .parent_hops
                        .checked_add(1)
                        .ok_or(AmlError::BadNameString)?;
                }
            }
            _ => {}
        }
        match self.peek().ok_or(AmlError::Truncated)? {
            0x00 => {
                // NullName.
                self.pos += 1;
            }
            DUAL_NAME_PREFIX => {
                self.pos += 1;
                path.segs.push(self.name_seg()?);
                path.segs.push(self.name_seg()?);
            }
            MULTI_NAME_PREFIX => {
                self.pos += 1;
                let count = self.next_u8()? as usize;
                for _ in 0..count {
                    path.segs.push(self.name_seg()?);
                }
            }
            b if is_lead_name_char(b) => {
                path.segs.push(self.name_seg()?);
            }
            _ => return Err(AmlError::BadNameString),
        }
        Ok(path)
    }

    fn name_seg(&mut self) -> Result<[u8; 4], AmlError> {
        let raw = self.take(4)?;
        if !is_lead_name_char(raw[0])
            || !is_name_char(raw[1])
            || !is_name_char(raw[2])
            || !is_name_char(raw[3])
        {
            return Err(AmlError::BadNameString);
        }
        Ok([raw[0], raw[1], raw[2], raw[3]])
    }

    /// Does the byte at the cursor start a `NameString`? (Used to
    /// distinguish a term-position name/method-invocation from an opcode.)
    pub fn at_name_string(&self) -> bool {
        matches!(
            self.peek(),
            Some(ROOT_CHAR)
                | Some(PARENT_PREFIX_CHAR)
                | Some(DUAL_NAME_PREFIX)
                | Some(MULTI_NAME_PREFIX)
        ) || self.peek().map(is_lead_name_char).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_length_one_byte() {
        // Lead 0x0A → length 10, end = 0 + 10.
        let data = [0x0Au8; 10];
        let mut s = Stream::new(&data);
        assert_eq!(s.pkg_end(), Ok(10));
        assert_eq!(s.pos(), 1);
    }

    #[test]
    fn pkg_length_multi_byte() {
        // Two-byte form: lead 0x44 (follow=1, low nibble 4), next 0x02 →
        // length = 4 | 2<<4 = 0x24 = 36.
        let mut data = alloc::vec![0x44u8, 0x02];
        data.resize(36, 0);
        let mut s = Stream::new(&data);
        assert_eq!(s.pkg_end(), Ok(36));
        assert_eq!(s.pos(), 2);
    }

    #[test]
    fn pkg_length_rejects_reserved_bits_and_overrun() {
        // Multi-byte lead with bits 5:4 set is malformed.
        let data = [0x74u8, 0x02, 0, 0];
        assert_eq!(Stream::new(&data).pkg_end(), Err(AmlError::BadPkgLength));
        // Length larger than the buffer is malformed.
        let data = [0x3Fu8, 0, 0];
        assert_eq!(Stream::new(&data).pkg_end(), Err(AmlError::BadPkgLength));
        // Empty stream is truncated.
        assert_eq!(Stream::new(&[]).pkg_end(), Err(AmlError::Truncated));
    }

    #[test]
    fn name_string_forms() {
        // Single segment.
        let mut s = Stream::new(b"PCI0");
        let p = s.name_string().unwrap();
        assert_eq!(p.display(), "PCI0");
        // Root + dual.
        let mut s = Stream::new(b"\\\x2E_SB_PCI0");
        let p = s.name_string().unwrap();
        assert!(p.root);
        assert_eq!(p.display(), "\\_SB.PCI0");
        // Parent hops + multi (3 segs).
        let mut s = Stream::new(b"^^\x2F\x03_SB_PCI0I2C1");
        let p = s.name_string().unwrap();
        assert_eq!(p.parent_hops, 2);
        assert_eq!(p.display(), "^^_SB.PCI0.I2C1");
        // NullName.
        let mut s = Stream::new(&[0x00u8]);
        let p = s.name_string().unwrap();
        assert!(p.segs.is_empty() && !p.root && p.parent_hops == 0);
    }

    #[test]
    fn name_string_rejects_bad_lead() {
        // Lowercase lead byte is not a valid NameSeg start.
        let mut s = Stream::new(b"pci0");
        assert_eq!(s.name_string(), Err(AmlError::BadNameString));
        // Truncated segment.
        let mut s = Stream::new(b"PC");
        assert_eq!(s.name_string(), Err(AmlError::Truncated));
    }

    #[test]
    fn stream_bounds() {
        let mut s = Stream::new(&[1, 2]);
        assert_eq!(s.next_u16(), Ok(0x0201));
        assert_eq!(s.next_u8(), Err(AmlError::Truncated));
        assert!(s.is_empty());
        assert_eq!(s.seek(3), Err(AmlError::Truncated));
        assert_eq!(s.seek(0), Ok(()));
        assert_eq!(s.take(2).unwrap(), &[1, 2]);
    }
}
