//! Block-driver IPC protocol schema — Phase 55b Track A.2.
//!
//! TDD red-phase stub: types and function shapes exist so the test module
//! compiles; encoding is deliberately wrong so assertions fail. The green
//! commit replaces `encode_*` / `decode_*` with the real implementation.

#![allow(clippy::needless_range_loop)]

// ------------------------------------------------------------------------
// Message-label constants — shapes only; values are deliberately wrong in
// the red stub so the pinning tests fail until the green commit lands.
// ------------------------------------------------------------------------

pub const BLK_READ: u16 = 0;
pub const BLK_WRITE: u16 = 0;
pub const BLK_STATUS: u16 = 0;

pub const MAX_SECTORS_PER_REQUEST: u32 = 0;

pub const BLK_REQUEST_HEADER_SIZE: usize = 30;
pub const BLK_REPLY_HEADER_SIZE: usize = 20;

// ------------------------------------------------------------------------
// BlockDriverError
// ------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum BlockDriverError {
    Ok,
    IoError,
    InvalidLba,
    DeviceAbsent,
    Busy,
    DriverRestarting,
    InvalidRequest,
}

impl BlockDriverError {
    pub const fn to_byte(self) -> u8 {
        // Red stub: all variants collapse to zero so the byte round-trip
        // test fails.
        let _ = self;
        0
    }

    pub const fn from_byte(b: u8) -> Option<Self> {
        let _ = b;
        None
    }
}

// ------------------------------------------------------------------------
// BlkRequestHeader / BlkReplyHeader
// ------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlkRequestHeader {
    pub kind: u16,
    pub cmd_id: u64,
    pub lba: u64,
    pub sector_count: u32,
    pub flags: u32,
}

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DecodeError {
    Truncated,
    ReservedNonZero,
    UnknownKind,
    UnknownStatus,
}

// ------------------------------------------------------------------------
// Encode / decode helpers — red stub: always-zero output, always-Truncated
// decode. Green commit replaces these with the real encoding.
// ------------------------------------------------------------------------

pub const fn encode_blk_request(
    _header: BlkRequestHeader,
    _payload_grant: u32,
) -> [u8; BLK_REQUEST_HEADER_SIZE] {
    [0u8; BLK_REQUEST_HEADER_SIZE]
}

pub fn decode_blk_request(_bytes: &[u8]) -> Result<(BlkRequestHeader, u32), DecodeError> {
    Err(DecodeError::Truncated)
}

pub const fn encode_blk_reply(
    _header: BlkReplyHeader,
    _payload_grant: u32,
) -> [u8; BLK_REPLY_HEADER_SIZE] {
    [0u8; BLK_REPLY_HEADER_SIZE]
}

pub fn decode_blk_reply(_bytes: &[u8]) -> Result<(BlkReplyHeader, u32), DecodeError> {
    Err(DecodeError::Truncated)
}

// ------------------------------------------------------------------------
// Tests — authoritative for Phase 55b Track A.2. These are the Red-phase
// assertions; they exercise every Acceptance bullet. They fail against the
// stub above and pass once the green commit lands.
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
