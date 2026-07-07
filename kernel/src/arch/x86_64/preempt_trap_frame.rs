//! On-stack interrupt trap frame types for the voluntary-preemption entry stubs.
//!
//! Two ring-typed structs avoid the synthesis problem that any "uniform layout"
//! forced on the IRQ stack would create — synthetic slots cannot be inserted
//! *above* the CPU-pushed iretq frame because that memory belongs to the
//! interrupted kernel stack, and inserting them *below* puts them at the wrong
//! offset relative to the declared fields.  Each ring-typed frame matches exactly
//! what the CPU pushes for that ring (Intel SDM Vol 3A §6.14).
//!
//! ## GPR order (both structs)
//!
//! `gprs[0..14]` = `[rax, rbx, rcx, rdx, rsi, rdi, rbp, r8, r9, r10, r11, r12, r13, r14, r15]`
//!
//! The assembly stubs push registers in **reverse** order (r15 first, rax last)
//! so that rax ends up at the lowest address (`gprs[0]`), matching the field
//! layout expected by [`to_preempt_frame`].
//!
//! Source ref: phase-57d-track-B.1

use core::mem::offset_of;

use kernel_core::preempt_frame::PreemptFrame;

// ---------------------------------------------------------------------------
// Ring-3 on-stack trap frame
// ---------------------------------------------------------------------------

/// On-stack trap frame captured when the timer / reschedule-IPI interrupt
/// fires while the CPU was executing **ring-3** (user mode).
///
/// Layout (low → high address):
/// 1. `gprs[0..14]` — 15 × u64 GPR save area pushed by the asm stub.
/// 2. CPU-pushed 5-field iretq frame: `rip`, `cs`, `rflags`, `rsp`, `ss`.
///
/// The struct fields for `rip`…`ss` are **the same memory** as the
/// CPU-pushed iretq frame; modifying them in a handler directly patches the
/// return context (used by `maybe_redirect_group_exit_trampoline_user`).
#[repr(C)]
pub struct PreemptTrapFrameUser {
    /// GPR block pushed by the asm stub (index 0 = lowest address).
    /// Order: `[rax, rbx, rcx, rdx, rsi, rdi, rbp, r8, r9, r10, r11, r12, r13, r14, r15]`
    pub gprs: [u64; 15],
    // CPU-pushed iretq frame (ring-3 variant: 5 fields)
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

// ---------------------------------------------------------------------------
// Phase 110 Track A.3b — generic full-GPR trap frame (any ring, any vector)
// ---------------------------------------------------------------------------

/// On-stack trap frame captured by the generic KPTI-aware IRQ entry stubs
/// (`kpti_irq_stub` / `kpti_device_stub` in `interrupts.rs`) for every
/// converted no-error-code vector (device IRQs, keyboard/serial/mouse/SCI,
/// spurious, TLB-shootdown/cache-drain IPIs).
///
/// Unlike the preempt frames above there is **no ring split**: in 64-bit mode
/// the CPU pushes the full 5-field iretq frame (`rip/cs/rflags/rsp/ss`)
/// unconditionally — SS:RSP is pushed even without a privilege change (Intel
/// SDM Vol 3A §6.14.2) — so one layout serves both rings and handlers ring-test
/// in Rust via `cs & 3` (the same shape as `debug::DebugTrapFrame`).
///
/// Layout (low → high address): `gprs[0..14]` (asm-pushed, same order as
/// [`PreemptTrapFrameUser`]), then the CPU-pushed `rip/cs/rflags/rsp/ss`.
#[repr(C)]
pub struct TrapFrame {
    /// GPR block — same order as [`PreemptTrapFrameUser`]:
    /// `[rax, rbx, rcx, rdx, rsi, rdi, rbp, r8, r9, r10, r11, r12, r13, r14, r15]`
    pub gprs: [u64; 15],
    // CPU-pushed iretq frame (always 5 fields in 64-bit mode)
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

const _: () = assert!(
    offset_of!(TrapFrame, gprs) == 0 && offset_of!(TrapFrame, rip) == 15 * 8,
    "TrapFrame: gprs at 0, rip at 120 (after 15 GPRs)"
);
const _: () = assert!(
    offset_of!(TrapFrame, cs) == 16 * 8
        && offset_of!(TrapFrame, rflags) == 17 * 8
        && offset_of!(TrapFrame, rsp) == 18 * 8
        && offset_of!(TrapFrame, ss) == 19 * 8,
    "TrapFrame: CPU frame fields must follow the GPR block contiguously"
);
const _: () = assert!(
    core::mem::size_of::<TrapFrame>() == 20 * 8,
    "TrapFrame must be 160 bytes (15 GPRs + 5 CPU fields)"
);

// ---------------------------------------------------------------------------
// Ring-0 on-stack trap frame
// ---------------------------------------------------------------------------

/// On-stack trap frame captured when the timer / reschedule-IPI interrupt
/// fires while the CPU was executing **ring-0** (kernel mode).
///
/// Layout (low → high address):
/// 1. `gprs[0..14]` — 15 × u64 GPR save area pushed by the asm stub.
/// 2. CPU-pushed 3-field iretq frame: `rip`, `cs`, `rflags`.
///    (No `rsp`/`ss` — no privilege switch on a ring-0 interrupt.)
///
/// The interrupted kernel RSP is synthesised by the asm stub as
/// `rsp + 15*8 + 3*8 = rsp + 144` (after pushing all GPRs) and is passed
/// as a separate argument to the Rust handler.
#[repr(C)]
pub struct PreemptTrapFrameKernel {
    /// GPR block — same order as [`PreemptTrapFrameUser`].
    pub gprs: [u64; 15],
    // CPU-pushed iretq frame (ring-0 variant: 3 fields, no rsp/ss)
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
}

// ---------------------------------------------------------------------------
// Compile-time offset assertions
// ---------------------------------------------------------------------------

const _: () = assert!(
    offset_of!(PreemptTrapFrameUser, gprs) == 0,
    "PreemptTrapFrameUser: gprs must be at offset 0"
);
const _: () = assert!(
    offset_of!(PreemptTrapFrameUser, rip) == 15 * 8,
    "PreemptTrapFrameUser: rip must be at offset 120 (after 15 GPRs)"
);
const _: () = assert!(
    offset_of!(PreemptTrapFrameUser, cs) == 16 * 8,
    "PreemptTrapFrameUser: cs must be at offset 128"
);
const _: () = assert!(
    offset_of!(PreemptTrapFrameUser, rflags) == 17 * 8,
    "PreemptTrapFrameUser: rflags must be at offset 136"
);
const _: () = assert!(
    offset_of!(PreemptTrapFrameUser, rsp) == 18 * 8,
    "PreemptTrapFrameUser: rsp must be at offset 144"
);
const _: () = assert!(
    offset_of!(PreemptTrapFrameUser, ss) == 19 * 8,
    "PreemptTrapFrameUser: ss must be at offset 152"
);
const _: () = assert!(
    core::mem::size_of::<PreemptTrapFrameUser>() == 20 * 8,
    "PreemptTrapFrameUser must be 160 bytes (15 GPRs + 5 CPU fields)"
);

const _: () = assert!(
    offset_of!(PreemptTrapFrameKernel, gprs) == 0,
    "PreemptTrapFrameKernel: gprs must be at offset 0"
);
const _: () = assert!(
    offset_of!(PreemptTrapFrameKernel, rip) == 15 * 8,
    "PreemptTrapFrameKernel: rip must be at offset 120 (after 15 GPRs)"
);
const _: () = assert!(
    offset_of!(PreemptTrapFrameKernel, cs) == 16 * 8,
    "PreemptTrapFrameKernel: cs must be at offset 128"
);
const _: () = assert!(
    offset_of!(PreemptTrapFrameKernel, rflags) == 17 * 8,
    "PreemptTrapFrameKernel: rflags must be at offset 136"
);
const _: () = assert!(
    core::mem::size_of::<PreemptTrapFrameKernel>() == 18 * 8,
    "PreemptTrapFrameKernel must be 144 bytes (15 GPRs + 3 CPU fields)"
);

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

impl PreemptTrapFrameUser {
    /// Copy captured register state into a [`PreemptFrame`] (the phase-57b
    /// storage shape stored in `Task::preempt_frame`).
    ///
    /// All 5 CPU-pushed iretq fields (`rip`, `cs`, `rflags`, `rsp`, `ss`)
    /// are preserved so that the resume path can reconstruct the full iretq
    /// frame regardless of which ring was interrupted.
    #[allow(dead_code)]
    pub fn to_preempt_frame(&self) -> PreemptFrame {
        PreemptFrame {
            rax: self.gprs[0],
            rbx: self.gprs[1],
            rcx: self.gprs[2],
            rdx: self.gprs[3],
            rsi: self.gprs[4],
            rdi: self.gprs[5],
            rbp: self.gprs[6],
            r8: self.gprs[7],
            r9: self.gprs[8],
            r10: self.gprs[9],
            r11: self.gprs[10],
            r12: self.gprs[11],
            r13: self.gprs[12],
            r14: self.gprs[13],
            r15: self.gprs[14],
            rip: self.rip,
            cs: self.cs,
            rflags: self.rflags,
            rsp: self.rsp,
            ss: self.ss,
        }
    }
}

impl PreemptTrapFrameKernel {
    /// Copy captured register state into a [`PreemptFrame`], providing the
    /// captured kernel RSP separately (passed from the asm stub as
    /// `rsp + 15*8 + 3*8 = rsp + 144` after all GPR pushes).
    ///
    /// `ss` is set to 0 — on a ring-0 interrupt the CPU does not push SS, and
    /// the kernel has no meaningful stack segment to restore.
    #[allow(dead_code)]
    pub fn to_preempt_frame(&self, captured_kernel_rsp: u64) -> PreemptFrame {
        PreemptFrame {
            rax: self.gprs[0],
            rbx: self.gprs[1],
            rcx: self.gprs[2],
            rdx: self.gprs[3],
            rsi: self.gprs[4],
            rdi: self.gprs[5],
            rbp: self.gprs[6],
            r8: self.gprs[7],
            r9: self.gprs[8],
            r10: self.gprs[9],
            r11: self.gprs[10],
            r12: self.gprs[11],
            r13: self.gprs[12],
            r14: self.gprs[13],
            r15: self.gprs[14],
            rip: self.rip,
            cs: self.cs,
            rflags: self.rflags,
            rsp: captured_kernel_rsp,
            ss: 0,
        }
    }
}
