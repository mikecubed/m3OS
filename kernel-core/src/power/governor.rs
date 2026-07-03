//! Phase 103 E.3 — the conservative cpufreq governor (pure logic,
//! host-tested; the `kernel-core::net::dhcp` pattern).
//!
//! The governor maps a per-core load sample onto a **target performance
//! level** on an abstract 0–255 scale (the HWP `IA32_HWP_REQUEST`
//! desired-performance width; the legacy `IA32_PERF_CTL` mechanism maps
//! the scale onto its P-state table). Policy is decoupled from the MSR
//! mechanism (`kernel/src/arch/x86_64/cpufreq.rs`): the kernel ticks
//! [`Governor::next`] with a load sample and applies whatever target
//! comes back.
//!
//! The **conservative** mode steps gradually (one increment per tick in
//! either direction) with hysteresis — the classic Linux-conservative
//! shape — so a load spike does not slam the clock and a quiet system
//! walks down slowly. A Track C thermal cap clamps every mode.

/// Abstract performance scale (matches HWP's 8-bit request fields).
pub const PERF_MIN: u8 = 1;
pub const PERF_MAX: u8 = 255;

/// Load above this steps performance up (percent).
pub const LOAD_UP_THRESHOLD: u8 = 75;
/// Load below this steps performance down (percent).
pub const LOAD_DOWN_THRESHOLD: u8 = 30;
/// Conservative per-tick step (≈1/8 of the scale — reaches full range
/// in ~8 ticks of sustained load).
pub const STEP: u8 = 32;

/// Governor policy mode, settable from userspace (`powerd` / the
/// settings panel) through the Track E syscall surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GovernorMode {
    /// Pin the maximum (thermal cap still applies).
    Performance,
    /// Pin the minimum.
    Powersave,
    /// Load-following with hysteresis (the default).
    Conservative,
}

impl GovernorMode {
    /// Stable wire/display encoding.
    pub fn as_str(&self) -> &'static str {
        match self {
            GovernorMode::Performance => "performance",
            GovernorMode::Powersave => "powersave",
            GovernorMode::Conservative => "conservative",
        }
    }

    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(GovernorMode::Conservative),
            1 => Some(GovernorMode::Performance),
            2 => Some(GovernorMode::Powersave),
            _ => None,
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            GovernorMode::Conservative => 0,
            GovernorMode::Performance => 1,
            GovernorMode::Powersave => 2,
        }
    }
}

/// The per-core governor state machine.
#[derive(Clone, Copy, Debug)]
pub struct Governor {
    pub mode: GovernorMode,
    current: u8,
}

impl Governor {
    pub fn new(mode: GovernorMode) -> Self {
        Self {
            mode,
            // Start mid-scale: the first ticks settle toward reality
            // without either a cold-start crawl or a full-bore spin-up.
            current: PERF_MAX / 2,
        }
    }

    /// The last target this governor produced.
    pub fn current(&self) -> u8 {
        self.current
    }

    /// One governor tick: fold a load sample (0–100 %) and an optional
    /// thermal cap into the next target performance level.
    pub fn next(&mut self, load_pct: u8, thermal_cap: Option<u8>) -> u8 {
        let cap = thermal_cap.unwrap_or(PERF_MAX).max(PERF_MIN);
        let target = match self.mode {
            GovernorMode::Performance => PERF_MAX,
            GovernorMode::Powersave => PERF_MIN,
            GovernorMode::Conservative => {
                if load_pct >= LOAD_UP_THRESHOLD {
                    self.current.saturating_add(STEP)
                } else if load_pct <= LOAD_DOWN_THRESHOLD {
                    self.current.saturating_sub(STEP)
                } else {
                    self.current // hysteresis band: hold
                }
            }
        };
        self.current = target.clamp(PERF_MIN, cap);
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_ramps_up_gradually_under_load() {
        let mut g = Governor::new(GovernorMode::Conservative);
        let start = g.current();
        let mut last = start;
        let mut ticks = 0;
        while g.next(100, None) < PERF_MAX {
            let now = g.current();
            assert!(now > last, "must increase every tick under full load");
            assert!(now - last <= STEP, "never jumps more than one step");
            last = now;
            ticks += 1;
            assert!(ticks < 32, "must converge");
        }
        assert_eq!(g.current(), PERF_MAX);
        assert!(ticks >= 2, "full range takes multiple ticks (gradual)");
    }

    #[test]
    fn conservative_walks_down_when_idle() {
        let mut g = Governor::new(GovernorMode::Conservative);
        for _ in 0..16 {
            g.next(100, None);
        }
        assert_eq!(g.current(), PERF_MAX);
        let mut ticks = 0;
        while g.next(0, None) > PERF_MIN {
            ticks += 1;
            assert!(ticks < 32, "must converge to the floor");
        }
        assert_eq!(g.current(), PERF_MIN);
    }

    #[test]
    fn hysteresis_band_holds_steady() {
        let mut g = Governor::new(GovernorMode::Conservative);
        let settled = g.next(50, None);
        for _ in 0..8 {
            assert_eq!(g.next(50, None), settled, "mid-band load must hold");
        }
    }

    #[test]
    fn thermal_cap_clamps_every_mode() {
        for mode in [
            GovernorMode::Performance,
            GovernorMode::Conservative,
            GovernorMode::Powersave,
        ] {
            let mut g = Governor::new(mode);
            for _ in 0..16 {
                let t = g.next(100, Some(64));
                assert!(t <= 64, "{mode:?} exceeded the thermal cap");
                assert!(t >= PERF_MIN);
            }
        }
        // Cap release: performance snaps back to max next tick.
        let mut g = Governor::new(GovernorMode::Performance);
        g.next(100, Some(64));
        assert_eq!(g.next(100, None), PERF_MAX);
    }

    #[test]
    fn fixed_modes_pin_their_ends() {
        let mut hi = Governor::new(GovernorMode::Performance);
        assert_eq!(hi.next(0, None), PERF_MAX);
        let mut lo = Governor::new(GovernorMode::Powersave);
        assert_eq!(lo.next(100, None), PERF_MIN);
    }

    #[test]
    fn mode_bytes_round_trip() {
        for mode in [
            GovernorMode::Conservative,
            GovernorMode::Performance,
            GovernorMode::Powersave,
        ] {
            assert_eq!(GovernorMode::from_byte(mode.to_byte()), Some(mode));
        }
        assert_eq!(GovernorMode::from_byte(9), None);
    }
}
