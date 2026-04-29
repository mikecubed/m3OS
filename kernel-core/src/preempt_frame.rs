//! Phase 57b — Track A.4: `PreemptFrame` save-area layout.
//!
//! This module pins the **register layout** that Phase 57d's assembly
//! entry stub will populate when a preemption fires.  The Rust struct and
//! the assembly stub must agree on every field offset; this file is the
//! source of truth, and the [`const _: () = ...`] assertions below fail
//! the build immediately on any layout drift.
//!
//! ### Why a separate save area?
//!
//! Today's [`switch_context`](https://docs.rs/m3os/latest/m3os/task/scheduler/fn.switch_context.html)
//! saves only callee-saved registers (`rbx`, `rbp`, `r12`–`r15`, `rsp`,
//! `rip`).  That is fine for cooperative yields, where the call boundary
//! ABI guarantees the caller-saved registers are dead.  A pre-emption
//! point fires *mid-instruction-stream* — every register may be live —
//! so the entry stub must save the full GPR set plus the IRET frame
//! (`rip`, `cs`, `rflags`, `rsp`, `ss`).
//!
//! ### Two CPU-frame shapes, one [`PreemptFrame`]
//!
//! When a preemption is taken from **ring 3** (user mode), the CPU pushes
//! a five-field IRET frame onto the new privilege stack: `rip`, `cs`,
//! `rflags`, `rsp`, `ss`.
//!
//! When a preemption is taken from **ring 0** (kernel mode, full
//! preemption), the CPU pushes only a three-field IRET frame: `rip`,
//! `cs`, `rflags` (no privilege change → the existing `rsp` is unchanged
//! and `ss` is not touched).
//!
//! The 57d assembly entry stub adapts both shapes into the *same*
//! [`PreemptFrame`] layout — the ring-0 path synthesises `rsp` from the
//! current stack pointer and `ss` from the kernel data segment selector.
//! This module pins the canonical layout the stub writes to, regardless
//! of which ring the interrupt fired in.
//!
//! ### Field order (stable ABI with the assembly stub)
//!
//! 1. General-purpose registers: `rax`, `rbx`, `rcx`, `rdx`, `rsi`,
//!    `rdi`, `rbp`, `r8`–`r15` (all `u64`).  Total: 15 GPRs × 8 bytes =
//!    120 bytes.
//! 2. CPU frame: `rip`, `cs`, `rflags`, `rsp`, `ss` (all `u64`,
//!    pushed by the CPU on the IRET frame plus synthesised values for
//!    the ring-0 entry path).  Total: 5 fields × 8 bytes = 40 bytes.
//!
//! Total layout: **160 bytes**, all `u64`-aligned, `#[repr(C)]`.
//!
//! Source ref: phase-57b-track-A.4

use core::mem::offset_of;

/// Save area for a preempted task.
///
/// Phase 57d's assembly entry stub writes every GPR plus the
/// IRET-frame fields into this struct.  Phase 57e's resume routine reads
/// the same struct in the same layout to issue an `iretq` back to the
/// preempted instruction.
///
/// `#[repr(C)]` guarantees the field order matches the source-text order
/// below.  The compile-time assertions at the bottom of this file fail
/// the build if any offset drifts; that is the regression gate the
/// assembly stub depends on.
///
/// On a ring-3 preemption the CPU's pushed IRET frame (`rip`, `cs`,
/// `rflags`, `rsp`, `ss`) lands directly into the matching fields.  On a
/// ring-0 preemption the CPU pushes only `rip`, `cs`, `rflags`; the
/// entry stub synthesises `rsp` (current `rsp` after the push) and `ss`
/// (kernel data-segment selector) into the remaining slots so the resume
/// path is uniform.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct PreemptFrame {
    /// Saved RAX.  GPRs are saved in the canonical "push-all" order so
    /// the assembly stub can use a deterministic series of `mov`
    /// instructions; this minimises the chance of a register clobber
    /// landing in the wrong slot.
    pub rax: u64,
    /// Saved RBX.
    pub rbx: u64,
    /// Saved RCX.
    pub rcx: u64,
    /// Saved RDX.
    pub rdx: u64,
    /// Saved RSI.
    pub rsi: u64,
    /// Saved RDI.
    pub rdi: u64,
    /// Saved RBP.
    pub rbp: u64,
    /// Saved R8.
    pub r8: u64,
    /// Saved R9.
    pub r9: u64,
    /// Saved R10.
    pub r10: u64,
    /// Saved R11.
    pub r11: u64,
    /// Saved R12.
    pub r12: u64,
    /// Saved R13.
    pub r13: u64,
    /// Saved R14.
    pub r14: u64,
    /// Saved R15.
    pub r15: u64,
    /// Instruction pointer at the moment of preemption.  Pushed by the
    /// CPU as the first IRET-frame field on both ring-3 and ring-0
    /// preemption.
    pub rip: u64,
    /// Code segment selector at the moment of preemption.  Pushed by
    /// the CPU as the second IRET-frame field.  `cs.rpl` lets the
    /// resume routine distinguish a ring-3 preemption (5-field frame)
    /// from a ring-0 preemption (3-field frame) when reconstructing
    /// `rsp` / `ss`.
    pub cs: u64,
    /// RFLAGS at the moment of preemption.  Pushed by the CPU as the
    /// third IRET-frame field.  IF is preserved here so resume restores
    /// the original interrupt-enable state.
    pub rflags: u64,
    /// Stack pointer at the moment of preemption.  On ring-3 entry the
    /// CPU pushes this as the fourth IRET-frame field.  On ring-0
    /// entry the assembly stub synthesises this from the current `rsp`
    /// (after the CPU's three-field push).
    pub rsp: u64,
    /// Stack segment selector at the moment of preemption.  On ring-3
    /// entry the CPU pushes this as the fifth IRET-frame field.  On
    /// ring-0 entry the assembly stub synthesises this from the kernel
    /// data-segment selector.
    pub ss: u64,
}

// ---------------------------------------------------------------------------
// Public offset constants — load-bearing for Phase 57d's assembly stub.
// ---------------------------------------------------------------------------

/// Byte offset of [`PreemptFrame::rax`].  Asserted to be 0.
pub const PREEMPT_FRAME_OFFSET_RAX: usize = offset_of!(PreemptFrame, rax);

/// Byte offset of [`PreemptFrame::rip`].  Asserted to be 14 × 8 = 112
/// (15 GPRs precede `rip`, of which `rip` is the 16th `u64`-aligned
/// slot at index 14 — after `rax..r15`).
pub const PREEMPT_FRAME_OFFSET_RIP: usize = offset_of!(PreemptFrame, rip);

/// Byte offset of [`PreemptFrame::cs`].
pub const PREEMPT_FRAME_OFFSET_CS: usize = offset_of!(PreemptFrame, cs);

/// Byte offset of [`PreemptFrame::rflags`].
pub const PREEMPT_FRAME_OFFSET_RFLAGS: usize = offset_of!(PreemptFrame, rflags);

/// Byte offset of [`PreemptFrame::rsp`].
pub const PREEMPT_FRAME_OFFSET_RSP: usize = offset_of!(PreemptFrame, rsp);

/// Byte offset of [`PreemptFrame::ss`].
pub const PREEMPT_FRAME_OFFSET_SS: usize = offset_of!(PreemptFrame, ss);

// ---------------------------------------------------------------------------
// Compile-time layout assertions — fail the build on layout drift.
// ---------------------------------------------------------------------------

const _: () = assert!(
    PREEMPT_FRAME_OFFSET_RAX == 0,
    "PreemptFrame::rax must be at offset 0"
);
const _: () = assert!(
    PREEMPT_FRAME_OFFSET_RIP == 15 * 8,
    "PreemptFrame::rip must be at offset 120 (after 15 u64 GPRs)"
);
const _: () = assert!(
    PREEMPT_FRAME_OFFSET_CS == 16 * 8,
    "PreemptFrame::cs must be at offset 128"
);
const _: () = assert!(
    PREEMPT_FRAME_OFFSET_RFLAGS == 17 * 8,
    "PreemptFrame::rflags must be at offset 136"
);
const _: () = assert!(
    PREEMPT_FRAME_OFFSET_RSP == 18 * 8,
    "PreemptFrame::rsp must be at offset 144"
);
const _: () = assert!(
    PREEMPT_FRAME_OFFSET_SS == 19 * 8,
    "PreemptFrame::ss must be at offset 152"
);
const _: () = assert!(
    core::mem::size_of::<PreemptFrame>() == 20 * 8,
    "PreemptFrame total size must be 160 bytes (20 u64 fields)"
);
const _: () = assert!(
    core::mem::align_of::<PreemptFrame>() == 8,
    "PreemptFrame must be u64-aligned"
);

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check at runtime that the offset constants match
    /// `offset_of!`.  The compile-time `const _:` assertions above are
    /// the durable guard; this test just confirms the public constants
    /// are wired through correctly so a downstream consumer that
    /// imports `PREEMPT_FRAME_OFFSET_*` sees the expected values.
    #[test]
    fn offsets_match_field_order() {
        assert_eq!(PREEMPT_FRAME_OFFSET_RAX, 0);
        assert_eq!(PREEMPT_FRAME_OFFSET_RIP, 120);
        assert_eq!(PREEMPT_FRAME_OFFSET_CS, 128);
        assert_eq!(PREEMPT_FRAME_OFFSET_RFLAGS, 136);
        assert_eq!(PREEMPT_FRAME_OFFSET_RSP, 144);
        assert_eq!(PREEMPT_FRAME_OFFSET_SS, 152);
    }

    #[test]
    fn size_and_alignment() {
        assert_eq!(core::mem::size_of::<PreemptFrame>(), 160);
        assert_eq!(core::mem::align_of::<PreemptFrame>(), 8);
    }

    #[test]
    fn default_initialises_to_zero() {
        let f = PreemptFrame::default();
        assert_eq!(f.rax, 0);
        assert_eq!(f.r15, 0);
        assert_eq!(f.rip, 0);
        assert_eq!(f.ss, 0);
    }
}
