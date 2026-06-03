//! Command issue, completion poll, and the IDENTIFY / READ / WRITE / FLUSH data
//! path — Phase 82 Track C.1 / C.2 / C.3 (with C.4 error recovery folded in).
//!
//! At 1.0 m3OS keeps **one command in flight per port** (single-queue, no NCQ),
//! so the issue→poll loop is the whole data-path engine: pick a free slot
//! ([`crate::pick_slot`] over `PxSACT | PxCI`), build the command header +
//! command table (CFIS + a single PRDT entry whose `DBA`/`DBAU` is the bounce
//! buffer **IOVA**), wait `PxTFD & (BSY|DRQ) == 0`, ring the slot bit in
//! `PxCI`, then poll [`crate::poll_outcome`] (`PxCI` auto-clears on completion)
//! until the slot bit clears with no `PxIS` error. A fatal error or a timeout
//! routes through [`Port::recover_port`] and surfaces an error to the facade.

use kernel_core::driver_ipc::block::BlockDriverError;
use kernel_core::storage::ahci::FisRegH2D;
use kernel_core::storage::ahci::{
    HbaPrdtEntry, PX_CI, PX_IS, PX_TFD, TFD_BSY, TFD_DRQ, encode_dbc,
};
use kernel_core::storage::ata::{
    AtaIdentify, encode_flush_fis, encode_identify_fis, encode_rw_fis, parse_identify,
};

use crate::port::Port;
use crate::{CFL_DWORDS, CmdOutcome, MMIO_SPIN_BUDGET, pick_slot, poll_outcome};

/// Every fallible command returns one of these. Collapsed to a
/// [`BlockDriverError`] for the IPC reply path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdError {
    /// `PxIS` latched a fatal error (`TFES`/`HBFS`/`HBDS`/`IFS`).
    TaskFileError,
    /// The command did not complete within the polling budget.
    Timeout,
    /// The port never became ready to accept the command (`BSY`/`DRQ` stuck).
    NotReady,
    /// No free command slot (should not happen on the single-in-flight path).
    Busy,
}

impl From<CmdError> for BlockDriverError {
    fn from(e: CmdError) -> Self {
        match e {
            CmdError::Busy => BlockDriverError::Busy,
            _ => BlockDriverError::IoError,
        }
    }
}

/// Build the command header (slot `slot`) and command table (CFIS + PRDT) for a
/// single command. `prdtl` is 1 for data transfers, 0 for the non-data FLUSH.
fn prepare_command(
    port: &mut Port,
    slot: u8,
    fis: &FisRegH2D,
    write: bool,
    prdtl: u16,
    data_iova: u64,
    byte_count: u32,
) {
    let table_iova = port.cmd_table.iova();

    // Command table: zero CFIS + ACMD, copy the 20-byte H2D FIS, set the PRDT.
    {
        let table = &mut *port.cmd_table;
        table.cfis = [0u8; 64];
        table.acmd = [0u8; 16];
        // SAFETY: `FisRegH2D` is `#[repr(C)]` and exactly 20 bytes; the
        // first 20 bytes of `cfis` receive it verbatim.
        let fis_bytes =
            unsafe { core::slice::from_raw_parts(fis as *const FisRegH2D as *const u8, 20) };
        table.cfis[..20].copy_from_slice(fis_bytes);
        if prdtl > 0 {
            table.prdt[0].dba = (data_iova & 0xFFFF_FFFF) as u32;
            table.prdt[0].dbau = (data_iova >> 32) as u32;
            table.prdt[0]._rsv = 0;
            // Interrupt-on-completion bit set so the IRQ path (C.5) gets a
            // wakeup; the data path still polls `PxCI`.
            table.prdt[0].dbc = encode_dbc(byte_count, true);
        } else {
            table.prdt[0] = HbaPrdtEntry::default();
        }
    }

    // Command header: CFL = 5 dwords, write bit, PRDTL, and the command-table
    // IOVA (re-set here in case a recovery zeroed the header).
    {
        let headers = &mut *port.cmd_list;
        let hdr = &mut headers[slot as usize];
        *hdr = kernel_core::storage::ahci::HbaCmdHeader::default();
        hdr.set_cfl(CFL_DWORDS);
        hdr.set_write(write);
        hdr.prdtl = prdtl;
        hdr.prdbc = 0;
        hdr.ctba = (table_iova & 0xFFFF_FFFF) as u32;
        hdr.ctbau = (table_iova >> 32) as u32;
    }
}

/// Wait for `PxTFD & (BSY|DRQ) == 0`, release the structure writes, then set the
/// slot bit in `PxCI` to issue the command. Returns `false` if the port never
/// became ready.
fn issue_command(port: &Port, slot: u8) -> bool {
    let mut i = 0u64;
    while port.pread(PX_TFD) & (TFD_BSY | TFD_DRQ) != 0 {
        if i >= MMIO_SPIN_BUDGET {
            return false;
        }
        core::hint::spin_loop();
        i += 1;
    }
    // Release the command-list / command-table writes before the HBA reads them.
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    port.pwrite(PX_CI, 1u32 << slot);
    true
}

/// Poll `PxCI` / `PxIS` until slot `slot` completes, errors, or the budget
/// expires. On success clears `PxIS` (W1C) so the next command starts clean.
fn await_completion(port: &Port, slot: u8) -> Result<(), CmdError> {
    let mut i = 0u64;
    loop {
        let ci = port.pread(PX_CI);
        let is = port.pread(PX_IS);
        match poll_outcome(ci, slot, is) {
            CmdOutcome::Complete => {
                // Acquire the device's DMA writes before any data read.
                core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
                // Clear the W1C interrupt-status latch.
                port.pwrite(PX_IS, is);
                return Ok(());
            }
            CmdOutcome::Failed => return Err(CmdError::TaskFileError),
            CmdOutcome::Pending => {}
        }
        if i >= MMIO_SPIN_BUDGET {
            return Err(CmdError::Timeout);
        }
        core::hint::spin_loop();
        i += 1;
    }
}

/// Issue one command end-to-end: pick a free slot, build it, issue it, poll for
/// completion. On a fatal error or timeout, recover the port (C.4) and return
/// the error so the facade can surface a restart/retry.
fn run_command(
    port: &mut Port,
    fis: FisRegH2D,
    write: bool,
    prdtl: u16,
    data_iova: u64,
    byte_count: u32,
) -> Result<(), CmdError> {
    let sact = port.pread(kernel_core::storage::ahci::PX_SACT);
    let ci = port.pread(PX_CI);
    let slot = pick_slot(sact, ci, port.ncs).ok_or(CmdError::Busy)?;

    prepare_command(port, slot, &fis, write, prdtl, data_iova, byte_count);

    if !issue_command(port, slot) {
        port.recover_port(false);
        return Err(CmdError::NotReady);
    }

    match await_completion(port, slot) {
        Ok(()) => Ok(()),
        Err(e) => {
            // A task-file/interface error needs a COMRESET; a plain timeout
            // restarts the engine without one.
            let interface_error = matches!(e, CmdError::TaskFileError);
            port.recover_port(interface_error);
            Err(e)
        }
    }
}

/// IDENTIFY DEVICE (Track C.2) — the recommended first command. Issues
/// `0xEC` with a single PRDT pointing at the 512-byte head of the bounce
/// buffer, then parses the 256-word block.
pub fn identify(port: &mut Port) -> Result<AtaIdentify, CmdError> {
    let fis = encode_identify_fis();
    let data_iova = port.data_bounce.iova();
    debug_assert_ne!(
        data_iova as usize,
        port.data_bounce.user_ptr() as usize,
        "PRDT must carry the IOVA, never the user VA"
    );
    run_command(port, fis, false, 1, data_iova, 512)?;
    // SAFETY: the bounce buffer is page-aligned (so u16-aligned) and at least
    // 512 bytes; the device DMA'd 256 little-endian words into it.
    let words = unsafe { &*(port.data_bounce.user_ptr() as *const [u16; 256]) };
    Ok(parse_identify(words))
}

/// READ DMA EXT (Track C.3). The data lands in the port's bounce buffer; the
/// caller reads it back via [`Port::data_slice`].
pub fn read_sectors(port: &mut Port, lba: u64, count: u16) -> Result<(), CmdError> {
    let byte_count = count as u32 * 512;
    let fis = encode_rw_fis(false, lba, count);
    let data_iova = port.data_bounce.iova();
    run_command(port, fis, false, 1, data_iova, byte_count)
}

/// WRITE DMA EXT (Track C.3). Copies `data` into the bounce buffer first, then
/// issues the write. `data` must be `count * 512` bytes.
pub fn write_sectors(port: &mut Port, lba: u64, count: u16, data: &[u8]) -> Result<(), CmdError> {
    let byte_count = count as usize * 512;
    let n = byte_count.min(data.len()).min(crate::DATA_BOUNCE_BYTES);
    // SAFETY: the bounce buffer is `DATA_BOUNCE_BYTES` long and `n` is clamped.
    let dst = unsafe {
        core::slice::from_raw_parts_mut(port.data_bounce.user_ptr() as *mut u8, byte_count)
    };
    dst[..n].copy_from_slice(&data[..n]);
    let fis = encode_rw_fis(true, lba, count);
    let data_iova = port.data_bounce.iova();
    run_command(port, fis, true, 1, data_iova, byte_count as u32)
}

/// FLUSH CACHE EXT (Track C.3) — `0xEA`, non-data (`PRDTL == 0`). Reports a
/// write durable only after this completes without error.
pub fn flush(port: &mut Port) -> Result<(), CmdError> {
    let fis = encode_flush_fis();
    run_command(port, fis, false, 0, 0, 0)
}
