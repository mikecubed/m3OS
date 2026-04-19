//! E1000 descriptor ring allocation — Phase 55b Track E.2.
//!
//! [`RxDescRing`] and [`TxDescRing`] own the `DmaBuffer<[E1000{Rx,Tx}Desc;
//! N]>` + per-slot `DmaBuffer<[u8; RX_BUF_SIZE]>` allocations required by
//! the e1000 bring-up path. The ring structs are a direct port of the
//! Phase 55 in-kernel `kernel/src/net/e1000.rs` rings; what changes is the
//! `DmaBuffer` provenance — ring and buffer allocations now route through
//! `driver_runtime::DmaBuffer` (which is in turn backed by
//! `sys_device_dma_alloc`), and their device-visible addresses are
//! **IOVAs** (per the Phase 55a IOMMU contract), not host physical
//! addresses.
//!
//! # DmaBuffer typing
//!
//! `DmaBuffer::<T>::allocate` requires `T: Sized`. Writing
//! `DmaBuffer::<[E1000RxDesc]>::allocate(...)` is a compile error — the
//! slice type `[E1000RxDesc]` is unsized. We therefore type the rings as
//! fixed-size arrays and use [`RX_RING_SIZE`] / [`TX_RING_SIZE`] as the
//! array length:
//!
//! ```ignore
//! let rx_ring: DmaBuffer<[E1000RxDesc; RX_RING_SIZE]> = DmaBuffer::allocate(&dev, bytes, align)?;
//! let tx_ring: DmaBuffer<[E1000TxDesc; TX_RING_SIZE]> = DmaBuffer::allocate(&dev, bytes, align)?;
//! ```
//!
//! Per-slot packet buffers are sized arrays as well:
//!
//! ```ignore
//! let rx_buf: DmaBuffer<[u8; RX_BUF_SIZE]> = DmaBuffer::allocate(&dev, RX_BUF_SIZE, 8)?;
//! ```
//!
//! This mirrors the Phase 55a gotcha written up in the Track E.2 task
//! brief: the `T: Sized` bound is load-bearing and drivers that want a
//! byte-slice view bounce through
//! `core::slice::from_raw_parts_mut(buf.user_ptr() as *mut u8, buf.len())`.

#![allow(dead_code)] // E.3 consumes the ring accessors; keep them built today.

extern crate alloc;

use alloc::vec::Vec;

use driver_runtime::{DeviceHandle, DmaBuffer, DriverRuntimeError};
use kernel_core::e1000::{E1000RxDesc, E1000TxDesc};

/// Receive descriptor ring depth. Multiple of 8 per Intel §13.4.27 and
/// matches Intel's recommended default for the 82540EM. Held as a
/// `const` so the compiler sees it in the `[T; N]` type of the
/// `DmaBuffer` allocations.
pub const RX_RING_SIZE: usize = 256;

/// Transmit descriptor ring depth — see [`RX_RING_SIZE`] notes.
pub const TX_RING_SIZE: usize = 256;

/// Per-descriptor receive buffer size. Paired with the `RCTL.BSIZE=00`
/// (2 KiB) programming in [`super::init`].
pub const RX_BUF_SIZE: usize = 2048;

/// Per-descriptor transmit buffer size. One MTU-sized buffer per slot.
pub const TX_BUF_SIZE: usize = 2048;

// Compile-time gates matching Intel §13.4.27 / §13.4.40: ring length in
// **bytes** must be a multiple of 128. With a 16-byte descriptor, that
// reduces to a multiple-of-8 constraint on the slot count.
const _: () = assert!(RX_RING_SIZE.is_multiple_of(8));
const _: () = assert!(TX_RING_SIZE.is_multiple_of(8));
const _: () = assert!(RX_RING_SIZE <= 4096);
const _: () = assert!(TX_RING_SIZE <= 4096);

/// Byte length of the RX descriptor ring — the `RDLEN` register value.
pub const RX_RING_BYTES: usize = RX_RING_SIZE * core::mem::size_of::<E1000RxDesc>();

/// Byte length of the TX descriptor ring — the `TDLEN` register value.
pub const TX_RING_BYTES: usize = TX_RING_SIZE * core::mem::size_of::<E1000TxDesc>();

// Spec §13.4.27 requires RDLEN to be a multiple of 128 bytes. TDLEN
// likewise. The compile-time gates above make this automatic, but we
// spell the byte-level invariant too so a future tweak to the
// descriptor size can't silently violate it.
const _: () = assert!(RX_RING_BYTES.is_multiple_of(128));
const _: () = assert!(TX_RING_BYTES.is_multiple_of(128));

/// RX descriptor ring + per-slot packet buffers.
///
/// Ownership contract: the `DmaBuffer`s are held on the struct for the
/// driver's lifetime; dropping the ring releases the underlying
/// capabilities (the kernel frees the DMA allocations on process exit —
/// see `driver_runtime::DmaBuffer` module docs).
pub struct RxDescRing {
    /// Ring of `RX_RING_SIZE` descriptors as a fixed-size `DmaBuffer`.
    pub(crate) descs: DmaBuffer<[E1000RxDesc; RX_RING_SIZE]>,
    /// One DMA-mapped packet buffer per ring slot. Kept individually so
    /// Drop reclaims them separately and so a future phase can swap in
    /// an SGL-style multi-buffer layout without touching the ring.
    pub(crate) bufs: Vec<DmaBuffer<[u8; RX_BUF_SIZE]>>,
    /// Cached IOVA of the first descriptor. Programmed into
    /// `RDBAL`/`RDBAH`.
    pub(crate) ring_iova: u64,
    /// Cached per-slot buffer IOVAs. Programmed into each descriptor's
    /// `buffer_addr` field at `prepare_all` time.
    pub(crate) buf_iova: Vec<u64>,
    /// Software tail — the next slot the task will hand back to hardware.
    pub(crate) next_to_read: usize,
}

/// TX descriptor ring + per-slot packet buffers.
pub struct TxDescRing {
    pub(crate) descs: DmaBuffer<[E1000TxDesc; TX_RING_SIZE]>,
    pub(crate) bufs: Vec<DmaBuffer<[u8; TX_BUF_SIZE]>>,
    pub(crate) ring_iova: u64,
    pub(crate) buf_iova: Vec<u64>,
    pub(crate) next_to_write: usize,
}

impl RxDescRing {
    /// Allocate the RX descriptor ring and its per-slot packet buffers.
    ///
    /// Returns a ring whose descriptors are pre-populated with their
    /// per-slot buffer IOVAs and zeroed status bytes — the caller (the
    /// init path) programs `RDBAL/RDBAH/RDLEN` from [`Self::ring_iova`]
    /// / [`RX_RING_BYTES`] and then advances `RDT` to
    /// [`RX_RING_SIZE`] - 1 to hand every slot to the MAC.
    pub fn allocate(handle: &DeviceHandle) -> Result<Self, DriverRuntimeError> {
        let descs = DmaBuffer::<[E1000RxDesc; RX_RING_SIZE]>::allocate(
            handle,
            RX_RING_BYTES,
            core::mem::align_of::<E1000RxDesc>().max(128),
        )?;
        let ring_iova = descs.iova();

        let mut bufs: Vec<DmaBuffer<[u8; RX_BUF_SIZE]>> = Vec::with_capacity(RX_RING_SIZE);
        let mut buf_iova: Vec<u64> = Vec::with_capacity(RX_RING_SIZE);
        for _ in 0..RX_RING_SIZE {
            let buf = DmaBuffer::<[u8; RX_BUF_SIZE]>::allocate(handle, RX_BUF_SIZE, 8)?;
            buf_iova.push(buf.iova());
            bufs.push(buf);
        }

        let mut ring = Self {
            descs,
            bufs,
            ring_iova,
            buf_iova,
            next_to_read: 0,
        };
        ring.prepare_all();
        Ok(ring)
    }

    /// Fill every descriptor with its per-slot IOVA and zero the
    /// hardware-written status fields. Run once at init and on every
    /// reset-after-link-up.
    pub fn prepare_all(&mut self) {
        // SAFETY: `self.descs` is a `DmaBuffer<[E1000RxDesc; N]>` with
        // non-null, aligned backing memory; `DerefMut` on `DmaBuffer<T>`
        // hands back `&mut [E1000RxDesc; N]` of the same length.
        let descs: &mut [E1000RxDesc; RX_RING_SIZE] = &mut self.descs;
        for (i, desc) in descs.iter_mut().enumerate() {
            *desc = E1000RxDesc {
                buffer_addr: self.buf_iova[i],
                length: 0,
                checksum: 0,
                status: 0,
                errors: 0,
                special: 0,
            };
        }
        self.next_to_read = 0;
    }

    /// Borrow the descriptor array immutably. Length equals
    /// [`RX_RING_SIZE`] by construction.
    pub fn descs(&self) -> &[E1000RxDesc; RX_RING_SIZE] {
        &self.descs
    }

    /// Borrow the descriptor array mutably.
    pub fn descs_mut(&mut self) -> &mut [E1000RxDesc; RX_RING_SIZE] {
        &mut self.descs
    }
}

impl TxDescRing {
    /// Allocate the TX descriptor ring and its per-slot packet buffers.
    pub fn allocate(handle: &DeviceHandle) -> Result<Self, DriverRuntimeError> {
        let descs = DmaBuffer::<[E1000TxDesc; TX_RING_SIZE]>::allocate(
            handle,
            TX_RING_BYTES,
            core::mem::align_of::<E1000TxDesc>().max(128),
        )?;
        let ring_iova = descs.iova();

        let mut bufs: Vec<DmaBuffer<[u8; TX_BUF_SIZE]>> = Vec::with_capacity(TX_RING_SIZE);
        let mut buf_iova: Vec<u64> = Vec::with_capacity(TX_RING_SIZE);
        for _ in 0..TX_RING_SIZE {
            let buf = DmaBuffer::<[u8; TX_BUF_SIZE]>::allocate(handle, TX_BUF_SIZE, 8)?;
            buf_iova.push(buf.iova());
            bufs.push(buf);
        }

        let mut ring = Self {
            descs,
            bufs,
            ring_iova,
            buf_iova,
            next_to_write: 0,
        };
        // Pre-seed each TX descriptor with its per-slot IOVA so the
        // hot-path in E.3 can overwrite only `length`, `cmd`, and
        // `status`.
        {
            let descs: &mut [E1000TxDesc; TX_RING_SIZE] = &mut ring.descs;
            for (i, desc) in descs.iter_mut().enumerate() {
                *desc = E1000TxDesc::default();
                desc.buffer_addr = ring.buf_iova[i];
            }
        }
        Ok(ring)
    }

    pub fn descs(&self) -> &[E1000TxDesc; TX_RING_SIZE] {
        &self.descs
    }

    pub fn descs_mut(&mut self) -> &mut [E1000TxDesc; TX_RING_SIZE] {
        &mut self.descs
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — tested on host
// ---------------------------------------------------------------------------

/// Split a 64-bit IOVA into the `(low32, high32)` pair the e1000
/// `*DBAL` / `*DBAH` registers expect. Hardware reads low first.
#[inline]
pub const fn split_iova(iova: u64) -> (u32, u32) {
    ((iova & 0xFFFF_FFFF) as u32, (iova >> 32) as u32)
}

/// Initial value for the RX tail register `RDT` after pre-posting every
/// descriptor. Intel §13.4.28: `RDH == RDT` means "ring empty"; setting
/// `RDT = RDH - 1` hands all but one slot to the MAC. With `RDH = 0`
/// that reduces to `RX_RING_SIZE - 1`.
#[inline]
pub const fn initial_rdt() -> u32 {
    (RX_RING_SIZE as u32) - 1
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
        // Round-trip through the hardware split must reconstruct the
        // original IOVA.
        assert_eq!(((hi as u64) << 32) | (lo as u64), iova);
    }

    #[test]
    fn split_iova_under_4gib_high_is_zero() {
        // Identity-map fallback IOVAs live in low memory; the high half
        // register should write zero there.
        let (lo, hi) = split_iova(0x0000_0000_1000_0000);
        assert_eq!(lo, 0x1000_0000);
        assert_eq!(hi, 0);
    }

    #[test]
    fn initial_rdt_is_head_minus_one_for_full_ring_prepost() {
        // Acceptance bullet: RX pre-post leaves RDT one short of head.
        // With RDH=0, RDT must be RX_RING_SIZE - 1 so the hardware sees
        // `(RDH, RDT] == [0, RX_RING_SIZE-1]` of live descriptors.
        assert_eq!(initial_rdt(), (RX_RING_SIZE as u32) - 1);
        assert_eq!(initial_rdt(), 255);
    }

    #[test]
    fn ring_sizes_satisfy_intel_multiple_of_eight_rule() {
        // §13.4.27 and §13.4.40: ring element count must make RDLEN /
        // TDLEN a multiple of 128 bytes. The compile-time gates catch
        // this too, but the explicit test documents intent.
        assert!(RX_RING_SIZE.is_multiple_of(8));
        assert!(TX_RING_SIZE.is_multiple_of(8));
    }

    #[test]
    fn buffer_sizes_match_rctl_bsize_2048_programming() {
        // RCTL programs 2 KiB buffers (BSIZE=00 + BSEX=0). The per-slot
        // buffers must match exactly — a smaller buffer would be
        // overrun by a max-size frame, a larger one wastes IOVA space.
        assert_eq!(RX_BUF_SIZE, 2048);
        assert_eq!(TX_BUF_SIZE, 2048);
    }
}
