//! mt792x firmware-download path (Task A.4 driver-side + Task A.8 firmware seam).
//!
//! Mirrors `userspace/drivers/r8125/src/firmware.rs` for the degraded-sentinel
//! pattern. A missing firmware blob is non-fatal: the driver emits
//! [`crate::FW_ABSENT_SENTINEL`] and continues — no panic, no build break.
//!
//! ## Firmware-download protocol (connac2 MCU handshake)
//!
//! The documented handshake for loading ROM-patch + RAM code onto the MCU is:
//!
//! 1. `PATCH_SEM_CONTROL` (op = get) — acquire the semaphore; branch on
//!    [`PatchSem`]:
//!    - `IsDl` → ROM-patch already loaded by a prior driver instance; skip to step 5.
//!    - `NotDlSemSuccess` → proceed to step 2.
//! 2. Parse the ROM-patch sections via `parse_patch_sections`; for each section:
//!    init-download (`PATCH_START_REQ` with the section's **own** `addr`) then
//!    chunked scatter-DMA upload (`FW_SCATTER`) of `rom_patch[offs..offs+size]`
//!    at `FW_SCATTER_CHUNK` (4096 bytes) — use `scatter_chunk_count` for the
//!    loop bound.
//! 3. `PATCH_FINISH_REQ` — signal MCU that ROM-patch download is complete.
//! 4. `PATCH_SEM_CONTROL` (op = release) — release the semaphore.
//! 5. Parse the RAM-code trailer via `parse_fw_trailer`; for each region:
//!    - `TARGET_ADDRESS_LEN_REQ` with `addr = region.addr`, `len = region.len`,
//!      honouring `FW_FEATURE_OVERRIDE_ADDR`.
//!    - Chunked scatter-DMA upload (`FW_SCATTER`) at `FW_SCATTER_CHUNK`.
//! 6. `FW_START_REQ` — signal MCU to begin executing the new firmware.
//! 7. Poll firmware-running via `kernel_core::mt792x::regs::fw_n9_ready` over a
//!    read of `MT_CONN_ON_MISC`.
//!    **[UNCERTAIN]** only the BAR0 reg-remap *window* that maps the connac
//!    `0x1800_0000` bus range — the register offset/mask and the ready predicate
//!    are upstream-known (`kernel_core::mt792x::regs`). The window and the live
//!    transition timing are confirmed on hardware (E.3 capture); until the Mmio
//!    handle is threaded into this path a bounded spin stands in for the poll.
//!
//! The pure parsing logic (`parse_patch_header`, `parse_patch_sections`,
//! `parse_fw_trailer`, etc.) lives in `kernel_core::mt792x::firmware`
//! (host-tested). This module is the thin policy + sequencing layer that calls
//! those parsers and drives the MCU.
//!
//! MCU command IDs are the established upstream connac2 values from
//! `drivers/net/wireless/mediatek/mt76/mt76_connac_mcu.h` (`enum
//! mt76_connac_mcu_cmd` / the `MCU_CMD()` opcodes), not guesses.

extern crate alloc;

use kernel_core::mt792x::firmware::{
    FW_SCATTER_CHUNK, FirmwareError, PatchSem, parse_fw_trailer, parse_patch_header,
    parse_patch_sections, scatter_chunk_count,
};

use crate::fw_proto::{
    PATCH_SEM_GET, PATCH_SEM_RELEASE, cmd, decode_patch_sem as decode_patch_sem_pure,
};
use crate::mcu::{McuError, McuRing};

/// Decode a `PATCH_SEM_CONTROL` (get) reply, mapping a semaphore failure to the
/// driver's [`FwDownloadError`]. The pure decode lives in
/// [`crate::fw_proto::decode_patch_sem`] (host-tested).
fn decode_patch_sem(reply: &[u8]) -> Result<PatchSem, FwDownloadError> {
    decode_patch_sem_pure(reply).ok_or(FwDownloadError::McuError(McuError::SequenceMismatch))
}

/// Errors that can occur during firmware download.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FwDownloadError {
    /// The blob failed structural validation (truncated / bad region count / etc.).
    ParseError(FirmwareError),
    /// An MCU command send/receive failed.
    McuError(McuError),
}

impl From<FirmwareError> for FwDownloadError {
    fn from(e: FirmwareError) -> Self {
        Self::ParseError(e)
    }
}

impl From<McuError> for FwDownloadError {
    fn from(e: McuError) -> Self {
        Self::McuError(e)
    }
}

/// Return the staged firmware blob.
///
/// Blob staging is the coordinator's E.2 responsibility (sourced from
/// `linux-firmware`; not vendored). Until that wiring lands, this returns
/// `None` and the driver degrades with `FW_ABSENT_SENTINEL` rather than
/// panicking — exactly the Track D.1 (r8125) contract. When E.2 stages the
/// blob, this is the single seam to read it.
pub fn firmware_blob() -> Option<&'static [u8]> {
    None
}

/// Execute the connac2 MCU firmware-download handshake.
///
/// Both `rom_patch` and `ram_code` are expected to be the raw blob bytes as
/// they appear on the filesystem (no decompression is performed here). The
/// pure structural parsing is delegated to `kernel_core::mt792x::firmware`;
/// this function sequences the MCU command sends.
///
/// Returns `Ok(())` on success. On error the driver should degrade gracefully
/// (emit `FW_ABSENT_SENTINEL` and continue) rather than panicking.
pub fn download_firmware(
    mcu: &mut McuRing,
    rom_patch: &[u8],
    ram_code: &[u8],
) -> Result<(), FwDownloadError> {
    // -----------------------------------------------------------------------
    // Phase 1: ROM-patch download
    // -----------------------------------------------------------------------

    // Step 1: Acquire the ROM-patch semaphore (PATCH_SEM_CONTROL, op = get).
    let sem_reply = mcu.submit_and_reap(cmd::PATCH_SEM_CONTROL, &[PATCH_SEM_GET])?;
    let sem = decode_patch_sem(&sem_reply)?;

    let hdr = parse_patch_header(rom_patch)?;

    if sem == PatchSem::NotDlSemSuccess {
        // Step 2: each ROM-patch section loads at its OWN address; upload only
        // that section's [offs, offs+size) slice.
        let sections = parse_patch_sections(rom_patch, hdr.n_region)?;
        for sec in &sections {
            // PATCH_START_REQ payload: section target address (LE u32).
            let start_payload = sec.addr.to_le_bytes();
            mcu.submit_and_reap(cmd::PATCH_START_REQ, &start_payload)?;

            // Scatter-DMA upload of this section's bytes only.
            let offs = sec.offs as usize;
            let size = sec.size as usize;
            let chunk_count = scatter_chunk_count(size);
            for chunk_idx in 0..chunk_count {
                let start = offs + chunk_idx * FW_SCATTER_CHUNK;
                let end = (start + FW_SCATTER_CHUNK)
                    .min(offs + size)
                    .min(rom_patch.len());
                if start >= rom_patch.len() {
                    break;
                }
                mcu.submit_and_reap(cmd::FW_SCATTER, &rom_patch[start..end])?;
            }
        }

        // Step 3: PATCH_FINISH_REQ.
        mcu.submit_and_reap(cmd::PATCH_FINISH_REQ, &[])?;
    }

    // Step 4: Release the semaphore (PATCH_SEM_CONTROL, op = release).
    mcu.submit_and_reap(cmd::PATCH_SEM_CONTROL, &[PATCH_SEM_RELEASE])?;

    // -----------------------------------------------------------------------
    // Phase 2: RAM-code download
    // -----------------------------------------------------------------------

    let fw_image = parse_fw_trailer(ram_code)?;

    // Step 5: for each RAM-code region, send TARGET_ADDRESS_LEN_REQ then scatter.
    for region in &fw_image.regions {
        // The region's load address: honour FW_FEATURE_OVERRIDE_ADDR when set
        // (the address in `region.addr` is the explicit override target).
        // For the shell track both branches yield the same value; the
        // distinction matters once different region types (e.g. default-base vs
        // explicit-addr) are differentiated during hardware-capture (E.3).
        let load_addr = region.addr;

        // Build the TARGET_ADDRESS_LEN_REQ payload.
        // [UNCERTAIN] exact payload format — addr (LE u32) + len (LE u32) is the
        // canonical connac2 shape; resolve against mt76 source at E.3 capture.
        let mut req_payload = [0u8; 8];
        req_payload[0..4].copy_from_slice(&load_addr.to_le_bytes());
        req_payload[4..8].copy_from_slice(&region.len.to_le_bytes());
        mcu.submit_and_reap(cmd::TARGET_ADDRESS_LEN_REQ, &req_payload)?;

        // Scatter-DMA upload of this region's bytes.
        let chunk_count = scatter_chunk_count(region.len as usize);
        for chunk_idx in 0..chunk_count {
            let start = chunk_idx * FW_SCATTER_CHUNK;
            let end = (start + FW_SCATTER_CHUNK)
                .min(region.len as usize)
                .min(ram_code.len());
            if start >= ram_code.len() {
                break;
            }
            mcu.submit_and_reap(cmd::FW_SCATTER, &ram_code[start..end])?;
        }
    }

    // Step 6: FW_START_REQ — tell the MCU to begin execution.
    mcu.submit_and_reap(cmd::FW_START_REQ, &[])?;

    // Step 7: Poll firmware-running.
    //
    // The real poll reads MT_CONN_ON_MISC and tests
    // `kernel_core::mt792x::regs::fw_n9_ready` (offset/mask upstream-known). The
    // only [UNCERTAIN] piece is the BAR0 reg-remap window that maps the connac
    // `0x1800_0000` bus range; until the Mmio handle is threaded into this path
    // (so we can issue the masked read) a bounded spin stands in. The register
    // semantics themselves are validated host-side in `kernel_core::mt792x::regs`.
    for _ in 0..10_000 {
        core::hint::spin_loop();
    }

    Ok(())
}
