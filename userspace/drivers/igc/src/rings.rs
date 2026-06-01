//! igc advanced-descriptor ring allocation — Phase 79 Track B.2.
//!
//! Identical in shape to the igb ring (the advanced read/write-back descriptor
//! union is shared between the two families); kept as a separate module so the
//! igc driver is self-contained and the ring sizes can diverge later if a
//! 2.5GbE-specific buffer policy is wanted. Both `AdvRxDesc` and `AdvTxDesc`
//! are 16 bytes, so the shared `ring_len_is_valid` / `ring_len_bytes` helpers
//! gate the ring sizes the same way as every other Intel family.

#![allow(dead_code)] // hardware-only family; the IO loop consumes these.

extern crate alloc;

use alloc::vec::Vec;

use driver_runtime::{
    AdvRxDesc, AdvTxDesc, Advanced, DeviceHandle, DmaBuffer, DriverRuntimeError, NicDescriptors,
    ring_len_bytes, ring_len_is_valid,
};

/// Receive descriptor ring depth.
pub const RX_RING_SIZE: usize = 256;
/// Transmit descriptor ring depth.
pub const TX_RING_SIZE: usize = 256;
/// Per-descriptor receive buffer size — paired with `SRRCTL.BSIZEPKT = 2 KiB`.
pub const RX_BUF_SIZE: usize = 2048;
/// Per-descriptor transmit buffer size.
pub const TX_BUF_SIZE: usize = 2048;

const _: () = assert!(ring_len_is_valid(RX_RING_SIZE, Advanced::RX_DESC_SIZE));
const _: () = assert!(ring_len_is_valid(TX_RING_SIZE, Advanced::TX_DESC_SIZE));

/// Byte length of the RX descriptor ring — the `RDLEN0` register value.
pub const RX_RING_BYTES: usize = ring_len_bytes(RX_RING_SIZE, Advanced::RX_DESC_SIZE);
/// Byte length of the TX descriptor ring — the `TDLEN0` register value.
pub const TX_RING_BYTES: usize = ring_len_bytes(TX_RING_SIZE, Advanced::TX_DESC_SIZE);

const _: () = assert!(RX_RING_BYTES.is_multiple_of(128));
const _: () = assert!(TX_RING_BYTES.is_multiple_of(128));

/// Initial `RDT0` value after pre-posting every RX descriptor.
#[inline]
pub const fn initial_rx_tail() -> u32 {
    driver_runtime::initial_rdt(RX_RING_SIZE)
}

/// RX advanced-descriptor ring + per-slot packet buffers.
pub struct RxDescRing {
    pub(crate) descs: DmaBuffer<[AdvRxDesc; RX_RING_SIZE]>,
    pub(crate) bufs: Vec<DmaBuffer<[u8; RX_BUF_SIZE]>>,
    pub(crate) ring_iova: u64,
    pub(crate) buf_iova: Vec<u64>,
    pub(crate) next_to_read: usize,
}

/// TX advanced-descriptor ring + per-slot packet buffers.
pub struct TxDescRing {
    pub(crate) descs: DmaBuffer<[AdvTxDesc; TX_RING_SIZE]>,
    pub(crate) bufs: Vec<DmaBuffer<[u8; TX_BUF_SIZE]>>,
    pub(crate) ring_iova: u64,
    pub(crate) buf_iova: Vec<u64>,
    pub(crate) next_to_write: usize,
}

impl RxDescRing {
    pub fn allocate(handle: &DeviceHandle) -> Result<Self, DriverRuntimeError> {
        let descs = DmaBuffer::<[AdvRxDesc; RX_RING_SIZE]>::allocate(
            handle,
            RX_RING_BYTES,
            core::mem::align_of::<AdvRxDesc>().max(128),
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

    pub fn prepare_all(&mut self) {
        let descs: &mut [AdvRxDesc; RX_RING_SIZE] = &mut self.descs;
        for (i, desc) in descs.iter_mut().enumerate() {
            *desc = Advanced::rx_init(self.buf_iova[i]);
        }
        self.next_to_read = 0;
    }
}

impl TxDescRing {
    pub fn allocate(handle: &DeviceHandle) -> Result<Self, DriverRuntimeError> {
        let descs = DmaBuffer::<[AdvTxDesc; TX_RING_SIZE]>::allocate(
            handle,
            TX_RING_BYTES,
            core::mem::align_of::<AdvTxDesc>().max(128),
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
        {
            let descs: &mut [AdvTxDesc; TX_RING_SIZE] = &mut ring.descs;
            for desc in descs.iter_mut() {
                *desc = AdvTxDesc::default();
            }
        }
        Ok(ring)
    }
}
