//! Phase 103 A — the `power` IPC control protocol between `powerd`
//! (server) and its clients (`m3ctl`, the Phase 105 settings panel).
//!
//! Mirrors the `wifi_core::control` shape: request labels + a
//! length-stable byte codec with host-tested round-trips. A `POWER_STATUS`
//! request carries no body; the reply bulk is one encoded
//! [`PowerStatusWire`].

/// `m3ctl power status` / `m3ctl battery` request label.
pub const POWER_STATUS: u16 = 0x5701;

/// The `powerd` IPC service name.
pub const POWER_SERVICE_NAME: &str = "power";

/// AC-adapter state as reported by `_PSR` (or assumed on platforms with
/// no `ACPI0003` device — every desktop/VM).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcState {
    Offline,
    Online,
    /// No AC-adapter device in the namespace: mains power is assumed
    /// (the QEMU/desktop case).
    AssumedOnline,
}

impl AcState {
    fn to_byte(self) -> u8 {
        match self {
            AcState::Offline => 0,
            AcState::Online => 1,
            AcState::AssumedOnline => 2,
        }
    }

    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(AcState::Offline),
            1 => Some(AcState::Online),
            2 => Some(AcState::AssumedOnline),
            _ => None,
        }
    }
}

/// Sentinel for [`PowerStatusWire::percent`] when no percentage can be
/// computed (no battery, or ACPI "unknown" fields).
pub const PERCENT_UNKNOWN: u8 = 0xFF;

/// The `POWER_STATUS` reply payload.
///
/// Wire layout (14 bytes LE):
/// `battery_present[1] | percent[1] | ac[1] | state[4] | rate[4] | reserved[3]`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerStatusWire {
    pub battery_present: bool,
    /// 0–100, or [`PERCENT_UNKNOWN`].
    pub percent: u8,
    pub ac: AcState,
    /// Raw `_BST` state bits (0 when no battery).
    pub state: u32,
    /// Present rate in the battery's `_BIF` unit (0 when unknown).
    pub rate: u32,
}

/// Encoded size of [`PowerStatusWire`].
pub const POWER_STATUS_WIRE_LEN: usize = 14;

impl PowerStatusWire {
    /// The no-battery platform snapshot (QEMU/desktop).
    pub fn no_battery() -> Self {
        Self {
            battery_present: false,
            percent: PERCENT_UNKNOWN,
            ac: AcState::AssumedOnline,
            state: 0,
            rate: 0,
        }
    }

    pub fn encode(&self) -> [u8; POWER_STATUS_WIRE_LEN] {
        let mut out = [0u8; POWER_STATUS_WIRE_LEN];
        out[0] = u8::from(self.battery_present);
        out[1] = self.percent;
        out[2] = self.ac.to_byte();
        out[3..7].copy_from_slice(&self.state.to_le_bytes());
        out[7..11].copy_from_slice(&self.rate.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < POWER_STATUS_WIRE_LEN {
            return None;
        }
        if bytes[0] > 1 {
            return None;
        }
        Some(Self {
            battery_present: bytes[0] == 1,
            percent: bytes[1],
            ac: AcState::from_byte(bytes[2])?,
            state: u32::from_le_bytes(bytes[3..7].try_into().ok()?),
            rate: u32::from_le_bytes(bytes[7..11].try_into().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_battery_and_no_battery_shapes() {
        for wire in [
            PowerStatusWire::no_battery(),
            PowerStatusWire {
                battery_present: true,
                percent: 50,
                ac: AcState::Offline,
                state: 0b001,
                rate: 8_760,
            },
            PowerStatusWire {
                battery_present: true,
                percent: 100,
                ac: AcState::Online,
                state: 0b010,
                rate: 0,
            },
        ] {
            let bytes = wire.encode();
            assert_eq!(PowerStatusWire::decode(&bytes), Some(wire));
        }
    }

    #[test]
    fn short_or_junk_input_decodes_to_none() {
        assert_eq!(PowerStatusWire::decode(&[]), None);
        assert_eq!(PowerStatusWire::decode(&[1, 2, 3]), None);
        let mut bad_ac = PowerStatusWire::no_battery().encode();
        bad_ac[2] = 9;
        assert_eq!(PowerStatusWire::decode(&bad_ac), None);
        let mut bad_present = PowerStatusWire::no_battery().encode();
        bad_present[0] = 7;
        assert_eq!(PowerStatusWire::decode(&bad_present), None);
    }
}
