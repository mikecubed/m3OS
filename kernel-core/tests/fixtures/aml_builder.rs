//! Tiny AML assembler for integration-test fixtures (Phase 101).
//!
//! Emits the handful of encodings the ACPI tests need — definition-block
//! headers, `PkgLength`, `Scope`/`Device`/`Method`/`Name`, literals, and
//! the touchpad-shaped `_CRS` descriptor bytes — so tests can build
//! synthetic DSDT/SSDT blobs without checking in opaque binaries. This
//! mirrors what `iasl` would produce for the equivalent ASL, kept
//! minimal on purpose (it is a *test* fixture, not a compiler).

/// Encode a `PkgLength` for `body` and return `PkgLength ++ body`. The
/// encoded value counts its own bytes, so pick the width first.
pub fn pkg(body: &[u8]) -> Vec<u8> {
    let l = body.len();
    let mut out = Vec::with_capacity(l + 2);
    if l + 1 <= 0x3F {
        out.push((l + 1) as u8);
    } else if l + 2 <= 0xFFF {
        let v = l + 2;
        out.push(0x40 | (v & 0x0F) as u8);
        out.push((v >> 4) as u8);
    } else {
        let v = l + 3;
        assert!(v <= 0xF_FFFF, "fixture too large");
        out.push(0x80 | (v & 0x0F) as u8);
        out.push(((v >> 4) & 0xFF) as u8);
        out.push(((v >> 12) & 0xFF) as u8);
    }
    out.extend_from_slice(body);
    out
}

/// A 4-char NameSeg, `_`-padded.
pub fn seg(name: &str) -> Vec<u8> {
    let mut s = [b'_'; 4];
    s[..name.len()].copy_from_slice(name.as_bytes());
    s.to_vec()
}

/// `Scope(path) { body }` — `path_bytes` is a pre-encoded NameString.
pub fn scope(path_bytes: &[u8], body: &[u8]) -> Vec<u8> {
    let mut inner = path_bytes.to_vec();
    inner.extend_from_slice(body);
    let mut out = vec![0x10];
    out.extend_from_slice(&pkg(&inner));
    out
}

/// `Device(SEG) { body }`.
pub fn device(name: &str, body: &[u8]) -> Vec<u8> {
    let mut inner = seg(name);
    inner.extend_from_slice(body);
    let mut out = vec![0x5B, 0x82];
    out.extend_from_slice(&pkg(&inner));
    out
}

/// `ThermalZone(SEG) { body }` (Phase 103 C).
pub fn thermal_zone(name: &str, body: &[u8]) -> Vec<u8> {
    let mut inner = seg(name);
    inner.extend_from_slice(body);
    let mut out = vec![0x5B, 0x85];
    out.extend_from_slice(&pkg(&inner));
    out
}

/// `Method(SEG, argc) { body }`.
pub fn method(name: &str, argc: u8, body: &[u8]) -> Vec<u8> {
    let mut inner = seg(name);
    inner.push(argc & 0x07);
    inner.extend_from_slice(body);
    let mut out = vec![0x14];
    out.extend_from_slice(&pkg(&inner));
    out
}

/// `Name(SEG, <value bytes>)`.
pub fn name(nm: &str, value: &[u8]) -> Vec<u8> {
    let mut out = vec![0x08];
    out.extend_from_slice(&seg(nm));
    out.extend_from_slice(value);
    out
}

/// `Package { elements }` — each element pre-encoded (Phase 103 B).
pub fn package(elements: &[Vec<u8>]) -> Vec<u8> {
    assert!(elements.len() <= 0xFF, "use VarPackage for more");
    let mut inner = vec![elements.len() as u8];
    for e in elements {
        inner.extend_from_slice(e);
    }
    let mut out = vec![0x12];
    out.extend_from_slice(&pkg(&inner));
    out
}

/// String literal.
pub fn string(s: &str) -> Vec<u8> {
    let mut out = vec![0x0D];
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    out
}

/// DWord integer literal.
pub fn dword(v: u32) -> Vec<u8> {
    let mut out = vec![0x0C];
    out.extend_from_slice(&v.to_le_bytes());
    out
}

/// Buffer literal with a byte-prefix size operand.
pub fn buffer(bytes: &[u8]) -> Vec<u8> {
    assert!(bytes.len() <= 0xFF, "use a wider size operand");
    let mut inner = vec![0x0A, bytes.len() as u8];
    inner.extend_from_slice(bytes);
    let mut out = vec![0x11];
    out.extend_from_slice(&pkg(&inner));
    out
}

/// Wrap a term-list `body` in a definition-block SDT header with the
/// given signature, correct length, and checksum.
pub fn table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let len = 36 + body.len();
    let mut t = Vec::with_capacity(len);
    t.extend_from_slice(signature);
    t.extend_from_slice(&(len as u32).to_le_bytes());
    t.push(2); // revision ≥ 2 → 64-bit arithmetic
    t.push(0); // checksum patched below
    t.extend_from_slice(b"M3OSTS"); // OEM ID
    t.extend_from_slice(b"FIXTURE_"); // OEM table ID
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(b"M3OS");
    t.extend_from_slice(&1u32.to_le_bytes());
    assert_eq!(t.len(), 36);
    t.extend_from_slice(body);
    let sum: u8 = t.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);
    t
}

/// `_CRS` bytes shaped like the Dell touchpad's: an I2C SerialBus
/// (`addr` @ 400 kHz on `\_SB.PCI0.I2C1`) + a GpioInt (level,
/// active-low, `pin` on `\_SB.GPI0`) + End Tag.
pub fn touchpad_crs(addr: u16, pin: u16) -> Vec<u8> {
    let mut v = Vec::new();
    // I2C SerialBus (large 0x8E, bus type 1).
    let src = b"\\_SB.PCI0.I2C1";
    let type_len = 6u16;
    let body_len = 9 + type_len as usize + src.len() + 1;
    v.push(0x8E);
    v.extend_from_slice(&(body_len as u16).to_le_bytes());
    v.push(2); // revision
    v.push(0); // source index
    v.push(1); // bus type: I2C
    v.push(0x02); // general flags
    v.extend_from_slice(&0u16.to_le_bytes()); // type flags
    v.push(1); // type revision
    v.extend_from_slice(&type_len.to_le_bytes());
    v.extend_from_slice(&400_000u32.to_le_bytes());
    v.extend_from_slice(&addr.to_le_bytes());
    v.extend_from_slice(src);
    v.push(0);
    // GpioInt (large 0x8C, connection type 0).
    let src2 = b"\\_SB.GPI0";
    let pin_off = 23u16;
    let src_off = pin_off + 2;
    let vendor_off = src_off + src2.len() as u16 + 1;
    let body2_len = (vendor_off - 3) as usize;
    v.push(0x8C);
    v.extend_from_slice(&(body2_len as u16).to_le_bytes());
    v.push(1); // revision
    v.push(0); // connection type: Interrupt
    v.extend_from_slice(&0u16.to_le_bytes()); // general flags
    v.extend_from_slice(&0x0002u16.to_le_bytes()); // level, active-low
    v.push(1); // pin config: pull-up
    v.extend_from_slice(&0u16.to_le_bytes()); // drive strength
    v.extend_from_slice(&0u16.to_le_bytes()); // debounce
    v.extend_from_slice(&pin_off.to_le_bytes());
    v.push(0); // source index
    v.extend_from_slice(&src_off.to_le_bytes());
    v.extend_from_slice(&vendor_off.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes()); // vendor data length
    v.extend_from_slice(&pin.to_le_bytes());
    v.extend_from_slice(src2);
    v.push(0);
    // End Tag.
    v.push(0x79);
    v.push(0x00);
    v
}
