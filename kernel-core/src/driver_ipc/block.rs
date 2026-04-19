//! Block-driver IPC protocol schema — Phase 55b Track A.2.
//!
//! Single source of truth for the block-driver protocol spoken between the
//! kernel-side `RemoteBlockDevice` facade (Phase 55b Track D.4) and the
//! userspace NVMe driver process (Phase 55b Tracks D.2 / D.3). Declaring the
//! schema in `kernel-core` makes it host-testable and guarantees both sides
//! compile against the same message layout — divergence becomes a compile
//! error rather than a runtime corruption bug.
//!
//! Layout is packed little-endian. Bulk payload data (write data on the
//! request side, read data on the reply side) does not appear inline; it
//! travels through a separate grant capability referenced by a `u32`
//! payload-grant handle carried alongside the header. This schema pins the
//! handle's byte offset so the encode and decode halves stay in lock-step
//! across the kernel-ring and userspace-ring implementations.

#![allow(clippy::needless_range_loop)]

// ------------------------------------------------------------------------
// Message-label constants
// ------------------------------------------------------------------------

/// IPC message label for a block read request.
///
/// Reserved from the label range kept clear by the Phase 54 VFS protocol
/// block so the `0x5500`-range stays collision-free for Phase 55b drivers.
pub const BLK_READ: u16 = 0x5501;

/// IPC message label for a block write request.
pub const BLK_WRITE: u16 = 0x5502;

/// IPC message label for a block status / reply envelope.
pub const BLK_STATUS: u16 = 0x5503;

/// Hard upper bound on the number of sectors a single request may carry.
///
/// The bound is enforced at the `RemoteBlockDevice` facade (Phase 55b Track
/// D.4), *not* inside the driver process — a compliant driver must still
/// reject an oversized request, but the kernel-side facade is the first
/// line of defence. Pinned here so every participant agrees on the same
/// number.
pub const MAX_SECTORS_PER_REQUEST: u32 = 256;

/// Serialized size of a [`BlkRequestHeader`] plus payload-grant handle.
///
/// Layout (packed little-endian):
///
/// - `[0..2]`   `kind: u16`
/// - `[2..10]`  `cmd_id: u64`
/// - `[10..18]` `lba: u64`
/// - `[18..22]` `sector_count: u32`
/// - `[22..26]` `flags: u32`
/// - `[26..30]` `payload_grant: u32` (IPC grant handle; `0` for no payload)
///
/// The grant handle rides with the header so the receiver can resolve it in
/// the same frame it pulls the header out of. Higher-level plumbing (the
/// kernel IPC layer) decides how a `u32` handle maps back to a
/// `Capability::Grant`; this schema is only concerned with where in the
/// payload byte-stream the handle sits.
pub const BLK_REQUEST_HEADER_SIZE: usize = 30;

/// Serialized size of a [`BlkReplyHeader`] plus payload-grant handle.
///
/// Layout (packed little-endian):
///
/// - `[0..8]`   `cmd_id: u64`
/// - `[8]`      `status: u8`  (see [`BlockDriverError::to_byte`])
/// - `[9..12]`  reserved, must be zero
/// - `[12..16]` `bytes: u32`
/// - `[16..20]` `payload_grant: u32` (IPC grant handle carrying the read
///   data; `0` for write replies or error replies with no data)
pub const BLK_REPLY_HEADER_SIZE: usize = 20;

// ------------------------------------------------------------------------
// BlockDriverError
// ------------------------------------------------------------------------

/// Error kinds emitted by the block-driver IPC path.
///
/// Variants are *data*, never strings — both the kernel-side
/// `RemoteBlockDevice` and the userspace NVMe driver pattern-match on them
/// without any allocation. `Ok` is included as the success discriminant so
/// [`BlkReplyHeader::status`] can carry any outcome in a single byte.
///
/// `#[non_exhaustive]` lets later phases add variants without forcing
/// downstream `match` sites to be exhaustive; the defining crate still
/// exhaustively matches every arm (see unit tests).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum BlockDriverError {
    /// The operation completed successfully.
    Ok,
    /// Underlying media / transport reported an I/O error.
    IoError,
    /// `lba + sector_count` exceeds the device's logical block count, or
    /// `lba` is otherwise outside the addressable range.
    InvalidLba,
    /// The target device has been removed or is no longer claimed.
    DeviceAbsent,
    /// The driver process is still servicing a previous request and cannot
    /// accept another one right now.
    Busy,
    /// The driver process crashed and the service manager is bringing a
    /// fresh instance up — the caller should retry within
    /// `DRIVER_RESTART_TIMEOUT_MS`.
    DriverRestarting,
    /// The request header was malformed (bad kind, oversized sector count,
    /// missing grant, etc.).
    InvalidRequest,
}

impl BlockDriverError {
    /// Stable single-byte encoding used on the wire.
    pub const fn to_byte(self) -> u8 {
        match self {
            BlockDriverError::Ok => 0,
            BlockDriverError::IoError => 1,
            BlockDriverError::InvalidLba => 2,
            BlockDriverError::DeviceAbsent => 3,
            BlockDriverError::Busy => 4,
            BlockDriverError::DriverRestarting => 5,
            BlockDriverError::InvalidRequest => 6,
        }
    }

    /// Inverse of [`Self::to_byte`]; returns `None` for unknown
    /// discriminants so malformed payloads produce a decode error rather
    /// than a silent substitution.
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(BlockDriverError::Ok),
            1 => Some(BlockDriverError::IoError),
            2 => Some(BlockDriverError::InvalidLba),
            3 => Some(BlockDriverError::DeviceAbsent),
            4 => Some(BlockDriverError::Busy),
            5 => Some(BlockDriverError::DriverRestarting),
            6 => Some(BlockDriverError::InvalidRequest),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------------
// BlkRequestHeader / BlkReplyHeader
// ------------------------------------------------------------------------

/// Request envelope sent from `RemoteBlockDevice` (kernel) to the driver
/// process.
///
/// `kind` holds one of [`BLK_READ`] / [`BLK_WRITE`] / [`BLK_STATUS`]. The
/// bulk payload — write data on the request side — rides in a separate IPC
/// grant referenced by a `payload_grant` handle written alongside the
/// header (see [`BLK_REQUEST_HEADER_SIZE`] for the exact byte offsets).
/// The kernel-side facade is responsible for rejecting any request whose
/// `sector_count` exceeds [`MAX_SECTORS_PER_REQUEST`].
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlkRequestHeader {
    pub kind: u16,
    pub cmd_id: u64,
    pub lba: u64,
    pub sector_count: u32,
    pub flags: u32,
}

/// Reply envelope returned from the driver process to `RemoteBlockDevice`.
///
/// `cmd_id` echoes the request's command id so pipelined requests can be
/// matched. `status` carries a [`BlockDriverError`] — on success
/// ([`BlockDriverError::Ok`]) the `bytes` field reports the number of bytes
/// actually transferred and, for read replies, the `payload_grant` handle
/// written alongside the header references a grant carrying the bulk read
/// data (see [`BLK_REPLY_HEADER_SIZE`] for byte offsets).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlkReplyHeader {
    pub cmd_id: u64,
    pub status: BlockDriverError,
    pub bytes: u32,
}

// ------------------------------------------------------------------------
// Decode errors
// ------------------------------------------------------------------------

/// Reasons a [`decode_blk_request`] / [`decode_blk_reply`] call can fail.
///
/// Variants are data, not strings, and `#[non_exhaustive]` so later phases
/// may extend the taxonomy without breaking downstream match exhaustiveness.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DecodeError {
    /// Input slice was shorter than the minimum header length.
    Truncated,
    /// A reserved field was non-zero (payload was crafted against a wire
    /// format this build does not understand).
    ReservedNonZero,
    /// The `kind` field did not match any known label.
    UnknownKind,
    /// The `status` byte did not map to any [`BlockDriverError`] variant.
    UnknownStatus,
}

// ------------------------------------------------------------------------
// Encode / decode helpers
// ------------------------------------------------------------------------

/// Encode a [`BlkRequestHeader`] together with the IPC grant handle that
/// carries the bulk write payload (pass `0` for read requests, which have
/// no inline payload). Returns the fixed-width byte stamp the IPC layer
/// will put on the wire.
pub const fn encode_blk_request(
    header: BlkRequestHeader,
    payload_grant: u32,
) -> [u8; BLK_REQUEST_HEADER_SIZE] {
    let mut out = [0u8; BLK_REQUEST_HEADER_SIZE];
    let kind = header.kind.to_le_bytes();
    out[0] = kind[0];
    out[1] = kind[1];
    let cmd = header.cmd_id.to_le_bytes();
    let mut i = 0;
    while i < 8 {
        out[2 + i] = cmd[i];
        i += 1;
    }
    let lba = header.lba.to_le_bytes();
    let mut i = 0;
    while i < 8 {
        out[10 + i] = lba[i];
        i += 1;
    }
    let sc = header.sector_count.to_le_bytes();
    let mut i = 0;
    while i < 4 {
        out[18 + i] = sc[i];
        i += 1;
    }
    let fl = header.flags.to_le_bytes();
    let mut i = 0;
    while i < 4 {
        out[22 + i] = fl[i];
        i += 1;
    }
    let pg = payload_grant.to_le_bytes();
    let mut i = 0;
    while i < 4 {
        out[26 + i] = pg[i];
        i += 1;
    }
    out
}

/// Decode a [`BlkRequestHeader`] out of an on-the-wire byte slice. Returns
/// the decoded header plus the trailing `payload_grant` handle on success.
///
/// Rejects slices shorter than [`BLK_REQUEST_HEADER_SIZE`] and rejects any
/// `kind` value that does not match one of the three documented labels —
/// both paths surface as `Err(DecodeError)` rather than a panic so the
/// caller can treat a malformed peer as a protocol error.
pub fn decode_blk_request(bytes: &[u8]) -> Result<(BlkRequestHeader, u32), DecodeError> {
    if bytes.len() < BLK_REQUEST_HEADER_SIZE {
        return Err(DecodeError::Truncated);
    }
    let kind = u16::from_le_bytes([bytes[0], bytes[1]]);
    if kind != BLK_READ && kind != BLK_WRITE && kind != BLK_STATUS {
        return Err(DecodeError::UnknownKind);
    }
    let cmd_id = u64::from_le_bytes([
        bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
    ]);
    let lba = u64::from_le_bytes([
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17],
    ]);
    let sector_count = u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
    let flags = u32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
    let payload_grant = u32::from_le_bytes([bytes[26], bytes[27], bytes[28], bytes[29]]);
    Ok((
        BlkRequestHeader {
            kind,
            cmd_id,
            lba,
            sector_count,
            flags,
        },
        payload_grant,
    ))
}

/// Encode a [`BlkReplyHeader`] plus the grant handle carrying any bulk read
/// payload. Write replies and error replies pass `0` for `payload_grant`.
pub const fn encode_blk_reply(
    header: BlkReplyHeader,
    payload_grant: u32,
) -> [u8; BLK_REPLY_HEADER_SIZE] {
    let mut out = [0u8; BLK_REPLY_HEADER_SIZE];
    let cmd = header.cmd_id.to_le_bytes();
    let mut i = 0;
    while i < 8 {
        out[i] = cmd[i];
        i += 1;
    }
    out[8] = header.status.to_byte();
    // out[9..12] are reserved and remain zero.
    let b = header.bytes.to_le_bytes();
    let mut i = 0;
    while i < 4 {
        out[12 + i] = b[i];
        i += 1;
    }
    let pg = payload_grant.to_le_bytes();
    let mut i = 0;
    while i < 4 {
        out[16 + i] = pg[i];
        i += 1;
    }
    out
}

/// Decode a [`BlkReplyHeader`] out of an on-the-wire byte slice. Returns
/// the decoded header plus the trailing `payload_grant` handle on success.
///
/// Rejects slices shorter than [`BLK_REPLY_HEADER_SIZE`], rejects non-zero
/// reserved bytes, and rejects unknown status discriminants — all three
/// paths surface as `Err(DecodeError)` without panicking.
pub fn decode_blk_reply(bytes: &[u8]) -> Result<(BlkReplyHeader, u32), DecodeError> {
    if bytes.len() < BLK_REPLY_HEADER_SIZE {
        return Err(DecodeError::Truncated);
    }
    let cmd_id = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    let status = match BlockDriverError::from_byte(bytes[8]) {
        Some(s) => s,
        None => return Err(DecodeError::UnknownStatus),
    };
    if bytes[9] != 0 || bytes[10] != 0 || bytes[11] != 0 {
        return Err(DecodeError::ReservedNonZero);
    }
    let b = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let payload_grant = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    Ok((
        BlkReplyHeader {
            cmd_id,
            status,
            bytes: b,
        },
        payload_grant,
    ))
}

// ------------------------------------------------------------------------
// Tests — authoritative for Phase 55b Track A.2. Introduced in the Red
// commit; identical here to demonstrate the Green implementation satisfies
// every Acceptance bullet without the test code being tweaked.
// ------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ---- Message labels --------------------------------------------------

    #[test]
    fn message_labels_are_pinned_values() {
        assert_eq!(BLK_READ, 0x5501);
        assert_eq!(BLK_WRITE, 0x5502);
        assert_eq!(BLK_STATUS, 0x5503);
    }

    #[test]
    fn message_labels_are_distinct() {
        assert_ne!(BLK_READ, BLK_WRITE);
        assert_ne!(BLK_WRITE, BLK_STATUS);
        assert_ne!(BLK_READ, BLK_STATUS);
    }

    #[test]
    fn max_sectors_per_request_is_pinned() {
        assert_eq!(MAX_SECTORS_PER_REQUEST, 256);
    }

    // ---- BlockDriverError ------------------------------------------------

    #[test]
    fn block_driver_error_variants_are_all_constructible_and_equal() {
        let all = [
            BlockDriverError::Ok,
            BlockDriverError::IoError,
            BlockDriverError::InvalidLba,
            BlockDriverError::DeviceAbsent,
            BlockDriverError::Busy,
            BlockDriverError::DriverRestarting,
            BlockDriverError::InvalidRequest,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn block_driver_error_byte_round_trip_is_total() {
        let all = [
            BlockDriverError::Ok,
            BlockDriverError::IoError,
            BlockDriverError::InvalidLba,
            BlockDriverError::DeviceAbsent,
            BlockDriverError::Busy,
            BlockDriverError::DriverRestarting,
            BlockDriverError::InvalidRequest,
        ];
        for e in all {
            let b = e.to_byte();
            assert_eq!(BlockDriverError::from_byte(b), Some(e));
        }
        assert_eq!(BlockDriverError::from_byte(0xff), None);
    }

    // ---- Encode/decode round-trip (deterministic) ------------------------

    #[test]
    fn blk_request_round_trip_read() {
        let hdr = BlkRequestHeader {
            kind: BLK_READ,
            cmd_id: 0x1234_5678_9abc_def0,
            lba: 0x0000_0000_0000_0010,
            sector_count: 8,
            flags: 0,
        };
        let bytes = encode_blk_request(hdr, 0);
        let (back, grant) = decode_blk_request(&bytes).expect("round-trip");
        assert_eq!(back, hdr);
        assert_eq!(grant, 0);
    }

    #[test]
    fn blk_request_round_trip_write_carries_grant() {
        let hdr = BlkRequestHeader {
            kind: BLK_WRITE,
            cmd_id: 42,
            lba: 0x1000,
            sector_count: 16,
            flags: 0x0000_0001,
        };
        let bytes = encode_blk_request(hdr, 0xdead_beef);
        let (back, grant) = decode_blk_request(&bytes).expect("round-trip");
        assert_eq!(back, hdr);
        assert_eq!(grant, 0xdead_beef);
    }

    #[test]
    fn blk_reply_round_trip_ok() {
        let hdr = BlkReplyHeader {
            cmd_id: 7,
            status: BlockDriverError::Ok,
            bytes: 4096,
        };
        let bytes = encode_blk_reply(hdr, 0xfeed_face);
        let (back, grant) = decode_blk_reply(&bytes).expect("round-trip");
        assert_eq!(back, hdr);
        assert_eq!(grant, 0xfeed_face);
    }

    #[test]
    fn blk_reply_round_trip_error() {
        let hdr = BlkReplyHeader {
            cmd_id: 9,
            status: BlockDriverError::IoError,
            bytes: 0,
        };
        let bytes = encode_blk_reply(hdr, 0);
        let (back, grant) = decode_blk_reply(&bytes).expect("round-trip");
        assert_eq!(back, hdr);
        assert_eq!(grant, 0);
    }

    // ---- Decode rejects malformed / truncated payloads -------------------

    #[test]
    fn decode_blk_request_rejects_truncated() {
        for len in 0..BLK_REQUEST_HEADER_SIZE {
            let buf = [0u8; BLK_REQUEST_HEADER_SIZE];
            let r = decode_blk_request(&buf[..len]);
            assert_eq!(r, Err(DecodeError::Truncated));
        }
    }

    #[test]
    fn decode_blk_request_rejects_unknown_kind() {
        let hdr = BlkRequestHeader {
            kind: BLK_READ,
            cmd_id: 0,
            lba: 0,
            sector_count: 0,
            flags: 0,
        };
        let mut bytes = encode_blk_request(hdr, 0);
        bytes[0] = 0x34;
        bytes[1] = 0x12;
        let r = decode_blk_request(&bytes);
        assert_eq!(r, Err(DecodeError::UnknownKind));
    }

    #[test]
    fn decode_blk_reply_rejects_truncated() {
        for len in 0..BLK_REPLY_HEADER_SIZE {
            let buf = [0u8; BLK_REPLY_HEADER_SIZE];
            let r = decode_blk_reply(&buf[..len]);
            assert_eq!(r, Err(DecodeError::Truncated));
        }
    }

    #[test]
    fn decode_blk_reply_rejects_unknown_status() {
        let mut bytes = encode_blk_reply(
            BlkReplyHeader {
                cmd_id: 1,
                status: BlockDriverError::Ok,
                bytes: 0,
            },
            0,
        );
        bytes[8] = 0x7f;
        let r = decode_blk_reply(&bytes);
        assert_eq!(r, Err(DecodeError::UnknownStatus));
    }

    #[test]
    fn decode_blk_reply_rejects_reserved_nonzero() {
        let mut bytes = encode_blk_reply(
            BlkReplyHeader {
                cmd_id: 1,
                status: BlockDriverError::Ok,
                bytes: 0,
            },
            0,
        );
        bytes[9] = 0x01;
        let r = decode_blk_reply(&bytes);
        assert_eq!(r, Err(DecodeError::ReservedNonZero));
    }

    // ---- Property tests --------------------------------------------------

    fn any_kind() -> impl Strategy<Value = u16> {
        prop_oneof![Just(BLK_READ), Just(BLK_WRITE), Just(BLK_STATUS)]
    }

    fn any_block_driver_error() -> impl Strategy<Value = BlockDriverError> {
        prop_oneof![
            Just(BlockDriverError::Ok),
            Just(BlockDriverError::IoError),
            Just(BlockDriverError::InvalidLba),
            Just(BlockDriverError::DeviceAbsent),
            Just(BlockDriverError::Busy),
            Just(BlockDriverError::DriverRestarting),
            Just(BlockDriverError::InvalidRequest),
        ]
    }

    proptest! {
        #[test]
        fn prop_blk_request_round_trip(
            kind in any_kind(),
            cmd_id in any::<u64>(),
            lba in any::<u64>(),
            sector_count in any::<u32>(),
            flags in any::<u32>(),
            payload_grant in any::<u32>(),
        ) {
            let hdr = BlkRequestHeader { kind, cmd_id, lba, sector_count, flags };
            let bytes = encode_blk_request(hdr, payload_grant);
            let (back, grant) = decode_blk_request(&bytes).expect("encode then decode must round-trip");
            prop_assert_eq!(back, hdr);
            prop_assert_eq!(grant, payload_grant);
        }

        #[test]
        fn prop_blk_reply_round_trip(
            cmd_id in any::<u64>(),
            status in any_block_driver_error(),
            bytes_count in any::<u32>(),
            payload_grant in any::<u32>(),
        ) {
            let hdr = BlkReplyHeader { cmd_id, status, bytes: bytes_count };
            let bytes = encode_blk_reply(hdr, payload_grant);
            let (back, grant) = decode_blk_reply(&bytes).expect("encode then decode must round-trip");
            prop_assert_eq!(back, hdr);
            prop_assert_eq!(grant, payload_grant);
        }

        /// Arbitrary (possibly-garbage) payload bytes never panic the
        /// decoders. They return either `Ok` or `Err(DecodeError)`, but
        /// never panic and never over-read the slice.
        #[test]
        fn prop_decode_blk_request_never_panics(
            payload in proptest::collection::vec(any::<u8>(), 0..64),
        ) {
            let _ = decode_blk_request(&payload);
        }

        #[test]
        fn prop_decode_blk_reply_never_panics(
            payload in proptest::collection::vec(any::<u8>(), 0..64),
        ) {
            let _ = decode_blk_reply(&payload);
        }

        /// Truncated valid-looking payloads decode to `Err(DecodeError)` and
        /// never succeed — guards against off-by-one reads past end-of-slice.
        #[test]
        fn prop_truncated_request_is_error(
            kind in any_kind(),
            cmd_id in any::<u64>(),
            lba in any::<u64>(),
            sector_count in any::<u32>(),
            flags in any::<u32>(),
            payload_grant in any::<u32>(),
            truncate_to in 0usize..BLK_REQUEST_HEADER_SIZE,
        ) {
            let hdr = BlkRequestHeader { kind, cmd_id, lba, sector_count, flags };
            let bytes = encode_blk_request(hdr, payload_grant);
            let r = decode_blk_request(&bytes[..truncate_to]);
            prop_assert!(matches!(r, Err(DecodeError::Truncated)));
        }

        #[test]
        fn prop_truncated_reply_is_error(
            cmd_id in any::<u64>(),
            status in any_block_driver_error(),
            bytes_count in any::<u32>(),
            payload_grant in any::<u32>(),
            truncate_to in 0usize..BLK_REPLY_HEADER_SIZE,
        ) {
            let hdr = BlkReplyHeader { cmd_id, status, bytes: bytes_count };
            let bytes = encode_blk_reply(hdr, payload_grant);
            let r = decode_blk_reply(&bytes[..truncate_to]);
            prop_assert!(matches!(r, Err(DecodeError::Truncated)));
        }
    }
}
