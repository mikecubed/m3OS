//! r8169 NIC bring-up: claim, BAR map, reset, chip-version detect, ring setup,
//! enable. (Track C.1/C.2.)
//!
//! Register offsets, descriptor encoding, the XID version table, and the
//! soft-reset poll predicate all come from `kernel_core::r8169` so the
//! bit-level logic is host-tested. This module only sequences MMIO writes
//! against the real hardware (there is no QEMU r8169 model).

extern crate alloc;

use driver_runtime::{DeviceCapKey, DeviceHandle, DriverRuntimeError, Mmio};
use kernel_core::r8169 as hw;
use syscall_lib as sys;

use crate::rings::{CplusRing, RX_BUF_SIZE, RX_RING_SIZE, TX_BUF_SIZE, TX_RING_SIZE};

/// BAR index for the r8169 register window. Realtek exposes its registers on
/// BAR0 (I/O) and BAR2 (MMIO); modern parts and Linux prefer the MMIO BAR.
pub const R8169_BAR_INDEX: u8 = 2;
/// MMIO window length — the r8169 register file fits well under 4 KiB; map a
/// page so the V2 interrupt block at 0x150..0x15C is in range.
pub const R8169_BAR_LEN: usize = 0x1000;

/// Reasons bring-up can fail before any RX/TX path runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpError {
    /// A device-host syscall (claim / mmio_map / dma_alloc) failed.
    Runtime(DriverRuntimeError),
    /// ChipCmd.RST did not self-clear within the bounded poll.
    ResetTimeout,
}

impl From<DriverRuntimeError> for BringUpError {
    fn from(e: DriverRuntimeError) -> Self {
        Self::Runtime(e)
    }
}

/// Typestate marker for the r8169 BAR window.
pub struct R8169Regs;

/// The ring-3 r8169 driver state. One per claimed NIC.
pub struct Nic {
    pub pci: DeviceHandle,
    pub mmio: Mmio<R8169Regs>,
    pub version: hw::MacVersion,
    pub rx: CplusRing,
    pub tx: CplusRing,
}

/// Convenience: a `u32` register offset into the `usize` the MMIO API wants.
#[inline]
fn off(reg: u32) -> usize {
    reg as usize
}

/// Log `label` followed by `val` as 8 hex digits + newline (no `alloc`/fmt).
fn write_hex32(label: &str, val: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 9];
    for i in 0..8 {
        buf[i] = HEX[((val >> (28 - i * 4)) & 0xF) as usize];
    }
    buf[8] = b'\n';
    let _ = sys::write_str(sys::STDOUT_FILENO, label);
    // SAFETY: `buf` holds only ASCII hex digits + '\n', valid UTF-8.
    let _ = sys::write_str(sys::STDOUT_FILENO, unsafe {
        core::str::from_utf8_unchecked(&buf)
    });
}

impl Nic {
    /// Claim `key`, map the BAR, reset the MAC, detect the chip version, set up
    /// rings, and enable RX/TX. Whether firmware is required is decided by the
    /// caller from [`Nic::version`] via `kernel_core::r8169::resolve_firmware`.
    pub fn bring_up(key: DeviceCapKey) -> Result<Self, BringUpError> {
        let pci = DeviceHandle::claim(key)?;
        let mmio = Mmio::<R8169Regs>::map(&pci, R8169_BAR_INDEX, R8169_BAR_LEN)?;

        // Mask the classic 16-bit interrupt block before touching the device.
        mmio.write_reg::<u16>(off(hw::REG_INTR_MASK), 0x0000);

        // Detect the chip version from the TxConfig XID *before* reset (TxConfig
        // is stable across the soft reset; reading it first mirrors Linux).
        let tx_config = mmio.read_reg::<u32>(off(hw::REG_TX_CONFIG));
        let version = hw::mac_version_from_tx_config(tx_config);

        // Soft reset: write ChipCmd.RST and poll until it self-clears.
        Self::soft_reset(&mmio)?;

        // Ack any pending status from before/after reset.
        mmio.write_reg::<u16>(off(hw::REG_INTR_STATUS), 0xFFFF);

        // Allocate rings: RX posts all buffers to the NIC; TX starts host-owned.
        let rx = CplusRing::alloc(&pci, RX_RING_SIZE, RX_BUF_SIZE, true)?;
        let tx = CplusRing::alloc(&pci, TX_RING_SIZE, TX_BUF_SIZE, false)?;

        let mut nic = Nic {
            pci,
            mmio,
            version,
            rx,
            tx,
        };
        nic.program_rings();
        nic.enable();
        // Kick the PHY's auto-negotiation, then return *immediately* — do NOT
        // block on link here. Auto-negotiation takes seconds; blocking bring-up
        // on it would delay the driver's `net.nic` service registration and lose
        // the first-come-first-served race to another NIC (e.g. an emulated
        // e1000e). The link comes up asynchronously and is polled/logged in the
        // I/O loop after the service is registered. The two NIC families reach
        // their PHY differently:
        //   * classic 8168 GbE → PHYAR window (0x60)
        //   * 8125/8126 2.5G   → GPHY-OCP window (0xB8)
        // Empirically confirmed on a real RTL8125B that a PHYAR autoneg-restart
        // is a no-op (link stays down); the OCP path is required.
        if nic.version.is_8125() {
            nic.phy_bring_up_ocp();
        } else {
            nic.phy_kick_autoneg();
        }
        Ok(nic)
    }

    /// Read the MAC's `PHYstatus` byte (`0x6C`) — a direct MMIO read of the
    /// link state (no MDIO transaction needed).
    pub fn phy_status(&self) -> u8 {
        self.mmio.read_reg::<u8>(off(hw::REG_PHYSTATUS))
    }

    /// Read the station MAC address from the `MAC0` register file (6 bytes at
    /// `0x00`, little-endian). The kernel net stack needs this as the source
    /// address for ARP/IP frames and to accept inbound frames addressed to us.
    pub fn mac(&self) -> [u8; 6] {
        let lo = self.mmio.read_reg::<u32>(off(hw::REG_MAC0));
        let hi = self.mmio.read_reg::<u16>(off(hw::REG_MAC0 + 4));
        [
            lo as u8,
            (lo >> 8) as u8,
            (lo >> 16) as u8,
            (lo >> 24) as u8,
            hi as u8,
            (hi >> 8) as u8,
        ]
    }

    /// Write a PHY register `reg` via the `PHYAR` MDIO interface (bounded poll).
    pub fn mdio_write(&self, reg: u32, val: u16) {
        self.mmio.write_reg::<u32>(
            off(hw::REG_PHYAR),
            hw::PHYAR_FLAG | ((reg & 0x1f) << 16) | (val as u32),
        );
        for _ in 0..100 {
            if self.mmio.read_reg::<u32>(off(hw::REG_PHYAR)) & hw::PHYAR_FLAG == 0 {
                break;
            }
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }
    }

    /// Read a PHY register `reg` via the `PHYAR` MDIO interface (bounded poll).
    pub fn mdio_read(&self, reg: u32) -> u16 {
        self.mmio
            .write_reg::<u32>(off(hw::REG_PHYAR), (reg & 0x1f) << 16);
        for _ in 0..100 {
            let v = self.mmio.read_reg::<u32>(off(hw::REG_PHYAR));
            if v & hw::PHYAR_FLAG != 0 {
                return (v & 0xffff) as u16;
            }
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }
        0
    }

    /// Restart PHY auto-negotiation (BMCR = ANE | restart-AN, powered up).
    pub fn phy_kick_autoneg(&self) {
        self.mdio_write(0x00, hw::BMCR_AUTONEG_RESTART);
    }

    // --- GPHY-OCP PHY access (8125/8126; see kernel_core::r8169) ---

    /// Raw GPHY-OCP write to OCP byte address `addr` (bounded busy-poll).
    fn gphy_ocp_write(&self, addr: u32, data: u16) {
        self.mmio
            .write_reg::<u32>(off(hw::REG_GPHY_OCP), hw::gphy_ocp_write_cmd(addr, data));
        for _ in 0..100 {
            if !hw::gphy_ocp_busy(self.mmio.read_reg::<u32>(off(hw::REG_GPHY_OCP))) {
                return;
            }
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }
    }

    /// Raw GPHY-OCP read from OCP byte address `addr` (bounded busy-poll).
    fn gphy_ocp_read(&self, addr: u32) -> u16 {
        self.mmio
            .write_reg::<u32>(off(hw::REG_GPHY_OCP), hw::gphy_ocp_read_cmd(addr));
        for _ in 0..100 {
            let v = self.mmio.read_reg::<u32>(off(hw::REG_GPHY_OCP));
            if hw::gphy_ocp_busy(v) {
                return hw::gphy_ocp_read_data(v);
            }
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }
        0
    }

    /// Write a paged PHY register via GPHY-OCP (`page` then in-page `reg`).
    pub fn phy_ocp_write(&self, page: u16, reg: u32, val: u16) {
        let base = hw::ocp_base_for_page(page);
        self.gphy_ocp_write(hw::phy_ocp_addr(base, reg), val);
    }

    /// Read a paged PHY register via GPHY-OCP (`page` then in-page `reg`).
    pub fn phy_ocp_read(&self, page: u16, reg: u32) -> u16 {
        let base = hw::ocp_base_for_page(page);
        self.gphy_ocp_read(hw::phy_ocp_addr(base, reg))
    }

    /// Read the PHY identifier (PHYSID1<<16 | PHYSID2) over GPHY-OCP. A sane,
    /// non-`0x0000`/`0xFFFF` value confirms the OCP accessor reaches the PHY.
    pub fn phy_id_ocp(&self) -> u32 {
        let id1 = self.phy_ocp_read(0, 0x02) as u32;
        let id2 = self.phy_ocp_read(0, 0x03) as u32;
        (id1 << 16) | id2
    }

    /// Bring the 8125 PHY up over GPHY-OCP: log the PHY ID (accessor probe),
    /// then issue a BMCR reset + autoneg-restart + power-up (`0x9240`).
    pub fn phy_bring_up_ocp(&self) {
        let id = self.phy_id_ocp();
        write_hex32("r8169: phy_id(ocp)=0x", id);
        // BMCR (standard-page reg 0): reset + ANE + restart-AN + power-up.
        self.phy_ocp_write(0, 0x00, hw::BMCR_AUTONEG_RESTART);
    }

    /// Poll `PHYstatus` (0x6C) for link-up, up to roughly `max_ms` milliseconds.
    /// Returns `true` as soon as the link bit is set.
    pub fn wait_for_link(&self, max_ms: u32) -> bool {
        // ~1 ms worth of spin per inner loop iteration (calibration is rough;
        // this is a bring-up settle wait, not a precise timer).
        for _ in 0..max_ms {
            if self.phy_status() & hw::PHYSTATUS_LINK != 0 {
                return true;
            }
            for _ in 0..200_000 {
                core::hint::spin_loop();
            }
        }
        self.phy_status() & hw::PHYSTATUS_LINK != 0
    }

    /// Issue ChipCmd.RST and poll the self-clearing bit within a bounded spin.
    fn soft_reset(mmio: &Mmio<R8169Regs>) -> Result<(), BringUpError> {
        mmio.write_reg::<u8>(off(hw::REG_CHIP_CMD), hw::CHIP_CMD_RST);
        for _ in 0..hw::SOFT_RESET_POLL_MAX {
            let cmd = mmio.read_reg::<u8>(off(hw::REG_CHIP_CMD));
            if hw::soft_reset_complete(cmd) {
                return Ok(());
            }
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }
        Err(BringUpError::ResetTimeout)
    }

    /// Program the RX/TX descriptor base-address registers (split 64-bit) and
    /// the RX max-frame size, bracketed by the Cfg9346 unlock/lock window.
    fn program_rings(&mut self) {
        let rx_iova = self.rx.base_iova();
        let tx_iova = self.tx.base_iova();
        let (rxl, rxh) = ((rx_iova & 0xFFFF_FFFF) as u32, (rx_iova >> 32) as u32);
        let (txl, txh) = ((tx_iova & 0xFFFF_FFFF) as u32, (tx_iova >> 32) as u32);

        self.mmio
            .write_reg::<u8>(off(hw::REG_CFG9346), hw::CFG9346_UNLOCK);

        self.mmio
            .write_reg::<u32>(off(hw::REG_RX_DESC_START_ADDR_LOW), rxl);
        self.mmio
            .write_reg::<u32>(off(hw::REG_RX_DESC_START_ADDR_HIGH), rxh);
        self.mmio
            .write_reg::<u32>(off(hw::REG_TX_DESC_START_ADDR_LOW), txl);
        self.mmio
            .write_reg::<u32>(off(hw::REG_TX_DESC_START_ADDR_HIGH), txh);

        self.mmio
            .write_reg::<u16>(off(hw::REG_RX_MAX_SIZE), RX_BUF_SIZE as u16);
        self.mmio.write_reg::<u8>(off(hw::REG_MTPS), 0x3F);

        self.mmio
            .write_reg::<u8>(off(hw::REG_CFG9346), hw::CFG9346_LOCK);
    }

    /// Enable the C+ engine, RX/TX, and unmask the classic RX/TX interrupts.
    /// The 8125 V2 interrupt block is armed separately (see the r8125 driver).
    fn enable(&self) {
        // C+ command register: leave checksum/VLAN offload off at 1.0.
        self.mmio.write_reg::<u16>(off(hw::REG_CPLUS_CMD), 0x0000);
        // ChipCmd: RxEnb | TxEnb.
        self.mmio.write_reg::<u8>(
            off(hw::REG_CHIP_CMD),
            hw::CHIP_CMD_RX_ENB | hw::CHIP_CMD_TX_ENB,
        );
        // RxConfig/TxConfig: the 8125 RX/TX DMA engines need the fetch-default +
        // DMA-burst fields programmed or they never move frames (link comes up
        // but the rings stay idle). The classic 8169 only needs the accept bits.
        if self.version.is_8125() {
            // Ungate RXDV first: the 8125 drops every inbound frame before it
            // reaches the RX ring while RXDV_GATED_EN (MISC bit 19) is set. The
            // classic bring-up never clears it, so RX stays dead despite link.
            let misc = self.mmio.read_reg::<u32>(off(hw::REG_MISC));
            self.mmio
                .write_reg::<u32>(off(hw::REG_MISC), misc & !hw::RXDV_GATED_EN);
            self.mmio
                .write_reg::<u32>(off(hw::REG_TX_CONFIG), hw::txconfig_8125());
            self.mmio
                .write_reg::<u32>(off(hw::REG_RX_CONFIG), hw::rxconfig_8125());
        } else {
            self.mmio
                .write_reg::<u32>(off(hw::REG_RX_CONFIG), hw::RX_CONFIG_ACCEPT);
        }
        // Unmask RX OK (bit0) + TX OK (bit2) on the classic 16-bit IMR.
        self.mmio.write_reg::<u16>(off(hw::REG_INTR_MASK), 0x0005);
    }

    /// Doorbell: tell the NIC to poll the TX ring for newly-owned descriptors.
    #[inline]
    pub fn kick_tx(&self) {
        self.mmio
            .write_reg::<u8>(off(hw::REG_TX_POLL), hw::TX_POLL_NPQ);
    }

    /// Read + write-1-clear the classic 16-bit interrupt status. Returns the
    /// snapshot the caller decodes for RX/TX/link causes.
    #[inline]
    pub fn ack_isr(&self) -> u16 {
        let isr = self.mmio.read_reg::<u16>(off(hw::REG_INTR_STATUS));
        self.mmio.write_reg::<u16>(off(hw::REG_INTR_STATUS), isr);
        isr
    }

    /// Report the detected chip version to the serial log.
    pub fn log_version(&self) {
        let _ = match self.version {
            hw::MacVersion::Ver(_) => {
                sys::write_str(sys::STDOUT_FILENO, "r8169: detected MAC version\n")
            }
            hw::MacVersion::Unknown => sys::write_str(
                sys::STDOUT_FILENO,
                "r8169: WARNING unknown MAC version (XID unmatched)\n",
            ),
        };
    }
}
