//! Phase 103 A — a wire codec for [`AmlValue`], the reply format of
//! acpid's `ACPI_EVAL` IPC verb.
//!
//! The Phase 101 split hosts the AML interpreter in ring-3 `acpid`;
//! Phase 103's `powerd` (and later consumers) evaluate control methods
//! (`_BST`, `_BIF`, `_PSR`, `_TMP`, …) by IPC and receive the resulting
//! [`AmlValue`] in this encoding. Tagged, length-prefixed, bounded:
//!
//! ```text
//! value   := tag(u8) payload
//! tag 0   := Uninitialized                 (no payload)
//! tag 1   := Integer                       u64 LE
//! tag 2   := String                        u16 LE len + bytes (UTF-8)
//! tag 3   := Buffer                        u16 LE len + bytes
//! tag 4   := Package                       u16 LE count + count × value
//! tag 6   := ObjectRef                     u32 LE raw NodeId (opaque —
//!            only meaningful inside the emitting acpid's namespace)
//! tag 7   := NamePath                      u16 LE len + bytes
//! ```
//!
//! Tags mirror [`AmlValue::object_type`] where one exists (5 is the
//! ACPI FieldUnit type, which never escapes evaluation). Decoders are
//! adversarial-input-safe: bounded depth, no panics, every failure a
//! typed [`AmlError`].

use alloc::string::String;
use alloc::vec::Vec;

use super::object::{AmlError, AmlValue};
use crate::acpi::namespace::NodeId;

const TAG_UNINITIALIZED: u8 = 0;
const TAG_INTEGER: u8 = 1;
const TAG_STRING: u8 = 2;
const TAG_BUFFER: u8 = 3;
const TAG_PACKAGE: u8 = 4;
const TAG_OBJECT_REF: u8 = 6;
const TAG_NAME_PATH: u8 = 7;

/// Nesting bound for `Package` values (matches the interpreter's own
/// conservative posture — real power/thermal packages nest ≤ 2 deep).
const MAX_DEPTH: usize = 8;

/// Byte bound for one encoded value — fits an IPC bulk reply.
pub const MAX_ENCODED_LEN: usize = 4096;

/// Encode `value`; fails with [`AmlError::Malformed`] if the encoding
/// would exceed [`MAX_ENCODED_LEN`] or nest deeper than [`MAX_DEPTH`].
pub fn encode(value: &AmlValue) -> Result<Vec<u8>, AmlError> {
    let mut out = Vec::new();
    encode_into(value, &mut out, 0)?;
    Ok(out)
}

fn encode_into(value: &AmlValue, out: &mut Vec<u8>, depth: usize) -> Result<(), AmlError> {
    if depth > MAX_DEPTH {
        return Err(AmlError::RecursionLimit);
    }
    match value {
        AmlValue::Uninitialized => out.push(TAG_UNINITIALIZED),
        AmlValue::Integer(v) => {
            out.push(TAG_INTEGER);
            out.extend_from_slice(&v.to_le_bytes());
        }
        AmlValue::String(s) => {
            out.push(TAG_STRING);
            push_len_bytes(s.as_bytes(), out)?;
        }
        AmlValue::Buffer(b) => {
            out.push(TAG_BUFFER);
            push_len_bytes(b, out)?;
        }
        AmlValue::Package(elems) => {
            out.push(TAG_PACKAGE);
            let count = u16::try_from(elems.len()).map_err(|_| AmlError::Malformed)?;
            out.extend_from_slice(&count.to_le_bytes());
            for e in elems {
                encode_into(e, out, depth + 1)?;
            }
        }
        AmlValue::ObjectRef(node) => {
            out.push(TAG_OBJECT_REF);
            out.extend_from_slice(&node.0.to_le_bytes());
        }
        AmlValue::NamePath(p) => {
            out.push(TAG_NAME_PATH);
            push_len_bytes(p.as_bytes(), out)?;
        }
    }
    if out.len() > MAX_ENCODED_LEN {
        return Err(AmlError::Malformed);
    }
    Ok(())
}

fn push_len_bytes(bytes: &[u8], out: &mut Vec<u8>) -> Result<(), AmlError> {
    let len = u16::try_from(bytes.len()).map_err(|_| AmlError::Malformed)?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

/// Decode one value from `buf`; returns the value and bytes consumed.
pub fn decode(buf: &[u8]) -> Result<(AmlValue, usize), AmlError> {
    decode_at(buf, 0, 0)
}

fn decode_at(buf: &[u8], pos: usize, depth: usize) -> Result<(AmlValue, usize), AmlError> {
    if depth > MAX_DEPTH {
        return Err(AmlError::RecursionLimit);
    }
    let tag = *buf.get(pos).ok_or(AmlError::Truncated)?;
    let mut cursor = pos + 1;
    let value = match tag {
        TAG_UNINITIALIZED => AmlValue::Uninitialized,
        TAG_INTEGER => {
            let bytes = buf
                .get(cursor..cursor + 8)
                .ok_or(AmlError::Truncated)?
                .try_into()
                .map_err(|_| AmlError::Truncated)?;
            cursor += 8;
            AmlValue::Integer(u64::from_le_bytes(bytes))
        }
        TAG_STRING => {
            let (bytes, next) = read_len_bytes(buf, cursor)?;
            cursor = next;
            let s = core::str::from_utf8(bytes).map_err(|_| AmlError::Malformed)?;
            AmlValue::String(String::from(s))
        }
        TAG_BUFFER => {
            let (bytes, next) = read_len_bytes(buf, cursor)?;
            cursor = next;
            AmlValue::Buffer(bytes.to_vec())
        }
        TAG_PACKAGE => {
            let count_bytes = buf
                .get(cursor..cursor + 2)
                .ok_or(AmlError::Truncated)?
                .try_into()
                .map_err(|_| AmlError::Truncated)?;
            cursor += 2;
            let count = u16::from_le_bytes(count_bytes) as usize;
            let mut elems = Vec::with_capacity(count.min(64));
            for _ in 0..count {
                let (elem, consumed) = decode_at(buf, cursor, depth + 1)?;
                cursor += consumed;
                elems.push(elem);
            }
            AmlValue::Package(elems)
        }
        TAG_OBJECT_REF => {
            let bytes = buf
                .get(cursor..cursor + 4)
                .ok_or(AmlError::Truncated)?
                .try_into()
                .map_err(|_| AmlError::Truncated)?;
            cursor += 4;
            AmlValue::ObjectRef(NodeId(u32::from_le_bytes(bytes)))
        }
        TAG_NAME_PATH => {
            let (bytes, next) = read_len_bytes(buf, cursor)?;
            cursor = next;
            let s = core::str::from_utf8(bytes).map_err(|_| AmlError::Malformed)?;
            AmlValue::NamePath(String::from(s))
        }
        _ => return Err(AmlError::Malformed),
    };
    Ok((value, cursor - pos))
}

fn read_len_bytes(buf: &[u8], pos: usize) -> Result<(&[u8], usize), AmlError> {
    let len_bytes = buf
        .get(pos..pos + 2)
        .ok_or(AmlError::Truncated)?
        .try_into()
        .map_err(|_| AmlError::Truncated)?;
    let len = u16::from_le_bytes(len_bytes) as usize;
    let start = pos + 2;
    let bytes = buf.get(start..start + len).ok_or(AmlError::Truncated)?;
    Ok((bytes, start + len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn round_trip(v: AmlValue) {
        let bytes = encode(&v).expect("encode");
        let (back, consumed) = decode(&bytes).expect("decode");
        assert_eq!(back, v);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn round_trips_every_variant() {
        round_trip(AmlValue::Uninitialized);
        round_trip(AmlValue::Integer(0));
        round_trip(AmlValue::Integer(u64::MAX));
        round_trip(AmlValue::String(String::from("PNP0C0A")));
        round_trip(AmlValue::Buffer(vec![0u8, 1, 2, 255]));
        round_trip(AmlValue::ObjectRef(NodeId(42)));
        round_trip(AmlValue::NamePath(String::from("\\_SB.BAT0")));
    }

    #[test]
    fn round_trips_a_bst_shaped_package() {
        // A realistic `_BST` result: [state, rate, remaining, voltage].
        round_trip(AmlValue::Package(vec![
            AmlValue::Integer(1),
            AmlValue::Integer(1500),
            AmlValue::Integer(43_200),
            AmlValue::Integer(11_400),
        ]));
    }

    #[test]
    fn round_trips_a_bif_shaped_nested_package() {
        // `_BIF` mixes integers and strings; nest one package for depth.
        round_trip(AmlValue::Package(vec![
            AmlValue::Integer(0),
            AmlValue::Integer(56_999),
            AmlValue::Integer(50_110),
            AmlValue::Integer(1),
            AmlValue::Integer(11_400),
            AmlValue::String(String::from("DELL M59JH14")),
            AmlValue::Package(vec![AmlValue::Integer(7)]),
        ]));
    }

    #[test]
    fn truncation_never_panics() {
        let full = encode(&AmlValue::Package(vec![
            AmlValue::String(String::from("abc")),
            AmlValue::Integer(7),
        ]))
        .unwrap();
        for cut in 0..full.len() {
            // Every prefix either decodes to something shorter or errors.
            let _ = decode(&full[..cut]);
        }
    }

    #[test]
    fn depth_limit_rejects_deep_nesting() {
        let mut v = AmlValue::Integer(1);
        for _ in 0..12 {
            v = AmlValue::Package(vec![v]);
        }
        assert_eq!(encode(&v), Err(AmlError::RecursionLimit));
    }

    #[test]
    fn unknown_tag_is_malformed() {
        assert_eq!(decode(&[9u8]), Err(AmlError::Malformed));
    }
}
