//! Ring-3 USB-Ethernet driver for Realtek RTL815x — Phase 96 Stage-1a/1b.
//!
//! Stage-1a scope: **bring-up probe** — claim the real RTL8156 dongle
//! (`0x0bda:0x8156`) via the xHCI server's `NextAttach` cursor, read the MAC
//! address over the EP0 control path (OCP/PLA vendor IN transfer), print it.
//!
//! Stage-1b scope: **chip init, auto-selected by device ownership state.**
//! `PLA_OOB_CTRL.NOW_IS_OOB` distinguishes a **cold** device (firmware owns the
//! MAC — bare-metal cold attach) from a host-pre-initialized one (QEMU usb-host
//! passthrough). Cold → the full faithful OpenBSD ure(4) attach sequence
//! (`ure_rtl8153b_init` + `ure_rtl8153_nic_reset` + `ure_ifmedia_init` +
//! `ure_iff`, exact order, EP0-critical CDC_ECM toggle preserved) which fully
//! enables the RX datapath. Pre-initialized → a light-touch `ure_init_minimal`
//! that enables RX/TX without re-running the vendor reset (which would tear down
//! the host-established USB link the passthrough can't re-enumerate). Same image
//! works on both QEMU and bare metal.
//!
//! RTL815x register constants and OCP read/write sequences re-expressed from
//! OpenBSD ure(4) (`if_urereg.h`/`if_ure.c`, BSD-2-Clause); Linux r8152.c used
//! for facts only.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::Layout;

use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;
use usb_core::protocol::{USB_MSG_MAX, USB_REQ_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest};

mod net;
// Register map (re-expressed from OpenBSD ure(4)). Most constants describe the
// full chip and are unused by the Stage-1a/1b minimal path; keep the complete
// reference map rather than trimming it to the current call sites.
#[allow(dead_code)]
mod regs;
use regs::{
    BMU_RESET_EP_IN, BMU_RESET_EP_OUT, LPM_U1U2_EN, URE_ACT_ODMA, URE_AUTOLOAD_DONE,
    URE_BMREQTYPE_READ, URE_BMREQTYPE_WRITE, URE_BREQUEST, URE_BYPASS_MAC_RESET, URE_BYTE_EN_BYTE,
    URE_BYTE_EN_DWORD, URE_BYTE_EN_SIX_BYTES, URE_BYTE_EN_WORD, URE_CDC_ECM_EN, URE_CR_RE,
    URE_CR_TE, URE_CTRL_TIMER_EN, URE_EN_ALL_SPEED, URE_FLOW_CTRL_EN, URE_FMC_FCR_MCU_EN,
    URE_LINK_CHANGE_FLAG, URE_LINK_CHG_EVENT, URE_LINK_LIST_READY, URE_LINK_OFF_WAKE_EN,
    URE_MCU_BORW_EN, URE_MCU_TYPE_PLA, URE_MCU_TYPE_USB, URE_NOW_IS_OOB, URE_PCUT_STATUS,
    URE_PHYSTATUS_10MBPS, URE_PHYSTATUS_100MBPS, URE_PHYSTATUS_1000MBPS, URE_PHYSTATUS_2500MBPS,
    URE_PHYSTATUS_LINK, URE_PLA_BACKUP, URE_PLA_BOOT_CTRL, URE_PLA_CONFIG34, URE_PLA_CPCR,
    URE_PLA_CR, URE_PLA_CRWECR, URE_PLA_EXTRA_STATUS, URE_PLA_FMC, URE_PLA_IDR,
    URE_PLA_INDICATE_FALG, URE_PLA_MAC_PWR_CTRL3, URE_PLA_MCU_SPDWN_EN, URE_PLA_MISC_1,
    URE_PLA_MTPS, URE_PLA_OOB_CTRL, URE_PLA_PHYSTATUS, URE_PLA_RCR, URE_PLA_RCR1,
    URE_PLA_REALWOW_TIMER, URE_PLA_RMS, URE_PLA_RSTTALLY, URE_PLA_RX_FIFO_EMPTY,
    URE_PLA_RX_FIFO_FULL, URE_PLA_SFF_STS_7, URE_PLA_SUSPEND_FLAG, URE_PLA_TEREDO_CFG,
    URE_PLA_TEREDO_TIMER, URE_PLA_TXFIFO_CTRL, URE_PLA_TXFIFO_FULL, URE_PLA_WDT6_CTRL, URE_PWR_EN,
    URE_RCR_AB, URE_RCR_ACPT_ALL, URE_RCR_APM, URE_RMS_DEFAULT, URE_RX_AGG_DISABLE, URE_RX_ZERO_EN,
    URE_RXDY_GATED_EN, URE_SLOT_EN, URE_TALLY_RESET, URE_TIMEOUT, URE_U2P3_ENABLE,
    URE_UPCOMING_RUNTIME_D3, URE_UPS_EN, URE_UPS_FORCE_PWR_DOWN, URE_UPS_NO_UPS,
    URE_USB_BMU_CONFIG, URE_USB_BMU_RESET, URE_USB_ECM_OP, URE_USB_ECM_OPTION, URE_USB_FC_TIMER,
    URE_USB_LPM_CONFIG, URE_USB_MISC_0, URE_USB_MISC_2, URE_USB_MSC_TIMER, URE_USB_PM_CTRL_STATUS,
    URE_USB_POWER_CUT, URE_USB_RX_BUF_TH, URE_USB_RX_EARLY_AGG, URE_USB_SPEED_OPTION,
    URE_USB_U1U2_TIMER, URE_USB_U2P3_CTRL, URE_USB_USB_CTRL, URE_USP_PREWAKE, URE_WDT6_SET_MODE,
};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "ure: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "ure: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

// ---------------------------------------------------------------------------
// Device identity
// ---------------------------------------------------------------------------

/// USB vendor ID for Realtek.
const VENDOR_REALTEK: u16 = 0x0bda;

/// Bare-metal log-spam gate. The Dell 5560 has no serial port, so all driver
/// stdout lands on the *uncached* framebuffer console where each line costs
/// ~hundreds of ms — flooding it both hides the useful lines (de-risk banner,
/// timer readout) and throttles the very traffic the diagnostics report (the SSH
/// lockups). Default `false` keeps only one-shot bring-up + sentinels; flip to
/// `true` and rebuild for the full RX/TX/heartbeat trace during driver debug.
/// (Once on-drive log persistence lands, the full detail lives there anyway.)
pub(crate) const VERBOSE: bool = false;

/// Product ID for the RTL8156 USB-Ethernet adapter (the Phase 96 target).
const PRODUCT_RTL8156: u16 = 0x8156;

/// Realtek USB-Ethernet product IDs the `ure` register sequences support. The
/// RTL8153B/8156/8157 family shares the OCP/PLA/USB register layout `ure(4)`
/// drives; matching the family (not just 0x8156) means a dongle that reports a
/// sibling PID — common across RTL815x revisions — is still claimed rather than
/// silently skipped. Gated on `VENDOR_REALTEK`, so non-NIC Realtek devices
/// (card readers etc., different PIDs) are unaffected.
const SUPPORTED_REALTEK_NIC_PIDS: &[u16] = &[
    0x8152,          // RTL8152 (10/100)
    0x8153,          // RTL8153 (gigabit)
    0x8155,          // RTL8156 (2.5G, early)
    PRODUCT_RTL8156, // RTL8156 (2.5G) — the Phase 96 target
    0x8157,          // RTL8157 (5G)
];

/// Maximum number of surfaced devices the heartbeat replays. The xHCI server
/// surfaces one notice per attached device, so this bounds the bare-metal
/// device count we echo in the no-NIC heartbeat.
const MAX_SEEN: usize = 16;

// ---------------------------------------------------------------------------
// IPC helpers
// ---------------------------------------------------------------------------

/// Issue a `UsbRequest` to the xHCI server and decode the `UsbReply`.
/// Returns `None` when the IPC call fails or the reply cannot be decoded.
fn usb_call(usb_ep: u32, req: &UsbRequest) -> Option<UsbReply> {
    let req_bytes = req.encode();
    let rc = syscall_lib::ipc_call_buf(usb_ep, USB_REQ_LABEL, 0, &req_bytes);
    if rc == u64::MAX {
        return None;
    }
    let mut reply_buf = [0u8; USB_MSG_MAX];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX {
        return None;
    }
    UsbReply::decode(&reply_buf[..n as usize])
}

// ---------------------------------------------------------------------------
// OCP register access (re-expressed from ure(4) ure_read_*/ure_write_*)
// ---------------------------------------------------------------------------
//
// Every OCP access targets a **dword-aligned** register through a 4-byte data
// window (`wLength = 4`). Reads return the 4-byte window and the caller shifts
// out the requested lane; writes OR a (possibly shifted) byte-enable mask into
// `wIndex` so the chip commits only the addressed bytes. `wIndex` carries the
// MCU bank select (`URE_MCU_TYPE_PLA` / `_USB`) OR'd with that byte-enable.

/// Raw OCP IN transfer: read `len` bytes from `reg` with `index` (the MCU bank
/// select). `index` is passed through unchanged — byte-enables are write-only.
fn ocp_read_bytes(usb_ep: u32, slot_id: u8, reg: u16, index: u16, len: u16) -> Option<Vec<u8>> {
    let setup = [
        URE_BMREQTYPE_READ,
        URE_BREQUEST,
        (reg & 0xff) as u8,
        (reg >> 8) as u8,
        (index & 0xff) as u8,
        (index >> 8) as u8,
        (len & 0xff) as u8,
        (len >> 8) as u8,
    ];
    match usb_call(
        usb_ep,
        &UsbRequest::ControlRequest {
            slot_id,
            setup,
            length: len,
        },
    ) {
        Some(UsbReply::ControlData {
            data,
            completion_code: 1,
        }) => Some(data),
        _ => None,
    }
}

/// Read a 6-byte MAC from `reg` (an 8-byte PLA window) and return it only if it
/// is a plausible unicast address (not all-zero, not all-`0xff`). Used to prefer
/// the efuse-loaded `PLA_BACKUP` over the Realtek-default `PLA_IDR`.
fn read_valid_mac(usb_ep: u32, slot_id: u8, reg: u16) -> Option<[u8; 6]> {
    let data = ocp_read_bytes(usb_ep, slot_id, reg, URE_MCU_TYPE_PLA, 8)?;
    if data.len() < 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&data[..6]);
    let all_zero = mac.iter().all(|&b| b == 0x00);
    let all_ff = mac.iter().all(|&b| b == 0xFF);
    if all_zero || all_ff { None } else { Some(mac) }
}

/// Raw OCP OUT transfer: write `data` to `reg` with `index` (MCU bank select
/// OR'd with the byte-enable mask) via the `ControlWrite` IPC path.
fn ocp_write_bytes(usb_ep: u32, slot_id: u8, reg: u16, index: u16, data: &[u8]) -> bool {
    let len = data.len() as u16;
    let setup = [
        URE_BMREQTYPE_WRITE,
        URE_BREQUEST,
        (reg & 0xff) as u8,
        (reg >> 8) as u8,
        (index & 0xff) as u8,
        (index >> 8) as u8,
        (len & 0xff) as u8,
        (len >> 8) as u8,
    ];
    matches!(
        usb_call(
            usb_ep,
            &UsbRequest::ControlWrite {
                slot_id,
                setup,
                data: data.to_vec(),
            },
        ),
        Some(UsbReply::ControlData {
            completion_code: 1,
            ..
        })
    )
}

/// Read the 4-byte window at `reg & !3`.
fn ure_read_4(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16) -> Option<u32> {
    let data = ocp_read_bytes(usb_ep, slot_id, reg & !3, mcu, 4)?;
    if data.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

/// Read a 16-bit register, extracting the addressed lane from the dword window.
fn ure_read_2(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16) -> Option<u16> {
    let shift = ((reg & 2) << 3) as u32;
    let val = ure_read_4(usb_ep, slot_id, reg, mcu)?;
    Some(((val >> shift) & 0xffff) as u16)
}

/// Read an 8-bit register, extracting the addressed lane from the dword window.
fn ure_read_1(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16) -> Option<u8> {
    let shift = ((reg & 3) << 3) as u32;
    let val = ure_read_4(usb_ep, slot_id, reg, mcu)?;
    Some(((val >> shift) & 0xff) as u8)
}

/// Write a 32-bit register (all four byte lanes enabled). `reg` is dword-aligned.
fn ure_write_4(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16, val: u32) -> bool {
    ocp_write_bytes(
        usb_ep,
        slot_id,
        reg,
        mcu | URE_BYTE_EN_DWORD,
        &val.to_le_bytes(),
    )
}

/// Write a 16-bit register, shifting the value + byte-enable into the lane
/// addressed by `reg & 2` (ure(4) `ure_write_2`).
fn ure_write_2(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16, val: u16) -> bool {
    let mut byen = URE_BYTE_EN_WORD;
    let mut value = val as u32;
    let mut aligned = reg;
    if reg & 2 != 0 {
        let shift = reg & 2;
        byen <<= shift;
        value <<= shift << 3;
        aligned &= !3;
    }
    ocp_write_bytes(usb_ep, slot_id, aligned, mcu | byen, &value.to_le_bytes())
}

/// Write an 8-bit register, shifting the value + byte-enable into the lane
/// addressed by `reg & 3` (ure(4) `ure_write_1`).
fn ure_write_1(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16, val: u8) -> bool {
    let mut byen = URE_BYTE_EN_BYTE;
    let mut value = val as u32;
    let mut aligned = reg;
    if reg & 3 != 0 {
        let shift = reg & 3;
        byen <<= shift;
        value <<= shift << 3;
        aligned &= !3;
    }
    ocp_write_bytes(usb_ep, slot_id, aligned, mcu | byen, &value.to_le_bytes())
}

// ---------------------------------------------------------------------------
// Read-modify-write helpers (set/clr bit)
// ---------------------------------------------------------------------------

/// Read-modify-write: set bits in an 8-bit register.
fn ure_set_bit_1(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16, bits: u8) {
    let v = ure_read_1(usb_ep, slot_id, reg, mcu).unwrap_or(0);
    let _ = ure_write_1(usb_ep, slot_id, reg, mcu, v | bits);
}

/// Read-modify-write: clear bits in an 8-bit register.
fn ure_clr_bit_1(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16, bits: u8) {
    let v = ure_read_1(usb_ep, slot_id, reg, mcu).unwrap_or(0);
    let _ = ure_write_1(usb_ep, slot_id, reg, mcu, v & !bits);
}

/// Read-modify-write: set bits in a 16-bit register.
fn ure_set_bit_2(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16, bits: u16) {
    let v = ure_read_2(usb_ep, slot_id, reg, mcu).unwrap_or(0);
    let _ = ure_write_2(usb_ep, slot_id, reg, mcu, v | bits);
}

/// Read-modify-write: clear bits in a 16-bit register.
fn ure_clr_bit_2(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16, bits: u16) {
    let v = ure_read_2(usb_ep, slot_id, reg, mcu).unwrap_or(0);
    let _ = ure_write_2(usb_ep, slot_id, reg, mcu, v & !bits);
}

/// Read-modify-write: set bits in a 32-bit register.
#[allow(dead_code)]
fn ure_set_bit_4(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16, bits: u32) {
    let v = ure_read_4(usb_ep, slot_id, reg, mcu).unwrap_or(0);
    let _ = ure_write_4(usb_ep, slot_id, reg, mcu, v | bits);
}

/// Read-modify-write: clear bits in a 32-bit register.
fn ure_clr_bit_4(usb_ep: u32, slot_id: u8, reg: u16, mcu: u16, bits: u32) {
    let v = ure_read_4(usb_ep, slot_id, reg, mcu).unwrap_or(0);
    let _ = ure_write_4(usb_ep, slot_id, reg, mcu, v & !bits);
}

// ---------------------------------------------------------------------------
// Phase A — ure_rtl8153b_init(): attach-time power-up
// ---------------------------------------------------------------------------
//
// Re-expressed from OpenBSD ure(4) `ure_rtl8153b_init` (the RTL8153B/8156
// path). Run once at attach time to bring the chip out of deep power-save
// state and configure USB link-power management.

fn ure_rtl8153b_init(usb_ep: u32, slot_id: u8) {
    // Disable "all speed" ECM OP so we control speed explicitly.
    ure_clr_bit_1(
        usb_ep,
        slot_id,
        URE_USB_ECM_OP,
        URE_MCU_TYPE_USB,
        URE_EN_ALL_SPEED as u8,
    );

    // Clear the USB speed-option register (let firmware/hardware set speed).
    let _ = ure_write_2(usb_ep, slot_id, URE_USB_SPEED_OPTION, URE_MCU_TYPE_USB, 0);

    // Bypass the MAC reset gate so our subsequent ure_nic_reset controls it.
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_USB_ECM_OPTION,
        URE_MCU_TYPE_USB,
        URE_BYPASS_MAC_RESET,
    );

    // Disable U1/U2 link-power-management (prevents mid-transfer sleep).
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_USB_LPM_CONFIG,
        URE_MCU_TYPE_USB,
        LPM_U1U2_EN,
    );

    // Poll PLA_BOOT_CTRL until firmware autoload completes. Non-fatal if it
    // doesn't complete in time — the subsequent init will still proceed.
    let mut autoload_done = false;
    for _ in 0..50u32 {
        let boot = ure_read_2(usb_ep, slot_id, URE_PLA_BOOT_CTRL, URE_MCU_TYPE_PLA).unwrap_or(0);
        if boot & URE_AUTOLOAD_DONE != 0 {
            autoload_done = true;
            break;
        }
        // ~20 ms per iteration
        let _ = syscall_lib::nanosleep_for(0, 20_000_000);
    }
    syscall_lib::write_str(STDOUT_FILENO, "ure: autoload=");
    syscall_lib::write_str(STDOUT_FILENO, if autoload_done { "ok" } else { "timeout" });
    syscall_lib::write_str(STDOUT_FILENO, "\n");

    // Disable U2P3 (SuperSpeed P3 power state).
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_USB_U2P3_CTRL,
        URE_MCU_TYPE_USB,
        URE_U2P3_ENABLE,
    );

    // Set MSC timer to max (4095 ms) and U1/U2 entry timer to 500 µs.
    let _ = ure_write_2(usb_ep, slot_id, URE_USB_MSC_TIMER, URE_MCU_TYPE_USB, 4095);
    let _ = ure_write_2(usb_ep, slot_id, URE_USB_U1U2_TIMER, URE_MCU_TYPE_USB, 500);

    // Disable power cut.
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_USB_POWER_CUT,
        URE_MCU_TYPE_USB,
        URE_PWR_EN,
    );

    // Clear power-cut status flag.
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_USB_MISC_0,
        URE_MCU_TYPE_USB,
        URE_PCUT_STATUS,
    );

    // Clear UPS_EN and USP_PREWAKE in POWER_CUT (8-bit access for the upper
    // byte — these are bit-fields in the same register, accessed as byte).
    ure_clr_bit_1(
        usb_ep,
        slot_id,
        URE_USB_POWER_CUT,
        URE_MCU_TYPE_USB,
        (URE_UPS_EN | URE_USP_PREWAKE) as u8,
    );

    // Clear UPS_MISC_2 flags: FORCE_PWR_DOWN and NO_UPS.
    ure_clr_bit_1(
        usb_ep,
        slot_id,
        URE_USB_MISC_2,
        URE_MCU_TYPE_USB,
        URE_UPS_FORCE_PWR_DOWN | URE_UPS_NO_UPS,
    );

    // Clear PLA runtime-D3 and link-change-event flags.
    ure_clr_bit_1(
        usb_ep,
        slot_id,
        URE_PLA_INDICATE_FALG,
        URE_MCU_TYPE_PLA,
        URE_UPCOMING_RUNTIME_D3,
    );
    ure_clr_bit_1(
        usb_ep,
        slot_id,
        URE_PLA_SUSPEND_FLAG,
        URE_MCU_TYPE_PLA,
        URE_LINK_CHG_EVENT,
    );
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_EXTRA_STATUS,
        URE_MCU_TYPE_PLA,
        URE_LINK_CHANGE_FLAG,
    );

    // Unlock config registers, clear link-off wake, re-lock.
    let _ = ure_write_1(
        usb_ep,
        slot_id,
        URE_PLA_CRWECR,
        URE_MCU_TYPE_PLA,
        regs::URE_CRWECR_CONFIG,
    );
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_CONFIG34,
        URE_MCU_TYPE_PLA,
        URE_LINK_OFF_WAKE_EN,
    );
    let _ = ure_write_1(
        usb_ep,
        slot_id,
        URE_PLA_CRWECR,
        URE_MCU_TYPE_PLA,
        regs::URE_CRWECR_NORMAL,
    );

    // Clear SLOT_EN in RCR (disables slot-based acceptance during init).
    ure_clr_bit_4(usb_ep, slot_id, URE_PLA_RCR, URE_MCU_TYPE_PLA, URE_SLOT_EN);

    // Enable flow control in CPCR.
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_CPCR,
        URE_MCU_TYPE_PLA,
        URE_FLOW_CTRL_EN,
    );

    // Set FC timer: CTRL_TIMER_EN + 75 ticks.
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_USB_FC_TIMER,
        URE_MCU_TYPE_USB,
        URE_CTRL_TIMER_EN | 75,
    );

    // Disable MAC MCU speed-down.
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_MAC_PWR_CTRL3,
        URE_MCU_TYPE_PLA,
        URE_PLA_MCU_SPDWN_EN,
    );

    // Enable RX aggregation (clear the disable bits) so the chip can pipeline
    // frames into the bulk-IN endpoint. Also clear RX_ZERO_EN.
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_USB_USB_CTRL,
        URE_MCU_TYPE_USB,
        URE_RX_AGG_DISABLE | URE_RX_ZERO_EN,
    );

    // Activate output DMA.
    ure_set_bit_1(
        usb_ep,
        slot_id,
        URE_USB_BMU_CONFIG,
        URE_MCU_TYPE_USB,
        URE_ACT_ODMA as u8,
    );

    // Reset tally counters.
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_RSTTALLY,
        URE_MCU_TYPE_PLA,
        URE_TALLY_RESET,
    );
}

// ---------------------------------------------------------------------------
// Phase B — ure_nic_reset(): EP0-critical reset sequence
// ---------------------------------------------------------------------------
//
// Re-expressed from OpenBSD ure(4) `ure_rtl8153_nic_reset` (RTL8156 path).
// ORDER IS CRITICAL — the CDC_ECM toggle in ure_reset() is an EP0 hazard:
// the BMU must be pre-flushed and the sequence must not be reordered.
//
// NOT CALLED on QEMU-passthrough (drops the host-established link + wedges EP0
// without a re-enumeration); retained for the bare-metal/cold-attach path.
fn ure_nic_reset(usb_ep: u32, slot_id: u8) {
    // Disable LPM and U2P3 before touching the MAC.
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_USB_LPM_CONFIG,
        URE_MCU_TYPE_USB,
        LPM_U1U2_EN,
    );
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_USB_U2P3_CTRL,
        URE_MCU_TYPE_USB,
        URE_U2P3_ENABLE,
    );

    // Gate RX: set RXDY_GATED_EN so no frames reach the bulk-IN endpoint.
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_MISC_1,
        URE_MCU_TYPE_PLA,
        URE_RXDY_GATED_EN,
    );

    // Disable teredo / WoW timers (they would generate spurious wakeups).
    let _ = ure_write_1(usb_ep, slot_id, URE_PLA_TEREDO_CFG, URE_MCU_TYPE_PLA, 0xff);
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_PLA_WDT6_CTRL,
        URE_MCU_TYPE_PLA,
        URE_WDT6_SET_MODE,
    );
    let _ = ure_write_2(usb_ep, slot_id, URE_PLA_REALWOW_TIMER, URE_MCU_TYPE_PLA, 0);
    let _ = ure_write_4(usb_ep, slot_id, URE_PLA_TEREDO_TIMER, URE_MCU_TYPE_PLA, 0);

    // Stop accepting all frames in RCR during reset.
    ure_clr_bit_4(
        usb_ep,
        slot_id,
        URE_PLA_RCR,
        URE_MCU_TYPE_PLA,
        URE_RCR_ACPT_ALL,
    );

    // --- ure_reset(): CDC_ECM toggle — the EP0-critical sequence.
    // ORDER MUST NOT BE CHANGED. The BMU EP_IN must be toggled around the
    // CDC_ECM_EN bit to avoid a protocol-level deadlock on EP0 SETUP packets.

    // Stop TE (TX engine) first.
    ure_clr_bit_1(usb_ep, slot_id, URE_PLA_CR, URE_MCU_TYPE_PLA, URE_CR_TE);
    // De-assert BMU EP IN reset.
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_USB_BMU_RESET,
        URE_MCU_TYPE_USB,
        BMU_RESET_EP_IN as u16,
    );
    // Enable CDC-ECM mode (temporarily — gives the chip a clean endpoint state).
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_USB_USB_CTRL,
        URE_MCU_TYPE_USB,
        URE_CDC_ECM_EN,
    );
    // Stop RE (RX engine).
    ure_clr_bit_1(usb_ep, slot_id, URE_PLA_CR, URE_MCU_TYPE_PLA, URE_CR_RE);
    // Re-assert BMU EP IN reset.
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_USB_BMU_RESET,
        URE_MCU_TYPE_USB,
        BMU_RESET_EP_IN as u16,
    );
    // Disable CDC-ECM mode (return to proprietary bulk mode).
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_USB_USB_CTRL,
        URE_MCU_TYPE_USB,
        URE_CDC_ECM_EN,
    );

    // --- ure_reset_bmu(): full flush — de-assert then assert both directions.
    ure_clr_bit_1(
        usb_ep,
        slot_id,
        URE_USB_BMU_RESET,
        URE_MCU_TYPE_USB,
        BMU_RESET_EP_IN | BMU_RESET_EP_OUT,
    );
    ure_set_bit_1(
        usb_ep,
        slot_id,
        URE_USB_BMU_RESET,
        URE_MCU_TYPE_USB,
        BMU_RESET_EP_IN | BMU_RESET_EP_OUT,
    );

    // --- OOB claim: driver takes ownership of the MAC from firmware.
    // ONLY after the reset+flush sequence above.
    ure_clr_bit_1(
        usb_ep,
        slot_id,
        URE_PLA_OOB_CTRL,
        URE_MCU_TYPE_PLA,
        URE_NOW_IS_OOB,
    );
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_SFF_STS_7,
        URE_MCU_TYPE_PLA,
        URE_MCU_BORW_EN,
    );

    // 8156 SKIPS the LINK_LIST_READY poll entirely — do not poll.

    // rxvlan: clear inner/outer VLAN strip bits (simplest: write 0).
    let _ = ure_write_2(usb_ep, slot_id, URE_PLA_RCR1, URE_MCU_TYPE_PLA, 0);

    // RX max frame size and MTPS.
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_PLA_RMS,
        URE_MCU_TYPE_PLA,
        URE_RMS_DEFAULT,
    );
    let _ = ure_write_1(usb_ep, slot_id, URE_PLA_MTPS, URE_MCU_TYPE_PLA, 192);

    // RX/TX FIFO thresholds (RTL8156 values).
    // NOTE: URE_PLA_RX_FIFO_FULL is 0xC0A6 (distinct from URE_PLA_RXFIFO_FULL 0xC0A2).
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_PLA_RX_FIFO_FULL,
        URE_MCU_TYPE_PLA,
        1024,
    );
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_PLA_RX_FIFO_EMPTY,
        URE_MCU_TYPE_PLA,
        2048,
    );
    let _ = ure_write_2(usb_ep, slot_id, URE_PLA_TXFIFO_CTRL, URE_MCU_TYPE_PLA, 8);
    let _ = ure_write_2(usb_ep, slot_id, URE_PLA_TXFIFO_FULL, URE_MCU_TYPE_PLA, 128);

    // Activate output DMA and set the USB RX buffer threshold.
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_USB_BMU_CONFIG,
        URE_MCU_TYPE_USB,
        URE_ACT_ODMA,
    );
    let _ = ure_write_4(
        usb_ep,
        slot_id,
        URE_USB_RX_BUF_TH,
        URE_MCU_TYPE_USB,
        0x0060_0400,
    );

    // Re-enable U2P3.
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_USB_U2P3_CTRL,
        URE_MCU_TYPE_USB,
        URE_U2P3_ENABLE,
    );
}

// ---------------------------------------------------------------------------
// Phase C — ure_ifmedia_init(): MAC + FIFO + enable + ungate
// ---------------------------------------------------------------------------
//
// Re-expressed from OpenBSD ure(4) `ure_ifmedia_init` (RTL8153B/8156 path).
// Writes the MAC address, configures the RX early-aggregation coalescer,
// enables RX+TX engines, then clears RXDY_GATED_EN to open the datapath.

fn ure_ifmedia_init(usb_ep: u32, slot_id: u8, mac: &[u8; 6]) {
    // Unlock config registers so we can write IDR.
    let _ = ure_write_1(
        usb_ep,
        slot_id,
        URE_PLA_CRWECR,
        URE_MCU_TYPE_PLA,
        regs::URE_CRWECR_CONFIG,
    );

    // Write the 6-byte MAC to URE_PLA_IDR using a raw 8-byte OCP OUT with the
    // SIX_BYTES byte-enable (covers mac[0..5] + 2 bytes of don't-care padding).
    let mut buf = [0u8; 8];
    buf[..6].copy_from_slice(&mac[..6]);
    let _ = ocp_write_bytes(
        usb_ep,
        slot_id,
        URE_PLA_IDR,
        URE_MCU_TYPE_PLA | URE_BYTE_EN_SIX_BYTES,
        &buf,
    );

    // Re-lock config registers.
    let _ = ure_write_1(
        usb_ep,
        slot_id,
        URE_PLA_CRWECR,
        URE_MCU_TYPE_PLA,
        regs::URE_CRWECR_NORMAL,
    );

    // RX early-aggregation coalesce threshold (80 units).
    let _ = ure_write_2(usb_ep, slot_id, URE_USB_RX_EARLY_AGG, URE_MCU_TYPE_USB, 80);

    // PM control/status: set resume-indicate timeout (1875 µs).
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_USB_PM_CTRL_STATUS,
        URE_MCU_TYPE_USB,
        1875,
    );

    // Pulse FCR MCU enable in FMC: clear then set.
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_FMC,
        URE_MCU_TYPE_PLA,
        URE_FMC_FCR_MCU_EN,
    );
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_FMC,
        URE_MCU_TYPE_PLA,
        URE_FMC_FCR_MCU_EN,
    );

    // Enable RX and TX engines.
    ure_set_bit_1(
        usb_ep,
        slot_id,
        URE_PLA_CR,
        URE_MCU_TYPE_PLA,
        URE_CR_RE | URE_CR_TE,
    );

    // Open the receive datapath: clear RXDY_GATED_EN so received frames flow
    // to the bulk-IN endpoint.
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_MISC_1,
        URE_MCU_TYPE_PLA,
        URE_RXDY_GATED_EN,
    );
}

// ---------------------------------------------------------------------------
// Phase D — ure_iff(): RCR arm
// ---------------------------------------------------------------------------
//
// Set the RCR accept filter: unicast to our MAC (APM) + broadcast (AB).
// Re-expressed from OpenBSD ure(4) `ure_iff`.

fn ure_iff(usb_ep: u32, slot_id: u8) {
    // Accept unicast matching IDR (our MAC, written by ure_ifmedia_init) +
    // broadcast. No promiscuous AAP — it floods the single net task on a real
    // LAN and broke DHCP binding (see ure_init_minimal).
    let rxmode = ure_read_4(usb_ep, slot_id, URE_PLA_RCR, URE_MCU_TYPE_PLA).unwrap_or(0);
    let rxmode = (rxmode & !URE_RCR_ACPT_ALL) | URE_RCR_APM | URE_RCR_AB;
    let _ = ure_write_4(usb_ep, slot_id, URE_PLA_RCR, URE_MCU_TYPE_PLA, rxmode);
}

/// Non-destructive RX/TX FIFO + USB-buffer threshold setup (the subset of
/// `ure_nic_reset` that doesn't touch the OOB/reset/CDC_ECM path). On a
/// QEMU-passthrough'd device the host already reset + claimed the MAC, so the
/// destructive `ure_nic_reset` only tears down the working (already-linked)
/// state; these threshold writes give the chip the RX-DMA config it needs
/// without dropping the link.
#[allow(dead_code)]
fn ure_rx_fifo_setup(usb_ep: u32, slot_id: u8) {
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_PLA_RX_FIFO_FULL,
        URE_MCU_TYPE_PLA,
        1024,
    );
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_PLA_RX_FIFO_EMPTY,
        URE_MCU_TYPE_PLA,
        2048,
    );
    let _ = ure_write_2(usb_ep, slot_id, URE_PLA_TXFIFO_CTRL, URE_MCU_TYPE_PLA, 8);
    let _ = ure_write_2(usb_ep, slot_id, URE_PLA_TXFIFO_FULL, URE_MCU_TYPE_PLA, 128);
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_USB_BMU_CONFIG,
        URE_MCU_TYPE_USB,
        URE_ACT_ODMA,
    );
    let _ = ure_write_4(
        usb_ep,
        slot_id,
        URE_USB_RX_BUF_TH,
        URE_MCU_TYPE_USB,
        0x0060_0400,
    );
}

/// Minimal, **link-preserving** RX/TX enable — the active path on a
/// QEMU-passthrough'd device.
///
/// The host (Linux `r8152`) already power-managed, reset, and linked the chip
/// (`OOB_CTRL` reads `0x00`, link is up at 2.5G on arrival). The full cold-attach
/// init (`ure_rtl8153b_init` / `ure_nic_reset` / `ure_ifmedia_init` / `ure_iff`)
/// re-touches the power/USB/reset registers, which **tears down that
/// host-established USB connection** (link drops, EP0 wedges) because the
/// passthrough can't re-enumerate — so it is reserved for the bare-metal /
/// cold-attach path. This light-touch sequence only enables RX/TX + ungates the
/// RX datapath, leaving the working link intact. Returns whether `RE|TE` latched.
fn ure_init_minimal(usb_ep: u32, slot_id: u8, mac: &[u8; 6]) -> bool {
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_PLA_RMS,
        URE_MCU_TYPE_PLA,
        URE_RMS_DEFAULT,
    );

    // Program the unicast RX filter (IDR) with the MAC we read and advertise.
    //
    // The minimal path previously set `RCR=APM` (accept unicast matching IDR)
    // but never wrote IDR, so on a cold bare-metal device the hardware filter
    // did not match the MAC we put in our Ethernet src / ARP / DHCP — the host's
    // unicast ICMP/TCP to us was silently dropped at the dongle (only broadcast
    // ARP got through). Writing IDR = our MAC here aligns the hardware filter
    // with the address we advertise, so unicast reaches us — without the
    // promiscuous `AAP` flood that drowned the single net task on a real LAN and
    // broke DHCP binding. (Same unlock/write/relock as `ure_ifmedia_init`.)
    let _ = ure_write_1(
        usb_ep,
        slot_id,
        URE_PLA_CRWECR,
        URE_MCU_TYPE_PLA,
        regs::URE_CRWECR_CONFIG,
    );
    let mut idr = [0u8; 8];
    idr[..6].copy_from_slice(&mac[..6]);
    let _ = ocp_write_bytes(
        usb_ep,
        slot_id,
        URE_PLA_IDR,
        URE_MCU_TYPE_PLA | URE_BYTE_EN_SIX_BYTES,
        &idr,
    );
    let _ = ure_write_1(
        usb_ep,
        slot_id,
        URE_PLA_CRWECR,
        URE_MCU_TYPE_PLA,
        regs::URE_CRWECR_NORMAL,
    );

    // RX accept: unicast matching IDR (our MAC) + broadcast. No promiscuous AAP.
    let _ = ure_write_4(
        usb_ep,
        slot_id,
        URE_PLA_RCR,
        URE_MCU_TYPE_PLA,
        URE_RCR_APM | URE_RCR_AB,
    );
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_PLA_RX_FIFO_FULL,
        URE_MCU_TYPE_PLA,
        1024,
    );
    let _ = ure_write_2(
        usb_ep,
        slot_id,
        URE_PLA_RX_FIFO_EMPTY,
        URE_MCU_TYPE_PLA,
        2048,
    );
    let _ = ure_write_4(
        usb_ep,
        slot_id,
        URE_USB_RX_BUF_TH,
        URE_MCU_TYPE_USB,
        0x0060_0400,
    );
    // Flush each frame immediately (no coalescing).
    ure_set_bit_2(
        usb_ep,
        slot_id,
        URE_USB_USB_CTRL,
        URE_MCU_TYPE_USB,
        URE_RX_AGG_DISABLE,
    );
    // Enable RX + TX, then ungate the RX datapath.
    ure_set_bit_1(
        usb_ep,
        slot_id,
        URE_PLA_CR,
        URE_MCU_TYPE_PLA,
        URE_CR_RE | URE_CR_TE,
    );
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_MISC_1,
        URE_MCU_TYPE_PLA,
        URE_RXDY_GATED_EN,
    );
    let cr = ure_read_1(usb_ep, slot_id, URE_PLA_CR, URE_MCU_TYPE_PLA).unwrap_or(0);
    cr & (URE_CR_RE | URE_CR_TE) == (URE_CR_RE | URE_CR_TE)
}

/// Re-assert the RX datapath to recover an RTL8156 RX stall.
///
/// Under TX load the chip stops feeding the bulk-IN endpoint — the armed TRB
/// never completes and there is no USB error event (so the xHCI driver cannot
/// detect or reset anything), which wedges all inbound traffic (ping included).
/// The recovery is to re-enable the RX engine (`PLA_CR.RE`) and re-open the
/// RX-ready gate (`PLA_MISC_1.RXDY_GATED_EN`), exactly the two writes that end
/// `ure_init_minimal`. A no-op when RX is healthy, so it is safe to call from
/// the io-loop watchdog whenever inbound traffic stalls.
pub(crate) fn ure_kick_rx(usb_ep: u32, slot_id: u8) {
    ure_set_bit_1(usb_ep, slot_id, URE_PLA_CR, URE_MCU_TYPE_PLA, URE_CR_RE);
    ure_clr_bit_2(
        usb_ep,
        slot_id,
        URE_PLA_MISC_1,
        URE_MCU_TYPE_PLA,
        URE_RXDY_GATED_EN,
    );
}

// ---------------------------------------------------------------------------
// Legacy helpers kept for link-status polling
// ---------------------------------------------------------------------------

/// Read `PLA_PHYSTATUS` and return `(link_up, speed_label)`.
fn ure_link_status(usb_ep: u32, slot_id: u8) -> Option<(bool, &'static str)> {
    let phys = ure_read_2(usb_ep, slot_id, URE_PLA_PHYSTATUS, URE_MCU_TYPE_PLA)?;
    let up = phys & URE_PHYSTATUS_LINK != 0;
    let speed = if phys & URE_PHYSTATUS_2500MBPS != 0 {
        "2500M"
    } else if phys & URE_PHYSTATUS_1000MBPS != 0 {
        "1000M"
    } else if phys & URE_PHYSTATUS_100MBPS != 0 {
        "100M"
    } else if phys & URE_PHYSTATUS_10MBPS != 0 {
        "10M"
    } else {
        "?"
    };
    Some((up, speed))
}

// ---------------------------------------------------------------------------
// Helpers kept but no longer used in the main path — suppress dead-code lint
// ---------------------------------------------------------------------------

/// Poll `PLA_OOB_CTRL` until `LINK_LIST_READY` is set. Not used for 8156
/// (the 8156 NIC reset skips this poll), kept for reference.
#[allow(dead_code)]
fn poll_link_list_ready(usb_ep: u32, slot_id: u8) -> bool {
    for _ in 0..URE_TIMEOUT {
        let oob = ure_read_1(usb_ep, slot_id, URE_PLA_OOB_CTRL, URE_MCU_TYPE_PLA).unwrap_or(0);
        if oob & URE_LINK_LIST_READY != 0 {
            return true;
        }
        let _ = syscall_lib::nanosleep_for(0, 1_000_000);
    }
    false
}

// ---------------------------------------------------------------------------
// Formatters (no std)
// ---------------------------------------------------------------------------

/// Write a 6-byte MAC address as `aa:bb:cc:dd:ee:ff\n` (lowercase hex).
fn write_mac(mac: &[u8; 6]) {
    for (i, &byte) in mac.iter().enumerate() {
        write_u8_hex(byte);
        if i < 5 {
            syscall_lib::write_str(STDOUT_FILENO, ":");
        }
    }
    syscall_lib::write_str(STDOUT_FILENO, "\n");
}

/// Write a `u8` as exactly two lowercase hex digits to stdout.
fn write_u8_hex(n: u8) {
    let hi = (n >> 4) & 0xF;
    let lo = n & 0xF;
    let mut buf = [0u8; 2];
    buf[0] = if hi < 10 { b'0' + hi } else { b'a' + hi - 10 };
    buf[1] = if lo < 10 { b'0' + lo } else { b'a' + lo - 10 };
    // SAFETY: buf contains only ASCII hex digits.
    let s = unsafe { core::str::from_utf8_unchecked(&buf) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}

/// Write a `u16` as a fixed 4-digit lowercase-hex string.
fn write_u16_hex(n: u16) {
    write_u8_hex((n >> 8) as u8);
    write_u8_hex((n & 0xFF) as u8);
}

/// Write `n` as decimal digits (used for small slot IDs).
fn write_u8_dec(mut n: u8) {
    let mut buf = [0u8; 3];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10);
        n /= 10;
        if n == 0 {
            break;
        }
    }
    // SAFETY: buf[i..] contains only ASCII digits.
    let s = unsafe { core::str::from_utf8_unchecked(&buf[i..]) };
    syscall_lib::write_str(STDOUT_FILENO, s);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "ure: spawned\n");

    // 1. Wait for the xHCI driver to register the `usb` service.
    if !syscall_lib::ipc_wait_service(USB_SERVICE_NAME, 10_000) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "ure: 'usb' service never appeared — idling\n",
        );
        idle_loop();
    }
    let usb_ep = {
        let h = syscall_lib::ipc_lookup_service(USB_SERVICE_NAME);
        if h == u64::MAX {
            syscall_lib::write_str(STDOUT_FILENO, "ure: 'usb' lookup failed — idling\n");
            idle_loop();
        }
        h as u32
    };

    // 2. Walk the NextAttach cursor looking for VID=0x0bda PID=0x8156.
    let mut slot_id: Option<u8> = None;
    let mut interface_class: u8 = 0;
    let mut bulk_in_dci = 0u8;
    let mut bulk_out_dci = 0u8;
    let mut cursor = 0u8;
    // Record every surfaced device so the heartbeat can replay the full list at
    // the bottom of the scroll — on bare metal the single-shot "saw" lines below
    // scroll off behind other devices' descriptor dumps before they can be read.
    let mut seen: [(u16, u16, u8); MAX_SEEN] = [(0, 0, 0); MAX_SEEN];
    let mut seen_count = 0usize;
    while let Some(UsbReply::Attach {
        notice: Some(notice),
    }) = usb_call(usb_ep, &UsbRequest::NextAttach { cursor })
    {
        // Diagnostic: name every surfaced device so a bare-metal log shows the
        // dongle's real VID/PID/class even when it isn't claimed below.
        syscall_lib::write_str(STDOUT_FILENO, "ure: saw vid=0x");
        write_u16_hex(notice.vendor_id);
        syscall_lib::write_str(STDOUT_FILENO, " pid=0x");
        write_u16_hex(notice.product_id);
        syscall_lib::write_str(STDOUT_FILENO, " class=0x");
        write_u8_hex(notice.interface_class);
        syscall_lib::write_str(STDOUT_FILENO, "\n");
        if seen_count < MAX_SEEN {
            seen[seen_count] = (notice.vendor_id, notice.product_id, notice.interface_class);
            seen_count += 1;
        }

        if notice.vendor_id == VENDOR_REALTEK
            && SUPPORTED_REALTEK_NIC_PIDS.contains(&notice.product_id)
        {
            slot_id = Some(notice.slot_id);
            interface_class = notice.interface_class;
            bulk_in_dci = notice.bulk_in_dci;
            bulk_out_dci = notice.bulk_out_dci;
            break;
        }
        cursor = cursor.saturating_add(1);
    }

    let slot_id = match slot_id {
        Some(s) => s,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "ure: no RTL8156 found\n");
            // Heartbeat the full surfaced-device list + live host topology
            // forever so it stays at the bottom of the bare-metal scroll for
            // photographing.
            heartbeat_no_nic(usb_ep, &seen[..seen_count]);
        }
    };

    // Announce the claim.
    syscall_lib::write_str(STDOUT_FILENO, "ure: claimed 0bda:8156 slot=");
    write_u8_dec(slot_id);
    syscall_lib::write_str(STDOUT_FILENO, " class=");
    write_u8_hex(interface_class);
    syscall_lib::write_str(STDOUT_FILENO, "\n");

    // 3. Read the **factory** MAC from PLA_BACKUP (efuse-loaded at power-up),
    //    falling back to PLA_IDR. On a cold device IDR holds a Realtek-default
    //    placeholder (`00:e0:4c:…`), not the dongle's real address — reading it
    //    presents the wrong MAC to DHCP so the router never matches a
    //    MAC-keyed reservation. The real (e.g. Dell-OUI) address lives in
    //    BACKUP, exactly as Linux r8152 / OpenBSD ure(4) read it before copying
    //    it into IDR (the IDR write happens in the init paths below).
    let mut mac = [0u8; 6];
    let mac_ok = match read_valid_mac(usb_ep, slot_id, URE_PLA_BACKUP)
        .or_else(|| read_valid_mac(usb_ep, slot_id, URE_PLA_IDR))
    {
        Some(m) => {
            mac = m;
            syscall_lib::write_str(STDOUT_FILENO, "ure: MAC ");
            write_mac(&mac);
            true
        }
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "ure: MAC read FAILED\n");
            false
        }
    };

    // 4. Stage-1a sentinel on a valid MAC read (control IN proven).
    if mac_ok {
        syscall_lib::write_str(STDOUT_FILENO, "URE_STAGE1A:OK\n");
    }

    // 5. Stage-1b: init — auto-select cold-attach vs light-touch by the device's
    //    OOB ownership state.
    //
    // `PLA_OOB_CTRL` bit `NOW_IS_OOB` tells us who owns the MAC:
    //   - SET   → the chip is **cold** (firmware/OOB still owns it); nothing has
    //             initialized it. This is the **bare-metal / cold-attach** case,
    //             so run the FULL faithful vendor init (ure_rtl8153b_init power-up
    //             + ure_nic_reset reset+OOB-claim + ure_ifmedia_init + ure_iff),
    //             which fully enables the RX datapath.
    //   - CLEAR → a host driver already reset + claimed + linked the device. This
    //             is the **QEMU usb-host passthrough** case: re-running the vendor
    //             reset here tears down the host-established USB connection (link
    //             drops, EP0 wedges — the passthrough can't re-enumerate), so use
    //             the light-touch ure_init_minimal that preserves the link.
    //
    // Same image therefore does the right thing on QEMU (minimal) and on bare
    // metal (full). See the Phase 96 task doc's R4 datapath findings.
    let oob = ure_read_1(usb_ep, slot_id, URE_PLA_OOB_CTRL, URE_MCU_TYPE_PLA).unwrap_or(0);
    let cold_attach = oob & URE_NOW_IS_OOB != 0;
    let enabled = if cold_attach {
        syscall_lib::write_str(STDOUT_FILENO, "ure: cold device — full vendor init\n");
        ure_rtl8153b_init(usb_ep, slot_id);
        ure_nic_reset(usb_ep, slot_id);
        ure_ifmedia_init(usb_ep, slot_id, &mac);
        ure_iff(usb_ep, slot_id);
        let cr = ure_read_1(usb_ep, slot_id, URE_PLA_CR, URE_MCU_TYPE_PLA).unwrap_or(0);
        cr & (URE_CR_RE | URE_CR_TE) == (URE_CR_RE | URE_CR_TE)
    } else {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "ure: pre-initialized device — minimal init\n",
        );
        ure_init_minimal(usb_ep, slot_id, &mac)
    };

    // Readback PLA_CR (logged for the smoke gate).
    let cr = ure_read_1(usb_ep, slot_id, URE_PLA_CR, URE_MCU_TYPE_PLA).unwrap_or(0);
    syscall_lib::write_str(STDOUT_FILENO, "ure: PLA_CR=0x");
    write_u8_hex(cr);
    syscall_lib::write_str(STDOUT_FILENO, "\n");

    // Poll link a handful of times — auto-negotiation needs a moment.
    let mut link_label = ("link down", "");
    for _ in 0..10 {
        if let Some((up, speed)) = ure_link_status(usb_ep, slot_id)
            && up
        {
            link_label = ("link up ", speed);
            break;
        }
        let _ = syscall_lib::nanosleep_for(0, 200_000_000);
    }
    syscall_lib::write_str(STDOUT_FILENO, "ure: ");
    syscall_lib::write_str(STDOUT_FILENO, link_label.0);
    syscall_lib::write_str(STDOUT_FILENO, link_label.1);
    syscall_lib::write_str(STDOUT_FILENO, "\n");

    // Stage-1b sentinel: RE|TE latched (control-OUT correctness proven).
    if enabled {
        syscall_lib::write_str(STDOUT_FILENO, "URE_STAGE1B:OK\n");
    } else {
        syscall_lib::write_str(STDOUT_FILENO, "ure: PLA_CR RE|TE did not latch\n");
    }

    // 6. Stage 2: register as a `RemoteNic` and serve bulk RX/TX. Needs valid
    //    bulk endpoints + a plausible MAC; otherwise fall back to idling so the
    //    earlier stages' diagnostics remain visible.
    if mac_ok && bulk_in_dci != 0 && bulk_out_dci != 0 {
        net::run_io_loop(usb_ep, slot_id, bulk_in_dci, bulk_out_dci, mac);
    } else {
        syscall_lib::write_str(STDOUT_FILENO, "ure: no bulk endpoints — idling\n");
        idle_loop();
    }
}

/// Enter a permanent idle sleep loop. Diverges — the daemon must never exit.
fn idle_loop() -> ! {
    loop {
        let _ = syscall_lib::nanosleep_for(1, 0);
    }
}

/// Heartbeat the surfaced-device list forever when no Realtek NIC was found.
///
/// On bare metal there is no serial console — the user photographs the
/// framebuffer, and other devices' descriptor dumps scroll the single-shot
/// `ure: saw …` lines off the top. Reprinting the full list every few seconds
/// keeps it pinned at the bottom of the scroll so it can always be read,
/// answering the only question that matters: did the dongle enumerate at all,
/// and on which interface class? Diverges — the daemon must never exit.
fn heartbeat_no_nic(usb_ep: u32, seen: &[(u16, u16, u8)]) -> ! {
    loop {
        syscall_lib::write_str(STDOUT_FILENO, "ure: HB no-nic seen=");
        write_u8_dec(seen.len() as u8);
        for &(vid, pid, class) in seen {
            syscall_lib::write_str(STDOUT_FILENO, " [");
            write_u16_hex(vid);
            syscall_lib::write_str(STDOUT_FILENO, ":");
            write_u16_hex(pid);
            syscall_lib::write_str(STDOUT_FILENO, " c");
            write_u8_hex(class);
            syscall_lib::write_str(STDOUT_FILENO, "]");
        }
        syscall_lib::write_str(STDOUT_FILENO, "\n");

        // Live host topology: which controller each connected device sits on and
        // what speed it trained to. `disc` = controllers found via PCI; `up` =
        // brought-up count (a gap means one failed bring-up). Each `[cN pM SS]`
        // is a connected root-hub port. A SuperSpeed dongle that never surfaces
        // as a `seen` device but *does* appear here as a connected port pins the
        // failure to enumeration; its total absence pins it to the port not
        // detecting a connection (cable/adapter/power) or the controller.
        if let Some(UsbReply::Topology {
            discovered,
            port_counts,
            ports,
        }) = usb_call(usb_ep, &UsbRequest::Topology)
        {
            syscall_lib::write_str(STDOUT_FILENO, "ure: HB topo disc=");
            write_u8_dec(discovered);
            syscall_lib::write_str(STDOUT_FILENO, " up=");
            write_u8_dec(port_counts.len() as u8);
            for (idx, &count) in port_counts.iter().enumerate() {
                syscall_lib::write_str(STDOUT_FILENO, " c");
                write_u8_dec(idx as u8);
                syscall_lib::write_str(STDOUT_FILENO, "n=");
                write_u8_dec(count);
            }
            for p in &ports {
                syscall_lib::write_str(STDOUT_FILENO, " [c");
                write_u8_dec(p.ctrl);
                syscall_lib::write_str(STDOUT_FILENO, "p");
                write_u8_dec(p.port);
                syscall_lib::write_str(STDOUT_FILENO, " ");
                syscall_lib::write_str(STDOUT_FILENO, speed_label(p.speed_psi()));
                if !p.ped() {
                    syscall_lib::write_str(STDOUT_FILENO, "!ped");
                }
                syscall_lib::write_str(STDOUT_FILENO, "]");
            }
            syscall_lib::write_str(STDOUT_FILENO, "\n");
        }

        let _ = syscall_lib::nanosleep_for(3, 0);
    }
}

/// Short label for an xHCI default Protocol Speed ID (xHCI §7.2.1 Table 7-12).
fn speed_label(psi: u8) -> &'static str {
    match psi {
        1 => "fs",
        2 => "ls",
        3 => "hs",
        4 => "ss",
        0 => "--",
        _ => "s?",
    }
}
