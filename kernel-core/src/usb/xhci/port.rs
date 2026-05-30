//! xHCI Port Status and Control (PORTSC) register logic (xHCI 1.2b §5.4.8).
//!
//! Each root-hub port exposes a PORTSC register holding both *status* bits the
//! controller drives (connect, enable, link state, speed) and *change* bits the
//! controller sets on an event and the driver clears by **writing 1**
//! (RW1C). Several of those bits — including the change bits and the Port
//! Enabled/Disabled (PED) bit — share the RW1C-write-1-clears semantics, so a
//! naive read-modify-write of PORTSC will accidentally clear status the driver
//! meant to keep or re-fire a clear it did not intend.
//!
//! This module provides:
//!
//! * [`Portsc`] — typed accessors for the bits the bring-up path reads.
//! * RW1C-safe write helpers ([`portsc_write_preserving`],
//!   [`portsc_clear_change`]) that mask out the dangerous bits.
//! * The Protocol-Speed-ID → [`PortSpeed`] → EP0 max-packet-size mapping used to
//!   size the default control endpoint before the device descriptor is read.
//!
//! No MMIO: callers pass the value they read and apply the value this module
//! returns.

// ---------------------------------------------------------------------------
// PORTSC bit positions (xHCI §5.4.8)
// ---------------------------------------------------------------------------

/// CCS — Current Connect Status (bit 0). A device is attached.
pub const PORTSC_CCS: u32 = 1 << 0;
/// PED — Port Enabled/Disabled (bit 1). **Write-1-clears**: writing 1 disables
/// the port. The controller sets it after a successful reset; software clears
/// it to disable. Never re-assert it via a blind RMW.
pub const PORTSC_PED: u32 = 1 << 1;
/// OCA — Over-current Active (bit 3).
pub const PORTSC_OCA: u32 = 1 << 3;
/// PR — Port Reset (bit 4). Writing 1 starts a port reset.
pub const PORTSC_PR: u32 = 1 << 4;
/// PP — Port Power (bit 9).
pub const PORTSC_PP: u32 = 1 << 9;

/// Shift of PLS — Port Link State (bits 8:5).
pub const PORTSC_PLS_SHIFT: u32 = 5;
/// Mask of the Port Link State field after shifting (4 bits).
pub const PORTSC_PLS_MASK: u32 = 0xF;
/// Shift of Port Speed — Protocol Speed ID (bits 13:10).
pub const PORTSC_PORT_SPEED_SHIFT: u32 = 10;
/// Mask of the Port Speed field after shifting (4 bits).
pub const PORTSC_PORT_SPEED_MASK: u32 = 0xF;

// --- RW1C change/status bits ---------------------------------------------

/// CSC — Connect Status Change (bit 17). RW1C.
pub const PORTSC_CSC: u32 = 1 << 17;
/// PEC — Port Enabled/Disabled Change (bit 18). RW1C.
pub const PORTSC_PEC: u32 = 1 << 18;
/// WRC — Warm Port Reset Change (bit 19). RW1C.
pub const PORTSC_WRC: u32 = 1 << 19;
/// OCC — Over-current Change (bit 20). RW1C.
pub const PORTSC_OCC: u32 = 1 << 20;
/// PRC — Port Reset Change (bit 21). RW1C.
pub const PORTSC_PRC: u32 = 1 << 21;
/// PLC — Port Link State Change (bit 22). RW1C.
pub const PORTSC_PLC: u32 = 1 << 22;
/// CEC — Port Config Error Change (bit 23). RW1C.
pub const PORTSC_CEC: u32 = 1 << 23;

/// Mask of every RW1C change bit (CSC | PEC | WRC | OCC | PRC | PLC | CEC).
/// Writing any of these as 1 clears the corresponding change indication.
pub const PORTSC_RW1C_MASK: u32 =
    PORTSC_CSC | PORTSC_PEC | PORTSC_WRC | PORTSC_OCC | PORTSC_PRC | PORTSC_PLC | PORTSC_CEC;

/// Mask of bits that must be *preserved* (left zero in the value written) when
/// doing a read-modify-write that should not disturb RW1C state: every change
/// bit plus PED (bit 1), which is itself write-1-clears. Writing zero to these
/// bits leaves them unchanged; writing one would clear / re-trigger them.
pub const PORTSC_PRESERVE_MASK: u32 = !(PORTSC_RW1C_MASK | PORTSC_PED);

/// Build a PORTSC value that sets `set_bits` while leaving every RW1C change bit
/// and the PED bit untouched (xHCI §5.4.8 RW1C semantics).
///
/// `current` is the freshly-read register value. The result keeps `current`'s
/// non-RW1C status bits, forces all RW1C / PED bits to zero (so they are *not*
/// cleared or re-asserted), and ORs in `set_bits`. Use this to start a port
/// reset (`set_bits = PORTSC_PR`) without clobbering pending change bits.
pub const fn portsc_write_preserving(current: u32, set_bits: u32) -> u32 {
    (current & PORTSC_PRESERVE_MASK) | set_bits
}

/// Build a PORTSC value that clears exactly one RW1C change bit and disturbs
/// nothing else (xHCI §5.4.8).
///
/// `change_bit` should be a single RW1C bit (e.g. [`PORTSC_PRC`]). All other
/// RW1C bits and PED are forced to zero so they are not also cleared, and the
/// preserved status bits keep their current values.
pub const fn portsc_clear_change(current: u32, change_bit: u32) -> u32 {
    (current & PORTSC_PRESERVE_MASK) | change_bit
}

// ---------------------------------------------------------------------------
// Portsc accessor wrapper
// ---------------------------------------------------------------------------

/// Typed view over a raw PORTSC register value (xHCI §5.4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Portsc(pub u32);

impl Portsc {
    /// CCS — Current Connect Status (bit 0): a device is attached.
    pub const fn ccs(self) -> bool {
        self.0 & PORTSC_CCS != 0
    }

    /// PED — Port Enabled/Disabled (bit 1): the port is enabled.
    pub const fn ped(self) -> bool {
        self.0 & PORTSC_PED != 0
    }

    /// PR — Port Reset (bit 4): a reset is in progress.
    pub const fn pr(self) -> bool {
        self.0 & PORTSC_PR != 0
    }

    /// PLS — Port Link State (bits 8:5).
    pub const fn pls(self) -> u8 {
        ((self.0 >> PORTSC_PLS_SHIFT) & PORTSC_PLS_MASK) as u8
    }

    /// PP — Port Power (bit 9).
    pub const fn pp(self) -> bool {
        self.0 & PORTSC_PP != 0
    }

    /// Port Speed — the Protocol Speed ID (bits 13:10). Map it through
    /// [`port_speed_from_psi`] to get a [`PortSpeed`].
    pub const fn port_speed(self) -> u8 {
        ((self.0 >> PORTSC_PORT_SPEED_SHIFT) & PORTSC_PORT_SPEED_MASK) as u8
    }

    /// CSC — Connect Status Change (bit 17).
    pub const fn csc(self) -> bool {
        self.0 & PORTSC_CSC != 0
    }

    /// PEC — Port Enabled/Disabled Change (bit 18).
    pub const fn pec(self) -> bool {
        self.0 & PORTSC_PEC != 0
    }

    /// PRC — Port Reset Change (bit 21).
    pub const fn prc(self) -> bool {
        self.0 & PORTSC_PRC != 0
    }

    /// PLC — Port Link State Change (bit 22).
    pub const fn plc(self) -> bool {
        self.0 & PORTSC_PLC != 0
    }
}

// ---------------------------------------------------------------------------
// Protocol Speed ID → port speed → EP0 max packet size
// ---------------------------------------------------------------------------

/// Default xHCI Protocol Speed ID for Full Speed (xHCI §7.2.1 Table 7-12).
pub const PSI_FULL_SPEED: u8 = 1;
/// Default xHCI Protocol Speed ID for Low Speed.
pub const PSI_LOW_SPEED: u8 = 2;
/// Default xHCI Protocol Speed ID for High Speed.
pub const PSI_HIGH_SPEED: u8 = 3;
/// Default xHCI Protocol Speed ID for SuperSpeed (Gen1 x1).
pub const PSI_SUPER_SPEED: u8 = 4;

/// USB device speed as reported by a root-hub port (xHCI §7.2.1 default speed
/// IDs). Used to size the default control endpoint before any descriptor is
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortSpeed {
    /// USB 1.x Low Speed.
    Low,
    /// USB 1.x/2.0 Full Speed.
    Full,
    /// USB 2.0 High Speed.
    High,
    /// USB 3.x SuperSpeed.
    Super,
}

/// Map an xHCI **default** Protocol Speed ID to a [`PortSpeed`] (xHCI §7.2.1
/// Table 7-12). Returns `None` for an unknown / vendor-defined PSI.
///
/// Note the non-monotonic ordering of the default table: `1 = Full`, `2 = Low`,
/// `3 = High`, `4 = SuperSpeed`.
pub const fn port_speed_from_psi(psi: u8) -> Option<PortSpeed> {
    match psi {
        PSI_FULL_SPEED => Some(PortSpeed::Full),
        PSI_LOW_SPEED => Some(PortSpeed::Low),
        PSI_HIGH_SPEED => Some(PortSpeed::High),
        PSI_SUPER_SPEED => Some(PortSpeed::Super),
        _ => None,
    }
}

/// EP0 max packet size (in bytes) for Low/Full speed: bMaxPacketSize0 = 8.
pub const EP0_MPS_LOW_FULL: u16 = 8;
/// EP0 max packet size for High speed: bMaxPacketSize0 = 64.
pub const EP0_MPS_HIGH: u16 = 64;
/// EP0 max packet size for SuperSpeed: bMaxPacketSize0 = 9 (i.e. `2^9` = 512).
pub const EP0_MPS_SUPER: u16 = 512;

/// Initial max packet size for the default control endpoint (EP0) at a given
/// port speed (USB 2.0 §5.5.3 / USB 3.2 §8.5.3, surfaced through xHCI bring-up).
///
/// Low and Full speed start at 8 bytes (the device descriptor's
/// `bMaxPacketSize0` may raise Full speed to 8/16/32/64 afterwards); High speed
/// is fixed at 64; SuperSpeed is fixed at 512 (`bMaxPacketSize0` is encoded as
/// the exponent 9, so the actual size is `2^9`).
pub const fn ep0_max_packet_for_speed(speed: PortSpeed) -> u16 {
    match speed {
        PortSpeed::Low => EP0_MPS_LOW_FULL,
        PortSpeed::Full => EP0_MPS_LOW_FULL,
        PortSpeed::High => EP0_MPS_HIGH,
        PortSpeed::Super => EP0_MPS_SUPER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portsc_accessors() {
        // CCS | PED | PR | PP, PLS = 0x5, speed = 4.
        let raw = PORTSC_CCS
            | PORTSC_PED
            | PORTSC_PR
            | PORTSC_PP
            | (0x5 << PORTSC_PLS_SHIFT)
            | (4 << PORTSC_PORT_SPEED_SHIFT)
            | PORTSC_CSC
            | PORTSC_PRC;
        let p = Portsc(raw);
        assert!(p.ccs());
        assert!(p.ped());
        assert!(p.pr());
        assert!(p.pp());
        assert_eq!(p.pls(), 0x5);
        assert_eq!(p.port_speed(), 4);
        assert!(p.csc());
        assert!(!p.pec());
        assert!(p.prc());
        assert!(!p.plc());
    }

    #[test]
    fn rw1c_mask_matches_spec() {
        let expected =
            (1u32 << 17) | (1 << 18) | (1 << 19) | (1 << 20) | (1 << 21) | (1 << 22) | (1 << 23);
        assert_eq!(PORTSC_RW1C_MASK, expected);
        // Preserve mask excludes both the RW1C bits and PED (bit 1).
        assert_eq!(PORTSC_PRESERVE_MASK, !(expected | (1 << 1)));
    }

    #[test]
    fn write_preserving_does_not_clobber_rw1c_or_ped() {
        // Start with CSC=1 and PED=1 set; request a port reset (PR).
        let current = PORTSC_CCS | PORTSC_CSC | PORTSC_PED;
        let out = portsc_write_preserving(current, PORTSC_PR);

        // PR must be set in the written value.
        assert_ne!(out & PORTSC_PR, 0);
        // CSC must be 0 in the written value (do not re-assert the RW1C clear).
        assert_eq!(out & PORTSC_CSC, 0);
        // PED must be 0 in the written value (writing 1 would disable the port).
        assert_eq!(out & PORTSC_PED, 0);
        // The preserved CCS status bit survives.
        assert_ne!(out & PORTSC_CCS, 0);
    }

    #[test]
    fn clear_change_sets_only_target_bit() {
        // Several change bits pending; clear only PRC.
        let current = PORTSC_CCS | PORTSC_CSC | PORTSC_PRC | PORTSC_PLC | PORTSC_PED;
        let out = portsc_clear_change(current, PORTSC_PRC);

        // Only PRC asserted among the RW1C bits.
        assert_ne!(out & PORTSC_PRC, 0);
        assert_eq!(out & PORTSC_CSC, 0);
        assert_eq!(out & PORTSC_PLC, 0);
        // PED not re-asserted.
        assert_eq!(out & PORTSC_PED, 0);
        // CCS preserved.
        assert_ne!(out & PORTSC_CCS, 0);
    }

    #[test]
    fn psi_to_speed_default_table() {
        assert_eq!(port_speed_from_psi(1), Some(PortSpeed::Full));
        assert_eq!(port_speed_from_psi(2), Some(PortSpeed::Low));
        assert_eq!(port_speed_from_psi(3), Some(PortSpeed::High));
        assert_eq!(port_speed_from_psi(4), Some(PortSpeed::Super));
        // Unknown / vendor PSIs.
        assert_eq!(port_speed_from_psi(0), None);
        assert_eq!(port_speed_from_psi(5), None);
        assert_eq!(port_speed_from_psi(255), None);
    }

    #[test]
    fn ep0_max_packet_per_speed() {
        assert_eq!(ep0_max_packet_for_speed(PortSpeed::Low), 8);
        assert_eq!(ep0_max_packet_for_speed(PortSpeed::Full), 8);
        assert_eq!(ep0_max_packet_for_speed(PortSpeed::High), 64);
        // SuperSpeed: bMaxPacketSize0 = 9 -> 2^9 = 512.
        assert_eq!(ep0_max_packet_for_speed(PortSpeed::Super), 512);
    }
}
