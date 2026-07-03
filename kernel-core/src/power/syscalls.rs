//! Phase 103 E — the power syscall family (`0x116x`).
//!
//! Declared here (single source of truth, the `spectre::SYS_*` pattern)
//! so the kernel dispatcher and the `powerd` userspace caller share the
//! same numbers and wire layout. `0x1150` is taken by the feature-gated
//! kstack-overflow debug probe, so this family opens at `0x1160`.
//!
//! Division of labor (charter correction, mirrors slice 1): the
//! governor *policy* ticks in ring-3 `powerd` (userspace-first rule);
//! ring 0 keeps only the genuinely privileged mechanism — the HWP MSR
//! writes — plus a read-only status/load snapshot for the policy loop.

/// Apply a governor target on the abstract 1–255 performance scale.
/// `sys_power_set_perf(target) -> isize` (0 on success; `-EPERM` for
/// non-root callers; 0 with no effect when no mechanism was probed).
pub const SYS_POWER_SET_PERF: u64 = 0x1160;

/// Copy a [`CpufreqStatusWire`] snapshot (mechanism + last target +
/// cumulative CPU times for load sampling) into a user buffer.
/// `sys_power_cpufreq_status(buf_ptr, buf_len) -> isize` (bytes
/// written, or a negative errno).
pub const SYS_POWER_CPUFREQ_STATUS: u64 = 0x1161;

/// Enter ACPI S3 (suspend-to-RAM) and return after resume:
/// `sys_power_enter_sleep() -> isize` (0 = resumed successfully;
/// `-ENOSYS` when the platform never registered `\_S3` / has no FACS;
/// other negative errno on a failed entry — always fail-closed to a
/// live system). Root-only. The caller (`powerd`) evaluates `\_PTS(3)`
/// before and `\_WAK(3)` after, per the Phase 101 ring-3 AML split.
pub const SYS_POWER_ENTER_SLEEP: u64 = 0x1162;

/// [`CpufreqStatusWire::mechanism`] values.
pub const CPUFREQ_MECH_NONE: u8 = 0;
pub const CPUFREQ_MECH_HWP: u8 = 1;

/// Fixed wire length of an encoded [`CpufreqStatusWire`].
pub const CPUFREQ_STATUS_WIRE_LEN: usize = 28;

/// The kernel's cpufreq snapshot: what mechanism the CPUID probe found,
/// the last applied target, the HWP capability range (0 when absent),
/// and the scheduler's cumulative CPU times — `powerd` diffs successive
/// `user+system` vs `idle` samples into a load percentage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpufreqStatusWire {
    /// One of `CPUFREQ_MECH_*`.
    pub mechanism: u8,
    /// Last target applied via [`SYS_POWER_SET_PERF`] (0 = never).
    pub last_target: u8,
    /// `IA32_HWP_CAPABILITIES` highest/lowest performance (0 when no HWP).
    pub hwp_highest: u8,
    pub hwp_lowest: u8,
    /// Cumulative scheduler CPU times in ticks (units cancel in the
    /// load ratio).
    pub user_ticks: u64,
    pub system_ticks: u64,
    pub idle_ticks: u64,
}

impl CpufreqStatusWire {
    pub fn encode(&self) -> [u8; CPUFREQ_STATUS_WIRE_LEN] {
        let mut out = [0u8; CPUFREQ_STATUS_WIRE_LEN];
        out[0] = self.mechanism;
        out[1] = self.last_target;
        out[2] = self.hwp_highest;
        out[3] = self.hwp_lowest;
        out[4..12].copy_from_slice(&self.user_ticks.to_le_bytes());
        out[12..20].copy_from_slice(&self.system_ticks.to_le_bytes());
        out[20..28].copy_from_slice(&self.idle_ticks.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < CPUFREQ_STATUS_WIRE_LEN {
            return None;
        }
        if bytes[0] > CPUFREQ_MECH_HWP {
            return None;
        }
        Some(Self {
            mechanism: bytes[0],
            last_target: bytes[1],
            hwp_highest: bytes[2],
            hwp_lowest: bytes[3],
            user_ticks: u64::from_le_bytes(bytes[4..12].try_into().ok()?),
            system_ticks: u64::from_le_bytes(bytes[12..20].try_into().ok()?),
            idle_ticks: u64::from_le_bytes(bytes[20..28].try_into().ok()?),
        })
    }

    /// Fold the delta between two snapshots into a 0–100 load percent
    /// (busy = user + system). Returns `None` when no time elapsed.
    pub fn load_pct_since(&self, prev: &Self) -> Option<u8> {
        let busy = (self.user_ticks + self.system_ticks)
            .checked_sub(prev.user_ticks + prev.system_ticks)?;
        let idle = self.idle_ticks.checked_sub(prev.idle_ticks)?;
        let total = busy + idle;
        if total == 0 {
            return None;
        }
        Some(((busy * 100) / total).min(100) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_numbers_are_pinned() {
        // ABI pins: renumbering breaks every compiled userspace binary.
        assert_eq!(SYS_POWER_SET_PERF, 0x1160);
        assert_eq!(SYS_POWER_CPUFREQ_STATUS, 0x1161);
        assert_eq!(SYS_POWER_ENTER_SLEEP, 0x1162);
        assert_eq!(CPUFREQ_STATUS_WIRE_LEN, 28);
    }

    #[test]
    fn status_wire_round_trips() {
        for wire in [
            CpufreqStatusWire {
                mechanism: CPUFREQ_MECH_NONE,
                last_target: 0,
                hwp_highest: 0,
                hwp_lowest: 0,
                user_ticks: 0,
                system_ticks: 0,
                idle_ticks: 0,
            },
            CpufreqStatusWire {
                mechanism: CPUFREQ_MECH_HWP,
                last_target: 160,
                hwp_highest: 42,
                hwp_lowest: 4,
                user_ticks: 123_456,
                system_ticks: 7_890,
                idle_ticks: u64::MAX / 2,
            },
        ] {
            assert_eq!(CpufreqStatusWire::decode(&wire.encode()), Some(wire));
        }
        assert_eq!(CpufreqStatusWire::decode(&[0u8; 27]), None);
        let mut bad = [0u8; CPUFREQ_STATUS_WIRE_LEN];
        bad[0] = 9;
        assert_eq!(CpufreqStatusWire::decode(&bad), None);
    }

    #[test]
    fn load_percentage_from_deltas() {
        let zero = CpufreqStatusWire {
            mechanism: 0,
            last_target: 0,
            hwp_highest: 0,
            hwp_lowest: 0,
            user_ticks: 0,
            system_ticks: 0,
            idle_ticks: 0,
        };
        let busy_75 = CpufreqStatusWire {
            user_ticks: 600,
            system_ticks: 150,
            idle_ticks: 250,
            ..zero
        };
        assert_eq!(busy_75.load_pct_since(&zero), Some(75));
        // No elapsed time → no sample (the tick must hold, not step).
        assert_eq!(zero.load_pct_since(&zero), None);
        // Counters running backwards (never expected) → no sample.
        assert_eq!(zero.load_pct_since(&busy_75), None);
        // Fully idle interval.
        let idle_only = CpufreqStatusWire {
            idle_ticks: 1000,
            ..zero
        };
        assert_eq!(idle_only.load_pct_since(&zero), Some(0));
    }
}
