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
//! 1. `PATCH_SEM_GET` — acquire the semaphore; branch on [`PatchSem`]:
//!    - `IsDl` → ROM-patch already loaded by a prior driver instance; skip to step 5.
//!    - `NotDlSemSuccess` → proceed to step 2.
//! 2. Parse the ROM-patch header via `parse_patch_header`; for each of the
//!    `n_region` sections: init-download (`PATCH_START_REQ` with target
//!    `MCU_PATCH_ADDRESS`) then chunked scatter-DMA upload (`FW_SCATTER`) at
//!    `FW_SCATTER_CHUNK` (4096 bytes) — use `scatter_chunk_count` for the loop
//!    bound.
//! 3. `PATCH_FINISH_REQ` — signal MCU that ROM-patch download is complete.
//! 4. `PATCH_SEM_RELEASE` — release the semaphore.
//! 5. Parse the RAM-code trailer via `parse_fw_trailer`; for each region:
//!    - `TARGET_ADDRESS_LEN_REQ` with `addr = region.addr`, `len = region.len`,
//!      honouring `FW_FEATURE_OVERRIDE_ADDR`.
//!    - Chunked scatter-DMA upload (`FW_SCATTER`) at `FW_SCATTER_CHUNK`.
//! 6. `FW_START_REQ` — signal MCU to begin executing the new firmware.
//! 7. Poll firmware-running.
//!    **[UNCERTAIN]** firmware-running poll register/value — resolve on hardware
//!    (E.3 capture). A placeholder busy-loop is used here.
//!
//! The pure parsing logic (`parse_patch_header`, `parse_fw_trailer`, etc.) lives
//! in `kernel_core::mt792x::firmware` (host-tested). This module is the
//! thin policy + sequencing layer that calls those parsers and drives the MCU.

extern crate alloc;

use kernel_core::mt792x::firmware::{
    FW_SCATTER_CHUNK, FirmwareError, MCU_PATCH_ADDRESS, PatchSem, parse_fw_trailer,
    parse_patch_header, patch_sections_to_download, scatter_chunk_count,
};

use crate::mcu::{McuError, McuRing};

/// Placeholder MCU command IDs used during the firmware-download handshake.
/// These will be resolved against the upstream mt76 source at hardware-bring-up
/// time (Track E.3 capture); the exact values are [UNCERTAIN].
mod cmd {
    /// Request semaphore acquisition for ROM-patch download.
    pub const PATCH_SEM_GET: u8 = 0x10; // [UNCERTAIN] placeholder
    /// Release the ROM-patch download semaphore.
    pub const PATCH_SEM_RELEASE: u8 = 0x11; // [UNCERTAIN] placeholder
    /// Initiate a ROM-patch region download.
    pub const PATCH_START_REQ: u8 = 0x20; // [UNCERTAIN] placeholder
    /// Signal ROM-patch download completion.
    pub const PATCH_FINISH_REQ: u8 = 0x21; // [UNCERTAIN] placeholder
    /// Upload one scatter-DMA chunk.
    pub const FW_SCATTER: u8 = 0x30; // [UNCERTAIN] placeholder
    /// Specify RAM-code region target address + length.
    pub const TARGET_ADDRESS_LEN_REQ: u8 = 0x31; // [UNCERTAIN] placeholder
    /// Signal firmware execution start.
    pub const FW_START_REQ: u8 = 0x32; // [UNCERTAIN] placeholder
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

    // Step 1: Acquire the ROM-patch semaphore.
    let sem_reply = mcu.submit_and_reap(cmd::PATCH_SEM_GET, &[])?;
    // A reply byte of 0x01 means "already downloaded" (IsDl), 0x00 means
    // we acquired the semaphore and must download. This encoding is
    // [UNCERTAIN] pending hardware capture; use the first byte of the reply.
    let sem = if sem_reply.first().copied().unwrap_or(0) != 0 {
        PatchSem::IsDl
    } else {
        PatchSem::NotDlSemSuccess
    };

    let hdr = parse_patch_header(rom_patch)?;
    let sections_to_download = patch_sections_to_download(sem, hdr.n_region);

    if sections_to_download > 0 {
        // Step 2: for each ROM-patch section, send PATCH_START_REQ then scatter.
        for _sec in 0..sections_to_download as usize {
            // Build the PATCH_START_REQ payload: target address (LE u32) +
            // placeholder section info. [UNCERTAIN] exact payload format.
            let start_payload = MCU_PATCH_ADDRESS.to_le_bytes();
            mcu.submit_and_reap(cmd::PATCH_START_REQ, &start_payload)?;

            // Scatter-DMA upload of this section's data. For the shell track
            // we use the full rom_patch blob as a placeholder for the actual
            // per-section slice; the real section-slice indexing will be wired
            // in during Track E.3 hardware capture.
            let chunk_count = scatter_chunk_count(rom_patch.len());
            for chunk_idx in 0..chunk_count {
                let start = chunk_idx * FW_SCATTER_CHUNK;
                let end = (start + FW_SCATTER_CHUNK).min(rom_patch.len());
                mcu.submit_and_reap(cmd::FW_SCATTER, &rom_patch[start..end])?;
            }
        }

        // Step 3: PATCH_FINISH_REQ.
        mcu.submit_and_reap(cmd::PATCH_FINISH_REQ, &[])?;
    }

    // Step 4: Release the semaphore.
    mcu.submit_and_reap(cmd::PATCH_SEM_RELEASE, &[])?;

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
    // [UNCERTAIN] firmware-running poll register/value — resolve on hardware
    // (E.3 capture). The mt76 driver polls a status register in the MCU's
    // address space via an OCP/indirect-register read. For the shell track we
    // use a bounded spin-loop as a placeholder.
    for _ in 0..10_000 {
        core::hint::spin_loop();
    }

    Ok(())
}
