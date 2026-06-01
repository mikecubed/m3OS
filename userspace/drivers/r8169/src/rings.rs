//! r8169 C+ descriptor-ring construction (Track C.1).
//!
//! The bit-level descriptor encoding lives in `kernel_core::r8169` (host-tested);
//! this module wires DMA-backed ring buffers to that pure builder. The Realtek
//! C+ ring is structurally different from the Intel engine: ownership is
//! per-descriptor via the `OWN` bit and the last slot carries `EOR`, so this is
//! a from-scratch ring rather than a re-skin of `driver_runtime::net_ring`.

extern crate alloc;

use alloc::vec::Vec;

use driver_runtime::{DeviceHandle, DmaBuffer, DriverRuntimeError};
use kernel_core::device_host::DeviceHostError;
use kernel_core::r8169 as hw;

/// Ring depth (descriptors). 64 slots * 16-byte descriptors = 1024 bytes, so the
/// ring byte length is `RING_ALIGN`-aligned (1024 is a multiple of 256); combined
/// with the page-aligned DMA base this keeps every wrap aligned.
pub const RX_RING_SIZE: usize = 64;
/// TX ring depth.
pub const TX_RING_SIZE: usize = 64;
/// Per-slot packet buffer size (one MTU + headroom).
pub const RX_BUF_SIZE: usize = 2048;
/// Per-slot TX buffer size.
pub const TX_BUF_SIZE: usize = 2048;

/// Byte length of the RX descriptor ring.
pub const RX_RING_BYTES: usize = RX_RING_SIZE * hw::DESC_SIZE;
/// Byte length of the TX descriptor ring.
pub const TX_RING_BYTES: usize = TX_RING_SIZE * hw::DESC_SIZE;

// The C+ ring base must be 256-byte aligned and the byte length a multiple of
// 256 so the wrap stays aligned. Spell both invariants as compile gates.
const _: () = assert!(RX_RING_BYTES.is_multiple_of(hw::RING_ALIGN));
const _: () = assert!(TX_RING_BYTES.is_multiple_of(hw::RING_ALIGN));
const _: () = assert!(RX_RING_SIZE == TX_RING_SIZE);
const _: () = assert!(RX_BUF_SIZE == TX_BUF_SIZE);

/// A DMA-backed C+ descriptor ring plus its per-slot packet buffers.
///
/// The descriptor ring is held as a flat byte `DmaBuffer` (descriptors are
/// encoded via the pure `kernel_core::r8169` builder rather than a `#[repr(C)]`
/// struct, so a byte array keeps the layout under the host-tested encoder). RX
/// and TX rings share the concrete `DmaBuffer<[u8; RX_RING_BYTES]>` /
/// `DmaBuffer<[u8; RX_BUF_SIZE]>` types because the two depths and buffer sizes
/// are equal (asserted above).
pub struct CplusRing {
    ring: DmaBuffer<[u8; RX_RING_BYTES]>,
    bufs: Vec<DmaBuffer<[u8; RX_BUF_SIZE]>>,
    /// Software cursor: next slot the driver inspects (RX) / fills (TX).
    pub idx: usize,
    /// Slot count.
    pub count: usize,
    /// Per-slot buffer size advertised in the descriptor length field.
    pub buf_size: usize,
}

impl CplusRing {
    /// Allocate `count` descriptors + `count` packet buffers.
    ///
    /// `own` marks each descriptor NIC-owned: RX rings post all buffers to the
    /// NIC (`own = true`); TX rings start host-owned (`own = false`). The DMA
    /// ring base is page-aligned by `sys_device_dma_alloc` (we also request
    /// `RING_ALIGN`), satisfying [`hw::ring_base_is_aligned`].
    pub fn alloc(
        handle: &DeviceHandle,
        count: usize,
        buf_size: usize,
        own: bool,
    ) -> Result<Self, DriverRuntimeError> {
        // The descriptor ring backing is a fixed `[u8; RX_RING_BYTES]`
        // (RX_RING_SIZE slots) and each packet buffer is a fixed
        // `[u8; RX_BUF_SIZE]`. Reject out-of-range arguments up front: a caller
        // passing `count > RX_RING_SIZE` would overflow `build_ring`'s output
        // (it returns 0, silently yielding an empty ring in a release build),
        // and `buf_size > RX_BUF_SIZE` would advertise a length larger than the
        // real buffer (NIC RX-DMA overrun, and a `post_tx` slice-index panic).
        // These are programming errors from undocumented misuse, so surface
        // `Internal` (the service manager restarts the offending driver).
        if count > RX_RING_SIZE || buf_size > RX_BUF_SIZE {
            return Err(DriverRuntimeError::Device(DeviceHostError::Internal));
        }

        let ring =
            DmaBuffer::<[u8; RX_RING_BYTES]>::allocate(handle, RX_RING_BYTES, hw::RING_ALIGN)?;

        let mut bufs: Vec<DmaBuffer<[u8; RX_BUF_SIZE]>> = Vec::with_capacity(count);
        let mut iovas: Vec<u64> = Vec::with_capacity(count);
        for _ in 0..count {
            let b = DmaBuffer::<[u8; RX_BUF_SIZE]>::allocate(handle, RX_BUF_SIZE, 8)?;
            iovas.push(b.iova());
            bufs.push(b);
        }

        let mut ring = Self {
            ring,
            bufs,
            idx: 0,
            count,
            buf_size,
        };
        {
            // Build the descriptor words with the host-tested encoder into the
            // live DMA ring. `build_ring` writes exactly `count` 16-byte
            // descriptors; `count <= RX_RING_SIZE` (checked above) so the array
            // length fits.
            let backing: &mut [u8; RX_RING_BYTES] = &mut ring.ring;
            let n = hw::build_ring(backing.as_mut_slice(), &iovas, own, buf_size as u32);
            if n != count {
                // `build_ring` returns 0 on a size mismatch. With the bounds
                // check above this is unreachable from documented callers, but
                // enforce it at runtime (not just `debug_assert`) so a release
                // build never proceeds with a partially-built ring.
                return Err(DriverRuntimeError::Device(DeviceHostError::Internal));
            }
        }
        Ok(ring)
    }

    /// Base IOVA of the descriptor ring (programmed into the start-address regs).
    #[inline]
    pub fn base_iova(&self) -> u64 {
        self.ring.iova()
    }

    /// Read the `opts1` word of descriptor `slot` from the live DMA ring.
    #[inline]
    pub fn opts1(&self, slot: usize) -> u32 {
        let backing: &[u8; RX_RING_BYTES] = &self.ring;
        hw::read_opts1(backing.as_slice(), slot)
    }

    /// Re-arm an RX descriptor after the host has consumed it: set OWN (and EOR
    /// on the last slot) so the NIC can refill it. Only the `opts1` word changes;
    /// the buffer address is already in place from `alloc`.
    pub fn rearm_rx(&mut self, slot: usize) {
        let is_last = slot == self.count - 1;
        let opts1 = hw::encode_opts1(true, is_last, true, true, self.buf_size as u32);
        let backing: &mut [u8; RX_RING_BYTES] = &mut self.ring;
        let base = slot * hw::DESC_SIZE;
        backing[base..base + 4].copy_from_slice(&opts1.to_le_bytes());
    }

    /// Stamp a TX descriptor for transmission: copy `frame` into the slot buffer
    /// and set OWN|FS|LS (+ EOR on the last slot) with the frame length. Returns
    /// false if the frame is too large for the slot buffer.
    pub fn post_tx(&mut self, slot: usize, frame: &[u8]) -> bool {
        if frame.len() > self.buf_size {
            return false;
        }
        {
            let buf: &mut [u8; RX_BUF_SIZE] = &mut self.bufs[slot];
            buf[..frame.len()].copy_from_slice(frame);
        }
        let is_last = slot == self.count - 1;
        let opts1 = hw::encode_opts1(true, is_last, true, true, frame.len() as u32);
        let backing: &mut [u8; RX_RING_BYTES] = &mut self.ring;
        let base = slot * hw::DESC_SIZE;
        backing[base..base + 4].copy_from_slice(&opts1.to_le_bytes());
        true
    }

    /// Borrow the RX slot buffer as a byte slice (for forwarding a received
    /// frame). `len` is clamped to the buffer size.
    pub fn rx_slice(&self, slot: usize, len: usize) -> &[u8] {
        let buf: &[u8; RX_BUF_SIZE] = &self.bufs[slot];
        &buf[..len.min(RX_BUF_SIZE)]
    }
}
