//! Phase 103 A/C/E — the `power` IPC control protocol between `powerd`
//! (server) and its clients (`m3ctl`, the Phase 105 settings panel).
//!
//! Mirrors the `wifi_core::control` shape: request labels + a
//! length-stable byte codec with host-tested round-trips. A `POWER_STATUS`
//! request carries no body; the reply bulk is one encoded
//! [`PowerStatusWire`].

use super::governor::GovernorMode;
use super::thermal::ThermalState;

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

/// Sentinel for [`PowerStatusWire::temp_deci_c`] when the platform
/// declares no thermal zones (QEMU q35) or every `_TMP` read failed.
pub const TEMP_UNKNOWN_DECI_C: i16 = i16::MIN;

/// Thermal posture across all zones (worst wins), plus the "platform
/// has no zones" case the VM lanes exercise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThermalWire {
    NoZones,
    Normal,
    Passive,
    Critical,
}

impl ThermalWire {
    pub fn from_state(s: ThermalState) -> Self {
        match s {
            ThermalState::Normal => ThermalWire::Normal,
            ThermalState::Passive => ThermalWire::Passive,
            ThermalState::Critical => ThermalWire::Critical,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ThermalWire::NoZones => "none",
            ThermalWire::Normal => "normal",
            ThermalWire::Passive => "passive",
            ThermalWire::Critical => "critical",
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            ThermalWire::NoZones => 0,
            ThermalWire::Normal => 1,
            ThermalWire::Passive => 2,
            ThermalWire::Critical => 3,
        }
    }

    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(ThermalWire::NoZones),
            1 => Some(ThermalWire::Normal),
            2 => Some(ThermalWire::Passive),
            3 => Some(ThermalWire::Critical),
            _ => None,
        }
    }
}

/// The cpufreq mechanism the kernel probed (Track E). Mirrors
/// `super::syscalls::CPUFREQ_MECH_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpufreqMech {
    /// No HWP on this CPU (every QEMU TCG/KVM lane) — governor targets
    /// are computed but nothing is applied.
    None,
    /// Intel HWP: targets map onto `IA32_HWP_REQUEST`.
    Hwp,
}

impl CpufreqMech {
    pub fn as_str(&self) -> &'static str {
        match self {
            CpufreqMech::None => "none",
            CpufreqMech::Hwp => "hwp",
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            CpufreqMech::None => 0,
            CpufreqMech::Hwp => 1,
        }
    }

    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(CpufreqMech::None),
            1 => Some(CpufreqMech::Hwp),
            _ => None,
        }
    }
}

/// The `POWER_STATUS` reply payload.
///
/// Wire layout (18 bytes LE):
/// `battery_present[1] | percent[1] | ac[1] | state[4] | rate[4] |
///  temp_deci_c[2] | thermal[1] | governor[1] | mech[1] | perf[1] | reserved[1]`
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
    /// Hottest zone's `_TMP` in deci-celsius, or [`TEMP_UNKNOWN_DECI_C`].
    pub temp_deci_c: i16,
    /// Worst thermal posture across zones (Track C).
    pub thermal: ThermalWire,
    /// Active governor mode (Track E).
    pub governor: GovernorMode,
    /// Probed cpufreq mechanism.
    pub mech: CpufreqMech,
    /// Last governor target on the abstract 1–255 performance scale.
    pub perf: u8,
}

/// Encoded size of [`PowerStatusWire`].
pub const POWER_STATUS_WIRE_LEN: usize = 18;

impl PowerStatusWire {
    /// The no-battery, no-thermal-zone platform snapshot (QEMU/desktop),
    /// before the governor reports in.
    pub fn no_battery() -> Self {
        Self {
            battery_present: false,
            percent: PERCENT_UNKNOWN,
            ac: AcState::AssumedOnline,
            state: 0,
            rate: 0,
            temp_deci_c: TEMP_UNKNOWN_DECI_C,
            thermal: ThermalWire::NoZones,
            governor: GovernorMode::Conservative,
            mech: CpufreqMech::None,
            perf: 0,
        }
    }

    pub fn encode(&self) -> [u8; POWER_STATUS_WIRE_LEN] {
        let mut out = [0u8; POWER_STATUS_WIRE_LEN];
        out[0] = u8::from(self.battery_present);
        out[1] = self.percent;
        out[2] = self.ac.to_byte();
        out[3..7].copy_from_slice(&self.state.to_le_bytes());
        out[7..11].copy_from_slice(&self.rate.to_le_bytes());
        out[11..13].copy_from_slice(&self.temp_deci_c.to_le_bytes());
        out[13] = self.thermal.to_byte();
        out[14] = self.governor.to_byte();
        out[15] = self.mech.to_byte();
        out[16] = self.perf;
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
            temp_deci_c: i16::from_le_bytes(bytes[11..13].try_into().ok()?),
            thermal: ThermalWire::from_byte(bytes[13])?,
            governor: GovernorMode::from_byte(bytes[14])?,
            mech: CpufreqMech::from_byte(bytes[15])?,
            perf: bytes[16],
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
                temp_deci_c: 421, // 42.1 °C
                thermal: ThermalWire::Normal,
                governor: GovernorMode::Conservative,
                mech: CpufreqMech::Hwp,
                perf: 128,
            },
            PowerStatusWire {
                battery_present: true,
                percent: 100,
                ac: AcState::Online,
                state: 0b010,
                rate: 0,
                temp_deci_c: 953,
                thermal: ThermalWire::Critical,
                governor: GovernorMode::Powersave,
                mech: CpufreqMech::Hwp,
                perf: 1,
            },
        ] {
            let bytes = wire.encode();
            assert_eq!(PowerStatusWire::decode(&bytes), Some(wire));
        }
    }

    #[test]
    fn negative_temperature_survives_the_wire() {
        let wire = PowerStatusWire {
            temp_deci_c: -100, // -10.0 °C
            thermal: ThermalWire::Normal,
            ..PowerStatusWire::no_battery()
        };
        assert_eq!(PowerStatusWire::decode(&wire.encode()), Some(wire));
    }

    #[test]
    fn short_or_junk_input_decodes_to_none() {
        assert_eq!(PowerStatusWire::decode(&[]), None);
        assert_eq!(PowerStatusWire::decode(&[1, 2, 3]), None);
        // A 14-byte slice-1 frame is short for the slice-2 codec: both
        // sides ship together, so old frames must not half-decode.
        assert_eq!(PowerStatusWire::decode(&[0u8; 14]), None);
        let mut bad_ac = PowerStatusWire::no_battery().encode();
        bad_ac[2] = 9;
        assert_eq!(PowerStatusWire::decode(&bad_ac), None);
        let mut bad_present = PowerStatusWire::no_battery().encode();
        bad_present[0] = 7;
        assert_eq!(PowerStatusWire::decode(&bad_present), None);
        let mut bad_thermal = PowerStatusWire::no_battery().encode();
        bad_thermal[13] = 9;
        assert_eq!(PowerStatusWire::decode(&bad_thermal), None);
    }
}
