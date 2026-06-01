//! PCI capability-list walk + Power Management (PMCSR) helpers — Phase 80c
//! Track F.1.
//!
//! Pure-logic helpers a ring-3 driver uses to find a device's PCI capability
//! and force it to power state D0 before bring-up. The motivating case is the
//! AMD HDA controller under VFIO passthrough: the host's runtime-PM may have
//! left the function (and its internal codec block) in a low-power state, so a
//! driver that resets the controller while it is in D3 sees no codec in
//! `STATESTS`. Forcing D0 via the PM capability's PMCSR register is the
//! Linux-recommended mitigation (`snd_hda_intel` keeps the function at D0
//! during bring-up).
//!
//! The byte-level walk is expressed over a `read_u8` closure so it is
//! host-testable against a synthetic 256-byte config-space image without a real
//! PCI bus — mirroring how `config_read` keeps its rules in `kernel-core`.

/// PCI Status register (16-bit) at config offset `0x06`. Bit 4 indicates a
/// capabilities list is present.
pub const PCI_STATUS_REG: u16 = 0x06;
/// Status-register bit 4: a capabilities list exists (the `0x34` pointer is
/// valid only when this is set).
pub const PCI_STATUS_CAP_LIST: u16 = 1 << 4;
/// Capabilities Pointer (8-bit) at config offset `0x34` — offset of the first
/// capability header, or `0` for none. Low two bits are reserved (mask `0xFC`).
pub const PCI_CAP_PTR_REG: u8 = 0x34;

/// Capability ID for the PCI Power Management capability.
pub const PCI_CAP_ID_PM: u8 = 0x01;
/// Offset of the Power Management Control/Status Register (PMCSR, 16-bit) from
/// the PM capability header.
pub const PCI_PM_CTRL_OFFSET: u8 = 4;
/// PMCSR power-state field (bits `1:0`).
pub const PCI_PM_CTRL_STATE_MASK: u16 = 0x3;

/// A capability list is bounded by config-space size; a malformed list could
/// otherwise loop. 48 ≥ the maximum number of 4-byte-aligned capabilities that
/// fit in the 256-byte legacy config space (the first 64 bytes are the header).
const MAX_CAP_WALK: u32 = 48;

/// Walk the PCI capability list to find the header offset of `cap_id`.
///
/// `status` is the device's PCI Status register (offset `0x06`); the walk is
/// skipped (returns `None`) if its capabilities-list bit is clear. `read_u8`
/// reads one config-space byte and returns `None` to abort the walk (e.g. on a
/// failed syscall). Returns the config-space offset of the matching capability
/// header, or `None` if absent / list malformed.
pub fn find_capability<R>(status: u16, mut read_u8: R, cap_id: u8) -> Option<u8>
where
    R: FnMut(u8) -> Option<u8>,
{
    if status & PCI_STATUS_CAP_LIST == 0 {
        return None;
    }
    // Capability pointers are dword-aligned; mask the reserved low two bits.
    let mut ptr = read_u8(PCI_CAP_PTR_REG)? & 0xFC;
    for _ in 0..MAX_CAP_WALK {
        if ptr == 0 {
            return None;
        }
        let id = read_u8(ptr)?;
        if id == cap_id {
            return Some(ptr);
        }
        // Next-pointer is the second byte of the header.
        ptr = read_u8(ptr + 1)? & 0xFC;
    }
    None
}

/// Config-space offset of the PMCSR (16-bit) given a PM capability header
/// offset. Returned as `u16` so the caller can range-check `offset + 2`.
#[inline]
pub fn pmcsr_offset(pm_cap: u8) -> u16 {
    pm_cap as u16 + PCI_PM_CTRL_OFFSET as u16
}

/// Extract the power-state field (`0` = D0 … `3` = D3hot) from a PMCSR value.
#[inline]
pub fn pm_power_state(pmcsr: u16) -> u8 {
    (pmcsr & PCI_PM_CTRL_STATE_MASK) as u8
}

/// Clear the power-state field of a PMCSR value to D0, preserving every other
/// bit (PME enable, data-select, etc.). Returns the value to write back; if the
/// device is already in D0 this is `pmcsr` unchanged.
#[inline]
pub fn pmcsr_force_d0(pmcsr: u16) -> u16 {
    pmcsr & !PCI_PM_CTRL_STATE_MASK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic config space with a two-entry capability list:
    /// MSI (0x05) at 0x50 → PM (0x01) at 0x60. PMCSR at 0x64.
    fn synthetic_config(pmcsr: u16) -> [u8; 256] {
        let mut cfg = [0u8; 256];
        // Status register (0x06) with cap-list bit set.
        cfg[0x06] = (PCI_STATUS_CAP_LIST & 0xFF) as u8;
        // Cap pointer → 0x50.
        cfg[0x34] = 0x50;
        // MSI cap at 0x50: id=0x05, next=0x60.
        cfg[0x50] = 0x05;
        cfg[0x51] = 0x60;
        // PM cap at 0x60: id=0x01, next=0x00 (end).
        cfg[0x60] = 0x01;
        cfg[0x61] = 0x00;
        // PMCSR at 0x64 (little-endian).
        cfg[0x64] = (pmcsr & 0xFF) as u8;
        cfg[0x65] = (pmcsr >> 8) as u8;
        cfg
    }

    #[test]
    fn finds_pm_capability() {
        let cfg = synthetic_config(0x0000);
        let status = u16::from(cfg[0x06]);
        let pm = find_capability(status, |off| Some(cfg[off as usize]), PCI_CAP_ID_PM);
        assert_eq!(pm, Some(0x60));
        assert_eq!(pmcsr_offset(0x60), 0x64);
    }

    #[test]
    fn finds_msi_capability() {
        let cfg = synthetic_config(0x0000);
        let status = u16::from(cfg[0x06]);
        let msi = find_capability(status, |off| Some(cfg[off as usize]), 0x05);
        assert_eq!(msi, Some(0x50));
    }

    #[test]
    fn missing_capability_returns_none() {
        let cfg = synthetic_config(0x0000);
        let status = u16::from(cfg[0x06]);
        // 0x10 (PCIe cap) is not in this list.
        assert_eq!(
            find_capability(status, |off| Some(cfg[off as usize]), 0x10),
            None
        );
    }

    #[test]
    fn no_cap_list_bit_means_no_walk() {
        let mut cfg = synthetic_config(0x0000);
        cfg[0x06] = 0; // clear cap-list bit
        assert_eq!(
            find_capability(0, |off| Some(cfg[off as usize]), PCI_CAP_ID_PM),
            None
        );
    }

    #[test]
    fn aborted_read_returns_none() {
        // A read closure that always fails must not loop forever.
        assert_eq!(
            find_capability(PCI_STATUS_CAP_LIST, |_off| None, PCI_CAP_ID_PM),
            None
        );
    }

    #[test]
    fn malformed_loop_is_bounded() {
        // A capability whose next-pointer points back at itself must terminate
        // via the iteration bound rather than spin.
        let read = |off: u8| -> Option<u8> {
            match off {
                0x34 => Some(0x40), // cap ptr → 0x40
                0x40 => Some(0x09), // id = vendor-specific (not PM)
                0x41 => Some(0x40), // next → itself (loop)
                _ => Some(0),
            }
        };
        assert_eq!(
            find_capability(PCI_STATUS_CAP_LIST, read, PCI_CAP_ID_PM),
            None
        );
    }

    #[test]
    fn power_state_decode_and_force_d0() {
        // D3hot with PME-enable (bit 8) set.
        let pmcsr = 0x0103;
        assert_eq!(pm_power_state(pmcsr), 3);
        // Forcing D0 clears only the state field, preserving PME-enable.
        assert_eq!(pmcsr_force_d0(pmcsr), 0x0100);
        // Already-D0 is unchanged.
        assert_eq!(pmcsr_force_d0(0x0100), 0x0100);
        assert_eq!(pm_power_state(0x0100), 0);
    }
}
