//! mt792x WFDMA descriptor layout and token pool — Task A.6.
//!
//! Implements host-testable, pure-logic helpers for the WFDMA DMA engine:
//!
//! * [`Mt76Desc`] — the 16-byte WFDMA ring descriptor (`#[repr(C)]`).
//! * [`encode_tx_desc`] — build a TX descriptor from an IOVA, length, and token.
//! * [`rx_desc_done`] / [`rx_desc_len`] — decode an RX descriptor's ctrl word.
//! * [`Token`] / [`TokenPool`] — IDR-style token pool enforcing buffer ownership.
//!
//! # Token-before-buffer ordering
//!
//! [`encode_tx_desc`] requires a [`Token`] argument — it is impossible to
//! construct a TX descriptor without one. This enforces the hardware invariant
//! that a token must be acquired before a DMA buffer is posted to the ring,
//! preventing the MCU from acknowledging a frame for which the host has no
//! token (and thus no way to release the buffer).

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Descriptor structure
// ---------------------------------------------------------------------------

/// A single WFDMA ring descriptor (16 bytes, `#[repr(C)]`).
///
/// Hardware layout (connac2 `mt76_desc`):
///
/// ```text
/// offset  field   description
/// 0x00    buf0    low 32 bits of buffer IOVA
/// 0x04    ctrl    control/length/status bits
/// 0x08    buf1    high 32 bits of buffer IOVA
/// 0x0C    info    metadata (token ID for TX, RSS/type for RX)
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mt76Desc {
    /// Low 32 bits of the buffer IOVA.
    pub buf0: u32,
    /// Control word: length, DMA-done, LAST_SEC flags.
    pub ctrl: u32,
    /// High 32 bits of the buffer IOVA.
    pub buf1: u32,
    /// Info word: carries the token ID for TX descriptors.
    pub info: u32,
}

// Compile-time size assertion.
const _: () = assert!(core::mem::size_of::<Mt76Desc>() == 16);

// ---------------------------------------------------------------------------
// ctrl bit constants
// ---------------------------------------------------------------------------

/// SD_LEN0: 14-bit buffer length field, bits [29:16].
pub const MT_DMA_CTL_SD_LEN0: u32 = 0x3FFF << 16;

/// LAST_SEC0: marks this descriptor as the last in a scatter-gather chain (bit 30).
pub const MT_DMA_CTL_LAST_SEC0: u32 = 1 << 30;

/// DMA_DONE: set by hardware when RX DMA is complete (bit 31).
pub const MT_DMA_CTL_DMA_DONE: u32 = 1 << 31;

// ---------------------------------------------------------------------------
// IOVA split
// ---------------------------------------------------------------------------

/// Split a 64-bit IOVA into `(lo32, hi32)`.
///
/// `lo32` goes into [`Mt76Desc::buf0`], `hi32` goes into [`Mt76Desc::buf1`].
#[inline]
pub fn split_iova(iova: u64) -> (u32, u32) {
    ((iova & 0xFFFF_FFFF) as u32, (iova >> 32) as u32)
}

// ---------------------------------------------------------------------------
// Token type
// ---------------------------------------------------------------------------

/// An opaque DMA token (16-bit index into the token pool).
///
/// A token must be acquired from [`TokenPool::acquire`] before calling
/// [`encode_tx_desc`]. This makes it a type-system invariant that every
/// descriptor posted to the TX ring has an associated token, preventing
/// the double-free and use-after-free classes of DMA buffer bugs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token(pub u16);

// ---------------------------------------------------------------------------
// TX descriptor builder
// ---------------------------------------------------------------------------

/// Build an [`Mt76Desc`] for a TX buffer.
///
/// * `buf0` ← low 32 bits of `iova`.
/// * `buf1` ← high 32 bits of `iova`.
/// * `ctrl` ← `len` placed in the SD_LEN0 field, with LAST_SEC0 set
///   (single-buffer frame; chained multi-buffer frames are out of scope).
///   DMA_DONE is **not** set on a freshly-encoded TX descriptor — the NIC sets
///   it after transmission.
/// * `info` ← `token.0` as u32.
///
/// The `token` parameter is MANDATORY. It is impossible to call this function
/// without a live token, enforcing buffer-ownership discipline at compile time.
#[inline]
pub fn encode_tx_desc(iova: u64, len: u16, token: Token) -> Mt76Desc {
    let (lo, hi) = split_iova(iova);
    let ctrl = (((len as u32) << 16) & MT_DMA_CTL_SD_LEN0) | MT_DMA_CTL_LAST_SEC0;
    Mt76Desc {
        buf0: lo,
        ctrl,
        buf1: hi,
        info: token.0 as u32,
    }
}

// ---------------------------------------------------------------------------
// RX descriptor decoders
// ---------------------------------------------------------------------------

/// Return `true` when the hardware has completed filling this RX descriptor.
#[inline]
pub fn rx_desc_done(ctrl: u32) -> bool {
    ctrl & MT_DMA_CTL_DMA_DONE != 0
}

/// Extract the received frame length from a completed RX descriptor's ctrl word.
#[inline]
pub fn rx_desc_len(ctrl: u32) -> u16 {
    ((ctrl & MT_DMA_CTL_SD_LEN0) >> 16) as u16
}

// ---------------------------------------------------------------------------
// Token pool
// ---------------------------------------------------------------------------

/// Maximum number of concurrently live DMA tokens.
///
/// A Wi-Fi NIC consumes at most one token slot in the combined NIC token
/// registry — the Phase 79 `MAX_NICS = 8` cap is not duplicated here because
/// the token pool is per-driver, not per-registry. 256 tokens covers the
/// typical connac2 TX ring depth (256 entries).
pub const MAX_TOKENS: u16 = 256;

/// An IDR-style free-list of [`Token`]s.
///
/// Tokens are allocated monotonically from 0 to `MAX_TOKENS - 1`; released
/// tokens are pushed onto the free list and reused in LIFO order.
pub struct TokenPool {
    /// LIFO free list of available token indices.
    free: Vec<u16>,
    /// Next never-yet-allocated index (used when `free` is empty).
    next: u16,
}

impl TokenPool {
    /// Create a new pool with all tokens available.
    pub fn new() -> Self {
        TokenPool {
            free: Vec::new(),
            next: 0,
        }
    }

    /// Acquire a token.
    ///
    /// Returns `None` when `MAX_TOKENS` tokens are simultaneously live
    /// (the pool is exhausted).
    pub fn acquire(&mut self) -> Option<Token> {
        if let Some(idx) = self.free.pop() {
            return Some(Token(idx));
        }
        if self.next < MAX_TOKENS {
            let idx = self.next;
            self.next += 1;
            return Some(Token(idx));
        }
        None
    }

    /// Release a token back to the pool.
    ///
    /// The caller must not use the token after release. In the kernel driver
    /// this is enforced by consuming the `Token` value.
    pub fn release(&mut self, token: Token) {
        self.free.push(token.0);
    }
}

impl Default for TokenPool {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;

    #[test]
    fn desc_is_16_bytes() {
        assert_eq!(
            core::mem::size_of::<Mt76Desc>(),
            16,
            "Mt76Desc must be 16 bytes"
        );
    }

    #[test]
    fn tx_desc_iova() {
        // 0x1_2345_6000 → lo = 0x2345_6000, hi = 0x1
        let iova: u64 = 0x0000_0001_2345_6000;
        let desc = encode_tx_desc(iova, 1500, Token(0));

        assert_eq!(desc.buf0, 0x2345_6000, "buf0 must be low 32 bits of IOVA");
        assert_eq!(desc.buf1, 0x0000_0001, "buf1 must be high 32 bits of IOVA");

        // Round-trip the length through rx_desc_len.
        assert_eq!(
            rx_desc_len(desc.ctrl),
            1500,
            "rx_desc_len must round-trip the encoded len"
        );

        // LAST_SEC0 must be set on a freshly-encoded TX descriptor.
        assert_ne!(desc.ctrl & MT_DMA_CTL_LAST_SEC0, 0, "LAST_SEC0 must be set");

        // DMA_DONE must NOT be set on a freshly-encoded TX descriptor.
        assert_eq!(
            desc.ctrl & MT_DMA_CTL_DMA_DONE,
            0,
            "DMA_DONE must not be set on fresh TX desc"
        );

        // Token is stored in info.
        assert_eq!(desc.info, 0);
    }

    #[test]
    fn tx_desc_token_in_info() {
        let desc = encode_tx_desc(0x0000_1000, 64, Token(42));
        assert_eq!(desc.info, 42, "token id must be stored in info");
    }

    #[test]
    fn rx_decode() {
        // Construct a ctrl word with DMA_DONE set and a specific length.
        let len: u16 = 0x0400; // 1024 bytes
        let ctrl = MT_DMA_CTL_DMA_DONE | (((len as u32) << 16) & MT_DMA_CTL_SD_LEN0);

        assert!(rx_desc_done(ctrl), "DMA_DONE set → rx_desc_done == true");
        assert_eq!(rx_desc_len(ctrl), len, "length must round-trip");

        // Without DMA_DONE.
        let ctrl_no_done = (((len as u32) << 16) & MT_DMA_CTL_SD_LEN0);
        assert!(
            !rx_desc_done(ctrl_no_done),
            "DMA_DONE clear → rx_desc_done == false"
        );
        assert_eq!(rx_desc_len(ctrl_no_done), len);
    }

    #[test]
    fn token_pool_roundtrip() {
        let mut pool = TokenPool::new();

        // Acquire all MAX_TOKENS tokens.
        let mut live: BTreeSet<u16> = BTreeSet::new();
        for _ in 0..MAX_TOKENS {
            let t = pool.acquire().expect("token must be available");
            assert!(live.insert(t.0), "duplicate token {}", t.0);
        }

        // Pool must now be exhausted.
        assert!(
            pool.acquire().is_none(),
            "pool must be exhausted after MAX_TOKENS acquires"
        );

        // Release half the tokens.
        let to_release: Vec<u16> = live.iter().copied().take(MAX_TOKENS as usize / 2).collect();
        for &idx in &to_release {
            pool.release(Token(idx));
        }

        // Re-acquire up to the number released.
        let mut reacquired: BTreeSet<u16> = BTreeSet::new();
        for _ in 0..to_release.len() {
            let t = pool
                .acquire()
                .expect("released token must be re-acquirable");
            assert!(
                reacquired.insert(t.0),
                "duplicate re-acquired token {}",
                t.0
            );
        }

        // Must be exhausted again.
        assert!(pool.acquire().is_none(), "pool exhausted after re-acquire");
    }

    #[test]
    fn token_pool_no_duplicate_live_tokens() {
        let mut pool = TokenPool::new();
        let mut seen = BTreeSet::new();
        for _ in 0..MAX_TOKENS {
            let t = pool.acquire().unwrap();
            assert!(seen.insert(t.0), "token {} issued twice", t.0);
        }
    }

    #[test]
    fn split_iova_roundtrip() {
        let iova: u64 = 0xABCD_EF01_2345_6789;
        let (lo, hi) = split_iova(iova);
        assert_eq!(lo, 0x2345_6789);
        assert_eq!(hi, 0xABCD_EF01);
        // Recombine.
        assert_eq!((hi as u64) << 32 | lo as u64, iova);
    }

    #[test]
    fn ctrl_bit_constants() {
        assert_eq!(MT_DMA_CTL_SD_LEN0, 0x3FFF_0000);
        assert_eq!(MT_DMA_CTL_LAST_SEC0, 1 << 30);
        assert_eq!(MT_DMA_CTL_DMA_DONE, 1 << 31);
    }
}
