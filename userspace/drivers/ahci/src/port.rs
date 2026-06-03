//! Per-port bring-up + DMA-structure programming — Phase 82 Track B.4 / B.5
//! (plus the C.4 `recover_port` engine restart).
//!
//! The cardinal AHCI ordering rule is honored on both bring-up and recovery:
//! clear `PxCMD.ST` and confirm `PxCMD.CR == 0` **before** clearing `PxCMD.FRE`,
//! and confirm `CR == 0` **before** re-setting `ST`. `CR`/`FR` are read-only
//! status the HBA drives; reprogramming `PxCLB`/`PxFB` while the engine runs
//! corrupts the command-list pointer. Every device address programmed
//! (`PxCLB`/`PxFB`, each header's `ctba`, every PRDT `DBA`) is the
//! `DmaBuffer::iova()` — the device-visible IOVA under VT-d/AMD-Vi — never the
//! user VA.

use driver_runtime::{DeviceHandle, DmaBuffer, DriverRuntimeError, Mmio};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::write_str;

use kernel_core::storage::ahci::{
    CMD_CLO, CMD_CR, CMD_FR, CMD_FRE, CMD_ST, HbaCmdHeader, HbaCmdTable, HbaFis, PX_CLB, PX_CLBU,
    PX_CMD, PX_FB, PX_FBU, PX_IS, PX_SCTL, PX_SERR, PX_SIG, PX_SSTS, PX_TFD, PortDeviceType,
    TFD_BSY, TFD_DRQ, classify_port, port_base, port_present,
};

use crate::MMIO_SPIN_BUDGET;
use crate::init::{AhciAbar, HbaCaps};

/// Number of command-list slots the AHCI spec defines per port (always 32).
const CMD_LIST_SLOTS: usize = 32;
/// Command-list alignment: 32 headers × 32 B = 1 KiB, 1 KiB-aligned.
const CMD_LIST_ALIGN: usize = 1024;
/// Received-FIS area is 256 B, 256 B-aligned.
const RECV_FIS_ALIGN: usize = 256;
/// Command table alignment (128 B per AHCI spec; 4 KiB is a safe over-align).
const CMD_TABLE_ALIGN: usize = 128;
/// Data bounce-buffer alignment (page-aligned for clean IOMMU mapping).
const DATA_ALIGN: usize = 4096;

/// A short busy-spin delay (~1 ms order) for COMRESET de-assert timing.
const COMRESET_DELAY_ITERS: u64 = 100_000;

/// One implemented, brought-up AHCI port: owns its command list, received-FIS
/// area, single command table (single-in-flight data path), and data bounce
/// buffer as IOMMU-routed DMA.
pub struct Port<'a> {
    /// ABAR MMIO window (shared across ports; the port block is at `port_base`).
    pub(crate) mmio: &'a Mmio<AhciAbar>,
    /// Byte offset of this port's register block within the ABAR.
    pub(crate) port_base: usize,
    /// Port index (0-based) within the HBA.
    pub(crate) index: u8,
    /// Number of command slots (`CAP.NCS + 1`).
    pub(crate) ncs: u8,
    /// `CAP.SCLO` — may use `PxCMD.CLO` to clear a stuck BSY.
    pub(crate) sclo: bool,
    /// 1 KiB command list (32 × `HbaCmdHeader`).
    pub(crate) cmd_list: DmaBuffer<[HbaCmdHeader; CMD_LIST_SLOTS]>,
    /// 256 B received-FIS area.
    pub(crate) recv_fis: DmaBuffer<HbaFis>,
    /// One command table (CFIS + ACMD + reserved + a single PRDT entry).
    pub(crate) cmd_table: DmaBuffer<HbaCmdTable>,
    /// Data bounce buffer (`DATA_BOUNCE_BYTES`) the driver copies in/out around
    /// each transfer; its IOVA is what the PRDT `DBA`/`DBAU` carry.
    pub(crate) data_bounce: DmaBuffer<[u8; crate::DATA_BOUNCE_BYTES]>,
}

impl<'a> Port<'a> {
    /// Read a 32-bit port register at `off` within this port's block.
    #[inline]
    pub(crate) fn pread(&self, off: usize) -> u32 {
        self.mmio.read_reg::<u32>(self.port_base + off)
    }

    /// Write a 32-bit port register at `off` within this port's block.
    #[inline]
    pub(crate) fn pwrite(&self, off: usize, value: u32) {
        self.mmio.write_reg::<u32>(self.port_base + off, value);
    }

    /// Borrow the first `len` bytes of the data bounce buffer (where READ DMA
    /// EXT lands its data). `len` is clamped to the buffer size.
    pub fn data_slice(&self, len: usize) -> &[u8] {
        let p = self.data_bounce.user_ptr() as *const u8;
        // SAFETY: the bounce buffer is `DATA_BOUNCE_BYTES` long and live for the
        // port's lifetime; `len` is clamped below.
        unsafe { core::slice::from_raw_parts(p, len.min(crate::DATA_BOUNCE_BYTES)) }
    }

    /// `true` when port `index` reports a device present (`PxSSTS.DET == 3`).
    pub fn present(mmio: &Mmio<AhciAbar>, index: u8) -> bool {
        let ssts = mmio.read_reg::<u32>(port_base(index as usize) + PX_SSTS);
        port_present(ssts)
    }

    /// Allocate the per-port DMA structures and stop the engine. Does **not**
    /// yet program the HBA registers — call [`Port::program_dma_structures`]
    /// after the engine is confirmed idle.
    pub fn allocate(
        device: &DeviceHandle,
        mmio: &'a Mmio<AhciAbar>,
        index: u8,
        caps: &HbaCaps,
    ) -> Result<Self, DriverRuntimeError> {
        let cmd_list: DmaBuffer<[HbaCmdHeader; CMD_LIST_SLOTS]> = DmaBuffer::allocate(
            device,
            core::mem::size_of::<[HbaCmdHeader; CMD_LIST_SLOTS]>(),
            CMD_LIST_ALIGN,
        )?;
        let recv_fis: DmaBuffer<HbaFis> =
            DmaBuffer::allocate(device, core::mem::size_of::<HbaFis>(), RECV_FIS_ALIGN)?;
        let cmd_table: DmaBuffer<HbaCmdTable> =
            DmaBuffer::allocate(device, core::mem::size_of::<HbaCmdTable>(), CMD_TABLE_ALIGN)?;
        let data_bounce: DmaBuffer<[u8; crate::DATA_BOUNCE_BYTES]> =
            DmaBuffer::allocate(device, crate::DATA_BOUNCE_BYTES, DATA_ALIGN)?;

        Ok(Self {
            mmio,
            port_base: port_base(index as usize),
            index,
            ncs: caps.ncs,
            sclo: caps.sclo,
            cmd_list,
            recv_fis,
            cmd_table,
            data_bounce,
        })
    }

    /// Stop the command + FIS-receive engines in the spec-mandated order:
    /// clear `ST`, wait `CR == 0`, then clear `FRE`, wait `FR == 0`. Bounded so
    /// a wedged engine cannot hang the driver.
    pub fn stop_engine(&self) {
        // Clear ST, then wait for CR to clear.
        let cmd = self.pread(PX_CMD);
        self.pwrite(PX_CMD, cmd & !CMD_ST);
        let mut i = 0u64;
        while self.pread(PX_CMD) & CMD_CR != 0 && i < MMIO_SPIN_BUDGET {
            core::hint::spin_loop();
            i += 1;
        }
        // Clear FRE, then wait for FR to clear.
        let cmd = self.pread(PX_CMD);
        self.pwrite(PX_CMD, cmd & !CMD_FRE);
        let mut i = 0u64;
        while self.pread(PX_CMD) & CMD_FR != 0 && i < MMIO_SPIN_BUDGET {
            core::hint::spin_loop();
            i += 1;
        }
    }

    /// Program `PxCLB`/`PxCLBU`/`PxFB`/`PxFBU` and each command header's
    /// `ctba`/`ctbau` with the **IOVA** of the corresponding DMA buffer (never
    /// the user VA), after zeroing the command list / received-FIS / command
    /// table. The engine must already be stopped (`CR`/`FR` clear).
    pub fn program_dma_structures(&mut self) {
        // Zero the DMA structures before the HBA reads them.
        *self.cmd_list = [HbaCmdHeader::default(); CMD_LIST_SLOTS];
        *self.recv_fis = HbaFis::default();
        *self.cmd_table = HbaCmdTable::default();

        let clb = self.cmd_list.iova();
        let fb = self.recv_fis.iova();
        let ctba = self.cmd_table.iova();

        // Point every command header at the single shared command table (the
        // single-in-flight data path only ever issues on slot 0, but pointing
        // all headers keeps a stray slot from dereferencing a null ctba).
        for hdr in self.cmd_list.iter_mut() {
            hdr.ctba = (ctba & 0xFFFF_FFFF) as u32;
            hdr.ctbau = (ctba >> 32) as u32;
        }

        // Release the structure writes before the HBA can observe the base
        // pointers we are about to program.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);

        self.pwrite(PX_CLB, (clb & 0xFFFF_FFFF) as u32);
        self.pwrite(PX_CLBU, (clb >> 32) as u32);
        self.pwrite(PX_FB, (fb & 0xFFFF_FFFF) as u32);
        self.pwrite(PX_FBU, (fb >> 32) as u32);

        // B.4 acceptance: the programmed address is the IOVA, not the user VA.
        let clb_lo = self.pread(PX_CLB);
        debug_assert_eq!(
            clb_lo,
            (clb & 0xFFFF_FFFF) as u32,
            "PxCLB must read back the command-list IOVA low dword"
        );
        debug_assert_ne!(
            clb as usize,
            self.cmd_list.user_ptr() as usize,
            "PxCLB must be the IOVA, never the user VA"
        );
        write_str(
            STDOUT_FILENO,
            &alloc::format!(
                "AHCI: port {} CLB={:#x} FB={:#x} (IOVA)\n",
                self.index,
                clb,
                fb
            ),
        );
    }

    /// Enable FIS receive (`PxCMD.FRE`) and wait for `FR == 1`. This is what
    /// makes `PxSIG` valid on QEMU (it reads `0xFFFFFFFF` until FRE is on and
    /// the initial D2H FIS lands).
    pub fn enable_fis_rx(&self) {
        let cmd = self.pread(PX_CMD);
        self.pwrite(PX_CMD, cmd | CMD_FRE);
        let mut i = 0u64;
        while self.pread(PX_CMD) & CMD_FR == 0 && i < MMIO_SPIN_BUDGET {
            core::hint::spin_loop();
            i += 1;
        }
    }

    /// COMRESET the PHY (bare-metal-meaningful, QEMU-tolerant): write
    /// `PxSCTL.DET = 1`, wait ≥ 1 ms, write `DET = 0`, poll `PxSSTS.DET == 3`.
    /// On QEMU the link is always up, so this returns quickly.
    pub fn comreset(&self) {
        let sctl = self.pread(PX_SCTL);
        // DET field is bits 3:0 of PxSCTL.
        self.pwrite(PX_SCTL, (sctl & !0xF) | 0x1);
        crate::init_spin(COMRESET_DELAY_ITERS);
        // De-assert DET to bring the link back up, then poll for presence.
        let sctl = self.pread(PX_SCTL);
        self.pwrite(PX_SCTL, sctl & !0xF);
        let mut i = 0u64;
        while !port_present(self.pread(PX_SSTS)) && i < MMIO_SPIN_BUDGET {
            core::hint::spin_loop();
            i += 1;
        }
    }

    /// Wait until `PxTFD.BSY` and `PxTFD.DRQ` are both clear (the drive is ready
    /// to accept a command). If BSY stays stuck and `CAP.SCLO` is supported,
    /// issue a Command List Override to clear it. Bounded.
    pub fn wait_ready(&self) -> bool {
        let mut i = 0u64;
        loop {
            let tfd = self.pread(PX_TFD);
            if tfd & (TFD_BSY | TFD_DRQ) == 0 {
                return true;
            }
            if i >= MMIO_SPIN_BUDGET {
                // Stuck BSY: try a Command List Override if the HBA supports it.
                if self.sclo {
                    let cmd = self.pread(PX_CMD);
                    self.pwrite(PX_CMD, cmd | CMD_CLO);
                    let mut j = 0u64;
                    while self.pread(PX_CMD) & CMD_CLO != 0 && j < MMIO_SPIN_BUDGET {
                        core::hint::spin_loop();
                        j += 1;
                    }
                    return self.pread(PX_TFD) & (TFD_BSY | TFD_DRQ) == 0;
                }
                return false;
            }
            core::hint::spin_loop();
            i += 1;
        }
    }

    /// Classify the port from `(PxSSTS, PxSIG)`. Valid only after
    /// [`Port::enable_fis_rx`] — `PxSIG` reads `0xFFFFFFFF` before FRE.
    pub fn classify(&self) -> PortDeviceType {
        let ssts = self.pread(PX_SSTS);
        let sig = self.pread(PX_SIG);
        classify_port(ssts, sig)
    }

    /// Clear the write-1-to-clear `PxSERR` and `PxIS` latches. Must run before
    /// the engine starts or a stale bit immediately re-interrupts.
    pub fn clear_errors(&self) {
        let serr = self.pread(PX_SERR);
        self.pwrite(PX_SERR, serr); // W1C: write back the read value
        let is = self.pread(PX_IS);
        self.pwrite(PX_IS, is); // W1C
    }

    /// Start the command engine: confirm `CR == 0`, then set `FRE` and `ST`.
    /// Returns `true` once `CR` reads back 1.
    pub fn start_engine(&self) -> bool {
        // Never re-arm ST while CR is set.
        let mut i = 0u64;
        while self.pread(PX_CMD) & CMD_CR != 0 && i < MMIO_SPIN_BUDGET {
            core::hint::spin_loop();
            i += 1;
        }
        let cmd = self.pread(PX_CMD);
        self.pwrite(PX_CMD, cmd | CMD_FRE | CMD_ST);
        // Confirm the command engine is running.
        let mut i = 0u64;
        while self.pread(PX_CMD) & CMD_CR == 0 && i < MMIO_SPIN_BUDGET {
            core::hint::spin_loop();
            i += 1;
        }
        self.pread(PX_CMD) & CMD_CR != 0
    }

    /// Recover the port after a fatal `PxIS` error or a command timeout (C.4):
    /// capture `PxTFD`/`PxSERR`, stop the engine, clear both W1C latches
    /// (`PxSERR` then `PxIS`), COMRESET on an interface error, and restart the
    /// engine. Returns `true` if the engine restarts (`CR == 1`).
    pub fn recover_port(&self, interface_error: bool) -> bool {
        let tfd = self.pread(PX_TFD);
        let serr = self.pread(PX_SERR);
        write_str(
            STDOUT_FILENO,
            &alloc::format!(
                "AHCI: port {} recover (TFD={:#x} SERR={:#x})\n",
                self.index,
                tfd,
                serr
            ),
        );
        self.stop_engine();
        // Clear the W1C latches in order: SERR then IS.
        self.pwrite(PX_SERR, self.pread(PX_SERR));
        self.pwrite(PX_IS, self.pread(PX_IS));
        if interface_error {
            self.comreset();
            self.wait_ready();
        }
        self.start_engine()
    }
}

/// Pretty-print the classified device type for the bring-up log.
pub fn device_type_str(dt: PortDeviceType) -> &'static str {
    match dt {
        PortDeviceType::Sata => "SATA",
        PortDeviceType::Satapi => "SATAPI",
        PortDeviceType::PortMultiplier => "port-multiplier",
        PortDeviceType::Semb => "SEMB",
        PortDeviceType::None => "none",
        PortDeviceType::Unknown(_) => "unknown",
    }
}
