//! Phase 102 Track A — Intel LPSS **DesignWare I2C** master, pure logic.
//!
//! The Synopsys DesignWare I2C IP block that Intel LPSS exposes (OpenBSD
//! `dwiic(4)` — `sys/dev/acpi/dwiic.c` — re-expressed in Rust, cross-checked
//! against Linux `i2c-designware-*`). This module is the **host-testable** half:
//! the register offsets + bit layouts, the master-transfer command-FIFO
//! **planner** (turn a "write `addr` bytes, repeated-START, read `n` bytes"
//! request into the ordered `DW_IC_DATA_CMD` words), and the `TX_ABRT` abort
//! decode. No MMIO and no `unsafe` — the ring-3 daemon supplies the actual
//! register reads/writes through `driver_runtime::Mmio`, so this state machine
//! is exercised entirely on the host. QEMU models none of this hardware, so
//! this pure logic is the falsifiable CI surface (the live datapath is
//! Dell-validated per `docs/appendix/bare-metal-validation.md`).

use alloc::vec::Vec;

// ─── Register offsets (bytes from the controller MMIO base) ──────────────────

/// I2C control (master mode / speed / restart-enable / 7-bit addressing).
pub const DW_IC_CON: usize = 0x00;
/// Target (slave) address for master transfers — 7-bit in bits 0..=6.
pub const DW_IC_TAR: usize = 0x04;
/// Command/data FIFO: writes push a `DW_IC_DATA_CMD` word (see the CMD bits);
/// reads pop one received byte in bits 0..=7.
pub const DW_IC_DATA_CMD: usize = 0x10;
/// Standard-mode SCL high/low clock counts.
pub const DW_IC_SS_SCL_HCNT: usize = 0x14;
pub const DW_IC_SS_SCL_LCNT: usize = 0x18;
/// Fast-mode SCL high/low clock counts.
pub const DW_IC_FS_SCL_HCNT: usize = 0x1c;
pub const DW_IC_FS_SCL_LCNT: usize = 0x20;
/// Masked interrupt status (INTR_STAT), interrupt mask (INTR_MASK), and the
/// unmasked raw interrupt status (RAW_INTR_STAT — polled for STOP_DET/TX_ABRT).
pub const DW_IC_INTR_STAT: usize = 0x2c;
pub const DW_IC_INTR_MASK: usize = 0x30;
pub const DW_IC_RAW_INTR_STAT: usize = 0x34;
/// RX/TX FIFO threshold levels.
pub const DW_IC_RX_TL: usize = 0x38;
pub const DW_IC_TX_TL: usize = 0x3c;
/// Clear-all-interrupts (read to clear); clear TX_ABRT specifically.
pub const DW_IC_CLR_INTR: usize = 0x40;
pub const DW_IC_CLR_TX_ABRT: usize = 0x54;
pub const DW_IC_CLR_STOP_DET: usize = 0x60;
/// Controller enable (bit 0) / abort (bit 1).
pub const DW_IC_ENABLE: usize = 0x6c;
/// Controller status (FIFO-full/empty, master activity).
pub const DW_IC_STATUS: usize = 0x70;
/// Current TX / RX FIFO occupancy (drain reads against `DW_IC_RXFLR`).
pub const DW_IC_TXFLR: usize = 0x74;
pub const DW_IC_RXFLR: usize = 0x78;
/// SDA hold time.
pub const DW_IC_SDA_HOLD: usize = 0x7c;
/// Abort source: the reason bitmap latched when a transfer TX_ABRTs.
pub const DW_IC_TX_ABRT_SOURCE: usize = 0x80;
/// Enable status (bit 0 = IC_EN — the controller has actually enabled).
pub const DW_IC_ENABLE_STATUS: usize = 0x9c;
/// Component parameter 1 — FIFO depths etc. (probe on bring-up).
pub const DW_IC_COMP_PARAM_1: usize = 0xf4;

// ─── DW_IC_CON bits ──────────────────────────────────────────────────────────

pub const IC_CON_MASTER_MODE: u32 = 1 << 0;
/// Speed field (bits 1..=2): 1 = standard (100 kHz), 2 = fast (400 kHz).
pub const IC_CON_SPEED_STD: u32 = 1 << 1;
pub const IC_CON_SPEED_FAST: u32 = 2 << 1;
/// 0 = 7-bit master addressing (what a touchpad uses); set = 10-bit.
pub const IC_CON_10BITADDR_MASTER: u32 = 1 << 4;
/// Allow the repeated-START a combined write-then-read needs.
pub const IC_CON_RESTART_EN: u32 = 1 << 5;
/// Disable the (unused) slave engine.
pub const IC_CON_SLAVE_DISABLE: u32 = 1 << 6;

/// Bus speed selector for [`compose_con`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    /// 100 kHz standard mode.
    Standard,
    /// 400 kHz fast mode — the touchpad default.
    Fast,
}

/// Compose a `DW_IC_CON` value for a master transfer: master mode, the chosen
/// speed, restart-enable (required for combined write-read), the slave engine
/// disabled, and 7-bit addressing (10-bit only when `ten_bit`).
#[inline]
pub fn compose_con(speed: Speed, ten_bit: bool) -> u32 {
    let mut v = IC_CON_MASTER_MODE | IC_CON_RESTART_EN | IC_CON_SLAVE_DISABLE;
    v |= match speed {
        Speed::Standard => IC_CON_SPEED_STD,
        Speed::Fast => IC_CON_SPEED_FAST,
    };
    if ten_bit {
        v |= IC_CON_10BITADDR_MASTER;
    }
    v
}

// ─── DW_IC_DATA_CMD bits ─────────────────────────────────────────────────────

/// Data byte mask (write path) / received byte (read path).
pub const DATA_CMD_DAT_MASK: u16 = 0x00ff;
/// CMD: 1 = read this slot, 0 = write the data byte.
pub const DATA_CMD_READ: u16 = 1 << 8;
/// Issue an I2C STOP after this command.
pub const DATA_CMD_STOP: u16 = 1 << 9;
/// Issue a repeated-START before this command (turns the bus direction around).
pub const DATA_CMD_RESTART: u16 = 1 << 10;

// ─── DW_IC_RAW_INTR_STAT / INTR_STAT bits ────────────────────────────────────

pub const INTR_RX_UNDER: u32 = 1 << 0;
pub const INTR_RX_OVER: u32 = 1 << 1;
pub const INTR_RX_FULL: u32 = 1 << 2;
pub const INTR_TX_OVER: u32 = 1 << 3;
pub const INTR_TX_EMPTY: u32 = 1 << 4;
pub const INTR_RD_REQ: u32 = 1 << 5;
pub const INTR_TX_ABRT: u32 = 1 << 6;
pub const INTR_RX_DONE: u32 = 1 << 7;
pub const INTR_ACTIVITY: u32 = 1 << 8;
pub const INTR_STOP_DET: u32 = 1 << 9;
pub const INTR_START_DET: u32 = 1 << 10;
pub const INTR_GEN_CALL: u32 = 1 << 11;

// ─── DW_IC_ENABLE bits ───────────────────────────────────────────────────────

pub const IC_ENABLE_ENABLE: u32 = 1 << 0;
pub const IC_ENABLE_ABORT: u32 = 1 << 1;

// ─── DW_IC_TX_ABRT_SOURCE bits (the subset bring-up needs) ───────────────────

/// The addressed 7-bit slave did not ACK its address — no device there (the
/// classic wrong-address / device-asleep bring-up failure).
pub const ABRT_7B_ADDR_NOACK: u32 = 1 << 0;
pub const ABRT_10ADDR1_NOACK: u32 = 1 << 1;
pub const ABRT_10ADDR2_NOACK: u32 = 1 << 2;
/// A transmitted data byte was not ACK'd by the slave.
pub const ABRT_TXDATA_NOACK: u32 = 1 << 3;
pub const ABRT_GCALL_NOACK: u32 = 1 << 4;
/// The master engine was disabled mid-transfer.
pub const ABRT_MASTER_DIS: u32 = 1 << 11;
/// Bus arbitration was lost.
pub const ABRT_ARB_LOST: u32 = 1 << 12;

/// A decoded `DW_IC_TX_ABRT_SOURCE` — a typed reason instead of a raw bitmap
/// (and instead of a silent hang). Ordered by bring-up usefulness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortReason {
    /// No abort (`DW_IC_TX_ABRT_SOURCE == 0`).
    None,
    /// Slave address not ACK'd — nothing at that I2C address.
    AddressNack,
    /// A data byte was not ACK'd.
    DataNack,
    /// Arbitration lost on the bus.
    ArbitrationLost,
    /// The master engine was disabled mid-transfer.
    MasterDisabled,
    /// Some other abort source(s) — the raw bitmap is carried for logging.
    Other(u32),
}

/// Decode a `DW_IC_TX_ABRT_SOURCE` bitmap to a typed [`AbortReason`]. The two
/// address/data NACK causes are surfaced first because they are the common
/// bring-up failures (wrong slave address, asleep device); anything else is
/// carried as `Other(bits)` for the log.
#[inline]
pub fn decode_abort(tx_abrt_source: u32) -> AbortReason {
    if tx_abrt_source == 0 {
        AbortReason::None
    } else if tx_abrt_source & ABRT_7B_ADDR_NOACK != 0 {
        AbortReason::AddressNack
    } else if tx_abrt_source & ABRT_TXDATA_NOACK != 0 {
        AbortReason::DataNack
    } else if tx_abrt_source & ABRT_ARB_LOST != 0 {
        AbortReason::ArbitrationLost
    } else if tx_abrt_source & ABRT_MASTER_DIS != 0 {
        AbortReason::MasterDisabled
    } else {
        AbortReason::Other(tx_abrt_source)
    }
}

// ─── Master-transfer command-FIFO planner ────────────────────────────────────

/// Where a polled transfer stands, decoded from a `DW_IC_RAW_INTR_STAT`
/// snapshot (`TX_ABRT` beats `STOP_DET` — an abort is terminal even if a STOP
/// was also latched).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    /// Neither STOP_DET nor TX_ABRT yet — keep draining RX / polling.
    InProgress,
    /// STOP_DET latched with no abort — the transfer completed.
    Complete,
    /// TX_ABRT latched — read `DW_IC_TX_ABRT_SOURCE` + [`decode_abort`].
    Aborted,
}

/// Classify a polled `DW_IC_RAW_INTR_STAT` snapshot.
#[inline]
pub fn transfer_status(raw_intr_stat: u32) -> TransferStatus {
    if raw_intr_stat & INTR_TX_ABRT != 0 {
        TransferStatus::Aborted
    } else if raw_intr_stat & INTR_STOP_DET != 0 {
        TransferStatus::Complete
    } else {
        TransferStatus::InProgress
    }
}

/// Plan the ordered `DW_IC_DATA_CMD` words for a combined **write-then-read**
/// I2C transaction — the HID-over-I2C register-read shape: write the (little-
/// endian) register-address bytes, issue a repeated-START, then read `read_len`
/// length-prefixed report bytes.
///
/// - Each `write` byte becomes a plain write word (CMD bit clear). If there is
///   **no** read phase (`read_len == 0`), the last write word carries `STOP`.
/// - Each read slot is a `READ` word (the data byte is ignored on reads). The
///   **first** read carries `RESTART` *iff* there were preceding writes (to turn
///   the bus around); the **last** read carries `STOP`.
///
/// The number of `READ` words equals the expected received-byte count
/// (`read_len`) — the daemon drains exactly that many bytes against
/// `DW_IC_RXFLR`. A read-only transfer (`write` empty) issues no `RESTART` (the
/// controller emits a fresh START on enable). Returns an empty plan for an empty
/// transfer.
pub fn plan_transfer(write: &[u8], read_len: usize) -> Vec<u16> {
    let mut cmds = Vec::with_capacity(write.len() + read_len);
    let n_writes = write.len();
    for (i, &b) in write.iter().enumerate() {
        let mut w = (b as u16) & DATA_CMD_DAT_MASK;
        if read_len == 0 && i + 1 == n_writes {
            w |= DATA_CMD_STOP;
        }
        cmds.push(w);
    }
    for j in 0..read_len {
        let mut w = DATA_CMD_READ;
        if j == 0 && n_writes > 0 {
            w |= DATA_CMD_RESTART;
        }
        if j + 1 == read_len {
            w |= DATA_CMD_STOP;
        }
        cmds.push(w);
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn con_is_master_fast_restart_7bit() {
        let con = compose_con(Speed::Fast, false);
        assert!(con & IC_CON_MASTER_MODE != 0, "master mode");
        assert_eq!(con & (3 << 1), IC_CON_SPEED_FAST, "fast speed field");
        assert!(
            con & IC_CON_RESTART_EN != 0,
            "restart enabled (combined xfer)"
        );
        assert!(con & IC_CON_SLAVE_DISABLE != 0, "slave engine disabled");
        assert_eq!(con & IC_CON_10BITADDR_MASTER, 0, "7-bit addressing");

        let std10 = compose_con(Speed::Standard, true);
        assert_eq!(std10 & (3 << 1), IC_CON_SPEED_STD, "standard speed field");
        assert!(std10 & IC_CON_10BITADDR_MASTER != 0, "10-bit addressing");
    }

    #[test]
    fn write_read_plan_has_restart_on_first_read_stop_on_last() {
        // The HID-over-I2C descriptor-register read: write a 2-byte register
        // address, RESTART, read 4 bytes.
        let plan = plan_transfer(&[0x20, 0x00], 4);
        assert_eq!(plan.len(), 6, "2 writes + 4 reads");
        // Writes: plain data, no CMD/STOP/RESTART.
        assert_eq!(plan[0], 0x0020);
        assert_eq!(plan[1], 0x0000);
        // First read: READ | RESTART, no STOP.
        assert_eq!(plan[2], DATA_CMD_READ | DATA_CMD_RESTART);
        // Middle reads: READ only.
        assert_eq!(plan[3], DATA_CMD_READ);
        assert_eq!(plan[4], DATA_CMD_READ);
        // Last read: READ | STOP.
        assert_eq!(plan[5], DATA_CMD_READ | DATA_CMD_STOP);
        // No write word carries STOP when a read phase follows.
        assert_eq!(plan[0] & DATA_CMD_STOP, 0);
        assert_eq!(plan[1] & DATA_CMD_STOP, 0);
    }

    #[test]
    fn write_only_plan_stops_on_last_write() {
        // A command write (RESET/SET_POWER): write bytes, STOP, no reads.
        let plan = plan_transfer(&[0x22, 0x00, 0x00, 0x01], 0);
        assert_eq!(plan.len(), 4);
        assert_eq!(plan[0], 0x0022);
        assert_eq!(plan[1], 0x0000);
        assert_eq!(plan[2], 0x0000);
        assert_eq!(plan[3], 0x0001 | DATA_CMD_STOP, "STOP on the final write");
        assert!(plan[..3].iter().all(|w| w & DATA_CMD_STOP == 0));
    }

    #[test]
    fn read_only_plan_has_no_restart() {
        // An input-report read with the register pre-selected: read 8 bytes,
        // no preceding write ⇒ no RESTART on the first read.
        let plan = plan_transfer(&[], 8);
        assert_eq!(plan.len(), 8);
        assert_eq!(
            plan[0], DATA_CMD_READ,
            "no RESTART without preceding writes"
        );
        assert!(plan[1..7].iter().all(|&w| w == DATA_CMD_READ));
        assert_eq!(plan[7], DATA_CMD_READ | DATA_CMD_STOP);
    }

    #[test]
    fn empty_transfer_is_empty_plan() {
        assert!(plan_transfer(&[], 0).is_empty());
    }

    #[test]
    fn single_write_single_read() {
        let plan = plan_transfer(&[0xAA], 1);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], 0x00AA);
        // The single read is both first (RESTART, preceded by a write) and last
        // (STOP).
        assert_eq!(plan[1], DATA_CMD_READ | DATA_CMD_RESTART | DATA_CMD_STOP);
    }

    #[test]
    fn abort_decode_prioritizes_address_then_data_nack() {
        assert_eq!(decode_abort(0), AbortReason::None);
        assert_eq!(decode_abort(ABRT_7B_ADDR_NOACK), AbortReason::AddressNack);
        assert_eq!(decode_abort(ABRT_TXDATA_NOACK), AbortReason::DataNack);
        // Address NACK wins over a co-latched data NACK (address failed first).
        assert_eq!(
            decode_abort(ABRT_7B_ADDR_NOACK | ABRT_TXDATA_NOACK),
            AbortReason::AddressNack
        );
        assert_eq!(decode_abort(ABRT_ARB_LOST), AbortReason::ArbitrationLost);
        assert_eq!(decode_abort(ABRT_MASTER_DIS), AbortReason::MasterDisabled);
        // An unmodelled source is carried verbatim.
        assert_eq!(decode_abort(1 << 20), AbortReason::Other(1 << 20));
    }

    #[test]
    fn transfer_status_abort_beats_stop() {
        assert_eq!(transfer_status(0), TransferStatus::InProgress);
        assert_eq!(transfer_status(INTR_RX_FULL), TransferStatus::InProgress);
        assert_eq!(transfer_status(INTR_STOP_DET), TransferStatus::Complete);
        assert_eq!(transfer_status(INTR_TX_ABRT), TransferStatus::Aborted);
        // A transfer that aborted and then latched STOP is still Aborted.
        assert_eq!(
            transfer_status(INTR_TX_ABRT | INTR_STOP_DET),
            TransferStatus::Aborted
        );
    }
}
