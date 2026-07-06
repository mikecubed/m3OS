//! x86_64 debug-register (`DR6`/`DR7`) bit encode/decode — Phase 111 Track B.2.
//!
//! Pure logic, host-testable: the `#DB` handler and the `DebugRegs` hardware
//! wrapper in `kernel/src/arch/x86_64/debug.rs` translate raw `DR6`/`DR7`
//! values through here. Intel SDM Vol 3 §17.2.
//!
//! `DR7` layout (per breakpoint slot `i` in 0..4):
//! - bit `2i`     — local enable `Li`
//! - bit `2i+1`   — global enable `Gi`
//! - bits `16+4i..18+4i` — R/W condition `R/Wi`
//! - bits `18+4i..20+4i` — length `LENi`
//!
//! `DR6` status bits: `B0`–`B3` (bits 0–3, slot hit), `BD` (13, debug-reg
//! access), `BS` (14, single-step), `BT` (15, task switch).

/// Break condition for a hardware breakpoint slot (`DR7` R/W field).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BreakCondition {
    /// Instruction execution (R/W = 00). LEN must be 1 byte.
    Execute,
    /// Data write (R/W = 01).
    Write,
    /// I/O read/write (R/W = 10, requires `CR4.DE`).
    IoReadWrite,
    /// Data read or write (R/W = 11).
    ReadWrite,
}

impl BreakCondition {
    #[inline]
    fn to_bits(self) -> u64 {
        match self {
            BreakCondition::Execute => 0b00,
            BreakCondition::Write => 0b01,
            BreakCondition::IoReadWrite => 0b10,
            BreakCondition::ReadWrite => 0b11,
        }
    }

    #[inline]
    fn from_bits(bits: u64) -> Self {
        match bits & 0b11 {
            0b00 => BreakCondition::Execute,
            0b01 => BreakCondition::Write,
            0b10 => BreakCondition::IoReadWrite,
            _ => BreakCondition::ReadWrite,
        }
    }
}

/// Watched length for a hardware breakpoint slot (`DR7` LEN field).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BreakLength {
    /// 1 byte (LEN = 00). Required for `Execute` breakpoints.
    One,
    /// 2 bytes (LEN = 01).
    Two,
    /// 8 bytes (LEN = 10, on CPUs that support it).
    Eight,
    /// 4 bytes (LEN = 11).
    Four,
}

impl BreakLength {
    #[inline]
    fn to_bits(self) -> u64 {
        match self {
            BreakLength::One => 0b00,
            BreakLength::Two => 0b01,
            BreakLength::Eight => 0b10,
            BreakLength::Four => 0b11,
        }
    }

    #[inline]
    fn from_bits(bits: u64) -> Self {
        match bits & 0b11 {
            0b00 => BreakLength::One,
            0b01 => BreakLength::Two,
            0b10 => BreakLength::Eight,
            _ => BreakLength::Four,
        }
    }
}

/// Configuration for one of the four hardware breakpoint slots.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlotConfig {
    pub condition: BreakCondition,
    pub length: BreakLength,
}

/// `DR6` single-step (`BS`) bit.
pub const DR6_BS: u64 = 1 << 14;
/// `DR6` debug-register-access (`BD`) bit.
pub const DR6_BD: u64 = 1 << 13;
/// `DR6` task-switch (`BT`) bit.
pub const DR6_BT: u64 = 1 << 15;
/// Bits the kernel writes back to clear `DR6` after servicing (the sticky
/// status bits `B0`–`B3`, `BD`, `BS`, `BT`; the reserved high bits read as 1).
pub const DR6_STATUS_MASK: u64 = 0b1110_0000_0000_1111;

/// Build the `DR7` bits for a single slot `i` (0..4). Returns the enable +
/// R/W + LEN bits ORed into their slot positions; caller ORs the four slots.
/// Uses the **local** enable (`Li`) — kernel breakpoints are per-CPU and do not
/// need the global-enable's task-switch semantics.
pub fn dr7_slot_bits(slot: usize, cfg: SlotConfig) -> u64 {
    debug_assert!(slot < 4);
    let local_enable = 1u64 << (2 * slot);
    let rw = cfg.condition.to_bits() << (16 + 4 * slot);
    let len = cfg.length.to_bits() << (18 + 4 * slot);
    local_enable | rw | len
}

/// Assemble a full `DR7` from four optional slot configs. A `None` slot is
/// disabled (its enable bit and R/W/LEN fields are zero).
pub fn dr7_encode(slots: [Option<SlotConfig>; 4]) -> u64 {
    let mut dr7 = 0u64;
    for (i, slot) in slots.iter().enumerate() {
        if let Some(cfg) = slot {
            dr7 |= dr7_slot_bits(i, *cfg);
        }
    }
    dr7
}

/// True if slot `i`'s local enable (`Li`) bit is set in `dr7`.
pub fn dr7_slot_enabled(dr7: u64, slot: usize) -> bool {
    debug_assert!(slot < 4);
    dr7 & (1 << (2 * slot)) != 0
}

/// Decode slot `i`'s condition + length from `dr7` (regardless of enable).
pub fn dr7_slot_config(dr7: u64, slot: usize) -> SlotConfig {
    debug_assert!(slot < 4);
    let rw = (dr7 >> (16 + 4 * slot)) & 0b11;
    let len = (dr7 >> (18 + 4 * slot)) & 0b11;
    SlotConfig {
        condition: BreakCondition::from_bits(rw),
        length: BreakLength::from_bits(len),
    }
}

/// Decoded `DR6` status after a `#DB`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Dr6Status {
    /// Which of the four hardware breakpoint slots triggered (`B0`–`B3`).
    pub slot_hit: [bool; 4],
    /// Single-step trap (`BS`).
    pub single_step: bool,
    /// Debug-register access trap (`BD`).
    pub debug_access: bool,
    /// Task-switch trap (`BT`).
    pub task_switch: bool,
}

/// Decode a raw `DR6` value into its status bits.
pub fn dr6_decode(dr6: u64) -> Dr6Status {
    Dr6Status {
        slot_hit: [
            dr6 & (1 << 0) != 0,
            dr6 & (1 << 1) != 0,
            dr6 & (1 << 2) != 0,
            dr6 & (1 << 3) != 0,
        ],
        debug_access: dr6 & DR6_BD != 0,
        single_step: dr6 & DR6_BS != 0,
        task_switch: dr6 & DR6_BT != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dr7_execute_slot0() {
        // Slot 0, execute breakpoint, 1-byte: L0=bit0, R/W0=00, LEN0=00.
        let bits = dr7_slot_bits(
            0,
            SlotConfig {
                condition: BreakCondition::Execute,
                length: BreakLength::One,
            },
        );
        assert_eq!(bits, 0b1, "only the L0 enable bit is set for exec/1-byte");
        assert!(dr7_slot_enabled(bits, 0));
        assert!(!dr7_slot_enabled(bits, 1));
    }

    #[test]
    fn dr7_write_watchpoint_slot2() {
        // Slot 2, data write, 4 bytes: L2=bit4, R/W2 at bits 24-25 = 01,
        // LEN2 at bits 26-27 = 11.
        let cfg = SlotConfig {
            condition: BreakCondition::Write,
            length: BreakLength::Four,
        };
        let bits = dr7_slot_bits(2, cfg);
        assert_eq!(bits, (1 << 4) | (0b01 << 24) | (0b11 << 26));
        assert!(dr7_slot_enabled(bits, 2));
        assert_eq!(dr7_slot_config(bits, 2), cfg);
    }

    #[test]
    fn dr7_roundtrip_all_slots() {
        let slots = [
            Some(SlotConfig {
                condition: BreakCondition::Execute,
                length: BreakLength::One,
            }),
            Some(SlotConfig {
                condition: BreakCondition::ReadWrite,
                length: BreakLength::Eight,
            }),
            None,
            Some(SlotConfig {
                condition: BreakCondition::Write,
                length: BreakLength::Two,
            }),
        ];
        let dr7 = dr7_encode(slots);
        for (i, want) in slots.iter().enumerate() {
            assert_eq!(dr7_slot_enabled(dr7, i), want.is_some(), "slot {i} enable");
            if let Some(cfg) = want {
                assert_eq!(dr7_slot_config(dr7, i), *cfg, "slot {i} config");
            }
        }
    }

    #[test]
    fn dr6_single_step() {
        let s = dr6_decode(DR6_BS | 0xffff_0000); // BS + reserved high bits
        assert!(s.single_step);
        assert_eq!(s.slot_hit, [false; 4]);
        assert!(!s.debug_access && !s.task_switch);
    }

    #[test]
    fn dr6_breakpoint_slots() {
        let s = dr6_decode(0b1010); // B1 and B3 set
        assert_eq!(s.slot_hit, [false, true, false, true]);
        assert!(!s.single_step);
    }

    #[test]
    fn dr6_bd_bt() {
        assert!(dr6_decode(DR6_BD).debug_access);
        assert!(dr6_decode(DR6_BT).task_switch);
    }

    #[test]
    fn condition_length_bit_roundtrip() {
        for c in [
            BreakCondition::Execute,
            BreakCondition::Write,
            BreakCondition::IoReadWrite,
            BreakCondition::ReadWrite,
        ] {
            assert_eq!(BreakCondition::from_bits(c.to_bits()), c);
        }
        for l in [
            BreakLength::One,
            BreakLength::Two,
            BreakLength::Eight,
            BreakLength::Four,
        ] {
            assert_eq!(BreakLength::from_bits(l.to_bits()), l);
        }
    }
}
