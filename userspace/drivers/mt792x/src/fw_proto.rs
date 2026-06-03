//! Pure connac2 firmware-download protocol constants + decode logic.
//!
//! Split out of `fw.rs` (which is `#[cfg(not(test))]`, since it drives the
//! hardware-only `McuRing`) so the MCU command IDs and the patch-semaphore
//! decode can be **host-tested** — QEMU has no mt76 model, so these constants
//! are the only thing standing between an opcode typo and a silent
//! firmware-download failure on real silicon.
//!
//! All values are the established upstream connac2 constants from
//! `drivers/net/wireless/mediatek/mt76/mt76_connac_mcu.h`.

use kernel_core::mt792x::firmware::PatchSem;

/// MCU command IDs for the connac2 firmware-download handshake (`MCU_CMD_*`).
pub mod cmd {
    /// `MCU_CMD_TARGET_ADDRESS_LEN_REQ` — specify a RAM-code region target.
    pub const TARGET_ADDRESS_LEN_REQ: u8 = 0x01;
    /// `MCU_CMD_FW_START_REQ` — begin executing the loaded firmware.
    pub const FW_START_REQ: u8 = 0x02;
    /// `MCU_CMD_PATCH_START_REQ` — initiate a ROM-patch section download.
    pub const PATCH_START_REQ: u8 = 0x05;
    /// `MCU_CMD_PATCH_FINISH_REQ` — signal ROM-patch download completion.
    pub const PATCH_FINISH_REQ: u8 = 0x07;
    /// `MCU_CMD_PATCH_SEM_CONTROL` — acquire/release the ROM-patch semaphore
    /// (the get/release distinction is carried in the payload, NOT a second
    /// opcode).
    pub const PATCH_SEM_CONTROL: u8 = 0x10;
    /// `MCU_CMD_FW_SCATTER` — upload one scatter-DMA chunk.
    pub const FW_SCATTER: u8 = 0xEE;
}

/// `PATCH_SEM_CONTROL` payload operation: acquire the semaphore.
pub const PATCH_SEM_GET: u8 = 0x1;
/// `PATCH_SEM_CONTROL` payload operation: release the semaphore.
pub const PATCH_SEM_RELEASE: u8 = 0x0;

/// `PATCH_SEM_CONTROL` reply status byte: ROM-patch already downloaded.
pub const PATCH_IS_DL: u8 = 0x1;
/// `PATCH_SEM_CONTROL` reply status byte: semaphore acquired, download needed.
pub const PATCH_NOT_DL_SEM_SUCCESS: u8 = 0x2;

/// Decode a `PATCH_SEM_CONTROL` (get) reply into a [`PatchSem`].
///
/// Maps the upstream status enum: `1 → IsDl`, `2 → NotDlSemSuccess`. Anything
/// else (including `0 = PATCH_NOT_DL_SEM_FAIL` and an empty reply) is a
/// semaphore failure and yields `None`.
pub fn decode_patch_sem(reply: &[u8]) -> Option<PatchSem> {
    match reply.first().copied() {
        Some(PATCH_IS_DL) => Some(PatchSem::IsDl),
        Some(PATCH_NOT_DL_SEM_SUCCESS) => Some(PatchSem::NotDlSemSuccess),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The MCU download opcodes must equal the established upstream connac2
    /// values (`mt76_connac_mcu.h`), so a regression away from them is caught
    /// on the host even though the download itself only runs on silicon.
    #[test]
    fn fw_constants_match_upstream() {
        assert_eq!(cmd::TARGET_ADDRESS_LEN_REQ, 0x01);
        assert_eq!(cmd::FW_START_REQ, 0x02);
        assert_eq!(cmd::PATCH_START_REQ, 0x05);
        assert_eq!(cmd::PATCH_FINISH_REQ, 0x07);
        assert_eq!(cmd::PATCH_SEM_CONTROL, 0x10);
        assert_eq!(cmd::FW_SCATTER, 0xEE);
        // The semaphore get/release distinction is a payload op, not a 2nd opcode.
        assert_ne!(PATCH_SEM_GET, PATCH_SEM_RELEASE);
    }

    #[test]
    fn patch_sem_decode() {
        assert_eq!(decode_patch_sem(&[PATCH_IS_DL]), Some(PatchSem::IsDl));
        assert_eq!(
            decode_patch_sem(&[PATCH_NOT_DL_SEM_SUCCESS]),
            Some(PatchSem::NotDlSemSuccess)
        );
        // 0 = PATCH_NOT_DL_SEM_FAIL and an empty reply are both failures.
        assert_eq!(decode_patch_sem(&[0]), None);
        assert_eq!(decode_patch_sem(&[]), None);
    }
}
