//! Signal frame layout and delivery helpers — Phase 19.
//!
//! Defines the `Sigframe` struct that the kernel pushes onto the user stack
//! when delivering a signal to a user handler.  The layout matches the Linux
//! `rt_sigframe` / `ucontext_t` structure that musl expects.
//!
//! # Stack layout after signal delivery
//!
//! ```text
//! [high addresses — original user stack]
//! ┌──────────────────────────┐ ← original user RSP
//! │    alignment padding     │
//! ├──────────────────────────┤ ← frame pointer (= new RSP set for handler)
//! │ pretcode (restorer addr) │  ← [RSP+0] acts as return address for handler
//! │ uc_flags                 │
//! │ uc_link                  │
//! │ uc_stack (stack_t)       │  ss_sp, ss_flags, ss_size
//! │ uc_mcontext (sigcontext) │  r8–r15, rdi, rsi, rbp, rbx, rdx, rax,
//! │                          │  rcx, rsp, rip, rflags, cs/gs/fs, err,
//! │                          │  trapno, oldmask, cr2, fpstate, reserved
//! │ uc_sigmask               │  saved blocked-signal mask (128 bytes)
//! │ siginfo_t                │  128 bytes (zeroed)
//! └──────────────────────────┘
//! [low addresses]
//! ```

use crate::mm::user_mem::{UserSliceRo, UserSliceWo};

/// Saved user-space register state, read from the kernel syscall stack.
///
/// **Must be `#[repr(C)]`** — the `restore_and_enter_userspace` asm stub
/// in `syscall.rs` accesses fields by fixed byte offsets.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SavedUserRegs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
}

/// Read the saved user registers from the kernel syscall stack.
///
/// The asm stub in `syscall_entry` pushes registers onto the kernel stack
/// at known offsets from `SYSCALL_STACK_TOP`:
///
/// ```text
/// SYSCALL_STACK_TOP -   8: rcx  (user RIP)
/// SYSCALL_STACK_TOP -  16: r11  (user RFLAGS)
/// SYSCALL_STACK_TOP -  24: rbx
/// SYSCALL_STACK_TOP -  32: rbp
/// SYSCALL_STACK_TOP -  40: r12
/// SYSCALL_STACK_TOP -  48: r13
/// SYSCALL_STACK_TOP -  56: r14
/// SYSCALL_STACK_TOP -  64: r15
/// SYSCALL_STACK_TOP -  72: rdi
/// SYSCALL_STACK_TOP -  80: rsi
/// SYSCALL_STACK_TOP -  88: rdx
/// SYSCALL_STACK_TOP -  96: r10
/// SYSCALL_STACK_TOP - 104: r8
/// SYSCALL_STACK_TOP - 112: r9
/// ```
///
/// `SYSCALL_USER_RSP` holds the user's stack pointer.
/// `syscall_result` is the return value from `syscall_handler` (goes in rax).
///
/// # Safety
///
/// Must be called from the syscall return path (after `syscall_handler`
/// has been called) while still on the same kernel stack.  Single-CPU only.
pub unsafe fn read_saved_user_regs(syscall_result: u64) -> SavedUserRegs {
    unsafe {
        let top = crate::arch::x86_64::syscall::per_core_syscall_stack_top();
        let user_rsp = crate::arch::x86_64::syscall::per_core_syscall_user_rsp();

        // Helper: read a u64 at SYSCALL_STACK_TOP - offset.
        let read_at = |neg_offset: u64| -> u64 {
            let addr = (top - neg_offset) as *const u64;
            core::ptr::read_volatile(addr)
        };

        SavedUserRegs {
            rax: syscall_result,
            rcx: read_at(8),  // was user RIP (cpu puts rip in rcx on syscall)
            r11: read_at(16), // was user RFLAGS
            rbx: read_at(24),
            rbp: read_at(32),
            r12: read_at(40),
            r13: read_at(48),
            r14: read_at(56),
            r15: read_at(64),
            rdi: read_at(72),
            rsi: read_at(80),
            rdx: read_at(88),
            r10: read_at(96),
            r8: read_at(104),
            r9: read_at(112),
            rip: read_at(8),     // user RIP = what was in rcx
            rflags: read_at(16), // user RFLAGS = what was in r11
            rsp: user_rsp,
        }
    }
}

// ---------------------------------------------------------------------------
// Sigframe — matches Linux rt_sigframe layout for musl compatibility
// ---------------------------------------------------------------------------

/// Size of `sigcontext` (saved GPRs + metadata) — 256 bytes.
const SIGCONTEXT_SIZE: usize = 256;
/// Size of `siginfo_t` — 128 bytes (zeroed, no real siginfo yet).
const SIGINFO_SIZE: usize = 128;
/// Size of the signal mask in the ucontext (128 bytes for musl compat).
const SIGMASK_SIZE: usize = 128;

/// Total size of the core signal frame (GPR state + masks + siginfo).
///
/// Layout:
///   pretcode:       8 bytes
///   uc_flags:       8 bytes
///   uc_link:        8 bytes
///   uc_stack:       24 bytes (stack_t: ss_sp + ss_flags/pad + ss_size)
///   uc_mcontext:    256 bytes (sigcontext)
///   uc_sigmask:     128 bytes
///   siginfo:        128 bytes
///   Total:          560 bytes
///
/// The FPU save area (`FPU_AREA_SIZE` bytes) is appended immediately above
/// this at `frame_rsp + SIGFRAME_SIZE`.  It is written unaligned in memory
/// (no hardware xsave constraint because we copy bytes, not xsave into it),
/// and the fpstate pointer in mcontext is set to its user-space address.
pub const SIGFRAME_SIZE: usize = 8 + 8 + 8 + 24 + SIGCONTEXT_SIZE + SIGMASK_SIZE + SIGINFO_SIZE;

/// Size of the FPU save area appended above the core sigframe.
/// Must equal `crate::arch::x86_64::cpuid::XSAVE_AREA_SIZE` (832 bytes).
pub const FPU_AREA_SIZE: usize = crate::arch::x86_64::cpuid::XSAVE_AREA_SIZE; // 832

/// Total size of the extended signal frame including the FPU save area.
pub const SIGFRAME_EXTENDED_SIZE: usize = SIGFRAME_SIZE + FPU_AREA_SIZE;

// Offsets within the sigframe (from the frame base).
const OFF_PRETCODE: usize = 0;
/// Base of the `ucontext_t` within the frame (immediately after `pretcode`).
/// `uc_flags` is its first field. Phase 86d Track C — exposed so the signal
/// dispatcher can hand the handler a valid `ucontext` pointer (RDX, the 3rd
/// `SA_SIGINFO` argument); Go's `doSigPreempt` reads `uc_mcontext.gregs[RIP]`
/// at `ucontext + 0xa8`, which faults if RDX is null.
pub const OFF_UCONTEXT: usize = 8;
#[allow(dead_code)] // layout constant for nested ucontext
const OFF_UC_LINK: usize = 16;
const OFF_UC_STACK: usize = 24;
// uc_stack: ss_sp(8) + ss_flags(4) + _pad(4) + ss_size(8) = 24 bytes
const OFF_MCONTEXT: usize = 48;
// sigcontext layout (offsets within mcontext):
const MC_R8: usize = 0;
const MC_R9: usize = 8;
const MC_R10: usize = 16;
const MC_R11: usize = 24;
const MC_R12: usize = 32;
const MC_R13: usize = 40;
const MC_R14: usize = 48;
const MC_R15: usize = 56;
const MC_RDI: usize = 64;
const MC_RSI: usize = 72;
const MC_RBP: usize = 80;
const MC_RBX: usize = 88;
const MC_RDX: usize = 96;
const MC_RAX: usize = 104;
const MC_RCX: usize = 112;
const MC_RSP: usize = 120;
const MC_RIP: usize = 128;
const MC_RFLAGS: usize = 136;
// cs(2) + gs(2) + fs(2) + pad(2) = 8 bytes at offset 144
// err(8) at 152, trapno(8) at 160, oldmask(8) at 168, cr2(8) at 176,
// fpstate(8) at 184, reserved(64) at 192 → total 192+64 = 256 ✓
const MC_FPSTATE: usize = 184; // pointer to FPU save area, within mcontext

const OFF_SIGMASK: usize = OFF_MCONTEXT + SIGCONTEXT_SIZE; // 48 + 256 = 304
/// Offset of the `siginfo_t` within the frame. Phase 86d Track C — exposed so
/// the dispatcher can pass the handler a valid `siginfo` pointer (RSI, the 2nd
/// `SA_SIGINFO` argument).
pub const OFF_SIGINFO: usize = OFF_SIGMASK + SIGMASK_SIZE; // 304 + 128 = 432

/// User-space addresses on x86_64 must be in the lower canonical half
/// (bit 47 clear).  The highest valid user address is 0x0000_7FFF_FFFF_FFFF.
const USER_ADDR_LIMIT: u64 = 0x0000_8000_0000_0000;

/// Write the signal frame to the user stack and return the new user RSP.
///
/// When `fpu_bytes` is `Some`, the caller must supply exactly
/// `FPU_AREA_SIZE` bytes (the current task's XSaveArea snapshot).  These
/// are written to the FPU area appended above the core frame, and the
/// `fpstate` pointer in `uc_mcontext` is set to the user-space address of
/// that area so a signal handler (and `sigreturn`) can read/restore it.
///
/// When `fpu_bytes` is `None` (kernel task or no current task), the FPU
/// area is zeroed and `fpstate` is set to 0 — safe for handlers that do
/// not inspect the mcontext FPU state.
///
/// Returns `None` if the computed stack address is invalid (would write into
/// kernel space), in which case the caller should terminate with SIGSEGV.
pub fn setup_signal_frame(
    regs: &SavedUserRegs,
    blocked_signals: u64,
    signal_num: u32,
    restorer: u64,
    alt_stack_rsp: Option<u64>,
    fpu_bytes: Option<&[u8]>,
) -> Option<u64> {
    // Start from the alt stack if provided, else from the interrupted user RSP.
    let base_rsp = alt_stack_rsp.unwrap_or(regs.rsp);

    // Compute aligned frame position.  We allocate the extended frame
    // (core + FPU area) as one contiguous region.  Subtract the extended
    // size, align down to 16 bytes, then subtract 8 so that the handler
    // sees RSP % 16 == 8 (the call-convention alignment: after the handler
    // executes `call`, RSP % 16 == 0).
    let frame_rsp = base_rsp.checked_sub(SIGFRAME_EXTENDED_SIZE as u64)? & !15u64;
    let frame_rsp = frame_rsp.checked_sub(8)?;

    // Validate: frame must be in user space.
    if !(0x1000..USER_ADDR_LIMIT).contains(&frame_rsp) {
        return None;
    }

    // Zero-fill the core part of the frame.  The FPU area (if any) is
    // written separately below.
    let frame_buf = [0u8; SIGFRAME_SIZE];
    if UserSliceWo::new(frame_rsp, frame_buf.len())
        .and_then(|s| s.copy_from_kernel(&frame_buf))
        .is_err()
    {
        return None;
    }

    // Helper: write a u64 at frame_rsp + offset.
    let write_u64 = |off: usize, val: u64| -> bool {
        let bytes = val.to_ne_bytes();
        UserSliceWo::new(frame_rsp + off as u64, bytes.len())
            .and_then(|s| s.copy_from_kernel(&bytes))
            .is_ok()
    };

    // pretcode — return address for the handler, points to __restore_rt.
    if !write_u64(OFF_PRETCODE, restorer) {
        return None;
    }

    // uc_flags, uc_link — zero.
    // uc_stack — zeroed here; caller writes alt-stack info via write_sigframe_uc_stack.

    // uc_mcontext — saved GPRs.
    let mc = OFF_MCONTEXT;
    if !write_u64(mc + MC_R8, regs.r8)
        || !write_u64(mc + MC_R9, regs.r9)
        || !write_u64(mc + MC_R10, regs.r10)
        || !write_u64(mc + MC_R11, regs.r11)
        || !write_u64(mc + MC_R12, regs.r12)
        || !write_u64(mc + MC_R13, regs.r13)
        || !write_u64(mc + MC_R14, regs.r14)
        || !write_u64(mc + MC_R15, regs.r15)
        || !write_u64(mc + MC_RDI, regs.rdi)
        || !write_u64(mc + MC_RSI, regs.rsi)
        || !write_u64(mc + MC_RBP, regs.rbp)
        || !write_u64(mc + MC_RBX, regs.rbx)
        || !write_u64(mc + MC_RDX, regs.rdx)
        || !write_u64(mc + MC_RAX, regs.rax)
        || !write_u64(mc + MC_RCX, regs.rcx)
        || !write_u64(mc + MC_RSP, regs.rsp)
        || !write_u64(mc + MC_RIP, regs.rip)
        || !write_u64(mc + MC_RFLAGS, regs.rflags)
    {
        return None;
    }

    // Phase 86f Track B.1 — write the FPU save area and point fpstate at it.
    // The FPU area starts at frame_rsp + SIGFRAME_SIZE (immediately after the
    // core frame bytes).  We copy into it rather than xsaving directly, so
    // hardware alignment constraints on xsave64 do not apply here.
    let fpu_area_ptr = frame_rsp + SIGFRAME_SIZE as u64;
    match fpu_bytes {
        Some(bytes) if bytes.len() == FPU_AREA_SIZE => {
            if UserSliceWo::new(fpu_area_ptr, bytes.len())
                .and_then(|s| s.copy_from_kernel(bytes))
                .is_err()
            {
                return None;
            }
            // Write the fpstate pointer into mcontext so the kernel's own
            // sigreturn path (and any SA_SIGINFO handler that inspects it)
            // finds the FPU bytes.
            if !write_u64(mc + MC_FPSTATE, fpu_area_ptr) {
                return None;
            }
        }
        _ => {
            // No FPU bytes (kernel task) — zero the FPU area and leave
            // fpstate = 0 (already zeroed from the frame_buf above).
            // Zero FPU area.
            let zero_fpu = [0u8; FPU_AREA_SIZE];
            if UserSliceWo::new(fpu_area_ptr, zero_fpu.len())
                .and_then(|s| s.copy_from_kernel(&zero_fpu))
                .is_err()
            {
                return None;
            }
        }
    }

    // uc_sigmask — save the current blocked_signals mask so sigreturn
    // can restore it.
    if !write_u64(OFF_SIGMASK, blocked_signals) {
        return None;
    }

    // siginfo — leave zeroed.  Write si_signo at offset 0 of siginfo.
    if !write_u64(OFF_SIGINFO, signal_num as u64) {
        return None;
    }

    log::debug!(
        "[signal] sigframe at {:#x}, sig={}, handler ret→{:#x}, rip={:#x}, fpu={}",
        frame_rsp,
        signal_num,
        restorer,
        regs.rip,
        if fpu_bytes.is_some() {
            "saved"
        } else {
            "zeroed"
        },
    );

    Some(frame_rsp)
}

/// Restore saved registers, signal mask, and FPU bytes from a sigframe on the
/// user stack.
///
/// `user_rsp` is the user RSP at the time of the `sigreturn` syscall
/// (i.e., after the handler's `ret` popped pretcode).
///
/// Returns `(regs, saved_mask, fpu_bytes)`.  `fpu_bytes` is `Some` when the
/// frame contains a valid FPU save area (fpstate != 0 and within the frame);
/// it is `None` for frames written before Phase 86f (fpstate == 0) or kernel-
/// task frames — the caller must treat a `None` as "leave hardware FPU as-is".
///
/// Returns `None` if the sigframe pointer is invalid.
pub fn restore_sigframe(
    user_rsp: u64,
) -> Option<(SavedUserRegs, u64, Option<[u8; FPU_AREA_SIZE]>)> {
    // The frame starts 8 bytes before the current RSP (the `ret` from
    // the handler popped pretcode, advancing RSP by 8).
    let frame_rsp = user_rsp.wrapping_sub(8);

    if !(0x1000..USER_ADDR_LIMIT).contains(&frame_rsp) {
        return None;
    }

    // Helper: read a u64 from frame_rsp + offset.
    let read_u64 = |off: usize| -> Option<u64> {
        let mut buf = [0u8; 8];
        UserSliceRo::new(frame_rsp + off as u64, buf.len())
            .and_then(|s| s.copy_to_kernel(&mut buf))
            .ok()?;
        Some(u64::from_ne_bytes(buf))
    };

    let mc = OFF_MCONTEXT;
    let regs = SavedUserRegs {
        r8: read_u64(mc + MC_R8)?,
        r9: read_u64(mc + MC_R9)?,
        r10: read_u64(mc + MC_R10)?,
        r11: read_u64(mc + MC_R11)?,
        r12: read_u64(mc + MC_R12)?,
        r13: read_u64(mc + MC_R13)?,
        r14: read_u64(mc + MC_R14)?,
        r15: read_u64(mc + MC_R15)?,
        rdi: read_u64(mc + MC_RDI)?,
        rsi: read_u64(mc + MC_RSI)?,
        rbp: read_u64(mc + MC_RBP)?,
        rbx: read_u64(mc + MC_RBX)?,
        rdx: read_u64(mc + MC_RDX)?,
        rax: read_u64(mc + MC_RAX)?,
        rcx: read_u64(mc + MC_RCX)?,
        rsp: read_u64(mc + MC_RSP)?,
        rip: read_u64(mc + MC_RIP)?,
        rflags: read_u64(mc + MC_RFLAGS)?,
    };

    let saved_mask = read_u64(OFF_SIGMASK)?;

    // Phase 86f Track B.1 — read back the FPU area if fpstate is set.
    // We only trust a pointer that is exactly at frame_rsp + SIGFRAME_SIZE
    // (the fixed offset written by setup_signal_frame).  Any other value is
    // treated as absent — either a pre-86f frame (fpstate == 0) or a
    // tampered frame; in both cases we leave the hardware FPU untouched.
    let expected_fpu_ptr = frame_rsp + SIGFRAME_SIZE as u64;
    let fpstate_ptr = read_u64(mc + MC_FPSTATE).unwrap_or(0);
    let fpu_bytes: Option<[u8; FPU_AREA_SIZE]> = if fpstate_ptr == expected_fpu_ptr {
        let mut buf = [0u8; FPU_AREA_SIZE];
        if UserSliceRo::new(fpstate_ptr, buf.len())
            .and_then(|s| s.copy_to_kernel(&mut buf))
            .is_ok()
        {
            Some(buf)
        } else {
            None
        }
    } else {
        // fpstate == 0 (pre-86f frame) or tampered — skip FPU restore.
        None
    };

    // Note: rflags sanitization (clearing IOPL, NT, VM, etc.) is done
    // by restore_and_enter_userspace in syscall.rs before the iretq.

    Some((regs, saved_mask, fpu_bytes))
}

/// Write the `uc_stack` (stack_t) into a sigframe at the given frame_rsp.
pub fn write_sigframe_uc_stack(frame_rsp: u64, ss_sp: u64, ss_flags: u32, ss_size: u64) -> bool {
    let mut buf = [0u8; 24];
    buf[0..8].copy_from_slice(&ss_sp.to_ne_bytes());
    buf[8..12].copy_from_slice(&ss_flags.to_ne_bytes());
    // bytes 12..16 = padding (zero)
    buf[16..24].copy_from_slice(&ss_size.to_ne_bytes());
    UserSliceWo::new(frame_rsp + OFF_UC_STACK as u64, buf.len())
        .and_then(|s| s.copy_from_kernel(&buf))
        .is_ok()
}
