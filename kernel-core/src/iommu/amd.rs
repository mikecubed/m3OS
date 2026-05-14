//! AMD-Vi fault event decoder — Phase 67 Track B.
//!
//! Pure-logic decoder for the 128-bit event-log records the AMD-Vi
//! hardware writes when a DMA translation fails (or hits any other
//! event class the IOMMU reports). Lives in `kernel-core` so it is
//! host-testable via `cargo test -p kernel-core` and can be exercised
//! against synthetic raw bytes without booting the kernel.
//!
//! # Spec reference
//!
//! AMD I/O Virtualization Technology (IOMMU) Specification, revision
//! 3.00 (2016), §3.4 "Event Log Entries" — Table 57 enumerates the
//! defined event codes. The decoder recognises every code Phase 67
//! enumerates as required:
//!
//! | Code | Variant                | Meaning                                  |
//! |------|------------------------|------------------------------------------|
//! | 0x1  | `IllegalDevTableEntry` | Invalid device-table entry on a request. |
//! | 0x2  | `IoPageFault`          | I/O page-fault during translation walk.  |
//! | 0x3  | `DevTableHwError`      | HW error reading the device table.       |
//! | 0x4  | `PageTableHwError`     | HW error reading the page table.         |
//! | 0x5  | `IllegalCommandError`  | Malformed command in the command ring.   |
//! | 0x6  | `CommandHwError`       | HW error processing a command.           |
//! | 0x8  | `EventLogOverflow`     | Hardware lost events due to full ring.   |
//!
//! Unknown codes decode into `AmdViFaultCode::Unknown(raw)` so the
//! caller can still log the event without losing information.
//!
//! # Field layout
//!
//! Every event entry is 128 bits (two little-endian u64 words). The
//! header word carries:
//!
//! - bits 15:0  — Requestor BDF (the DeviceID field).
//! - bits 31:16 — PASID (Phase 67 ignores PASID — Phase 55a does not
//!   enable PASID translation).
//! - bits 47:32 — Domain ID for events that have one. Preserved by the
//!   typed decoder so the AMD-Vi `amdvi-detail` log line keeps parity
//!   with the pre-Phase-67 output.
//! - bits 59:52 — Flags byte. The exact flag layout is event-specific;
//!   Phase 67 preserves the raw bits so callers can dump them.
//! - bits 63:60 — Event code (4-bit).
//!
//! The second word carries the faulting IOVA for `IoPageFault`. For
//! events that do not carry an address the bits are reserved and the
//! decoder reports them as zero on a well-formed record.

use core::fmt;

/// Typed view of a single AMD-Vi event-log record.
///
/// Constructed by [`decode_event_log_entry`] from the raw 16-byte
/// little-endian record the hardware writes into the event-log ring.
/// Fields are flat (`Copy`) so the record can be passed across an IRQ
/// boundary without any allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmdViFaultEvent {
    /// PCI BDF that issued the faulting transaction.
    /// Layout: `(bus << 8) | (device << 3) | function`.
    pub requestor_bdf: u16,
    /// Domain id from header bits 47:32. Zero on events that do not
    /// carry one. Surfaced so the AMD-Vi `amdvi-detail` log line
    /// retains the `domain={:#x}` field the legacy `EventEntry::decode`
    /// path printed pre-Phase-67.
    pub domain_id: u16,
    /// Event class. See [`AmdViFaultCode`].
    pub fault_code: AmdViFaultCode,
    /// Faulting IOVA for events that carry one (`IoPageFault`); zero
    /// otherwise. The decoder does not validate alignment — callers
    /// that need the bus-address field for non-page-fault events can
    /// read the raw byte stream directly.
    pub iova: u64,
    /// Vendor-defined flags byte from header bits 59:52. Reserved bits
    /// are preserved verbatim so consumers can dump them on unknown
    /// flag patterns without re-parsing the raw entry.
    pub flags: u8,
}

/// AMD-Vi event-code enumeration. Variants correspond 1:1 with the
/// defined codes in spec Table 57; unknown codes round-trip through
/// `Unknown(raw)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmdViFaultCode {
    /// Code 0x1 — the device-table entry referenced by the requester
    /// BDF is malformed or marked invalid.
    IllegalDevTableEntry,
    /// Code 0x2 — translation walk produced a missing or
    /// permission-failing page-table entry.
    IoPageFault,
    /// Code 0x3 — hardware error reading the device table.
    DevTableHwError,
    /// Code 0x4 — hardware error reading a page-table entry.
    PageTableHwError,
    /// Code 0x5 — software wrote an illegal command into the command
    /// ring.
    IllegalCommandError,
    /// Code 0x6 — hardware error encountered while processing a
    /// command.
    CommandHwError,
    /// Code 0x8 — event-log overflow flag; hardware dropped events
    /// because software did not drain the ring fast enough.
    EventLogOverflow,
    /// Any code outside the defined set. The raw 4-bit value is
    /// preserved so the caller can still log a recognisable identifier.
    Unknown(u8),
}

impl AmdViFaultCode {
    /// Decode the 4-bit event-code field into a typed variant.
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0x1 => Self::IllegalDevTableEntry,
            0x2 => Self::IoPageFault,
            0x3 => Self::DevTableHwError,
            0x4 => Self::PageTableHwError,
            0x5 => Self::IllegalCommandError,
            0x6 => Self::CommandHwError,
            0x8 => Self::EventLogOverflow,
            other => Self::Unknown(other),
        }
    }

    /// Inverse of [`from_raw`]: returns the spec-defined 4-bit code.
    pub const fn to_raw(self) -> u8 {
        match self {
            Self::IllegalDevTableEntry => 0x1,
            Self::IoPageFault => 0x2,
            Self::DevTableHwError => 0x3,
            Self::PageTableHwError => 0x4,
            Self::IllegalCommandError => 0x5,
            Self::CommandHwError => 0x6,
            Self::EventLogOverflow => 0x8,
            Self::Unknown(raw) => raw & 0xF,
        }
    }

    /// Stable human-readable name used in structured log lines.
    pub const fn name(self) -> &'static str {
        match self {
            Self::IllegalDevTableEntry => "illegal_dev_table_entry",
            Self::IoPageFault => "io_page_fault",
            Self::DevTableHwError => "dev_tab_hw_error",
            Self::PageTableHwError => "page_tab_hw_error",
            Self::IllegalCommandError => "illegal_command_error",
            Self::CommandHwError => "command_hw_error",
            Self::EventLogOverflow => "event_log_overflow",
            Self::Unknown(_) => "unknown",
        }
    }
}

impl fmt::Display for AmdViFaultCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Errors returned by [`decode_event_log_entry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Reserved for forward compatibility. The decoder currently has no
    /// hard-failing input shape because the raw record is fixed-size
    /// (16 bytes) and the caller passes a `&[u8; 16]`, but a typed
    /// error keeps the public signature `Result<_, DecodeError>` so a
    /// future strict-mode validator can return it without an SAPI
    /// break.
    Reserved,
}

/// Decode a 16-byte little-endian AMD-Vi event-log record into the
/// typed [`AmdViFaultEvent`] shape.
///
/// The input length is enforced statically by `&[u8; 16]`. Bit layout:
///
/// ```text
/// word0 = bytes[0..8]  (little-endian)
///   bits 15:0   = device_id  (requestor BDF)
///   bits 31:16  = pasid       (Phase 67 ignores)
///   bits 47:32  = domain_id   (preserved verbatim)
///   bits 59:52  = flags       (preserved verbatim)
///   bits 63:60  = event_code  (mapped to AmdViFaultCode)
/// word1 = bytes[8..16] (little-endian)
///   bits 63:0   = address     (faulting IOVA for IoPageFault; zero
///                              otherwise on a well-formed record)
/// ```
pub fn decode_event_log_entry(raw: &[u8; 16]) -> Result<AmdViFaultEvent, DecodeError> {
    let word0 = u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]);
    let word1 = u64::from_le_bytes([
        raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
    ]);
    let code_raw = ((word0 >> 60) & 0xF) as u8;
    let requestor_bdf = (word0 & 0xFFFF) as u16;
    let domain_id = ((word0 >> 32) & 0xFFFF) as u16;
    let flags = ((word0 >> 52) & 0xFF) as u8;
    let fault_code = AmdViFaultCode::from_raw(code_raw);
    let iova = word1;
    Ok(AmdViFaultEvent {
        requestor_bdf,
        domain_id,
        fault_code,
        iova,
        flags,
    })
}

/// Encode an [`AmdViFaultEvent`] back into its 16-byte raw record.
///
/// The inverse of [`decode_event_log_entry`] for the fields the
/// decoder extracts. Bits the decoder does not extract (PASID) are
/// encoded as zero. Useful for test fixtures that need to construct
/// synthetic event records without hand-rolling the bit math.
pub fn encode_event_log_entry(event: &AmdViFaultEvent) -> [u8; 16] {
    let code = event.fault_code.to_raw() as u64;
    let word0: u64 = (event.requestor_bdf as u64)
        | (((event.domain_id as u64) & 0xFFFF) << 32)
        | ((event.flags as u64) << 52)
        | ((code & 0xF) << 60);
    let word1: u64 = event.iova;
    let mut out = [0u8; 16];
    out[0..8].copy_from_slice(&word0.to_le_bytes());
    out[8..16].copy_from_slice(&word1.to_le_bytes());
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(code: AmdViFaultCode, bdf: u16, iova: u64, flags: u8) {
        round_trip_with_domain(code, bdf, 0, iova, flags);
    }

    fn round_trip_with_domain(
        code: AmdViFaultCode,
        bdf: u16,
        domain_id: u16,
        iova: u64,
        flags: u8,
    ) {
        let event = AmdViFaultEvent {
            requestor_bdf: bdf,
            domain_id,
            fault_code: code,
            iova,
            flags,
        };
        let raw = encode_event_log_entry(&event);
        let decoded = decode_event_log_entry(&raw).expect("decoder accepts encoder output");
        assert_eq!(decoded, event, "round-trip differs for {:?}", code);
    }

    #[test]
    fn decode_illegal_dev_table_entry_extracts_bdf_and_flags() {
        round_trip(AmdViFaultCode::IllegalDevTableEntry, 0x0100, 0, 0xA5);
    }

    #[test]
    fn decode_io_page_fault_extracts_iova() {
        round_trip(AmdViFaultCode::IoPageFault, 0x0820, 0xDEAD_BEEF_0000, 0x12);
    }

    #[test]
    fn decode_dev_table_hw_error_round_trips() {
        round_trip(AmdViFaultCode::DevTableHwError, 0x00FF, 0, 0x00);
    }

    #[test]
    fn decode_page_table_hw_error_round_trips() {
        round_trip(AmdViFaultCode::PageTableHwError, 0x0001, 0x4000_0000, 0x00);
    }

    #[test]
    fn decode_illegal_command_error_round_trips() {
        round_trip(AmdViFaultCode::IllegalCommandError, 0x0000, 0, 0xFF);
    }

    #[test]
    fn decode_command_hw_error_round_trips() {
        round_trip(AmdViFaultCode::CommandHwError, 0x0123, 0, 0x00);
    }

    #[test]
    fn decode_event_log_overflow_round_trips() {
        round_trip(AmdViFaultCode::EventLogOverflow, 0xFFFF, 0, 0x00);
    }

    #[test]
    fn decode_preserves_domain_id_for_io_page_fault() {
        round_trip_with_domain(
            AmdViFaultCode::IoPageFault,
            0x0820,
            0x1234,
            0xDEAD_BEEF_0000,
            0x12,
        );
    }

    #[test]
    fn decode_preserves_domain_id_at_field_max() {
        round_trip_with_domain(
            AmdViFaultCode::IllegalDevTableEntry,
            0x0001,
            0xFFFF,
            0,
            0x00,
        );
    }

    #[test]
    fn unknown_event_code_preserved_via_unknown_variant() {
        // Hand-build a raw record with event-code 0xF.
        let mut raw = [0u8; 16];
        // word0: bits 63:60 = 0xF, low BDF = 0x0042
        let word0: u64 = 0x0042u64 | (0xFu64 << 60);
        raw[0..8].copy_from_slice(&word0.to_le_bytes());
        let event = decode_event_log_entry(&raw).expect("decoder accepts any 16-byte input");
        assert_eq!(event.requestor_bdf, 0x0042);
        assert_eq!(event.domain_id, 0);
        match event.fault_code {
            AmdViFaultCode::Unknown(raw) => assert_eq!(raw, 0xF),
            other => panic!("expected Unknown(0xF), got {:?}", other),
        }
    }

    #[test]
    fn fault_code_names_are_distinct_for_defined_variants() {
        let codes = [
            AmdViFaultCode::IllegalDevTableEntry,
            AmdViFaultCode::IoPageFault,
            AmdViFaultCode::DevTableHwError,
            AmdViFaultCode::PageTableHwError,
            AmdViFaultCode::IllegalCommandError,
            AmdViFaultCode::CommandHwError,
            AmdViFaultCode::EventLogOverflow,
        ];
        for i in 0..codes.len() {
            for j in (i + 1)..codes.len() {
                assert_ne!(
                    codes[i].name(),
                    codes[j].name(),
                    "names must be distinct for spec-defined codes"
                );
            }
        }
    }

    #[test]
    fn raw_round_trip_preserves_low_bytes() {
        // Hand-built raw record matching a real Aardvark event: code=2
        // (IoPageFault), BDF=0x0820, IOVA=0x0000_BEEF_C000, flags=0x10.
        let event = AmdViFaultEvent {
            requestor_bdf: 0x0820,
            domain_id: 0x00AB,
            fault_code: AmdViFaultCode::IoPageFault,
            iova: 0x0000_BEEF_C000,
            flags: 0x10,
        };
        let raw = encode_event_log_entry(&event);
        let word0 = u64::from_le_bytes([
            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
        ]);
        // event code lives at bits 63:60 = 0x2.
        assert_eq!(((word0 >> 60) & 0xF) as u8, 0x2);
        // BDF at bits 15:0.
        assert_eq!((word0 & 0xFFFF) as u16, 0x0820);
        // Domain id at bits 47:32.
        assert_eq!(((word0 >> 32) & 0xFFFF) as u16, 0x00AB);
        // Flags at bits 59:52.
        assert_eq!(((word0 >> 52) & 0xFF) as u8, 0x10);
        // IOVA at word1.
        let word1 = u64::from_le_bytes([
            raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
        ]);
        assert_eq!(word1, 0x0000_BEEF_C000);
    }
}
