//! E1000 device bring-up — Phase 55b Track E.2.
//!
//! Ports the Phase 55 E.1/E.2 init path from `kernel/src/net/e1000.rs` to
//! the ring-3 `driver_runtime` host. Responsibilities:
//!
//! 1. Claim the PCI function through `driver_runtime::DeviceHandle`.
//! 2. Map BAR0 (128 KiB MMIO, 82540EM) as an `Mmio<E1000Regs>` window.
//! 3. Mask interrupts (`IMC = 0xFFFFFFFF`), issue the `CTRL.RST` global
//!    reset, and poll the self-clearing bit within a bounded spin.
//! 4. Configure `CTRL`: `ASDE | SLU`, clear `LRST | PHY_RST`.
//! 5. Clear the Multicast Table Array.
//! 6. Read the MAC from `RAL0` / `RAH0`.
//! 7. Allocate TX/RX rings (E.2 proper) and program `RDBAL/RDBAH/RDLEN`
//!    and `TDBAL/TDBAH/TDLEN` with the ring **IOVA** (per the Phase 55a
//!    contract — `IOVA == PhysAddr` under the identity-map fallback).
//! 8. Pre-post RX: `RDT = RX_RING_SIZE - 1`.
//! 9. Program `RCTL` (2 KiB buffers, broadcast accept, strip CRC) and
//!    `TCTL` (enable + pad-short + CT=0x10 + COLD=0x40). `TIPG` takes
//!    Intel's recommended 82540EM value.
//!
//! IRQ install and the RX/TX hot paths are deferred to Track E.3.
//!
//! # Pure helpers
//!
//! Everything that does not need real MMIO lives as a pure `pub const
//! fn` or `pub fn` below, exercised from `#[cfg(test)]`. The
//! register-poking happens inside [`E1000Device::bring_up`] behind a
//! thin [`MmioOps`] seam so the reset / MAC sequence can be driven by a
//! host-side fake (see `tests` module) without a real
//! `driver_runtime::Mmio`.

#![allow(dead_code)] // IRQ install and TX/RX hot paths are Track E.3.

extern crate alloc;

use driver_runtime::{DeviceCapKey, DeviceHandle, DriverRuntimeError, Mmio};
use kernel_core::e1000::{
    E1000Regs, ctrl, decode_mac_from_ra, irq_cause, rctl, status as e_status, tctl,
};

#[cfg(test)]
use crate::rings::RX_RING_SIZE;
use crate::rings::{RX_RING_BYTES, RxDescRing, TX_RING_BYTES, TxDescRing, initial_rdt, split_iova};

/// BAR0 size for the 82540EM — 128 KiB MMIO window per §13.4.
pub const E1000_BAR0_LEN: usize = 0x0002_0000;

/// BAR0 index — classic e1000 exposes BAR0 as the register window.
pub const E1000_BAR0_INDEX: u8 = 0;

/// Bounded spin count for the self-clearing `CTRL.RST` bit. Matches the
/// Phase 55 E.1 value (`RESET_POLL_LIMIT`) so a broken NIC can never
/// hang the driver process indefinitely; on QEMU the bit clears in
/// well under 100 iterations.
pub const RESET_POLL_LIMIT: u32 = 2_000_000;

/// Reasons driver bring-up can fail before any IRQ or RX/TX path runs.
///
/// Every variant is a named failure mode matching the Phase 55b error-
/// discipline rule. Driver main() pattern-matches and exits with a
/// stable code; see `main.rs`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpError {
    /// `sys_device_claim` / `sys_device_mmio_map` / `sys_device_dma_alloc`
    /// surfaced a kernel error. Wraps the underlying
    /// [`DriverRuntimeError`] for observability.
    Runtime(DriverRuntimeError),
    /// `CTRL.RST` did not self-clear within [`RESET_POLL_LIMIT`]
    /// iterations — the hardware is wedged. This is the same failure
    /// mode Phase 55 E.1 named `"e1000 reset timeout"`.
    ResetTimeout,
}

impl From<DriverRuntimeError> for BringUpError {
    fn from(e: DriverRuntimeError) -> Self {
        Self::Runtime(e)
    }
}

/// Typestate marker for the BAR0 [`Mmio`] window — distinguishes
/// `Mmio<E1000Regs>` from any other device's BAR in the type system.
pub struct E1000Regs128k;

// ---------------------------------------------------------------------------
// MmioOps seam — production uses `driver_runtime::Mmio`, tests plug in a fake.
// ---------------------------------------------------------------------------

/// Minimal read / write surface the e1000 bring-up sequence needs.
///
/// `Mmio<E1000Regs128k>` implements this via its `read_reg` /
/// `write_reg` methods; test code implements it against a
/// byte-addressable mock.
pub trait MmioOps {
    fn read_u32(&self, offset: usize) -> u32;
    fn write_u32(&self, offset: usize, value: u32);
}

impl<T> MmioOps for Mmio<T> {
    fn read_u32(&self, offset: usize) -> u32 {
        self.read_reg::<u32>(offset)
    }
    fn write_u32(&self, offset: usize, value: u32) {
        self.write_reg::<u32>(offset, value)
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — register-value composition exercised from host tests.
// ---------------------------------------------------------------------------

/// Compose the `CTRL` value written to trigger a global reset.
#[inline]
pub const fn ctrl_reset_value(prev: u32) -> u32 {
    prev | ctrl::RST
}

/// Check whether the `CTRL.RST` bit has self-cleared.
#[inline]
pub const fn reset_complete(ctrl_snapshot: u32) -> bool {
    ctrl_snapshot & ctrl::RST == 0
}

/// Compose the post-reset `CTRL` value: keep everything the BIOS or
/// firmware set, set `ASDE | SLU`, clear `LRST | PHY_RST`.
#[inline]
pub const fn ctrl_post_reset_value(prev: u32) -> u32 {
    (prev | ctrl::ASDE | ctrl::SLU) & !(ctrl::LRST | ctrl::PHY_RST)
}

/// Canonical `RCTL` value: enable + broadcast accept + strip FCS + 2 KiB
/// buffers. Matches the Phase 55 E.2 programming.
#[inline]
pub const fn rctl_bring_up_value() -> u32 {
    rctl::EN | rctl::BAM | rctl::SECRC | rctl::BSIZE_2048
}

/// Canonical `TCTL` value: enable + pad short packets + collision
/// threshold 0x10 (full-duplex recommended) + collision distance 0x40
/// (§13.4.33 default).
#[inline]
pub const fn tctl_bring_up_value() -> u32 {
    tctl::EN | tctl::PSP | (0x10u32 << tctl::CT_SHIFT) | (0x40u32 << tctl::COLD_SHIFT)
}

/// Canonical `TIPG` value for the 82540EM (§13.4.34 recommended).
#[inline]
pub const fn tipg_bring_up_value() -> u32 {
    0x0060_200A
}

/// Interrupt-mask-clear value written to silence every cause before
/// bring-up.
#[inline]
pub const fn imc_mask_all_value() -> u32 {
    0xFFFF_FFFF
}

/// Interrupt-mask-set value armed **after** bring-up finishes. Track
/// E.3 writes this; Track E.2 does not yet arm IRQs, but declaring it
/// here keeps the composition next to `IMC` and exercisable today.
#[inline]
pub const fn ims_bring_up_value() -> u32 {
    irq_cause::RXT0 | irq_cause::RXDMT0 | irq_cause::RXO | irq_cause::LSC
}

/// Read the MAC address from `RAL0` / `RAH0` via the supplied MMIO
/// seam. Thin wrapper around `kernel_core::e1000::decode_mac_from_ra`
/// — exists so tests can drive the full sequence end-to-end without a
/// real `Mmio` handle.
#[inline]
pub fn read_mac<M: MmioOps>(mmio: &M) -> [u8; 6] {
    let ral0 = mmio.read_u32(E1000Regs::RAL0);
    let rah0 = mmio.read_u32(E1000Regs::RAH0);
    decode_mac_from_ra(ral0, rah0)
}

/// Clear the 128-dword Multicast Table Array (0x5200..=0x53FC).
pub fn clear_mta<M: MmioOps>(mmio: &M) {
    let mut off = E1000Regs::MTA;
    while off <= E1000Regs::MTA_END {
        mmio.write_u32(off, 0);
        off += 4;
    }
}

/// Issue the global `CTRL.RST` reset and poll for the self-clearing
/// bit, bounded by `limit` iterations.
///
/// Returns `Ok(iterations)` on success so callers can log the spin
/// count for observability; returns [`BringUpError::ResetTimeout`] on
/// exhaustion.
pub fn reset<M: MmioOps>(mmio: &M, limit: u32) -> Result<u32, BringUpError> {
    let prev = mmio.read_u32(E1000Regs::CTRL);
    mmio.write_u32(E1000Regs::CTRL, ctrl_reset_value(prev));
    for i in 0..limit {
        core::hint::spin_loop();
        if reset_complete(mmio.read_u32(E1000Regs::CTRL)) {
            return Ok(i);
        }
    }
    Err(BringUpError::ResetTimeout)
}

/// Program the RX descriptor base / length / head / tail registers from
/// `ring_iova` + `RX_RING_BYTES`, then pre-post every slot by advancing
/// `RDT` to `RX_RING_SIZE - 1` (Intel §13: `RDH == RDT` means empty).
pub fn program_rx_ring<M: MmioOps>(mmio: &M, ring_iova: u64) {
    let (lo, hi) = split_iova(ring_iova);
    mmio.write_u32(E1000Regs::RDBAL, lo);
    mmio.write_u32(E1000Regs::RDBAH, hi);
    mmio.write_u32(E1000Regs::RDLEN, RX_RING_BYTES as u32);
    mmio.write_u32(E1000Regs::RDH, 0);
    mmio.write_u32(E1000Regs::RDT, initial_rdt());
}

/// Program the TX descriptor base / length / head / tail registers.
/// `TDH == TDT == 0` — the ring starts empty on the TX side; E.3's
/// `handle_tx` advances `TDT` as packets are enqueued.
pub fn program_tx_ring<M: MmioOps>(mmio: &M, ring_iova: u64) {
    let (lo, hi) = split_iova(ring_iova);
    mmio.write_u32(E1000Regs::TDBAL, lo);
    mmio.write_u32(E1000Regs::TDBAH, hi);
    mmio.write_u32(E1000Regs::TDLEN, TX_RING_BYTES as u32);
    mmio.write_u32(E1000Regs::TDH, 0);
    mmio.write_u32(E1000Regs::TDT, 0);
}

// ---------------------------------------------------------------------------
// E1000Device — ring-3 port of the Phase 55 in-kernel driver state.
// ---------------------------------------------------------------------------

/// The ring-3 e1000 driver state. One per claimed NIC.
///
/// Owns the `DeviceHandle`, the BAR0 [`Mmio<E1000Regs128k>`] window,
/// and the RX/TX descriptor rings. IRQ subscription lives on a
/// follow-on field that Track E.3 populates.
#[allow(dead_code)]
pub struct E1000Device {
    pub(crate) pci: DeviceHandle,
    pub(crate) mmio: Mmio<E1000Regs128k>,
    pub(crate) mac: [u8; 6],
    pub(crate) rx: RxDescRing,
    pub(crate) tx: TxDescRing,
    /// Snapshot of `STATUS` taken at the end of bring-up; published so
    /// a future Track E.3 `link_state_atomic` can initialise itself
    /// without re-reading the register.
    pub(crate) initial_status: u32,
}

impl E1000Device {
    /// Claim `key`, map BAR0, reset the MAC, read the MAC address,
    /// allocate descriptor rings, and program RX/TX registers.
    ///
    /// This is the Track E.2 acceptance entry point. The method
    /// returns `Ok(Self)` once every step has succeeded; any failure
    /// surfaces as a [`BringUpError`] with enough information for the
    /// driver main to log and exit with a stable code.
    pub fn bring_up(key: DeviceCapKey) -> Result<Self, BringUpError> {
        let pci = DeviceHandle::claim(key)?;
        let mmio = Mmio::<E1000Regs128k>::map(&pci, E1000_BAR0_INDEX, E1000_BAR0_LEN)?;

        // Mask every IRQ cause before we touch the device; we do not
        // yet own an ISR seat. Track E.3 un-masks via `IMS`.
        mmio.write_reg::<u32>(E1000Regs::IMC, imc_mask_all_value());

        // Global reset with bounded spin.
        let _spun = reset(&mmio, RESET_POLL_LIMIT)?;

        // Mask again: the reset leaves IMS implementation-defined per
        // §13.4.19, so re-silence every cause before the next write.
        mmio.write_reg::<u32>(E1000Regs::IMC, imc_mask_all_value());

        // Post-reset CTRL: keep the prior value's firmware-set bits,
        // set ASDE|SLU, clear LRST|PHY_RST.
        let prev_ctrl = mmio.read_reg::<u32>(E1000Regs::CTRL);
        mmio.write_reg::<u32>(E1000Regs::CTRL, ctrl_post_reset_value(prev_ctrl));

        // MTA clear — no multicast filters before a driver policy adds them.
        clear_mta(&mmio);

        // Read the primary MAC.
        let mac = read_mac(&mmio);

        // Allocate rings (DmaBuffer routes through `sys_device_dma_alloc`
        // and the Phase 55a IOMMU domain).
        let rx = RxDescRing::allocate(&pci)?;
        let tx = TxDescRing::allocate(&pci)?;

        // Program the RX/TX ring registers with the IOVA the kernel
        // handed back (identity-map fallback makes this the PA; IOMMU
        // path makes it the IOVA — the driver does not care).
        program_rx_ring(&mmio, rx.ring_iova);
        program_tx_ring(&mmio, tx.ring_iova);

        // TIPG, then TCTL, then RCTL — RCTL last because it enables
        // reception against a ring the hardware has already been told
        // the shape of.
        mmio.write_reg::<u32>(E1000Regs::TIPG, tipg_bring_up_value());
        mmio.write_reg::<u32>(E1000Regs::TCTL, tctl_bring_up_value());
        mmio.write_reg::<u32>(E1000Regs::RCTL, rctl_bring_up_value());

        let initial_status = mmio.read_reg::<u32>(E1000Regs::STATUS);

        Ok(Self {
            pci,
            mmio,
            mac,
            rx,
            tx,
            initial_status,
        })
    }

    /// The MAC address read from `RAL0` / `RAH0` during bring-up.
    #[inline]
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Whether `STATUS.LU` was set at bring-up time.
    #[inline]
    pub fn link_up_initial(&self) -> bool {
        self.initial_status & e_status::LU != 0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    /// Host-side fake for `MmioOps`. Writes accumulate in a log so a
    /// test can assert on the sequence; reads consult a simple
    /// `offset -> value` table populated by the test.
    struct FakeMmio {
        log: RefCell<Vec<(usize, u32)>>,
        reg: RefCell<Vec<(usize, u32)>>,
    }

    impl FakeMmio {
        fn new() -> Self {
            Self {
                log: RefCell::new(Vec::new()),
                reg: RefCell::new(Vec::new()),
            }
        }

        fn set(&self, offset: usize, value: u32) {
            let mut table = self.reg.borrow_mut();
            if let Some(slot) = table.iter_mut().find(|(o, _)| *o == offset) {
                slot.1 = value;
            } else {
                table.push((offset, value));
            }
        }

        fn writes(&self) -> Vec<(usize, u32)> {
            self.log.borrow().clone()
        }
    }

    impl MmioOps for FakeMmio {
        fn read_u32(&self, offset: usize) -> u32 {
            self.reg
                .borrow()
                .iter()
                .find(|(o, _)| *o == offset)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write_u32(&self, offset: usize, value: u32) {
            self.log.borrow_mut().push((offset, value));
            // Self-clearing behavior for CTRL.RST: after one write the
            // fake flips the bit off so `reset()` converges.
            if offset == E1000Regs::CTRL {
                let stored = value & !ctrl::RST;
                self.set(offset, stored);
            } else {
                self.set(offset, value);
            }
        }
    }

    #[test]
    fn ctrl_reset_value_sets_only_the_reset_bit() {
        let prev = 0x4000_0042;
        let next = ctrl_reset_value(prev);
        assert_eq!(next, prev | ctrl::RST);
        // Never drops any existing bit.
        assert_eq!(next & prev, prev);
    }

    #[test]
    fn reset_complete_detects_self_cleared_bit() {
        assert!(reset_complete(0));
        assert!(reset_complete(ctrl::SLU));
        assert!(!reset_complete(ctrl::RST));
        assert!(!reset_complete(ctrl::RST | ctrl::SLU));
    }

    #[test]
    fn ctrl_post_reset_value_matches_phase_55_composition() {
        // The Phase 55 E.1 code computes
        //   (prev | ASDE | SLU) & !(LRST | PHY_RST)
        // — the port must produce byte-identical values.
        let prev = 0x8000_0080; // PHY_RST set, random payload bits.
        let expected = (prev | ctrl::ASDE | ctrl::SLU) & !(ctrl::LRST | ctrl::PHY_RST);
        assert_eq!(ctrl_post_reset_value(prev), expected);
        // PHY_RST must not survive.
        assert_eq!(ctrl_post_reset_value(prev) & ctrl::PHY_RST, 0);
        // SLU must be set.
        assert_ne!(ctrl_post_reset_value(prev) & ctrl::SLU, 0);
    }

    #[test]
    fn rctl_bring_up_value_matches_acceptance() {
        // Acceptance: RCTL configured for 2048-byte buffers, broadcast
        // accept, collision threshold. The collision-threshold bullet
        // refers to TCTL — RCTL owns receiver enable + broadcast +
        // CRC strip + buffer sizing.
        let value = rctl_bring_up_value();
        assert_ne!(value & rctl::EN, 0);
        assert_ne!(value & rctl::BAM, 0);
        assert_ne!(value & rctl::SECRC, 0);
        assert_eq!(value & rctl::BSIZE_1024, 0, "2048-byte buffers only");
        assert_eq!(value & rctl::BSIZE_512, 0);
        assert_eq!(value & rctl::BSIZE_256, 0);
    }

    #[test]
    fn tctl_bring_up_value_matches_acceptance() {
        // Acceptance: TCTL configured with the collision threshold.
        let value = tctl_bring_up_value();
        assert_ne!(value & tctl::EN, 0);
        assert_ne!(value & tctl::PSP, 0);
        // CT == 0x10 at the collision-threshold shift.
        assert_eq!((value >> tctl::CT_SHIFT) & 0xFF, 0x10);
        // COLD == 0x40 at the backoff shift.
        assert_eq!((value >> tctl::COLD_SHIFT) & 0x3FF, 0x40);
    }

    #[test]
    fn imc_mask_all_silences_every_cause() {
        assert_eq!(imc_mask_all_value(), 0xFFFF_FFFF);
    }

    #[test]
    fn ims_bring_up_value_arms_rx_and_lsc_only() {
        let value = ims_bring_up_value();
        assert_ne!(value & irq_cause::RXT0, 0);
        assert_ne!(value & irq_cause::RXDMT0, 0);
        assert_ne!(value & irq_cause::RXO, 0);
        assert_ne!(value & irq_cause::LSC, 0);
        // TX completions are handled inline; TXDW must be masked.
        assert_eq!(value & irq_cause::TXDW, 0);
    }

    #[test]
    fn read_mac_matches_kernel_core_decoder_for_qemu_default() {
        // QEMU's stock e1000 MAC 52:54:00:12:34:56 lands in RAL0 =
        // 0x00125452 and RAH0 = 0x80005634. The read_mac helper must
        // agree with kernel_core::decode_mac_from_ra on the byte order.
        let fake = FakeMmio::new();
        fake.set(E1000Regs::RAL0, 0x0012_5452);
        fake.set(E1000Regs::RAH0, 0x8000_5634);
        let mac = read_mac(&fake);
        assert_eq!(mac, [0x52, 0x54, 0x12, 0x00, 0x34, 0x56]);
    }

    #[test]
    fn reset_converges_after_one_iteration_on_fake_self_clear() {
        let fake = FakeMmio::new();
        fake.set(E1000Regs::CTRL, 0x0000_0042);
        let spun = reset(&fake, 16).expect("fake self-clears on first write");
        // The fake's write_u32 clears RST immediately, so the first
        // spin-loop poll returns true.
        assert!(spun <= 1);
        // Write sequence must include exactly one CTRL write with the
        // RST bit set.
        let writes = fake.writes();
        let ctrl_writes: Vec<_> = writes
            .iter()
            .filter(|(o, _)| *o == E1000Regs::CTRL)
            .collect();
        assert_eq!(ctrl_writes.len(), 1);
        assert_ne!(ctrl_writes[0].1 & ctrl::RST, 0);
    }

    /// Parallel fake whose `CTRL` write *does not* self-clear — the
    /// poll loop must give up after `limit` iterations with
    /// `ResetTimeout` rather than hanging.
    struct StuckMmio {
        reg: RefCell<Vec<(usize, u32)>>,
    }
    impl StuckMmio {
        fn new() -> Self {
            Self {
                reg: RefCell::new(Vec::new()),
            }
        }
    }
    impl MmioOps for StuckMmio {
        fn read_u32(&self, offset: usize) -> u32 {
            self.reg
                .borrow()
                .iter()
                .find(|(o, _)| *o == offset)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write_u32(&self, offset: usize, value: u32) {
            let mut table = self.reg.borrow_mut();
            if let Some(slot) = table.iter_mut().find(|(o, _)| *o == offset) {
                slot.1 = value;
            } else {
                table.push((offset, value));
            }
        }
    }

    #[test]
    fn reset_times_out_cleanly_on_stuck_nic() {
        let stuck = StuckMmio::new();
        stuck.write_u32(E1000Regs::CTRL, 0);
        let err = reset(&stuck, 4).expect_err("stuck NIC must not spin forever");
        assert_eq!(err, BringUpError::ResetTimeout);
    }

    #[test]
    fn clear_mta_writes_128_dwords_of_zero() {
        let fake = FakeMmio::new();
        clear_mta(&fake);
        let writes = fake.writes();
        let mta_writes: Vec<_> = writes
            .iter()
            .filter(|(o, _)| *o >= E1000Regs::MTA && *o <= E1000Regs::MTA_END)
            .collect();
        assert_eq!(mta_writes.len(), 128, "128 dwords of MTA to clear");
        for (off, v) in &mta_writes {
            assert_eq!(*v, 0);
            assert!(off.is_multiple_of(4));
        }
    }

    #[test]
    fn program_rx_ring_uses_iova_not_user_va() {
        // Acceptance bullet: RDBAL/RDBAH programmed with IOVA. We feed
        // in a distinctive IOVA and assert the low/high split lands in
        // the right registers, in the right order.
        let fake = FakeMmio::new();
        let iova: u64 = 0x0000_0001_DEAD_BEEF;
        program_rx_ring(&fake, iova);
        let writes = fake.writes();
        // Required writes: RDBAL, RDBAH, RDLEN, RDH, RDT.
        let rdbal = writes
            .iter()
            .find(|(o, _)| *o == E1000Regs::RDBAL)
            .unwrap()
            .1;
        let rdbah = writes
            .iter()
            .find(|(o, _)| *o == E1000Regs::RDBAH)
            .unwrap()
            .1;
        let rdlen = writes
            .iter()
            .find(|(o, _)| *o == E1000Regs::RDLEN)
            .unwrap()
            .1;
        let rdh = writes.iter().find(|(o, _)| *o == E1000Regs::RDH).unwrap().1;
        let rdt = writes.iter().find(|(o, _)| *o == E1000Regs::RDT).unwrap().1;
        assert_eq!(rdbal, 0xDEAD_BEEF);
        assert_eq!(rdbah, 0x0000_0001);
        assert_eq!(rdlen, RX_RING_BYTES as u32);
        assert_eq!(rdh, 0);
        // Acceptance: RX pre-post leaves RDT one short of head.
        assert_eq!(rdt, initial_rdt());
        assert_eq!(rdt, (RX_RING_SIZE as u32) - 1);
    }

    #[test]
    fn program_tx_ring_uses_iova_not_user_va() {
        let fake = FakeMmio::new();
        let iova: u64 = 0x0000_0002_CAFE_F00D;
        program_tx_ring(&fake, iova);
        let writes = fake.writes();
        let tdbal = writes
            .iter()
            .find(|(o, _)| *o == E1000Regs::TDBAL)
            .unwrap()
            .1;
        let tdbah = writes
            .iter()
            .find(|(o, _)| *o == E1000Regs::TDBAH)
            .unwrap()
            .1;
        let tdlen = writes
            .iter()
            .find(|(o, _)| *o == E1000Regs::TDLEN)
            .unwrap()
            .1;
        let tdh = writes.iter().find(|(o, _)| *o == E1000Regs::TDH).unwrap().1;
        let tdt = writes.iter().find(|(o, _)| *o == E1000Regs::TDT).unwrap().1;
        assert_eq!(tdbal, 0xCAFE_F00D);
        assert_eq!(tdbah, 0x0000_0002);
        assert_eq!(tdlen, TX_RING_BYTES as u32);
        assert_eq!(tdh, 0);
        assert_eq!(tdt, 0);
    }
}
