//! RTL815x register constants for the m3OS `ure` USB-Ethernet driver.
//!
//! Register offsets and bit-field values re-expressed from OpenBSD `ure(4)`
//! (`sys/dev/usb/if_urereg.h` rev 1.14, `if_ure.c` rev 1.37, BSD-2-Clause).
//! Linux `r8152.c` was consulted for cross-check only; all hex values are
//! transcribed from the OpenBSD source.

// --- OCP vendor-tunnel protocol constants ------------------------------------
//
// The RTL815x exposes its internal register space through a USB vendor control
// transfer.  `bRequest` is always `UR_SET_ADDRESS` (0x05).  `bmRequestType`
// selects direction.  `wValue` is the register address (4-byte aligned for
// read_{1,2}, natural alignment for read_4).  `wIndex` is `mcu_type | byte_en`
// — which MCU bank (PLA or USB) OR-ed with the byte-enable mask for the width
// of the access.  `wLength` is always 4.

/// `bRequest` used for all OCP register reads and writes (UR_SET_ADDRESS = 0x05).
pub const URE_BREQUEST: u8 = 0x05;

/// `bmRequestType` for a vendor OUT (host-to-device | vendor | device) transfer.
pub const URE_BMREQTYPE_WRITE: u8 = 0x40;

/// `bmRequestType` for a vendor IN (device-to-host | vendor | device) transfer.
pub const URE_BMREQTYPE_READ: u8 = 0xC0;

/// MCU bank selector for PLA (Processor Local Area) registers.
/// Placed in the upper byte of `wIndex` (`wIndex = URE_MCU_TYPE_PLA | byte_en`).
pub const URE_MCU_TYPE_PLA: u16 = 0x0100;

/// MCU bank selector for USB registers.
/// Placed in the upper byte of `wIndex` (`wIndex = URE_MCU_TYPE_USB | byte_en`).
pub const URE_MCU_TYPE_USB: u16 = 0x0000;

// --- Byte-enable masks -------------------------------------------------------
//
// These are OR-ed into the lower byte of `wIndex` to tell the chip which bytes
// of the 4-byte data window are valid for this access width.
//
// `ure_write_1` starts with `URE_BYTE_EN_BYTE` (0x11), then shifts it left by
// `(reg & 3)` to select the correct byte lane within the dword-aligned window.
// `ure_write_2` starts with `URE_BYTE_EN_WORD` (0x33), shifts by `(reg & 2)`.
// `ure_write_4` always passes `URE_BYTE_EN_DWORD` (0xff) unmodified.
// Example: `ure_write_4(sc, reg, URE_MCU_TYPE_PLA, val)` calls
//   `ure_write_mem(sc, reg, URE_MCU_TYPE_PLA | URE_BYTE_EN_DWORD, &temp, 4)`.

/// Byte-enable for a 32-bit (dword) access — all four byte lanes enabled.
pub const URE_BYTE_EN_DWORD: u16 = 0x00ff;

/// Byte-enable for a 16-bit (word) access — two byte lanes enabled (base mask).
pub const URE_BYTE_EN_WORD: u16 = 0x0033;

/// Byte-enable for an 8-bit (byte) access — one byte lane enabled (base mask).
pub const URE_BYTE_EN_BYTE: u16 = 0x0011;

/// Byte-enable for a 6-byte MAC-address write (`ure_write_mem` with len=8,
/// covering the 6 MAC bytes plus 2 bytes of padding in the IDR window).
pub const URE_BYTE_EN_SIX_BYTES: u16 = 0x003f;

// --- PLA register offsets ----------------------------------------------------
//
// PLA = Processor Local Area.  These offsets are placed in `wValue`; the MCU
// bank is selected by `URE_MCU_TYPE_PLA` in `wIndex`.

/// MAC address register (6 bytes at 0xC000–0xC005); write with CRWECR=CONFIG.
/// This is the *live* unicast filter, NOT the factory MAC — on a cold device it
/// holds a Realtek default (`00:e0:4c:…`) until a driver copies the real address
/// in from [`URE_PLA_BACKUP`].
pub const URE_PLA_IDR: u16 = 0xC000;

/// Backup MAC register (6 bytes at 0xD7B0), efuse-loaded at power-up with the
/// dongle's **factory** MAC. The RTL8153/8156 family stores the real hardware
/// address here; `IDR` is only the live filter. Linux r8152
/// (`determine_ethernet_addr` → `PLA_BACKUP`) and OpenBSD ure(4) read the
/// address from here, then write it into `IDR`. m3OS must do the same or it
/// presents the Realtek-default `IDR` value (wrong MAC → no DHCP reservation).
pub const URE_PLA_BACKUP: u16 = 0xD7B0;

/// RX config register (32-bit); controls which frames the MAC accepts.
pub const URE_PLA_RCR: u16 = 0xC010;

/// Additional RX config register (16-bit); inner/outer VLAN strip.
pub const URE_PLA_RCR1: u16 = 0xC012;

/// RX max frame size register (16-bit); written with the maximum Ethernet frame
/// length the MAC should accept (e.g. `ETHER_MAX_LEN + ETHER_VLAN_ENCAP_LEN`).
pub const URE_PLA_RMS: u16 = 0xC016;

/// RX FIFO control 0 (32-bit); threshold for FIFO full / OOB behaviour.
pub const URE_PLA_RXFIFO_CTRL0: u16 = 0xC0A0;

/// RX FIFO full threshold register (16-bit); used by 8156/8156B/8157 init.
pub const URE_PLA_RXFIFO_FULL: u16 = 0xC0A2;

/// RX FIFO full threshold register for RTL8156/8156B/8157 (16-bit) at 0xC0A6.
/// Distinct from `URE_PLA_RXFIFO_FULL` (0xC0A2) — used in `ure_rtl8153b_init`
/// and `ure_rtl8153_nic_reset` for 8156 chips.
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_PLA_RX_FIFO_FULL: u16 = 0xC0A6;

/// RX FIFO control 1 (32-bit).
pub const URE_PLA_RXFIFO_CTRL1: u16 = 0xC0A4;

/// RX FIFO empty threshold register (16-bit); used by 8156/8156B/8157 init.
pub const URE_PLA_RX_FIFO_EMPTY: u16 = 0xC0AA;

/// Teredo configuration register (8-bit); written 0xff during NIC reset on 8156.
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_PLA_TEREDO_CFG: u16 = 0xC0BC;

/// Dummy register 0 (16-bit); bit 1 = ECM ALDPS enable.
pub const URE_PLA_DMY_REG0: u16 = 0xC0B0;

/// Frame MCU control register (16-bit); bit 0 = FCR MCU enable.
pub const URE_PLA_FMC: u16 = 0xC0B4;

/// Multicast address register base (64-bit hash; two 32-bit words at 0xCD00).
pub const URE_PLA_MAR: u16 = 0xCD00;

/// BDC control register (16-bit); bit 0 = ALDPS proxy mode.
pub const URE_PLA_BDC_CR: u16 = 0xD1A0;

/// Teredo real-WoW timer register (16-bit); written 0 during NIC reset.
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_PLA_REALWOW_TIMER: u16 = 0xD2E8;

/// Teredo timer register (32-bit); written 0 during NIC reset.
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_PLA_TEREDO_TIMER: u16 = 0xD2CC;

/// Suspend flag register (8-bit); bit 0 = link-change event.
pub const URE_PLA_SUSPEND_FLAG: u16 = 0xD38A;

/// Indicate flag register (8-bit); bit 0 = upcoming runtime D3.
pub const URE_PLA_INDICATE_FALG: u16 = 0xD38C;

/// Extra status register (16-bit); contains CUR_LINK_OK and LINK_CHANGE_FLAG.
pub const URE_PLA_EXTRA_STATUS: u16 = 0xD398;

/// GPHY control register (16-bit).
pub const URE_PLA_GPHY_CTRL: u16 = 0xD3AE;

/// LED select register (16-bit).
pub const URE_PLA_LEDSEL: u16 = 0xDD90;

/// LED feature register (16-bit); LED_MODE_MASK = 0x0700.
pub const URE_PLA_LED_FEATURE: u16 = 0xDD92;

/// PHY access register (32-bit); BUSY flag + PHY data.
pub const URE_PLA_PHYAR: u16 = 0xDE00;

/// Boot control register (16-bit); bit 1 = autoload done.
pub const URE_PLA_BOOT_CTRL: u16 = 0xE004;

/// EEE control register (16-bit); bit 0 = EEE_RX_EN, bit 1 = EEE_TX_EN.
pub const URE_PLA_EEE_CR: u16 = 0xE040;

/// EEE-P control register (16-bit); bit 1 = EEEP_TX.
pub const URE_PLA_EEEP_CR: u16 = 0xE080;

/// MAC power control register (32-bit).
pub const URE_PLA_MAC_PWR_CTRL: u16 = 0xE0C0;

/// MAC power control register 2 (16-bit).
pub const URE_PLA_MAC_PWR_CTRL2: u16 = 0xE0CA;

/// MAC power control register 3 (16-bit).
pub const URE_PLA_MAC_PWR_CTRL3: u16 = 0xE0CC;

/// MAC power control register 4 (16-bit).
pub const URE_PLA_MAC_PWR_CTRL4: u16 = 0xE0CE;

/// Watchdog 6 control register (16-bit); bit 4 = WDT6_SET_MODE.
pub const URE_PLA_WDT6_CTRL: u16 = 0xE428;

/// TX config register 0 (16-bit); bit 7 = AUTO_FIFO, bit 11 = TX_EMPTY.
pub const URE_PLA_TCR0: u16 = 0xE610;

/// TX config register 1 (16-bit); bits [14:4] = chip version mask.
pub const URE_PLA_TCR1: u16 = 0xE612;

/// Max TX packet size register (8-bit); written with MTPS_* values.
pub const URE_PLA_MTPS: u16 = 0xE615;

/// TX FIFO control register (32-bit or 16-bit depending on chip).
pub const URE_PLA_TXFIFO_CTRL: u16 = 0xE618;

/// TX FIFO full threshold register (16-bit); used by 8156/8156B/8157 init.
pub const URE_PLA_TXFIFO_FULL: u16 = 0xE61A;

/// Tally reset register (16-bit); bit 0 = TALLY_RESET.
pub const URE_PLA_RSTTALLY: u16 = 0xE800;

/// Command register (8-bit); RST / RE / TE bits.
pub const URE_PLA_CR: u16 = 0xE813;

/// Config-write-enable control register (8-bit); must be set to CONFIG before
/// writing URE_PLA_IDR (MAC address) or other config registers.
pub const URE_PLA_CRWECR: u16 = 0xE81C;

/// Config3/4 register (16-bit); wake-on-LAN control bits.
pub const URE_PLA_CONFIG34: u16 = 0xE820;

/// Config5 register (16-bit); bit 1 = LAN_WAKE_EN.
pub const URE_PLA_CONFIG5: u16 = 0xE822;

/// PHY power register (16-bit).
pub const URE_PLA_PHY_PWR: u16 = 0xE84C;

/// OOB control register (8-bit); FIFO_EMPTY / NOW_IS_OOB / LINK_LIST_READY.
pub const URE_PLA_OOB_CTRL: u16 = 0xE84F;

/// Chip-level PHY/packet clock control register (16-bit).
pub const URE_PLA_CPCR: u16 = 0xE854;

/// Miscellaneous register 0 (16-bit).
pub const URE_PLA_MISC_0: u16 = 0xE858;

/// Miscellaneous register 1 (16-bit); bit 3 = RXDY_GATED_EN.
pub const URE_PLA_MISC_1: u16 = 0xE85A;

/// OCP GPHY base address register (16-bit); used by `ure_ocp_reg_{read,write}`.
pub const URE_PLA_OCP_GPHY_BASE: u16 = 0xE86C;

/// SFF status 7 register (16-bit); MCU_BORW_EN / RE_INIT_LL bits.
pub const URE_PLA_SFF_STS_7: u16 = 0xE8DE;

/// PHY status register (16-bit); link, duplex, and speed indicator bits.
/// Readable as a 16-bit word via `ure_read_2`.  Used by `ure_get_link_status`
/// and the `ure_ifmedia_sts` RTL8156/8156B/8157 path.
pub const URE_PLA_PHYSTATUS: u16 = 0xE908;

/// Config6 register (8-bit); bit 0 = LANWAKE_CLR_EN.
pub const URE_PLA_CONFIG6: u16 = 0xE90A;

/// USB configuration register (16-bit).
pub const URE_PLA_USB_CFG: u16 = 0xE952;

// --- USB register offsets ----------------------------------------------------
//
// USB-domain registers.  `wIndex = URE_MCU_TYPE_USB | byte_en`.

/// USB2 PHY control register (16-bit).
pub const URE_USB_USB2PHY: u16 = 0xB41E;

/// SS PHY link 1 register (16-bit).
pub const URE_USB_SSPHYLINK1: u16 = 0xB426;

/// SS PHY link 2 register (16-bit).
pub const URE_USB_SSPHYLINK2: u16 = 0xB428;

/// L1 control register (16-bit).
pub const URE_USB_L1_CTRL: u16 = 0xB45E;

/// U2P3 control register (16-bit); bit 0 = U2P3_ENABLE.
pub const URE_USB_U2P3_CTRL: u16 = 0xB460;

/// CSR dummy 1 register (16-bit).
pub const URE_USB_CSR_DUMMY1: u16 = 0xB464;

/// CSR dummy 2 register (16-bit).
pub const URE_USB_CSR_DUMMY2: u16 = 0xB466;

/// Device status register (16-bit); USB speed indicator.
pub const URE_USB_DEV_STAT: u16 = 0xB808;

/// U2P3 control 2 register (32-bit; 8156B/8157).
pub const URE_USB_U2P3_CTRL2: u16 = 0xC2C0;

/// Connect timer register.
pub const URE_USB_CONNECT_TIMER: u16 = 0xCBF8;

/// MSC timer register.
pub const URE_USB_MSC_TIMER: u16 = 0xCBFC;

/// Burst size register (16-bit).
pub const URE_USB_BURST_SIZE: u16 = 0xCFC0;

/// LPM config register (16-bit); bit 0 = LPM_U1U2_EN.
pub const URE_USB_LPM_CONFIG: u16 = 0xCFD8;

/// ECM option register (16-bit); bit 5 = BYPASS_MAC_RESET.
pub const URE_USB_ECM_OPTION: u16 = 0xCFEE;

/// Misc 2 register (8-bit).
pub const URE_USB_MISC_2: u16 = 0xCFFF;

/// ECM OP register (8-bit); bit 0 = EN_ALL_SPEED.
pub const URE_USB_ECM_OP: u16 = 0xD26B;

/// GPHY control register (16-bit); GPHY_PATCH_DONE / BYPASS_FLASH.
pub const URE_USB_GPHY_CTRL: u16 = 0xD284;

/// Speed option register (16-bit); RG_PWRDN_EN / ALL_SPEED_OFF.
pub const URE_USB_SPEED_OPTION: u16 = 0xD32A;

/// FW control register (16-bit); flow-control patch bits.
pub const URE_USB_FW_CTRL: u16 = 0xD334;

/// FC timer register (16-bit); bit 15 = CTRL_TIMER_EN.
pub const URE_USB_FC_TIMER: u16 = 0xD340;

/// USB control register (16-bit); CDC_ECM_EN / RX_AGG_DISABLE / RX_ZERO_EN.
pub const URE_USB_USB_CTRL: u16 = 0xD406;

/// PHY control register (16-bit).
pub const URE_USB_PHY_CTRL: u16 = 0xD408;

/// TX aggregation register (8-bit); max TX aggregation threshold.
pub const URE_USB_TX_AGG: u16 = 0xD40A;

/// RX buffer threshold register (32-bit); SUPER / HIGH / SLOW / B values.
pub const URE_USB_RX_BUF_TH: u16 = 0xD40C;

/// LPM control register (8-bit); FIFO_EMPTY_1FB / LPM_TIMER / ROK_EXIT_LPM.
pub const URE_USB_LPM_CTRL: u16 = 0xD41A;

/// USB timer register (16-bit).
pub const URE_USB_USB_TIMER: u16 = 0xD428;

/// RX early aggregation register (16-bit); coalesce threshold in units of 8.
pub const URE_USB_RX_EARLY_AGG: u16 = 0xD42C;

/// RX early size register (16-bit); max early-completion frame count.
pub const URE_USB_RX_EARLY_SIZE: u16 = 0xD42E;

/// PM control/status register (16-bit); bit 0 = RESUME_INDICATE.
pub const URE_USB_PM_CTRL_STATUS: u16 = 0xD432;

/// TX DMA register (32-bit); TEST_MODE_DISABLE / TX_SIZE_ADJUST1.
pub const URE_USB_TX_DMA: u16 = 0xD434;

/// UPT RX DMA own register (8-bit); OWN_UPDATE / OWN_CLEAR.
pub const URE_USB_UPT_RXDMA_OWN: u16 = 0xD437;

/// USB tolerance register.
pub const URE_USB_TOLERANCE: u16 = 0xD490;

/// BMU reset register (8-bit); BMU_RESET_EP_IN / BMU_RESET_EP_OUT.
pub const URE_USB_BMU_RESET: u16 = 0xD4B0;

/// BMU config register (16-bit); bit 1 = ACT_ODMA.
pub const URE_USB_BMU_CONFIG: u16 = 0xD4B4;

/// U1U2 timer register (16-bit).
pub const URE_USB_U1U2_TIMER: u16 = 0xD4DA;

/// FW task register (16-bit); bit 1 = FC_PATCH_TASK.
pub const URE_USB_FW_TASK: u16 = 0xD4E8;

/// RX aggregation count register (16-bit); mask = 0x1ff.
pub const URE_USB_RX_AGGR_NUM: u16 = 0xD4EE;

/// Command address register (16-bit); used with URE_USB_CMD.
pub const URE_USB_CMD_ADDR: u16 = 0xD5D6;

/// Command data register (32-bit); holds data for OCP command transfers.
pub const URE_USB_CMD_DATA: u16 = 0xD5D8;

/// Command register (16-bit); BMU_CMD / BUSY / WRITE / IP bits.
pub const URE_USB_CMD: u16 = 0xD5DC;

/// TGPHY address register (16-bit; RTL8157 PHY access path).
pub const URE_USB_TGPHY_ADDR: u16 = 0xD630;

/// TGPHY data register (16-bit; RTL8157 PHY access path).
pub const URE_USB_TGPHY_DATA: u16 = 0xD632;

/// TGPHY command register (16-bit; RTL8157 PHY access path).
pub const URE_USB_TGPHY_CMD: u16 = 0xD634;

/// UPS control register (16-bit); bit 8 = POWER_CUT.
pub const URE_USB_UPS_CTRL: u16 = 0xD800;

/// Power cut register (16-bit); PWR_EN / PHASE2_EN / UPS_EN / USP_PREWAKE.
pub const URE_USB_POWER_CUT: u16 = 0xD80A;

/// Misc 0 register (16-bit); bit 0 = PCUT_STATUS.
pub const URE_USB_MISC_0: u16 = 0xD81A;

/// AFE control 2 register (16-bit); SEN_VAL / SEL_RXIDLE.
pub const URE_USB_AFE_CTRL2: u16 = 0xD824;

/// UPS flags register (32-bit).
pub const URE_USB_UPS_FLAGS: u16 = 0xD848;

/// Watchdog 11 control register (16-bit); bit 0 = TIMER11_EN.
pub const URE_USB_WDT11_CTRL: u16 = 0xE43C;

// --- OCP PHY register offsets ------------------------------------------------
//
// Accessed via `ure_ocp_reg_{read,write}`, not directly as wValue.

/// OCP ALDPS config register; power-save / link-enable / save-disable bits.
pub const URE_OCP_ALDPS_CONFIG: u16 = 0x2010;

/// OCP EEE config 1.
pub const URE_OCP_EEE_CONFIG1: u16 = 0x2080;

/// OCP EEE config 2.
pub const URE_OCP_EEE_CONFIG2: u16 = 0x2092;

/// OCP EEE config 3.
pub const URE_OCP_EEE_CONFIG3: u16 = 0x2094;

/// OCP MII base — standard MII registers are at `URE_OCP_BASE_MII + reg * 2`.
pub const URE_OCP_BASE_MII: u16 = 0xA400;

/// OCP EEE auto-negotiation register.
pub const URE_OCP_EEE_AR: u16 = 0xA41A;

/// OCP EEE data register.
pub const URE_OCP_EEE_DATA: u16 = 0xA41C;

/// OCP PHY status register; PHY_STAT_MASK / PHY_STAT_{EXT_INIT,LAN_ON,PWRDN}.
pub const URE_OCP_PHY_STATUS: u16 = 0xA420;

/// OCP power config register; EEE_CLKDIV_EN / EN_ALDPS / EN_10M_PLLOFF.
pub const URE_OCP_POWER_CFG: u16 = 0xA430;

/// OCP EEE config; CTAP_SHORT_EN / EEE10_EN.
pub const URE_OCP_EEE_CFG: u16 = 0xA432;

/// OCP SRAM address register (used with OCP_SRAM_DATA for indirect SRAM access).
pub const URE_OCP_SRAM_ADDR: u16 = 0xA436;

/// OCP SRAM data register.
pub const URE_OCP_SRAM_DATA: u16 = 0xA438;

/// OCP down-speed register; EN_10M_BGOFF.
pub const URE_OCP_DOWN_SPEED: u16 = 0xA442;

/// OCP EEE ability register.
pub const URE_OCP_EEE_ABLE: u16 = 0xA5C4;

/// OCP EEE advertise register.
pub const URE_OCP_EEE_ADV: u16 = 0xA5D0;

/// OCP EEE LP ability register.
pub const URE_OCP_EEE_LPABLE: u16 = 0xA5D2;

/// OCP 10GBT control register; ADV_2500TFDX / ADV_5000TFDX bits.
pub const URE_OCP_10GBT_CTRL: u16 = 0xA5D4;

/// OCP PHY state register; TXDIS_STATE / ABD_STATE bits.
pub const URE_OCP_PHY_STATE: u16 = 0xA708;

/// OCP ADC config register; EN_EMI_L / ADC_EN / CKADSEL_L.
pub const URE_OCP_ADC_CFG: u16 = 0xBC06;

// --- SRAM register offsets ---------------------------------------------------

/// SRAM LPF configuration register.
pub const URE_SRAM_LPF_CFG: u16 = 0x8012;

/// SRAM 10M amplitude register 1.
pub const URE_SRAM_10M_AMP1: u16 = 0x8080;

/// SRAM 10M amplitude register 2.
pub const URE_SRAM_10M_AMP2: u16 = 0x8082;

/// SRAM impedance register.
pub const URE_SRAM_IMPEDANCE: u16 = 0x8084;

// --- URE_PLA_RCR bit fields --------------------------------------------------
//
// The RCR is a 32-bit register; these masks are applied to the full u32 value.

/// RCR: accept all packets (promiscuous).
pub const URE_RCR_AAP: u32 = 0x0000_0001;

/// RCR: accept physical match (unicast to our MAC).
pub const URE_RCR_APM: u32 = 0x0000_0002;

/// RCR: accept multicast.
pub const URE_RCR_AM: u32 = 0x0000_0004;

/// RCR: accept broadcast.
pub const URE_RCR_AB: u32 = 0x0000_0008;

/// RCR: enable slot — combined accept-all mask used during reset.
pub const URE_SLOT_EN: u32 = 0x0000_0800;

/// RCR: convenience mask for all accept bits (AAP | APM | AM | AB).
pub const URE_RCR_ACPT_ALL: u32 = URE_RCR_AAP | URE_RCR_APM | URE_RCR_AM | URE_RCR_AB;

// --- URE_PLA_RCR1 bit fields -------------------------------------------------

/// RCR1: strip inner VLAN tag.
pub const URE_INNER_VLAN: u16 = 0x0040;

/// RCR1: strip outer VLAN tag.
pub const URE_OUTER_VLAN: u16 = 0x0080;

// --- URE_PLA_CR bit fields ---------------------------------------------------
//
// 8-bit register; read with `ure_read_1`, written with `ure_write_1`.

/// CR: software reset — self-clearing after reset completes.
pub const URE_CR_RST: u8 = 0x10;

/// CR: RX enable.
pub const URE_CR_RE: u8 = 0x08;

/// CR: TX enable.
pub const URE_CR_TE: u8 = 0x04;

// --- URE_PLA_CRWECR bit fields -----------------------------------------------
//
// 8-bit register; must be set before writing config registers such as IDR.
// Note: the OpenBSD source spells "NORMAL" as "NORAML" — reproduced faithfully.

/// CRWECR: normal (locked) mode — write-protect config registers.
/// (OpenBSD spells this `URE_CRWECR_NORAML`; that typo is in the source.)
pub const URE_CRWECR_NORMAL: u8 = 0x00;

/// CRWECR: config mode — unlocks writes to IDR and other config registers.
pub const URE_CRWECR_CONFIG: u8 = 0xC0;

// --- URE_PLA_PHYSTATUS bit fields --------------------------------------------
//
// 16-bit register (read as u16 via `ure_read_2`).

/// PHYSTATUS: full-duplex indicator.
pub const URE_PHYSTATUS_FDX: u16 = 0x0001;

/// PHYSTATUS: link up.
pub const URE_PHYSTATUS_LINK: u16 = 0x0002;

/// PHYSTATUS: 10 Mbit/s link.
pub const URE_PHYSTATUS_10MBPS: u16 = 0x0004;

/// PHYSTATUS: 100 Mbit/s link.
pub const URE_PHYSTATUS_100MBPS: u16 = 0x0008;

/// PHYSTATUS: 1000 Mbit/s (Gigabit) link.
pub const URE_PHYSTATUS_1000MBPS: u16 = 0x0010;

/// PHYSTATUS: 2500 Mbit/s link (RTL8156 and later).
pub const URE_PHYSTATUS_2500MBPS: u16 = 0x0400;

/// PHYSTATUS: 5000 Mbit/s link (RTL8157).
pub const URE_PHYSTATUS_5000MBPS: u16 = 0x1000;

// --- URE_PLA_MISC_1 bit fields -----------------------------------------------

/// MISC_1: gate the RXDY signal — set during init/reset, cleared to open RX.
pub const URE_RXDY_GATED_EN: u16 = 0x0008;

// --- URE_PLA_SFF_STS_7 bit fields --------------------------------------------

/// SFF_STS_7: MCU burst/read-write enable.
pub const URE_MCU_BORW_EN: u16 = 0x4000;

/// SFF_STS_7: re-init link list.
pub const URE_RE_INIT_LL: u16 = 0x8000;

// --- URE_PLA_CPCR bit fields -------------------------------------------------

/// CPCR: flow-control enable.
pub const URE_FLOW_CTRL_EN: u16 = 0x0001;

/// CPCR: RX VLAN offload enable.
pub const URE_CPCR_RX_VLAN: u16 = 0x0040;

// --- URE_PLA_OOB_CTRL bit fields ---------------------------------------------

/// OOB_CTRL: disable MCU clear OOB.
pub const URE_DIS_MCU_CLROOB: u8 = 0x01;

/// OOB_CTRL: link list ready.
pub const URE_LINK_LIST_READY: u8 = 0x02;

/// OOB_CTRL: RX FIFO empty.
pub const URE_RXFIFO_EMPTY: u8 = 0x10;

/// OOB_CTRL: TX FIFO empty.
pub const URE_TXFIFO_EMPTY: u8 = 0x20;

/// OOB_CTRL: now is OOB (out-of-band).
pub const URE_NOW_IS_OOB: u8 = 0x80;

/// OOB_CTRL: both FIFOs empty (TX | RX).
pub const URE_FIFO_EMPTY: u8 = URE_TXFIFO_EMPTY | URE_RXFIFO_EMPTY;

// --- URE_PLA_TCR0 bit fields -------------------------------------------------

/// TCR0: auto-FIFO threshold mode.
pub const URE_TCR0_AUTO_FIFO: u16 = 0x0080;

/// TCR0: TX FIFO empty indicator.
pub const URE_TCR0_TX_EMPTY: u16 = 0x0800;

// --- URE_PLA_TCR1 bit fields -------------------------------------------------

/// TCR1: chip version field mask.
pub const URE_VERSION_MASK: u16 = 0x7CF0;

// --- URE_PLA_MTPS values -----------------------------------------------------

/// MTPS: default max TX packet size (unit = 64 bytes; 96 * 64 = 6144 B).
pub const MTPS_DEFAULT: u8 = 96;

/// MTPS: jumbo max TX packet size (192 * 64 = 12288 B).
pub const MTPS_JUMBO: u8 = 192;

/// MTPS: absolute maximum value (255).
pub const MTPS_MAX: u8 = 255;

// --- URE_PLA_PHYAR bit fields ------------------------------------------------

/// PHYAR: PHY data field (lower 16 bits).
pub const URE_PHYAR_PHYDATA: u32 = 0x0000_FFFF;

/// PHYAR: busy flag (bit 31) — set while a PHY read/write is in progress.
pub const URE_PHYAR_BUSY: u32 = 0x8000_0000;

// --- URE_PLA_EEE_CR bit fields -----------------------------------------------

/// EEE_CR: EEE RX enable.
pub const URE_EEE_RX_EN: u16 = 0x0001;

/// EEE_CR: EEE TX enable.
pub const URE_EEE_TX_EN: u16 = 0x0002;

// --- URE_PLA_BOOT_CTRL bit fields --------------------------------------------

/// BOOT_CTRL: autoload/firmware-load done.
pub const URE_AUTOLOAD_DONE: u16 = 0x0002;

// --- URE_PLA_EXTRA_STATUS bit fields -----------------------------------------

// --- URE_PLA_INDICATE_FALG bit fields ----------------------------------------

/// INDICATE_FALG: upcoming runtime D3 transition.
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_UPCOMING_RUNTIME_D3: u8 = 0x01;

// --- URE_PLA_SUSPEND_FLAG bit fields -----------------------------------------

/// SUSPEND_FLAG: link-change event pending.
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_LINK_CHG_EVENT: u8 = 0x01;

// --- URE_PLA_CONFIG34 bit fields ---------------------------------------------

/// CONFIG34: link-off wake enable.
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_LINK_OFF_WAKE_EN: u16 = 0x0008;

// --- URE_PLA_MAC_PWR_CTRL3 bit fields ----------------------------------------

/// MAC_PWR_CTRL3: MCU speed-down enable.
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_PLA_MCU_SPDWN_EN: u16 = 0x4000;

// --- URE_PLA_WDT6_CTRL bit fields --------------------------------------------

/// WDT6_CTRL: set WDT6 mode (bit 4).
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_WDT6_SET_MODE: u16 = 0x0010;

// --- URE_PLA_RSTTALLY bit fields ---------------------------------------------

/// RSTTALLY: tally reset (bit 0).
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_TALLY_RESET: u16 = 0x0001;

// --- URE_PLA_FMC bit fields --------------------------------------------------

/// FMC: FCR MCU enable (bit 0).
/// Source: OpenBSD `sys/dev/usb/if_urereg.h`.
pub const URE_FMC_FCR_MCU_EN: u16 = 0x0001;

// --- URE_PLA_EXTRA_STATUS bit fields -----------------------------------------

/// EXTRA_STATUS: poll link change.
pub const URE_POLL_LINK_CHG: u16 = 0x0001;

/// EXTRA_STATUS: link change flag.
pub const URE_LINK_CHANGE_FLAG: u16 = 0x0100;

/// EXTRA_STATUS: current link OK.
pub const URE_CUR_LINK_OK: u16 = 0x8000;

// --- USB register bit fields -------------------------------------------------

/// DEV_STAT: USB high-speed.
pub const URE_STAT_SPEED_HIGH: u16 = 0x0000;

/// DEV_STAT: USB full-speed.
pub const URE_STAT_SPEED_FULL: u16 = 0x0001;

/// DEV_STAT: speed mask.
pub const URE_STAT_SPEED_MASK: u16 = 0x0006;

/// U2P3_CTRL: U2P3 enable.
pub const URE_U2P3_ENABLE: u16 = 0x0001;

/// U2P3_CTRL: RX detect 8 lanes.
pub const URE_RX_DETECT8: u16 = 0x0008;

/// USB_USB_CTRL: CDC ECM enable.
pub const URE_CDC_ECM_EN: u16 = 0x0008;

/// USB_USB_CTRL: disable RX aggregation.
pub const URE_RX_AGG_DISABLE: u16 = 0x0010;

/// USB_USB_CTRL: RX zero-length-packet enable.
pub const URE_RX_ZERO_EN: u16 = 0x0080;

/// LPM_CONFIG: U1/U2 enable.
pub const LPM_U1U2_EN: u16 = 0x0001;

/// MISC_2: force power-down.
pub const URE_UPS_FORCE_PWR_DOWN: u8 = 0x01;

/// MISC_2: no UPS.
pub const URE_UPS_NO_UPS: u8 = 0x80;

/// ECM_OPTION: bypass MAC reset.
pub const URE_BYPASS_MAC_RESET: u16 = 0x0020;

/// GPHY_CTRL (USB): GPHY patch done.
pub const URE_GPHY_PATCH_DONE: u16 = 0x0004;

/// GPHY_CTRL (USB): bypass flash.
pub const URE_BYPASS_FLASH: u16 = 0x0020;

/// SPEED_OPTION: power-down enable.
pub const URE_RG_PWRDN_EN: u16 = 0x0100;

/// SPEED_OPTION: all speeds off.
pub const URE_ALL_SPEED_OFF: u16 = 0x0200;

/// FW_CTRL: flow-control patch option.
pub const URE_FLOW_CTRL_PATCH_OPT: u16 = 0x0002;

/// FW_CTRL: auto speed-up.
pub const URE_AUTO_SPEEDUP: u16 = 0x0008;

/// FW_CTRL: flow-control patch 2.
pub const URE_FLOW_CTRL_PATCH_2: u16 = 0x0100;

/// FC_TIMER: control timer enable.
pub const URE_CTRL_TIMER_EN: u16 = 0x8000;

/// ECM_OP: enable all speeds.
pub const URE_EN_ALL_SPEED: u16 = 0x0001;

/// TX_AGG: max TX aggregation threshold.
pub const URE_TX_AGG_MAX_THRESHOLD: u8 = 0x03;

/// RX_BUF_TH: SuperSpeed RX threshold.
pub const URE_RX_THR_SUPER: u32 = 0x0C35_0180;

/// RX_BUF_TH: high-speed RX threshold.
pub const URE_RX_THR_HIGH: u32 = 0x7A12_0180;

/// RX_BUF_TH: slow RX threshold.
pub const URE_RX_THR_SLOW: u32 = 0xFFFF_0180;

/// RX_BUF_TH: RTL8153B RX threshold.
pub const URE_RX_THR_B: u32 = 0x0001_0001;

/// TX_DMA: test-mode disable bit.
pub const URE_TEST_MODE_DISABLE: u32 = 0x0000_0001;

/// TX_DMA: TX size adjustment 1.
pub const URE_TX_SIZE_ADJUST1: u32 = 0x0000_0100;

/// UPT_RXDMA_OWN: update ownership.
pub const URE_OWN_UPDATE: u8 = 0x01;

/// UPT_RXDMA_OWN: clear ownership.
pub const URE_OWN_CLEAR: u8 = 0x02;

/// BMU_RESET: reset EP IN.
pub const BMU_RESET_EP_IN: u8 = 0x01;

/// BMU_RESET: reset EP OUT.
pub const BMU_RESET_EP_OUT: u8 = 0x02;

/// BMU_CONFIG: activate ODMA.
pub const URE_ACT_ODMA: u16 = 0x0002;

/// FW_TASK: FC patch task.
pub const URE_FC_PATCH_TASK: u16 = 0x0002;

/// RX_AGGR_NUM: mask for aggregation count field.
pub const URE_RX_AGGR_NUM_MASK: u16 = 0x01FF;

/// USB_CMD: BMU command type selector.
pub const URE_CMD_BMU: u16 = 0x0000;

/// USB_CMD: busy flag.
pub const URE_CMD_BUSY: u16 = 0x0001;

/// USB_CMD: write direction.
pub const URE_CMD_WRITE: u16 = 0x0002;

/// USB_CMD: IP command type selector.
pub const URE_CMD_IP: u16 = 0x0004;

/// TGPHY_CMD: busy flag (RTL8157 PHY access).
pub const URE_TGPHY_CMD_BUSY: u16 = 0x0001;

/// TGPHY_CMD: write direction (RTL8157 PHY access).
pub const URE_TGPHY_CMD_WRITE: u16 = 0x0002;

/// UPS_CTRL: power cut.
pub const URE_POWER_CUT: u16 = 0x0100;

/// PM_CTRL_STATUS: resume indicate.
pub const URE_RESUME_INDICATE: u16 = 0x0001;

/// POWER_CUT: power enable.
pub const URE_PWR_EN: u16 = 0x0001;

/// POWER_CUT: phase 2 enable.
pub const URE_PHASE2_EN: u16 = 0x0008;

/// POWER_CUT: UPS enable.
pub const URE_UPS_EN: u16 = 0x0010;

/// POWER_CUT: USP prewake.
pub const URE_USP_PREWAKE: u16 = 0x0020;

/// MISC_0 (USB): power-cut status.
pub const URE_PCUT_STATUS: u16 = 0x0001;

/// RX_EARLY_AGG: SuperSpeed coalesce threshold (microseconds, raw).
pub const URE_COALESCE_SUPER: u32 = 85_000;

/// RX_EARLY_AGG: high-speed coalesce threshold (microseconds, raw).
pub const URE_COALESCE_HIGH: u32 = 250_000;

/// RX_EARLY_AGG: slow coalesce threshold (microseconds, raw).
pub const URE_COALESCE_SLOW: u32 = 524_280;

/// WDT11_CTRL: timer 11 enable.
pub const URE_TIMER11_EN: u16 = 0x0001;

/// LPM_CTRL: FIFO empty 1FB flag.
pub const URE_FIFO_EMPTY_1FB: u8 = 0x30;

/// LPM_CTRL: LPM timer mask.
pub const URE_LPM_TIMER_MASK: u8 = 0x0C;

/// LPM_CTRL: LPM timer 500 ms.
pub const URE_LPM_TIMER_500MS: u8 = 0x04;

/// LPM_CTRL: LPM timer 500 µs.
pub const URE_LPM_TIMER_500US: u8 = 0x0C;

/// LPM_CTRL: exit LPM on ROK.
pub const URE_ROK_EXIT_LPM: u8 = 0x02;

/// AFE_CTRL2: SEN value mask.
pub const URE_SEN_VAL_MASK: u16 = 0xF800;

/// AFE_CTRL2: normal SEN value.
pub const URE_SEN_VAL_NORMAL: u16 = 0xA000;

/// AFE_CTRL2: select RX idle.
pub const URE_SEL_RXIDLE: u16 = 0x0100;

/// UPS_FLAGS: enable ALDPS.
pub const URE_UPS_FLAGS_EN_ALDPS: u32 = 0x0000_0008;

/// UPS_FLAGS: full mask.
pub const URE_UPS_FLAGS_MASK: u32 = 0xFFFF_FFFF;

// --- OCP PHY register bit fields ---------------------------------------------

/// ALDPS_CONFIG: enable power save.
pub const URE_ENPWRSAVE: u16 = 0x8000;

/// ALDPS_CONFIG: enable power-down PS.
pub const URE_ENPDNPS: u16 = 0x0200;

/// ALDPS_CONFIG: link enable.
pub const URE_LINKENA: u16 = 0x0100;

/// ALDPS_CONFIG: disable SD save.
pub const URE_DIS_SDSAVE: u16 = 0x0010;

/// OCP_PHY_STATUS: PHY status mask.
pub const URE_PHY_STAT_MASK: u16 = 0x0007;

/// OCP_PHY_STATUS: external init state.
pub const URE_PHY_STAT_EXT_INIT: u16 = 2;

/// OCP_PHY_STATUS: LAN on state.
pub const URE_PHY_STAT_LAN_ON: u16 = 3;

/// OCP_PHY_STATUS: powered-down state.
pub const URE_PHY_STAT_PWRDN: u16 = 5;

/// OCP_POWER_CFG: EEE clock divider enable.
pub const URE_EEE_CLKDIV_EN: u16 = 0x8000;

/// OCP_POWER_CFG: enable ALDPS.
pub const URE_EN_ALDPS: u16 = 0x0004;

/// OCP_POWER_CFG: enable 10M PLL off.
pub const URE_EN_10M_PLLOFF: u16 = 0x0001;

/// OCP_EEE_CFG: CTAP short enable.
pub const URE_CTAP_SHORT_EN: u16 = 0x0040;

/// OCP_EEE_CFG: EEE 10M enable.
pub const URE_EEE10_EN: u16 = 0x0010;

/// OCP_DOWN_SPEED: enable 10M BG off.
pub const URE_EN_10M_BGOFF: u16 = 0x0080;

/// OCP_PHY_STATE: TX disable state.
pub const URE_TXDIS_STATE: u16 = 0x0001;

/// OCP_PHY_STATE: ABD state.
pub const URE_ABD_STATE: u16 = 0x0002;

/// OCP_ADC_CFG: enable EMI L.
pub const URE_EN_EMI_L: u16 = 0x0040;

/// OCP_ADC_CFG: ADC enable.
pub const URE_ADC_EN: u16 = 0x0080;

/// OCP_ADC_CFG: CKADSEL L.
pub const URE_CKADSEL_L: u16 = 0x0100;

/// OCP_10GBT_CTRL: advertise 2500BASE-T FDX (RTL8156+).
pub const URE_ADV_2500TFDX: u16 = 0x0080;

/// OCP_10GBT_CTRL: advertise 5000BASE-T FDX (RTL8157).
pub const URE_ADV_5000TFDX: u16 = 0x0100;

// --- RX/TX descriptor (v1) layout --------------------------------------------
//
// The RTL815x prefixes every bulk-IN packet with a `ure_rxpkt` header and
// every bulk-OUT payload with a `ure_txpkt` header.  Both are 8 bytes (two
// `u32` LE words).  The v2 variants (RTL8157) are 16 bytes (four `u32`).
//
// RX descriptor (v1): two u32 LE words
//   word 0: packet length + status flags
//   word 1: VLAN tag + protocol type flags
//   (word 2..5 of the 24-byte struct in the header are reserved padding)
//
// TX descriptor (v1): two u32 LE words
//   word 0: TX_FS | TX_LS | length
//   word 1: VLAN tag + L4 protocol flags

/// RX v1: number of bytes in the descriptor prefix that precedes each received
/// frame in a bulk-IN buffer — the **full 24-byte `ure_rxpkt` struct** (six
/// 32-bit words: word 0 length, word 1 flags/VLAN, words 2..5 csum/reserved).
/// The Ethernet frame begins *after* all 24 bytes (matches OpenBSD ure(4)
/// `sizeof(struct ure_rxpkt)` and Linux r8152 `sizeof(struct rx_desc)`).
///
/// This was previously `8` — only the two words the driver decodes — which left
/// 16 bytes of reserved descriptor padding prepended to every frame, so the
/// kernel net stack read the EtherType (and MACs) from the descriptor instead of
/// the frame (`etype=0x0000`) and dropped every packet → "no route to host".
pub const URE_RXPKT_HDR_SIZE: usize = 24;

/// TX v1: number of bytes in the descriptor prefix prepended to each transmitted
/// frame — the **8-byte `ure_txpkt` struct** (two 32-bit words: word 0
/// FS/LS/length, word 1 VLAN/offload). Distinct from the 24-byte RX prefix.
pub const URE_TXPKT_HDR_SIZE: usize = 8;

/// RX v1 word 0: packet length mask (bits [14:0]).
pub const URE_RXPKT_LEN_MASK: u32 = 0x7FFF;

/// RX v1 word 1: UDP checksum OK indicator (bit 23).
pub const URE_RXPKT_UDP: u32 = 1 << 23;

/// RX v1 word 1: TCP checksum OK indicator (bit 22).
pub const URE_RXPKT_TCP: u32 = 1 << 22;

/// RX v1 word 1: IPv6 packet (bit 20).
pub const URE_RXPKT_IPV6: u32 = 1 << 20;

/// RX v1 word 1: IPv4 packet (bit 19).
pub const URE_RXPKT_IPV4: u32 = 1 << 19;

/// RX v1 word 1: VLAN tagged (bit 16).
pub const URE_RXPKT_VLAN_TAG: u32 = 1 << 16;

/// RX v1 word 1: VLAN data mask (bits [15:0]).
pub const URE_RXPKT_VLAN_DATA: u32 = 0xFFFF;

/// RX v1 word 2 (csum): IP checksum bad (bit 23).
pub const URE_RXPKT_IPSUMBAD: u32 = 1 << 23;

/// RX v1 word 2 (csum): UDP checksum bad (bit 22).
pub const URE_RXPKT_UDPSUMBAD: u32 = 1 << 22;

/// RX v1 word 2 (csum): TCP checksum bad (bit 21).
pub const URE_RXPKT_TCPSUMBAD: u32 = 1 << 21;

/// TX v1: first segment of the packet (bit 31 of word 0).
pub const URE_TXPKT_TX_FS: u32 = 1 << 31;

/// TX v1: last segment of the packet (bit 30 of word 0).
pub const URE_TXPKT_TX_LS: u32 = 1 << 30;

/// TX v1: packet length mask (bits [15:0] of word 0).
pub const URE_TXPKT_LEN_MASK: u32 = 0xFFFF;

/// TX v1: UDP checksum offload (bit 31 of word 1).
pub const URE_TXPKT_UDP: u32 = 1 << 31;

/// TX v1: TCP checksum offload (bit 30 of word 1).
pub const URE_TXPKT_TCP: u32 = 1 << 30;

/// TX v1: IPv4 checksum offload (bit 29 of word 1).
pub const URE_TXPKT_IPV4: u32 = 1 << 29;

/// TX v1: IPv6 (bit 28 of word 1).
pub const URE_TXPKT_IPV6: u32 = 1 << 28;

/// TX v1: VLAN insert (bit 16 of word 1).
pub const URE_TXPKT_VLAN_TAG: u32 = 1 << 16;

// --- RX/TX descriptor (v2) layout — RTL8157 ----------------------------------
//
// RX v2: four u32 LE words (16 bytes total).  `ure_pktlen` word 0 encodes
// the length in bits [31:17] (shifted right 17 to recover byte count).

/// RX v2: number of bytes in the v2 descriptor prefix.
pub const URE_RXPKT_V2_HDR_SIZE: usize = 16;

/// RX v2 word 0: packet length mask (bits [31:17]).
pub const URE_RXPKT_V2_LEN_MASK: u32 = 0xFFFE_0000;

/// RX v2 word 0: VLAN tag present (bit 3).
pub const URE_RXPKT_V2_VLAN_TAG: u32 = 1 << 3;

/// RX v2 word 2 (csum): IP checksum bad (bit 26).
pub const URE_RXPKT_V2_IPSUMBAD: u32 = 1 << 26;

/// RX v2 word 2 (csum): UDP checksum bad (bit 25).
pub const URE_RXPKT_V2_UDPSUMBAD: u32 = 1 << 25;

/// RX v2 word 2 (csum): TCP checksum bad (bit 24).
pub const URE_RXPKT_V2_TCPSUMBAD: u32 = 1 << 24;

/// RX v2 word 2: IPv6 (bit 15).
pub const URE_RXPKT_V2_IPV6: u32 = 1 << 15;

/// RX v2 word 2: IPv4 (bit 14).
pub const URE_RXPKT_V2_IPV4: u32 = 1 << 14;

/// RX v2 word 2: UDP (bit 11).
pub const URE_RXPKT_V2_UDP: u32 = 1 << 11;

/// RX v2 word 2: TCP (bit 10).
pub const URE_RXPKT_V2_TCP: u32 = 1 << 10;

/// TX v2: signature word value that must be placed in `ure_signature` (word 3).
pub const URE_TXPKT_SIGNATURE: u32 = 0xA800_0000;

// --- Framing / buffer size constants -----------------------------------------
//
// For `URE_PLA_RMS`:
//   - RTL8152 init writes `ETHER_MAX_LEN + ETHER_VLAN_ENCAP_LEN` (1518 + 4 = 1522).
//   - RTL8153 NIC reset writes `URE_FRAMELEN(ifp->if_mtu)` which at MTU=1500
//     expands to `mtu + ETHER_HDR_LEN + ETHER_CRC_LEN + ETHER_VLAN_ENCAP_LEN`
//     = 1500 + 14 + 4 + 4 = 1522.
// Both produce 1522 at standard MTU; value below is the RTL8152 path literal.

/// Standard Ethernet max frame size including VLAN encap; written to `URE_PLA_RMS`
/// in the RTL8152 init path.  Equals `ETHER_MAX_LEN (1518) + ETHER_VLAN_ENCAP_LEN (4)`.
pub const URE_RMS_DEFAULT: u16 = 1522;

/// Jumbo frame length (9 KiB, from `URE_JUMBO_FRAMELEN`).
pub const URE_JUMBO_FRAMELEN: u32 = 9 * 1024;

/// TX buffer size for standard (RTL8152/8153) chips.
pub const URE_TX_BUFSZ: u32 = 16384;

/// TX buffer size for RTL8156/8156B/8157 chips.
pub const URE_8156_TX_BUFSZ: u32 = 32768;

/// RX buffer size for RTL8152.
pub const URE_8152_RX_BUFSZ: u32 = 16384;

/// RX buffer size for RTL8153 and later.
pub const URE_8153_RX_BUFSZ: u32 = 32768;

/// Alignment requirement for RX buffers (standard chips).
pub const URE_RX_BUF_ALIGN: u32 = 8;

/// Alignment requirement for TX buffers.
pub const URE_TX_BUF_ALIGN: u32 = 4;

/// Alignment requirement for RTL8157 RX/TX buffers.
pub const URE_8157_BUF_ALIGN: u32 = 16;

// --- Driver/chip flag values -------------------------------------------------
//
// Copied from `ure_softc.ure_flags` for reference in Rust match arms.

/// Driver flag: link is up.
pub const URE_FLAG_LINK: u32 = 0x0001;

/// Driver flag: chip is RTL8152.
pub const URE_FLAG_8152: u32 = 0x0010;

/// Driver flag: chip is RTL8153B.
pub const URE_FLAG_8153B: u32 = 0x0020;

/// Driver flag: chip is RTL8156.
pub const URE_FLAG_8156: u32 = 0x0040;

/// Driver flag: chip is RTL8156B.
pub const URE_FLAG_8156B: u32 = 0x0080;

/// Driver flag: chip is RTL8157.
pub const URE_FLAG_8157: u32 = 0x0100;

/// Driver flag: mask for chip-type bits.
pub const URE_FLAG_CHIP_MASK: u32 = 0x01F0;

// --- Chip version constants --------------------------------------------------

pub const URE_CHIP_VER_4C00: u32 = 0x01;
pub const URE_CHIP_VER_4C10: u32 = 0x02;
pub const URE_CHIP_VER_5C00: u32 = 0x04;
pub const URE_CHIP_VER_5C10: u32 = 0x08;
pub const URE_CHIP_VER_5C20: u32 = 0x10;
pub const URE_CHIP_VER_5C30: u32 = 0x20;
pub const URE_CHIP_VER_6010: u32 = 0x40;
pub const URE_CHIP_VER_7420: u32 = 0x80;

// --- Miscellaneous timeouts / protocol constants -----------------------------

/// USB control transfer timeout in milliseconds.
pub const URE_TIMEOUT: u32 = 1000;

/// PHY operation timeout in milliseconds.
pub const URE_PHY_TIMEOUT: u32 = 2000;

// --- Byte-enable helper ------------------------------------------------------
//
// Mirrors the logic in `ure_write_1` / `ure_write_2` for computing the shifted
// byte-enable mask.  These are `const fn` so they can be used in `static`
// initialisers as well as at runtime.

/// Compute the shifted byte-enable mask for an 8-bit write to `reg`.
///
/// Equivalent to: `URE_BYTE_EN_BYTE << (reg & 3)`.
/// The result is OR-ed into the MCU-type word to form `wIndex`.
#[inline]
pub const fn byte_en_1(reg: u16) -> u16 {
    URE_BYTE_EN_BYTE << (reg & 3)
}

/// Compute the shifted byte-enable mask for a 16-bit write to `reg`.
///
/// Equivalent to: `URE_BYTE_EN_WORD << (reg & 2)`.
#[inline]
pub const fn byte_en_2(reg: u16) -> u16 {
    URE_BYTE_EN_WORD << (reg & 2)
}
