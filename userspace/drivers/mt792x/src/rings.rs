//! mt792x WFDMA TX/RX data rings (Task A.6 driver-side).
//!
//! Mirrors `userspace/drivers/r8169/src/rings.rs` for structure: DmaBuffer-backed
//! descriptor rings + per-slot packet buffers. Descriptor encoding uses the
//! host-tested `kernel_core::mt792x::dma` helpers.
//!
//! ## Token-before-buffer ordering
//!
//! `encode_tx_desc` requires a `Token` argument — it is impossible to encode a
//! TX descriptor without first acquiring a token from the `TokenPool`. This
//! enforces at compile time that every descriptor posted to the TX ring has an
//! associated token, preventing the MCU from acknowledging a frame for which
//! the host has no token (and thus no way to release the buffer). The ordering
//! is: `TokenPool::acquire` → `encode_tx_desc` → post descriptor.
//!
//! ## WFDMA descriptor layout
//!
//! Each `Mt76Desc` is 16 bytes (`#[repr(C)]`):
//! - `buf0` — low 32 bits of buffer IOVA
//! - `ctrl` — length (bits [29:16]), LAST_SEC0 (bit 30), DMA_DONE (bit 31)
//! - `buf1` — high 32 bits of buffer IOVA
//! - `info` — token ID (TX) or metadata (RX)

extern crate alloc;

use alloc::vec::Vec;

use driver_runtime::{DeviceHandle, DmaBuffer, DriverRuntimeError};
use kernel_core::mt792x::dma::{
    Mt76Desc, Token, TokenPool, encode_tx_desc, rx_desc_done, rx_desc_len,
};

/// Number of TX descriptors in the data ring.
pub const TX_RING_SIZE: usize = 64;
/// Number of RX descriptors in the data ring.
pub const RX_RING_SIZE: usize = 64;

/// Per-slot packet buffer size (one MTU + Wi-Fi overhead headroom).
pub const TX_BUF_SIZE: usize = 2048;
/// Per-slot RX packet buffer size.
pub const RX_BUF_SIZE: usize = 2048;

/// Byte length of the TX descriptor ring (64 × 16 bytes = 1024 bytes).
pub const TX_RING_BYTES: usize = TX_RING_SIZE * core::mem::size_of::<Mt76Desc>();
/// Byte length of the RX descriptor ring.
pub const RX_RING_BYTES: usize = RX_RING_SIZE * core::mem::size_of::<Mt76Desc>();

// Compile-time assertions — ring byte lengths must be multiples of 64 (the
// WFDMA ring-base alignment requirement from the connac2 specification).
const _: () = assert!(TX_RING_BYTES.is_multiple_of(64));
const _: () = assert!(RX_RING_BYTES.is_multiple_of(64));

/// A DMA-backed WFDMA data descriptor ring plus its per-slot packet buffers.
///
/// Uses `DmaBuffer<[u8; ...]>` for both the descriptor ring and the per-slot
/// packet buffers, matching the r8169 pattern. The descriptor ring bytes are
/// interpreted via the `Mt76Desc` layout (not a `#[repr(C)]` slice — the flat
/// bytes are encoded/decoded by the `kernel_core::mt792x::dma` helpers).
pub struct DescRing {
    /// Flat byte buffer backing the descriptor ring.
    ring: DmaBuffer<[u8; TX_RING_BYTES]>, // TX_RING_BYTES == RX_RING_BYTES
    /// Per-slot packet buffers.
    bufs: Vec<DmaBuffer<[u8; TX_BUF_SIZE]>>, // TX_BUF_SIZE == RX_BUF_SIZE
    /// Software cursor: next slot the driver fills (TX) or inspects (RX).
    pub idx: usize,
    /// Slot count.
    pub count: usize,
    /// Token pool for TX descriptor ownership tracking.
    pub tokens: TokenPool,
}

impl DescRing {
    /// Allocate `count` descriptors + `count` packet buffers.
    pub fn alloc(pci: &DeviceHandle, count: usize) -> Result<Self, DriverRuntimeError> {
        // Reject over-size requests that would overflow the fixed ring arrays.
        if count > TX_RING_SIZE {
            use kernel_core::device_host::DeviceHostError;
            return Err(DriverRuntimeError::Device(DeviceHostError::Internal));
        }

        let ring = DmaBuffer::<[u8; TX_RING_BYTES]>::allocate(pci, TX_RING_BYTES, 64)?;

        let mut bufs: Vec<DmaBuffer<[u8; TX_BUF_SIZE]>> = Vec::with_capacity(count);
        for _ in 0..count {
            let b = DmaBuffer::<[u8; TX_BUF_SIZE]>::allocate(pci, TX_BUF_SIZE, 16)?;
            bufs.push(b);
        }

        Ok(DescRing {
            ring,
            bufs,
            idx: 0,
            count,
            tokens: TokenPool::new(),
        })
    }

    /// Base IOVA of the descriptor ring (programmed into the WFDMA desc_base register).
    #[inline]
    pub fn base_iova(&self) -> u64 {
        self.ring.iova()
    }

    /// Post a TX descriptor for `slot`: acquire a token, copy `frame` into the
    /// slot buffer, encode the descriptor, and write it into the ring.
    ///
    /// Returns `false` if the frame is too large or no token is available.
    pub fn post_tx(&mut self, slot: usize, frame: &[u8]) -> bool {
        if frame.len() > TX_BUF_SIZE || slot >= self.count {
            return false;
        }
        let token = match self.tokens.acquire() {
            Some(t) => t,
            None => return false,
        };

        // Copy frame bytes into the DMA buffer for this slot.
        {
            let buf: &mut [u8; TX_BUF_SIZE] = &mut self.bufs[slot];
            buf[..frame.len()].copy_from_slice(frame);
        }

        // Encode the TX descriptor: acquire token FIRST (enforcing
        // token-before-buffer ordering), then encode.
        let iova = self.bufs[slot].iova();
        let desc = encode_tx_desc(iova, frame.len() as u16, token);
        self.write_desc(slot, desc);
        true
    }

    /// Check if the RX descriptor at `slot` has been filled by the hardware.
    pub fn rx_done(&self, slot: usize) -> bool {
        let desc = self.read_desc(slot);
        rx_desc_done(desc.ctrl)
    }

    /// Extract the received frame length from a completed RX descriptor.
    pub fn rx_len(&self, slot: usize) -> u16 {
        let desc = self.read_desc(slot);
        rx_desc_len(desc.ctrl)
    }

    /// Borrow the RX slot buffer as a byte slice. `len` is clamped to the buffer size.
    pub fn rx_slice(&self, slot: usize, len: usize) -> &[u8] {
        let buf: &[u8; RX_BUF_SIZE] = &self.bufs[slot];
        &buf[..len.min(RX_BUF_SIZE)]
    }

    /// Re-arm an RX descriptor after the host has consumed the frame: clear the
    /// DMA_DONE bit and program the buffer IOVA back into the descriptor so the
    /// hardware can refill it.
    pub fn rearm_rx(&mut self, slot: usize) {
        let iova = self.bufs[slot].iova();
        // For RX, we re-encode without a token (tokens are TX-only).
        // Use a placeholder Token(0) — the token field is ignored in RX descriptors.
        let desc = encode_tx_desc(iova, RX_BUF_SIZE as u16, Token(0));
        // Clear DMA_DONE so the hardware can reuse this slot.
        // encode_tx_desc sets LAST_SEC0 and places the length; the cleared
        // DMA_DONE bit signals "host-owned, ready for hardware to fill".
        self.write_desc(slot, desc);
    }

    // --- Internal helpers ---

    fn write_desc(&mut self, slot: usize, desc: Mt76Desc) {
        let backing: &mut [u8; TX_RING_BYTES] = &mut self.ring;
        let base = slot * core::mem::size_of::<Mt76Desc>();
        backing[base..base + 4].copy_from_slice(&desc.buf0.to_le_bytes());
        backing[base + 4..base + 8].copy_from_slice(&desc.ctrl.to_le_bytes());
        backing[base + 8..base + 12].copy_from_slice(&desc.buf1.to_le_bytes());
        backing[base + 12..base + 16].copy_from_slice(&desc.info.to_le_bytes());
    }

    fn read_desc(&self, slot: usize) -> Mt76Desc {
        let backing: &[u8; TX_RING_BYTES] = &self.ring;
        let base = slot * core::mem::size_of::<Mt76Desc>();
        let buf0 = u32::from_le_bytes([
            backing[base],
            backing[base + 1],
            backing[base + 2],
            backing[base + 3],
        ]);
        let ctrl = u32::from_le_bytes([
            backing[base + 4],
            backing[base + 5],
            backing[base + 6],
            backing[base + 7],
        ]);
        let buf1 = u32::from_le_bytes([
            backing[base + 8],
            backing[base + 9],
            backing[base + 10],
            backing[base + 11],
        ]);
        let info = u32::from_le_bytes([
            backing[base + 12],
            backing[base + 13],
            backing[base + 14],
            backing[base + 15],
        ]);
        Mt76Desc {
            buf0,
            ctrl,
            buf1,
            info,
        }
    }
}

/// WFDMA TX and RX data ring pair for first-light data path.
///
/// One data TXQ + one data RXQ as required for the initial bring-up. Wave 3
/// (DRV-net) will wire these into the net.nic IPC loop.
pub struct DataRings {
    pub txq: DescRing,
    pub rxq: DescRing,
}

impl DataRings {
    /// Allocate TX + RX descriptor rings and packet buffers.
    pub fn allocate(pci: &DeviceHandle) -> Result<Self, DriverRuntimeError> {
        let txq = DescRing::alloc(pci, TX_RING_SIZE)?;
        let rxq = DescRing::alloc(pci, RX_RING_SIZE)?;
        Ok(DataRings { txq, rxq })
    }

    /// TX ring base IOVA (programmed into the WFDMA TX desc_base register).
    #[inline]
    pub fn tx_base_iova(&self) -> u64 {
        self.txq.base_iova()
    }

    /// RX ring base IOVA (programmed into the WFDMA RX desc_base register).
    #[inline]
    pub fn rx_base_iova(&self) -> u64 {
        self.rxq.base_iova()
    }
}
