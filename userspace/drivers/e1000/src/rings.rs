//! E1000 descriptor rings — Phase 55b Track E.2 (Red).
//!
//! This file lands the failing-test scaffold. The real `allocate` /
//! `prepare_all` bodies land in the following commit; today the module
//! exposes just the constants and the pure helpers the tests need to
//! reference.

#![allow(dead_code)]

use kernel_core::e1000::{E1000RxDesc, E1000TxDesc};

/// Receive descriptor ring depth. Placeholder value; Green commit
/// promotes it to 256 per Intel §13.4.27.
pub const RX_RING_SIZE: usize = 1;

/// Transmit descriptor ring depth. Placeholder value.
pub const TX_RING_SIZE: usize = 1;

/// Per-descriptor receive buffer size. Placeholder; Green commit
/// promotes to 2048 to match `RCTL.BSIZE=00`.
pub const RX_BUF_SIZE: usize = 0;

/// Per-descriptor transmit buffer size. Placeholder.
pub const TX_BUF_SIZE: usize = 0;

/// Byte length of the RX descriptor ring.
pub const RX_RING_BYTES: usize = RX_RING_SIZE * core::mem::size_of::<E1000RxDesc>();

/// Byte length of the TX descriptor ring.
pub const TX_RING_BYTES: usize = TX_RING_SIZE * core::mem::size_of::<E1000TxDesc>();

/// Split a 64-bit IOVA into the `(low32, high32)` pair the e1000
/// `*DBAL` / `*DBAH` registers expect. Red stub — returns `(0, 0)`.
#[inline]
pub const fn split_iova(_iova: u64) -> (u32, u32) {
    (0, 0)
}

/// Initial value for `RDT` after pre-posting every descriptor. Red
/// stub — returns `0`, which makes the "one short of head" assertion
/// fail.
#[inline]
pub const fn initial_rdt() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rx_ring_bytes_is_multiple_of_128() {
        assert!(RX_RING_BYTES.is_multiple_of(128));
        assert_eq!(RX_RING_BYTES, 256 * 16);
    }

    #[test]
    fn tx_ring_bytes_is_multiple_of_128() {
        assert!(TX_RING_BYTES.is_multiple_of(128));
        assert_eq!(TX_RING_BYTES, 256 * 16);
    }

    #[test]
    fn split_iova_low_high_match_intel_ordering() {
        let iova: u64 = 0xDEAD_BEEF_CAFE_F00D;
        let (lo, hi) = split_iova(iova);
        assert_eq!(lo, 0xCAFE_F00D);
        assert_eq!(hi, 0xDEAD_BEEF);
        assert_eq!(((hi as u64) << 32) | (lo as u64), iova);
    }

    #[test]
    fn split_iova_under_4gib_high_is_zero() {
        let (lo, hi) = split_iova(0x0000_0000_1000_0000);
        assert_eq!(lo, 0x1000_0000);
        assert_eq!(hi, 0);
    }

    #[test]
    fn initial_rdt_is_head_minus_one_for_full_ring_prepost() {
        assert_eq!(initial_rdt(), (RX_RING_SIZE as u32) - 1);
        assert_eq!(initial_rdt(), 255);
    }

    #[test]
    fn ring_sizes_satisfy_intel_multiple_of_eight_rule() {
        assert!(RX_RING_SIZE.is_multiple_of(8));
        assert!(TX_RING_SIZE.is_multiple_of(8));
    }

    #[test]
    fn buffer_sizes_match_rctl_bsize_2048_programming() {
        assert_eq!(RX_BUF_SIZE, 2048);
        assert_eq!(TX_BUF_SIZE, 2048);
    }
}
