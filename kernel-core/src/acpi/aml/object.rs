//! AML value/object model, error type, and the `RegionSpace` hardware seam.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::acpi::namespace::NodeId;

/// Everything that can go wrong while decoding or evaluating AML. The
/// interpreter runs untrusted firmware bytecode, so every malformed-input
/// path must surface as one of these — never a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmlError {
    /// The byte stream ended before a complete encoding was read.
    Truncated,
    /// A `PkgLength` was self-inconsistent (zero, reserved bits set, or
    /// extending past the enclosing buffer).
    BadPkgLength,
    /// A `NameString` violated the lexical grammar (bad lead byte or
    /// segment character).
    BadNameString,
    /// An opcode outside the implemented enumeration subset. Carries the
    /// lead byte; extended (`0x5B`-prefixed) opcodes report the second
    /// byte with bit 15 set in [`AmlError::UnsupportedOpcode`]'s payload
    /// (`0x8000 | ext`).
    UnsupportedOpcode(u16),
    /// A `NameString` did not resolve to a namespace node.
    UnresolvedName,
    /// A method was invoked with the wrong number of arguments, or an
    /// `ArgN`/`LocalN` index was out of range.
    BadArg,
    /// Method-call / package nesting exceeded [`super::interp::MAX_DEPTH`].
    RecursionLimit,
    /// The global executed-operation budget [`super::interp::MAX_OPS`]
    /// was exhausted (runaway `While`).
    LoopLimit,
    /// An operand had a type the operator cannot accept.
    TypeMismatch,
    /// Integer division or modulus by zero.
    DivideByZero,
    /// An `Index` subscript was outside the buffer/package/string.
    IndexOutOfRange,
    /// The [`RegionSpace`] backend refused an `OperationRegion` access.
    RegionAccess,
    /// A namespace path attempted to create a node whose parent segment
    /// does not exist, or `^` walked above the root.
    BadScope,
    /// Structural invariant violated while evaluating (e.g. `Else`
    /// without `If`); indicates malformed AML.
    Malformed,
}

/// An evaluated AML object. This is the value domain of the interpreter:
/// everything a `Name`, `LocalN`, `ArgN`, or method return can hold.
#[derive(Debug, Clone, PartialEq)]
pub enum AmlValue {
    /// The `Uninitialized` object type (fresh locals, absent args).
    Uninitialized,
    /// `Integer` — always 64-bit (all tables this interpreter accepts
    /// declare `ComplianceRevision >= 2`; revision-1 32-bit arithmetic is
    /// deliberately not modeled).
    Integer(u64),
    /// `String`.
    String(String),
    /// `Buffer`.
    Buffer(Vec<u8>),
    /// `Package` / `VarPackage` elements.
    Package(Vec<AmlValue>),
    /// A reference to a namespace node (`RefOf` / `CondRefOf` results,
    /// method values stored by name). `DerefOf` reads through it.
    ObjectRef(NodeId),
    /// A name path recorded inside a `Package` that was not resolvable at
    /// load time (forward reference). Resolved lazily on `DerefOf` or
    /// when consumed by a query.
    NamePath(String),
}

impl AmlValue {
    /// Integer coercion per the implicit-conversion subset: integers pass
    /// through; buffers/strings convert where the spec mandates it for
    /// operator operands. Everything else is a `TypeMismatch`.
    pub fn as_integer(&self) -> Result<u64, AmlError> {
        match self {
            AmlValue::Integer(v) => Ok(*v),
            // Buffer → Integer: little-endian, first 8 bytes (ACPI 6.5
            // §19.3.5.5 implicit source conversion).
            AmlValue::Buffer(b) => {
                let mut v: u64 = 0;
                for (i, byte) in b.iter().take(8).enumerate() {
                    v |= (*byte as u64) << (8 * i);
                }
                Ok(v)
            }
            // String → Integer: hexadecimal prefix-free parse, per the
            // implicit-conversion table (stops at the first non-hex char).
            AmlValue::String(s) => {
                let mut v: u64 = 0;
                let mut any = false;
                for c in s.bytes() {
                    let d = match c {
                        b'0'..=b'9' => (c - b'0') as u64,
                        b'a'..=b'f' => (c - b'a' + 10) as u64,
                        b'A'..=b'F' => (c - b'A' + 10) as u64,
                        _ => break,
                    };
                    v = v.wrapping_shl(4) | d;
                    any = true;
                }
                if any {
                    Ok(v)
                } else {
                    Err(AmlError::TypeMismatch)
                }
            }
            _ => Err(AmlError::TypeMismatch),
        }
    }

    /// The AML `ObjectType` operator's numeric code for this value.
    pub fn object_type(&self) -> u64 {
        match self {
            AmlValue::Uninitialized => 0,
            AmlValue::Integer(_) => 1,
            AmlValue::String(_) => 2,
            AmlValue::Buffer(_) => 3,
            AmlValue::Package(_) => 4,
            AmlValue::ObjectRef(_) | AmlValue::NamePath(_) => 6, // Reference
        }
    }
}

/// Field access widths from a `Field` element's `FieldFlags` (bits 3:0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    /// `AnyAcc` — the interpreter picks; we use the widest width that
    /// stays within the region and naturally aligns (capped at 32-bit,
    /// matching ACPICA's default for `AnyAcc`).
    Any,
    Byte,
    Word,
    DWord,
    QWord,
}

impl AccessType {
    pub fn from_flags(flags: u8) -> AccessType {
        match flags & 0x0F {
            1 => AccessType::Byte,
            2 => AccessType::Word,
            3 => AccessType::DWord,
            4 => AccessType::QWord,
            // 0 = AnyAcc; 5 = BufferAcc (SMBus/GPIO transfer protocols —
            // outside the subset, treated as byte-wise); others reserved.
            _ => AccessType::Any,
        }
    }

    /// Access width in bits (resolving `Any` to 8 — the safe floor every
    /// region type supports; wider merging is an optimization, not a
    /// correctness requirement, for the enumeration subset).
    pub fn bits(self) -> u32 {
        match self {
            AccessType::Any | AccessType::Byte => 8,
            AccessType::Word => 16,
            AccessType::DWord => 32,
            AccessType::QWord => 64,
        }
    }
}

/// A named field unit: a bit-range window onto an `OperationRegion`.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldUnit {
    /// The `OperationRegion` node this unit windows onto.
    pub region: NodeId,
    /// Absolute bit offset within the region.
    pub bit_offset: u64,
    /// Width in bits.
    pub bit_len: u32,
    /// Access width for the underlying region reads/writes.
    pub access: AccessType,
}

/// An `IndexField` unit: reads/writes go through an (index, data) field
/// pair instead of a region (the classic banked-register idiom).
#[derive(Debug, Clone, PartialEq)]
pub struct IndexFieldUnit {
    /// Field unit written with the byte offset to select.
    pub index: NodeId,
    /// Field unit then read/written for the data.
    pub data: NodeId,
    pub bit_offset: u64,
    pub bit_len: u32,
    pub access: AccessType,
}

/// A buffer field created by `CreateByteField`/`CreateWordField`/… over a
/// *named* buffer object. (Buffer fields over `LocalN` sources are outside
/// the subset — the enumeration paths never need them.)
#[derive(Debug, Clone, PartialEq)]
pub struct BufferField {
    /// The node whose `Name` object holds the source buffer.
    pub buffer: NodeId,
    pub bit_offset: u64,
    pub bit_len: u32,
}

/// The seam where AML touches hardware: `OperationRegion` accesses.
///
/// The interpreter is pure logic; every `SystemMemory` / `SystemIO` /
/// `PCI_Config` / `EmbeddedController` / `GeneralPurposeIo` read or write
/// is delegated here. Host tests use [`MockRegionSpace`]; the production
/// ring-3 `acpid` implements this over the capability-gated `device_host`
/// syscalls (Track E).
///
/// `space` is the raw `RegionSpace` byte from the `OperationRegion`
/// declaration (0 = SystemMemory, 1 = SystemIO, 2 = PCI_Config, 3 =
/// EmbeddedControl, …). `addr` is absolute within that space (region base
/// already applied). `width_bits` ∈ {8, 16, 32, 64}.
pub trait RegionSpace {
    fn read(&mut self, space: u8, addr: u64, width_bits: u32) -> Result<u64, AmlError>;
    fn write(&mut self, space: u8, addr: u64, width_bits: u32, value: u64) -> Result<(), AmlError>;

    /// `Sleep`/`Stall` hook. Pure-logic default: no-op (host tests may
    /// count calls; `acpid` sleeps for real).
    fn sleep_ms(&mut self, _ms: u64) {}
}

/// Byte-addressed sparse mock backend for host tests: reads return
/// whatever was written (or seeded), unwritten bytes read as `0x00`.
/// Little-endian multi-byte assembly, matching x86 region semantics.
#[derive(Default)]
pub struct MockRegionSpace {
    /// (space, byte address) → byte value.
    pub bytes: BTreeMap<(u8, u64), u8>,
    /// Access log for assertions: (space, addr, width_bits, write?).
    pub log: Vec<(u8, u64, u32, bool)>,
    /// Total milliseconds requested via `sleep_ms`.
    pub slept_ms: u64,
}

impl MockRegionSpace {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a little-endian value at (space, addr).
    pub fn seed(&mut self, space: u8, addr: u64, width_bits: u32, value: u64) {
        for i in 0..(width_bits / 8) as u64 {
            self.bytes
                .insert((space, addr + i), (value >> (8 * i)) as u8);
        }
    }
}

impl RegionSpace for MockRegionSpace {
    fn read(&mut self, space: u8, addr: u64, width_bits: u32) -> Result<u64, AmlError> {
        self.log.push((space, addr, width_bits, false));
        let mut v: u64 = 0;
        for i in 0..(width_bits / 8) as u64 {
            let b = *self.bytes.get(&(space, addr + i)).unwrap_or(&0);
            v |= (b as u64) << (8 * i);
        }
        Ok(v)
    }

    fn write(&mut self, space: u8, addr: u64, width_bits: u32, value: u64) -> Result<(), AmlError> {
        self.log.push((space, addr, width_bits, true));
        for i in 0..(width_bits / 8) as u64 {
            self.bytes
                .insert((space, addr + i), (value >> (8 * i)) as u8);
        }
        Ok(())
    }

    fn sleep_ms(&mut self, ms: u64) {
        self.slept_ms += ms;
    }
}

/// Assignment destinations for `Store` and operator `Target`s, produced
/// by the interpreter's target-evaluation pass.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// `Zero` in target position = "no destination" (discard).
    Null,
    Local(u8),
    Arg(u8),
    /// A namespace node (a `Name` value, a `FieldUnit`, …).
    Node(NodeId),
    /// One element of a container reached via `Index`; the base chain
    /// bottoms out at a `Local`/`Arg`/`Node`.
    Index(Box<Target>, u64),
    /// The `Debug` object — stores are logged and discarded.
    Debug,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_coercions() {
        assert_eq!(AmlValue::Integer(7).as_integer(), Ok(7));
        // Buffer → LE integer, first 8 bytes.
        assert_eq!(
            AmlValue::Buffer(alloc::vec![0x34, 0x12]).as_integer(),
            Ok(0x1234)
        );
        // String → hex parse stopping at non-hex.
        assert_eq!(
            AmlValue::String(String::from("1A2b")).as_integer(),
            Ok(0x1A2B)
        );
        assert_eq!(
            AmlValue::String(String::from("zz")).as_integer(),
            Err(AmlError::TypeMismatch)
        );
        assert_eq!(
            AmlValue::Package(Vec::new()).as_integer(),
            Err(AmlError::TypeMismatch)
        );
    }

    #[test]
    fn mock_region_space_round_trips_and_logs() {
        let mut m = MockRegionSpace::new();
        m.write(1, 0x62, 16, 0xBEEF).unwrap();
        assert_eq!(m.read(1, 0x62, 16).unwrap(), 0xBEEF);
        assert_eq!(m.read(1, 0x63, 8).unwrap(), 0xBE);
        // Different space, same address → independent bytes.
        assert_eq!(m.read(0, 0x62, 16).unwrap(), 0);
        assert_eq!(m.log.len(), 4);
        assert!(m.log[0].3, "first access was a write");
    }

    #[test]
    fn access_type_decode() {
        assert_eq!(AccessType::from_flags(0x01), AccessType::Byte);
        assert_eq!(AccessType::from_flags(0x43), AccessType::DWord);
        assert_eq!(AccessType::from_flags(0x00), AccessType::Any);
        assert_eq!(AccessType::QWord.bits(), 64);
    }
}
