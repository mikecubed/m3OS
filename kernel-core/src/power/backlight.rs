//! Phase 103 B — pure-logic backlight decode + level mapping (ACPI
//! Appendix B, `_BCL`/`_BCM`/`_BQC`). Host-tested; the evaluation rides
//! acpid's IPC like the battery/thermal objects.
//!
//! `_BCL` returns a package of integers: elements 0/1 are the
//! recommended full-power/battery levels, the remainder the supported
//! level list (unordered and possibly duplicated in real firmware —
//! Dell tables famously repeat the defaults). Levels are conventionally
//! percentages but the spec only requires them to be monotonic in
//! brightness, so the mapping here works on the sorted list itself.
//!
//! **Native PWM fallback (Phase 103 B.2, documented-only):** some
//! panels expose a stub `_BCM` and are really driven by the Intel GPU's
//! backlight PWM — `BLC_PWM_CTL`/`BLC_PWM_DATA` (Tiger Lake:
//! `0xC8250`/`0xC8254` in the GT MMIO BAR, `SBLC_PWM_CTL2` on the PCH
//! path), duty-cycle = level/max into the DATA register. That is
//! GPU-register work with no ACPI abstraction, so it is scoped as a
//! deferred fallback: if the Dell reference panel's `_BCM` proves to be
//! a no-op during hardware validation, the PWM path gets flagged as a
//! follow-on rather than silently failing (`_BQC` read-back after
//! `_BCM` is the detection — a stub leaves the level unchanged).

use alloc::vec::Vec;

use super::super::acpi::aml::object::AmlValue;

/// Wire sentinel for "no backlight device / unknown level".
pub const BACKLIGHT_UNKNOWN: u8 = 0xFF;

/// A decoded `_BCL` package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BclLevels {
    /// Element 0 — recommended level on AC power.
    pub ac_default: u32,
    /// Element 1 — recommended level on battery.
    pub battery_default: u32,
    /// The supported levels, sorted ascending, deduplicated.
    pub levels: Vec<u32>,
}

/// Decode a `_BCL` evaluation result. Requires the two defaults plus at
/// least two distinct levels (a single-level "list" cannot be a
/// control); rejects non-integer elements.
pub fn decode_bcl(value: &AmlValue) -> Option<BclLevels> {
    let AmlValue::Package(elems) = value else {
        return None;
    };
    if elems.len() < 4 {
        return None;
    }
    let mut ints = Vec::with_capacity(elems.len());
    for e in elems {
        match e {
            AmlValue::Integer(v) if *v <= u32::MAX as u64 => ints.push(*v as u32),
            _ => return None,
        }
    }
    let ac_default = ints[0];
    let battery_default = ints[1];
    let mut levels: Vec<u32> = ints[2..].to_vec();
    levels.sort_unstable();
    levels.dedup();
    if levels.len() < 2 {
        return None;
    }
    Some(BclLevels {
        ac_default,
        battery_default,
        levels,
    })
}

/// Decode a `_BQC` result (the current level, one of the `_BCL` values
/// on conformant firmware — tolerated when it is not).
pub fn decode_bqc(value: &AmlValue) -> Option<u32> {
    match value {
        AmlValue::Integer(v) if *v <= u32::MAX as u64 => Some(*v as u32),
        _ => None,
    }
}

impl BclLevels {
    /// Map a 0–100 percent onto the supported level whose *position* in
    /// the sorted list is nearest — position-based so a sparse list
    /// ({0, 50, 100}) still spreads the percent range evenly, and a
    /// non-percentage unit list still lands on a legal value.
    pub fn nearest_level(&self, pct: u8) -> u32 {
        let pct = pct.min(100) as usize;
        let n = self.levels.len();
        // Round-to-nearest index over 0..n-1.
        let idx = (pct * (n - 1) + 50) / 100;
        self.levels[idx]
    }

    /// Inverse of [`Self::nearest_level`]: the percent a level sits at
    /// within the sorted list (nearest match when the level is not in
    /// the list — non-conformant `_BQC`s happen).
    pub fn level_to_percent(&self, level: u32) -> u8 {
        let n = self.levels.len();
        let idx = match self.levels.binary_search(&level) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) if i >= n => n - 1,
            // Between two levels: pick the closer one.
            Err(i) => {
                if level - self.levels[i - 1] <= self.levels[i] - level {
                    i - 1
                } else {
                    i
                }
            }
        };
        ((idx * 100 + (n - 1) / 2) / (n - 1)) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn pkg(ints: &[u64]) -> AmlValue {
        AmlValue::Package(ints.iter().map(|&v| AmlValue::Integer(v)).collect())
    }

    #[test]
    fn decodes_dell_shaped_bcl() {
        // Dell-shaped: defaults repeated in the level list, unordered.
        let bcl = decode_bcl(&pkg(&[80, 50, 100, 80, 60, 40, 20, 0, 50])).expect("decodes");
        assert_eq!(bcl.ac_default, 80);
        assert_eq!(bcl.battery_default, 50);
        assert_eq!(bcl.levels, [0, 20, 40, 50, 60, 80, 100]);
    }

    #[test]
    fn rejects_junk_bcl() {
        assert_eq!(decode_bcl(&AmlValue::Integer(50)), None);
        assert_eq!(decode_bcl(&pkg(&[80, 50, 100])), None); // too short
        assert_eq!(decode_bcl(&pkg(&[80, 50, 100, 100])), None); // one distinct level
        let mut mixed = vec![
            AmlValue::Integer(80),
            AmlValue::Integer(50),
            AmlValue::Integer(0),
            AmlValue::Buffer(vec![1]),
        ];
        assert_eq!(
            decode_bcl(&AmlValue::Package(core::mem::take(&mut mixed))),
            None
        );
    }

    #[test]
    fn percent_maps_to_nearest_level_and_back() {
        let bcl = decode_bcl(&pkg(&[80, 50, 0, 25, 50, 75, 100])).expect("decodes");
        assert_eq!(bcl.nearest_level(0), 0);
        assert_eq!(bcl.nearest_level(100), 100);
        assert_eq!(bcl.nearest_level(50), 50);
        assert_eq!(bcl.nearest_level(60), 50); // rounds to nearest position
        assert_eq!(bcl.nearest_level(63), 75);
        assert_eq!(bcl.nearest_level(200), 100); // clamped

        assert_eq!(bcl.level_to_percent(0), 0);
        assert_eq!(bcl.level_to_percent(50), 50);
        assert_eq!(bcl.level_to_percent(100), 100);
        // Non-conformant _BQC value between levels snaps to the closer one.
        assert_eq!(bcl.level_to_percent(70), 75);
    }

    #[test]
    fn sparse_list_spreads_percent_range() {
        let bcl = decode_bcl(&pkg(&[100, 0, 0, 50, 100])).expect("decodes");
        assert_eq!(bcl.nearest_level(0), 0);
        assert_eq!(bcl.nearest_level(24), 0);
        assert_eq!(bcl.nearest_level(26), 50);
        assert_eq!(bcl.nearest_level(74), 50);
        assert_eq!(bcl.nearest_level(76), 100);
        // Round-trips stay stable: set(pct) → level → percent → same level.
        for pct in [0u8, 30, 50, 80, 100] {
            let level = bcl.nearest_level(pct);
            assert_eq!(bcl.nearest_level(bcl.level_to_percent(level)), level);
        }
    }

    #[test]
    fn bqc_decode() {
        assert_eq!(decode_bqc(&AmlValue::Integer(60)), Some(60));
        assert_eq!(decode_bqc(&pkg(&[1, 2, 3, 4])), None);
    }
}
