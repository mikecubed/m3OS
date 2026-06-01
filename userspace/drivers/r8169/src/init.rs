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

/// Captured RTL8125B MAC-OCP configuration block: `(reg, mask, set)` applied as
/// read-modify-writes in order. This is the `rtl_hw_start_8125` MAC register
/// sequence, empirically traced from Linux's r8169 driver against this exact
/// chip (see `docs/research/r8125-phy-config-capture.md`). It sets up the RX/TX
/// FIFO thresholds, DMA, flow control, and feature gates the 8125 receive engine
/// requires before it will DMA inbound frames into the ring.
const MAC_OCP_CONFIG_8125: &[(u32, u16, u16)] = &[
    (0xc0ac, 0x1f80, 0x000),
    (0xe8de, 0x4000, 0x000),
    (0xe092, 0x00ff, 0x000),
    (0xd40a, 0x0010, 0x000),
    (0xd3e2, 0x0fff, 0x3a9),
    (0xd3e4, 0x00ff, 0x000),
    (0xe860, 0x0000, 0x080),
    (0xeb58, 0x0001, 0x000),
    (0xe614, 0x0700, 0x200),
    (0xe63e, 0x0c30, 0x000),
    (0xc0b4, 0x0000, 0x00c),
    (0xeb6a, 0x00ff, 0x033),
    (0xeb50, 0x03e0, 0x040),
    (0xe056, 0x00f0, 0x000),
    (0xe040, 0x1000, 0x000),
    (0xea1c, 0x0003, 0x001),
    (0xe0c0, 0x4f0f, 0x4403),
    (0xe052, 0x0080, 0x068),
    (0xd430, 0x0fff, 0x47f),
    (0xea1c, 0x0004, 0x000),
    (0xeb54, 0x0000, 0x001),
    (0xeb54, 0x0001, 0x000),
    (0xe040, 0x0000, 0x003),
    (0xc0ac, 0x0000, 0x1f80),
    (0xe094, 0xff00, 0x000),
    (0xe092, 0x00ff, 0x004),
];

/// RTL8125B PHY "parameter" writes via the direct PHY-OCP parameter register
/// pair (`rtl8125_phy_param`): `(parm, mask, val)`. Linux issues these as
/// MMD-VEND2 writes to `0xB87C` (selector) + `0xB87E` (data), which on the 8125
/// resolve to direct OCP register access.
const RTL8125_PHY_PARAMS: &[(u16, u16, u16)] = &[
    (0x80f5, 0xffff, 0x760e),
    (0x8107, 0xffff, 0x360e),
    (0x8551, 0xff00, 0x0800),
];

/// RTL8125B per-channel PHY "parameter" writes via the paged accessor
/// (`r8168g_phy_param`, page `0xa43`): each writes `parm` to reg `0x13` then
/// `(mask,val)`-modifies reg `0x14`. All ten use the same `(mask,val)`.
const R8168G_PHY_PARAM_SELECTORS: &[u16] = &[
    0x8044, 0x804a, 0x8050, 0x8056, 0x805c, 0x8062, 0x8068, 0x806e, 0x8074, 0x807a,
];

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
    pub fn bring_up(key: DeviceCapKey, fw: Option<&[u8]>) -> Result<Self, BringUpError> {
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
        // Order mirrors Linux's `rtl_open`: `rtl_hw_start` (MAC-OCP feature
        // block + `enable()` = RxConfig/RXDV/ChipCmd RxEnb|TxEnb — the MAC fully
        // STARTED) runs first, then `phy_start` → `rtl8125b_hw_phy_config` (the
        // PHY-MCU firmware + PHY config). The MAC's micro-controller must be
        // running to *execute* the streamed firmware patch; loading it while the
        // MAC is idle leaves the patch inert and the PHY link down.
        nic.program_rings();
        if nic.version.is_8125() {
            nic.apply_8125_mac_config();
        }
        nic.enable();
        if nic.version.is_8125() {
            // Full PHY bring-up: firmware → PHY signal-path modifies → autoneg
            // restart (GPHY-OCP). Returns immediately — link comes up
            // asynchronously and is polled in the I/O loop, so the `net.nic`
            // service still registers fast.
            nic.phy_config_8125(fw);
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

    /// Read-modify-write a paged PHY register: `(read & !mask) | set`.
    pub fn phy_ocp_modify(&self, page: u16, reg: u32, mask: u16, set: u16) {
        let v = self.phy_ocp_read(page, reg);
        self.phy_ocp_write(page, reg, (v & !mask) | set);
    }

    /// Read-modify-write a raw PHY-OCP byte address: `(read & !mask) | set`.
    fn gphy_ocp_modify(&self, addr: u32, mask: u16, set: u16) {
        let v = self.gphy_ocp_read(addr);
        self.gphy_ocp_write(addr, (v & !mask) | set);
    }

    /// `rtl8125_phy_param`: select `parm` into the PHY parameter register
    /// (`0xB87C`), then `(mask,val)`-modify the parameter data register
    /// (`0xB87E`) — direct PHY-OCP access (Linux issues these as MMD-VEND2).
    fn rtl8125_phy_param(&self, parm: u16, mask: u16, val: u16) {
        self.gphy_ocp_write(0xB87C, parm);
        self.gphy_ocp_modify(0xB87E, mask, val);
    }

    /// `r8168g_phy_param`: in page `0xa43`, write `parm` to reg `0x13`, then
    /// `(mask,val)`-modify reg `0x14`.
    fn r8168g_phy_param(&self, parm: u16, mask: u16, val: u16) {
        self.phy_ocp_write(0xa43, 0x13, parm);
        self.phy_ocp_modify(0xa43, 0x14, mask, val);
    }

    /// Unlock the PHY-MCU patch RAM with the chip's patch key so the firmware's
    /// `set_phy_mcu_patch_request` handshake (write `0xB820`, poll `0xB800` for
    /// `0x40`) can succeed. Mirrors the RTL8125 vendor driver's
    /// `acquire_phy_mcu_patch_key_lock` (8125B key `0x3700`). Direct PHY-OCP.
    fn phy_mcu_patch_key_acquire(&self) {
        self.gphy_ocp_write(0xA436, 0x8024);
        self.gphy_ocp_write(0xA438, 0x3701);
        self.gphy_ocp_write(0xB82E, 0x0001);
    }

    /// Release the PHY-MCU patch key after the patch is loaded
    /// (`release_phy_mcu_patch_key_lock`).
    fn phy_mcu_patch_key_release(&self) {
        self.gphy_ocp_write(0xA436, 0x8024);
        self.gphy_ocp_write(0xA438, 0x0000);
        self.gphy_ocp_write(0xB82E, 0x0000);
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

    /// RTL8125 PHY bring-up over GPHY-OCP.
    ///
    /// Without a firmware blob (`fw == None` — the default until blob staging
    /// lands) this is the proven minimal path: probe the PHY ID and issue a BMCR
    /// autoneg-restart (`0x9240`), which on real silicon brings the link up.
    ///
    /// With a firmware blob it additionally runs the full
    /// `rtl8125b_hw_phy_config` sequence — BMCR staging (`0x1840`→`0x1040`),
    /// PHY-MCU firmware, the captured PHY signal-path modifies, then the autoneg
    /// restart. NOTE: this experimental path is incomplete — the 8125 MCU-patch
    /// load protocol (the enable/disable bracketing around the firmware that
    /// Linux performs via untraced MAC-OCP writes) is not yet replicated, so the
    /// patched PHY does not relink. See `docs/research/r8125-phy-config-capture.md`.
    pub fn phy_config_8125(&self, fw: Option<&[u8]>) {
        write_hex32("r8169: phy_id(ocp)=0x", self.phy_id_ocp());
        let Some(blob) = fw else {
            // Proven minimal bring-up: just restart auto-negotiation.
            self.phy_ocp_write(0, 0x00, hw::BMCR_AUTONEG_RESTART);
            return;
        };
        // Full `rtl8125b_hw_phy_config`, in Linux order. Firmware FIRST, then the
        // PHY register sequence the patch sits on top of; phylib kicks autoneg
        // afterwards (we issue the BMCR restart at the end).
        //
        // Unlock the PHY-MCU patch RAM before streaming the firmware: the blob's
        // `set_phy_mcu_patch_request` handshake (poll `0xB800` for `0x40`) only
        // completes once the patch key is acquired. The MAC-OCP write path is
        // confirmed working on this silicon (a `0xF800` scratch round-trip echoed
        // exactly), so the patch RAM lands correctly — it just needs unlocking.
        self.phy_mcu_patch_key_acquire();
        match hw::parse_rtl_fw(blob) {
            Ok(img) => {
                let steps = self.apply_firmware(img.code);
                write_hex32("r8125: firmware applied, steps=0x", steps);
            }
            Err(_) => {
                let _ = sys::write_str(
                    sys::STDOUT_FILENO,
                    "r8125: firmware parse failed — continuing untuned\n",
                );
            }
        }
        self.phy_mcu_patch_key_release();
        // rtl8168g_enable_gphy_10m.
        self.phy_ocp_modify(0xa44, 0x11, 0x0000, 0x0800);
        self.phy_ocp_modify(0xac4, 0x13, 0x00f0, 0x0090);
        self.phy_ocp_modify(0xad3, 0x10, 0x0003, 0x0001);
        for &(parm, mask, val) in RTL8125_PHY_PARAMS {
            self.rtl8125_phy_param(parm, mask, val);
        }
        self.phy_ocp_modify(0xbf0, 0x10, 0xe000, 0xa000);
        self.phy_ocp_modify(0xbf4, 0x13, 0x0f00, 0x0300);
        for &parm in R8168G_PHY_PARAM_SELECTORS {
            self.r8168g_phy_param(parm, 0xffff, 0x2417);
        }
        self.phy_ocp_modify(0xa4c, 0x15, 0x0000, 0x0040);
        self.phy_ocp_modify(0xbf8, 0x12, 0xe000, 0xa000);
        // rtl8125_legacy_force_mode.
        self.phy_ocp_modify(0xa5b, 0x12, 0x8000, 0x0000);
        // rtl8168g_disable_aldps.
        self.phy_ocp_modify(0xa43, 0x10, 0x0004, 0x0000);
        // rtl8125_config_eee_phy (rtl8168g + 8125 common).
        self.phy_ocp_modify(0xa43, 0x11, 0x0000, 0x0010);
        self.phy_ocp_modify(0xa6d, 0x14, 0x0010, 0x0000);
        self.phy_ocp_modify(0xa42, 0x14, 0x0080, 0x0000);
        self.phy_ocp_modify(0xa4a, 0x11, 0x0200, 0x0000);
        // Enable + restart auto-negotiation, power up.
        self.phy_ocp_write(0, 0x00, hw::BMCR_AUTONEG_RESTART);
    }

    // --- MAC-OCP access (OCPDR window 0xB0; no busy-poll) ---

    /// MAC-OCP write to register `reg` (`0xC000..0xFFFF`). Same command-word
    /// encoding as the GPHY-OCP window, issued on `OCPDR`. The command flag
    /// (bit 31) self-clears when the OCP engine accepts the write; we wait for
    /// it before returning so a back-to-back stream of patch writes does not
    /// clobber the previous still-in-flight command on `OCPDR`.
    fn mac_ocp_write(&self, reg: u32, data: u16) {
        self.mmio
            .write_reg::<u32>(off(hw::REG_OCPDR), hw::gphy_ocp_write_cmd(reg, data));
        for _ in 0..1000 {
            if !hw::gphy_ocp_busy(self.mmio.read_reg::<u32>(off(hw::REG_OCPDR))) {
                break;
            }
            core::hint::spin_loop();
        }
    }

    /// MAC-OCP read: issue the read command, then read back the low 16 bits
    /// (immediate, mirroring Linux `__r8168_mac_ocp_read`).
    fn mac_ocp_read(&self, reg: u32) -> u16 {
        self.mmio
            .write_reg::<u32>(off(hw::REG_OCPDR), hw::gphy_ocp_read_cmd(reg));
        (self.mmio.read_reg::<u32>(off(hw::REG_OCPDR)) & 0xFFFF) as u16
    }

    /// MAC-OCP read-modify-write: `(read & !mask) | set`.
    fn mac_ocp_modify(&self, reg: u32, mask: u16, set: u16) {
        let data = self.mac_ocp_read(reg);
        self.mac_ocp_write(reg, (data & !mask) | set);
    }

    /// Apply the captured RTL8125B MAC-OCP configuration block (the
    /// `rtl_hw_start_8125` register sequence). This programs the RX/TX FIFO,
    /// DMA, flow-control and feature gates that the 8125 receive engine needs to
    /// actually move frames into the ring — without it the card links up and
    /// transmits but receives nothing. The sequence was empirically traced from
    /// Linux's r8169 driver against this exact chip; see
    /// `docs/research/r8125-phy-config-capture.md`.
    pub fn apply_8125_mac_config(&self) {
        for &(reg, mask, set) in MAC_OCP_CONFIG_8125 {
            self.mac_ocp_modify(reg, mask, set);
        }
    }

    /// Apply a parsed PHY-MCU firmware image (`code` = the `__le32` PHY-action
    /// slice from [`kernel_core::r8169::parse_rtl_fw`]). The 8125 requires this
    /// signed PHY blob — without the MCU patch the PHY completes auto-negotiation
    /// but its receive path stays non-functional. The interpreter
    /// ([`kernel_core::r8169::run_phy_action`]) drives a sink that routes each
    /// write to either the paged GPHY-OCP window (PHY mode) or the paged MAC-OCP
    /// window (MAC-MCU mode), exactly as Linux `mac_mcu_write`/`r8168g_mdio_write`.
    pub fn apply_firmware(&self, code: &[u8]) -> u32 {
        let mut sink = FwSink {
            nic: self,
            mac_mcu_mode: false,
            ocp_base: hw::OCP_STD_PHY_BASE,
        };
        hw::run_phy_action(code, &mut sink)
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

/// Firmware PHY-action sink: routes each interpreter read/write to the paged
/// GPHY-OCP window (PHY mode) or the paged MAC-OCP window (MAC-MCU mode),
/// replicating Linux's `r8168g_mdio_write` and `mac_mcu_write` address paging.
/// `PHY_MDIO_CHG` toggles the mode; register `0x1f` selects the OCP page/base.
struct FwSink<'a> {
    nic: &'a Nic,
    mac_mcu_mode: bool,
    ocp_base: u32,
}

impl hw::PhyActionSink for FwSink<'_> {
    fn read(&mut self, reg: u16) -> u16 {
        if reg == 0x1f {
            return if !self.mac_mcu_mode && self.ocp_base == hw::OCP_STD_PHY_BASE {
                0
            } else {
                (self.ocp_base >> 4) as u16
            };
        }
        if self.mac_mcu_mode {
            // mac_mcu_read: r8168_mac_ocp_read(ocp_base + reg).
            self.nic.mac_ocp_read(self.ocp_base + reg as u32)
        } else {
            // r8168g_mdio_read: (ocp_base + (reg - 0x10 if non-std page) * 2).
            let mut r = reg as u32;
            if self.ocp_base != hw::OCP_STD_PHY_BASE {
                r -= 0x10;
            }
            self.nic.gphy_ocp_read(self.ocp_base + r * 2)
        }
    }

    fn write(&mut self, reg: u16, val: u16) {
        if reg == 0x1f {
            // Page select. MAC-MCU: base = val<<4. PHY: val<<4, or the standard
            // base for page 0 (Linux `r8168g_mdio_write`).
            self.ocp_base = if !self.mac_mcu_mode && val == 0 {
                hw::OCP_STD_PHY_BASE
            } else {
                (val as u32) << 4
            };
            return;
        }
        if self.mac_mcu_mode {
            // mac_mcu_write: r8168_mac_ocp_write(ocp_base + reg, val) — plain add.
            self.nic.mac_ocp_write(self.ocp_base + reg as u32, val);
        } else {
            // r8168g_mdio_write: phy_ocp_write(ocp_base + (reg-0x10 if non-std)*2).
            let mut r = reg as u32;
            if self.ocp_base != hw::OCP_STD_PHY_BASE {
                r -= 0x10;
            }
            self.nic.gphy_ocp_write(self.ocp_base + r * 2, val);
        }
    }

    fn mdio_chg(&mut self, target: u16) {
        // Non-zero target selects the MAC-MCU register space; zero selects PHY.
        self.mac_mcu_mode = target != 0;
    }

    fn delay_ms(&mut self, ms: u16) {
        // Rough busy-wait (~ms milliseconds); firmware delays are short.
        for _ in 0..ms {
            for _ in 0..200_000 {
                core::hint::spin_loop();
            }
        }
    }
}
