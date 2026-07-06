//! Trap & debug-register substrate (Phase 111 Track B).
//!
//! The exception-level plumbing the in-kernel GDB stub (Track C) and the
//! `ptrace`-backed userspace debugger (Track D) both consume:
//!
//! - the `#DB` (vector 1) and `#BP` (vector 3) dispatch functions
//!   ([`on_debug_exception`], [`on_breakpoint`]) the IDT handlers in
//!   `interrupts.rs` call, with the `int3` RIP-`-1` fixup and a ring-0 vs
//!   ring-3 routing **seam** (the branch a future kernel stub / ptrace stop
//!   path slots into);
//! - single-step control ([`set_single_step`] / [`clear_single_step`] on a trap
//!   frame's `RFLAGS.TF`);
//! - a thin [`DebugRegs`] wrapper over `DR0`–`DR7` (hardware breakpoints /
//!   watchpoints), encoded/decoded via the host-tested
//!   [`kernel_core::debug_regs`] bit logic;
//! - a software-breakpoint patch primitive ([`insert_sw_breakpoint`] /
//!   [`remove_sw_breakpoint`]) that swaps `0xCC` for the original byte.
//!
//! Until Track C/D register a consumer, a stray `#BP`/`#DB` in a production
//! build is handled by a safe default (log once, clear `TF`, resume past the
//! trap) so this substrate never destabilises normal operation. The
//! `debug-substrate-test` feature adds a boot-time self-test that proves the
//! `int3` fixup and single-step end to end.

use core::mem::offset_of;

use kernel_core::debug_regs::{self, DR6_STATUS_MASK, SlotConfig};

/// `RFLAGS.TF` — the single-step trap-enable bit (bit 8).
pub const RFLAGS_TF: u64 = 1 << 8;

// ---------------------------------------------------------------------------
// Full-GPR debug trap frame (Phase 111 Track C.2 prerequisite)
// ---------------------------------------------------------------------------

/// On-stack trap frame captured by the naked-asm `#BP`/`#DB` entry stubs
/// (`bp_entry`/`db_entry` in `interrupts.rs`), giving a debugger consumer all
/// 15 GPRs — which the previous `extern "x86-interrupt"` handlers could not
/// see — plus the CPU-pushed iretq frame. GDB's `g`/`G` packets need the full
/// set.
///
/// Layout (low → high address):
/// 1. `gprs[0..14]` — 15 × u64 GPR save area pushed by the asm stub, same
///    order as [`crate::arch::x86_64::preempt_trap_frame`]:
///    `[rax, rbx, rcx, rdx, rsi, rdi, rbp, r8, r9, r10, r11, r12, r13, r14, r15]`
/// 2. CPU-pushed 5-field iretq frame: `rip`, `cs`, `rflags`, `rsp`, `ss`.
///
/// **One layout for both rings.** In 64-bit mode the CPU pushes `SS:RSP`
/// unconditionally on every interrupt/exception — with or without a privilege
/// change (Intel SDM Vol 3A §6.14.2, AMD APM Vol 2 §8.9.3) — and `iretq`
/// always pops all five fields. So unlike the ring-split preempt frames there
/// is no 3-field kernel variant to model; `cs & 3` distinguishes the rings.
///
/// Mutations to this frame (GPRs, `rip`, `rflags`, …) are live: the asm stub
/// pops the GPR block back into the registers and `iretq` consumes the CPU
/// fields, so a `G`/`s` register write-back takes effect on resume.
#[repr(C)]
pub struct DebugTrapFrame {
    /// GPR block pushed by the asm stub (index 0 = lowest address).
    /// Order: `[rax, rbx, rcx, rdx, rsi, rdi, rbp, r8, r9, r10, r11, r12, r13, r14, r15]`
    pub gprs: [u64; 15],
    // CPU-pushed iretq frame (always 5 fields in 64-bit mode).
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl DebugTrapFrame {
    /// True if the trap came from ring 3.
    #[inline]
    pub fn from_user(&self) -> bool {
        (self.cs & 3) == 3
    }
}

const _: () = assert!(
    offset_of!(DebugTrapFrame, gprs) == 0,
    "DebugTrapFrame: gprs must be at offset 0"
);
const _: () = assert!(
    offset_of!(DebugTrapFrame, rip) == 15 * 8,
    "DebugTrapFrame: rip must be at offset 120 (after 15 GPRs)"
);
const _: () = assert!(
    offset_of!(DebugTrapFrame, cs) == 16 * 8,
    "DebugTrapFrame: cs must be at offset 128"
);
const _: () = assert!(
    offset_of!(DebugTrapFrame, rflags) == 17 * 8,
    "DebugTrapFrame: rflags must be at offset 136"
);
const _: () = assert!(
    offset_of!(DebugTrapFrame, rsp) == 18 * 8,
    "DebugTrapFrame: rsp must be at offset 144"
);
const _: () = assert!(
    offset_of!(DebugTrapFrame, ss) == 19 * 8,
    "DebugTrapFrame: ss must be at offset 152"
);
const _: () = assert!(
    core::mem::size_of::<DebugTrapFrame>() == 20 * 8,
    "DebugTrapFrame must be 160 bytes (15 GPRs + 5 CPU fields)"
);

// ---------------------------------------------------------------------------
// Hardware debug register access (DR0–DR7)
// ---------------------------------------------------------------------------

/// Read `DR6` (debug status). Ring-0 only — a ring-3 `mov` from a debug
/// register already `#GP`s, which is the posture we keep.
#[inline]
pub fn read_dr6() -> u64 {
    let v: u64;
    // SAFETY: reading a debug register is a privileged but side-effect-free op.
    unsafe {
        core::arch::asm!("mov {}, dr6", out(reg) v, options(nomem, nostack, preserves_flags))
    };
    v
}

/// Write `DR6` (used to clear the sticky status bits after servicing a `#DB`).
#[inline]
pub fn write_dr6(v: u64) {
    // SAFETY: DR6 write is privileged; value is masked by the caller.
    unsafe { core::arch::asm!("mov dr6, {}", in(reg) v, options(nomem, nostack, preserves_flags)) };
}

/// Read `DR7` (debug control).
#[inline]
pub fn read_dr7() -> u64 {
    let v: u64;
    // SAFETY: privileged, side-effect-free read.
    unsafe {
        core::arch::asm!("mov {}, dr7", out(reg) v, options(nomem, nostack, preserves_flags))
    };
    v
}

/// Write `DR7` (debug control).
#[inline]
pub fn write_dr7(v: u64) {
    // SAFETY: privileged control-register write.
    unsafe { core::arch::asm!("mov dr7, {}", in(reg) v, options(nomem, nostack, preserves_flags)) };
}

/// Set the linear address in one of the four breakpoint address registers
/// (`DR0`–`DR3`).
#[inline]
pub fn write_dr_addr(slot: usize, addr: u64) {
    // SAFETY: privileged write; `slot` bounded to 0..4.
    unsafe {
        match slot {
            0 => {
                core::arch::asm!("mov dr0, {}", in(reg) addr, options(nomem, nostack, preserves_flags))
            }
            1 => {
                core::arch::asm!("mov dr1, {}", in(reg) addr, options(nomem, nostack, preserves_flags))
            }
            2 => {
                core::arch::asm!("mov dr2, {}", in(reg) addr, options(nomem, nostack, preserves_flags))
            }
            3 => {
                core::arch::asm!("mov dr3, {}", in(reg) addr, options(nomem, nostack, preserves_flags))
            }
            _ => {}
        }
    }
}

/// Thin owner of the four hardware breakpoint slots. Programs `DR0`–`DR3`
/// (address) + `DR7` (enable/condition/length via [`kernel_core::debug_regs`])
/// and reads `DR6` hit status back through the same host-tested codec.
pub struct DebugRegs;

impl DebugRegs {
    /// Arm hardware breakpoint `slot` (0..4) at `addr` with `cfg`. No-op for an
    /// out-of-range slot.
    pub fn arm(slot: usize, addr: u64, cfg: SlotConfig) {
        if slot >= 4 {
            return;
        }
        write_dr_addr(slot, addr);
        let dr7 = (read_dr7() & !slot_clear_mask(slot)) | debug_regs::dr7_slot_bits(slot, cfg);
        write_dr7(dr7);
    }

    /// Disarm hardware breakpoint `slot`.
    pub fn disarm(slot: usize) {
        if slot >= 4 {
            return;
        }
        write_dr7(read_dr7() & !slot_clear_mask(slot));
        write_dr_addr(slot, 0);
    }

    /// Decode the current `DR6` status (which slot / single-step / etc.).
    pub fn status() -> debug_regs::Dr6Status {
        debug_regs::dr6_decode(read_dr6())
    }
}

/// Bits in `DR7` belonging to slot `i`: its local+global enable and its
/// R/W + LEN field, so `arm`/`disarm` can clear just that slot.
fn slot_clear_mask(slot: usize) -> u64 {
    let enables = 0b11u64 << (2 * slot); // Li | Gi
    let rw_len = 0b1111u64 << (16 + 4 * slot); // R/Wi | LENi
    enables | rw_len
}

// ---------------------------------------------------------------------------
// Single-step (RFLAGS.TF)
// ---------------------------------------------------------------------------

/// Set `RFLAGS.TF` on `frame` so exactly one instruction executes after the
/// return before a `#DB` fires.
pub fn set_single_step(frame: &mut DebugTrapFrame) {
    frame.rflags |= RFLAGS_TF;
}

/// Clear `RFLAGS.TF` on `frame` (stop single-stepping).
pub fn clear_single_step(frame: &mut DebugTrapFrame) {
    frame.rflags &= !RFLAGS_TF;
}

// ---------------------------------------------------------------------------
// Software-breakpoint patch primitive (Track B.3)
// ---------------------------------------------------------------------------

/// `int3` opcode.
pub const INT3: u8 = 0xCC;

/// Overwrite the byte at kernel virtual address `addr` with `int3`, returning
/// the original byte to be restored later with [`remove_sw_breakpoint`].
///
/// # Safety
/// `addr` must be a valid, writable, currently-mapped kernel code address. The
/// caller owns the save/restore lifecycle (double-inserting at the same address
/// would save `0xCC` as the "original" and corrupt the restore).
pub unsafe fn insert_sw_breakpoint(addr: u64) -> u8 {
    let p = addr as *mut u8;
    // SAFETY: caller guarantees `addr` is a mapped, writable kernel byte.
    unsafe {
        let orig = core::ptr::read_volatile(p);
        with_wp_disabled(|| core::ptr::write_volatile(p, INT3));
        flush_icache(addr);
        orig
    }
}

/// Restore the original byte saved by [`insert_sw_breakpoint`].
///
/// # Safety
/// Same address constraints as [`insert_sw_breakpoint`]; `orig` must be the
/// value that call returned.
pub unsafe fn remove_sw_breakpoint(addr: u64, orig: u8) {
    let p = addr as *mut u8;
    // SAFETY: caller guarantees `addr` is the mapped, writable byte it patched.
    unsafe {
        with_wp_disabled(|| core::ptr::write_volatile(p, orig));
        flush_icache(addr);
    }
}

/// Run `f` with `CR0.WP` cleared so a ring-0 store to a read-only page (kernel
/// text is mapped R-X per its ELF flags) succeeds — the classic kprobes/kgdb
/// text-patch technique. Interrupt-safety: callers run either at boot (Track B
/// self-test) or inside the frozen all-stop stub (Track C), so no other code
/// observes the WP-off window on this core; other cores' CR0 is unaffected.
///
/// # Safety
/// Caller must ensure nothing else on this core can run during `f` (IRQs
/// disabled or single-threaded boot) — WP-off suspends kernel write protection.
unsafe fn with_wp_disabled<R>(f: impl FnOnce() -> R) -> R {
    use x86_64::registers::control::{Cr0, Cr0Flags};
    let wp_was_set = Cr0::read().contains(Cr0Flags::WRITE_PROTECT);
    if wp_was_set {
        // SAFETY: clearing WP only widens ring-0 write permission; restored below.
        unsafe { Cr0::update(|f| f.remove(Cr0Flags::WRITE_PROTECT)) };
    }
    let r = f();
    if wp_was_set {
        // SAFETY: restoring the original CR0.WP state.
        unsafe { Cr0::update(|f| f.insert(Cr0Flags::WRITE_PROTECT)) };
    }
    r
}

/// Read `out.len()` bytes starting at kernel virtual address `addr` (the GDB
/// `m` command). Plain volatile byte reads — the caller (the all-stop stub)
/// has already validated the address is canonical.
///
/// # Safety
/// `addr..addr+out.len()` must be a mapped, readable kernel range; an unmapped
/// address faults.
pub unsafe fn read_kernel_bytes(addr: u64, out: &mut [u8]) {
    // SAFETY: caller guarantees the range is mapped + readable.
    unsafe {
        for (i, o) in out.iter_mut().enumerate() {
            *o = core::ptr::read_volatile((addr + i as u64) as *const u8);
        }
    }
}

/// Write `data` starting at kernel virtual address `addr` (the GDB `M`
/// command), with `CR0.WP` cleared so a write to read-only kernel text
/// succeeds. Serializes the icache after.
///
/// # Safety
/// `addr..addr+data.len()` must be a mapped kernel range, and nothing else on
/// this core may run during the write (the frozen all-stop stub guarantees
/// this — see [`with_wp_disabled`]).
pub unsafe fn write_kernel_bytes(addr: u64, data: &[u8]) {
    // SAFETY: caller guarantees the range is mapped; WP-off is restored.
    unsafe {
        with_wp_disabled(|| {
            for (i, &b) in data.iter().enumerate() {
                core::ptr::write_volatile((addr + i as u64) as *mut u8, b);
            }
        });
    }
    flush_icache(addr);
}

/// Serialize after a code patch so the CPU refetches the modified byte. On
/// x86 a serializing instruction (`cpuid`-class) suffices for the patching
/// core; cross-core would need an IPI, which the single-stepped stub does not
/// need (the machine is all-stopped when Track C patches).
#[inline]
fn flush_icache(_addr: u64) {
    // SAFETY: `mfence` is always valid and orders the store before the refetch.
    unsafe { core::arch::asm!("mfence", options(nomem, nostack, preserves_flags)) };
}

// ---------------------------------------------------------------------------
// Dispatch seam (Track B.1) — called by the IDT handlers in interrupts.rs
// ---------------------------------------------------------------------------

/// Handle a `#BP` (vector 3). `bp_addr` is the breakpoint address (RIP already
/// decremented past the `0xCC` by the caller). `from_user` distinguishes a
/// ring-3 `int3` (→ future `ptrace` stop path, Track D) from a ring-0 one (→
/// the in-kernel `kgdb` stub, Track C). With no consumer active, the self-test
/// records the event and a production trap is logged once and resumed.
pub fn on_breakpoint(bp_addr: u64, frame: &mut DebugTrapFrame, from_user: bool) {
    #[cfg(feature = "debug-substrate-test")]
    if selftest::on_breakpoint(bp_addr, frame) {
        return;
    }

    // Track C consumer: ring-0 traps enter the kgdb stub when it is live.
    // (Track D's ptrace stop is the future ring-3 consumer.)
    #[cfg(feature = "kgdb")]
    if !from_user && crate::debug::gdbstub::on_breakpoint(bp_addr, frame) {
        return;
    }

    let _ = (from_user, frame);
    log::warn!(
        "[debug] unexpected int3 breakpoint at {:#x} (no debugger attached) — resuming",
        bp_addr
    );
}

/// Handle a `#DB` (vector 1). The caller has already read + cleared `DR6`;
/// `status` is its decode and `rip` the trapping instruction pointer.
pub fn on_debug_exception(
    status: debug_regs::Dr6Status,
    rip: u64,
    frame: &mut DebugTrapFrame,
    from_user: bool,
) {
    #[cfg(feature = "debug-substrate-test")]
    if selftest::on_debug_exception(status, rip, frame) {
        return;
    }

    // Track C consumer: ring-0 single-step / hw-breakpoint hits re-enter the
    // kgdb stub when it is live.
    #[cfg(feature = "kgdb")]
    if !from_user && crate::debug::gdbstub::on_debug_exception(&status, rip, frame) {
        return;
    }

    // No consumer: make sure we do not leave the machine single-stepping.
    let _ = (rip, from_user);
    if status.single_step {
        clear_single_step(frame);
    }
    log::warn!(
        "[debug] unexpected #DB at {:#x} (dr6: bs={} slots={:?}) — resuming",
        rip,
        status.single_step,
        status.slot_hit
    );
}

/// Read + clear `DR6` and decode it — used by the `#DB` IDT handler on entry so
/// the sticky status bits do not persist into the next `#DB`.
pub fn read_and_clear_dr6() -> debug_regs::Dr6Status {
    let raw = read_dr6();
    write_dr6(raw & !DR6_STATUS_MASK);
    debug_regs::dr6_decode(raw)
}

// ---------------------------------------------------------------------------
// Boot self-test (Track B validation) — `debug-substrate-test` feature only
// ---------------------------------------------------------------------------

#[cfg(feature = "debug-substrate-test")]
mod selftest {
    use super::*;
    use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static EXPECT_STEP: AtomicBool = AtomicBool::new(false);
    static LAST_BP_ADDR: AtomicU64 = AtomicU64::new(0);
    static STEP_COUNT: AtomicUsize = AtomicUsize::new(0);
    static ARM_STEP_ON_BP: AtomicBool = AtomicBool::new(false);

    /// Called from [`super::on_breakpoint`]; returns `true` if the self-test
    /// consumed the event.
    pub(super) fn on_breakpoint(bp_addr: u64, frame: &mut DebugTrapFrame) -> bool {
        if !ACTIVE.load(Ordering::Acquire) {
            return false;
        }
        LAST_BP_ADDR.store(bp_addr, Ordering::Release);
        // If the step sub-test asked, single-step the instruction that follows
        // this int3 so we can prove exactly one #DB results.
        if ARM_STEP_ON_BP.swap(false, Ordering::AcqRel) {
            EXPECT_STEP.store(true, Ordering::Release);
            STEP_COUNT.store(0, Ordering::Release);
            super::set_single_step(frame);
        }
        true
    }

    /// Called from [`super::on_debug_exception`]; returns `true` if consumed.
    pub(super) fn on_debug_exception(
        status: debug_regs::Dr6Status,
        _rip: u64,
        frame: &mut DebugTrapFrame,
    ) -> bool {
        if !ACTIVE.load(Ordering::Acquire) || !EXPECT_STEP.load(Ordering::Acquire) {
            return false;
        }
        if status.single_step {
            STEP_COUNT.fetch_add(1, Ordering::AcqRel);
            // One step is enough — stop stepping so we get exactly one #DB.
            super::clear_single_step(frame);
            EXPECT_STEP.store(false, Ordering::Release);
        }
        true
    }

    /// Boot-time self-test: proves the `#BP` RIP fixup and `RFLAGS.TF`
    /// single-step end to end, printing `DEBUG_SELFTEST:` sentinels the
    /// `debug-substrate-smoke` gate asserts on.
    pub fn run() {
        ACTIVE.store(true, Ordering::Release);

        // --- 1. int3 breakpoint RIP fixup ---------------------------------
        LAST_BP_ADDR.store(0, Ordering::Release);
        let int3_addr: u64;
        // SAFETY: `lea` loads the address of the next instruction (the int3);
        // executing int3 raises #BP, which on_breakpoint records. The handler
        // returns to the instruction after int3, so control continues here.
        unsafe {
            core::arch::asm!(
                "lea {a}, [rip]",  // address of the following instruction = int3
                "int3",
                a = out(reg) int3_addr,
                options(nostack),
            );
        }
        let seen = LAST_BP_ADDR.load(Ordering::Acquire);
        if seen == int3_addr {
            log::info!("DEBUG_SELFTEST:bp-rip ok addr={:#x}", seen);
        } else {
            log::info!(
                "DEBUG_SELFTEST:bp-rip FAIL seen={:#x} want={:#x}",
                seen,
                int3_addr
            );
        }

        // --- 2. single-step: exactly one #DB after one instruction --------
        // Ask the next int3's handler to arm single-step, so the instruction
        // right after that int3 single-steps and raises exactly one #DB.
        ARM_STEP_ON_BP.store(true, Ordering::Release);
        STEP_COUNT.store(0, Ordering::Release);
        // SAFETY: int3 → on_breakpoint arms TF → the following nop single-steps
        // → #DB → on_debug_exception clears TF. All handler-managed.
        unsafe {
            core::arch::asm!("int3", "nop", options(nostack),);
        }
        let count = STEP_COUNT.load(Ordering::Acquire);
        if count == 1 {
            log::info!("DEBUG_SELFTEST:single-step ok count=1");
        } else {
            log::info!("DEBUG_SELFTEST:single-step FAIL count={}", count);
        }

        // --- 3. DR7/DR6 hardware wrapper round-trips through the codec -----
        DebugRegs::arm(
            0,
            0xdead_0000,
            SlotConfig {
                condition: debug_regs::BreakCondition::Write,
                length: debug_regs::BreakLength::Four,
            },
        );
        let armed = debug_regs::dr7_slot_enabled(read_dr7(), 0);
        DebugRegs::disarm(0);
        let disarmed = !debug_regs::dr7_slot_enabled(read_dr7(), 0);
        if armed && disarmed {
            log::info!("DEBUG_SELFTEST:dr7 ok arm+disarm");
        } else {
            log::info!(
                "DEBUG_SELFTEST:dr7 FAIL armed={} disarmed={}",
                armed,
                disarmed
            );
        }

        ACTIVE.store(false, Ordering::Release);
        log::info!("DEBUG_SELFTEST:done");
    }
}

/// Run the Track B boot self-test (feature-gated; absent in production).
#[cfg(feature = "debug-substrate-test")]
pub fn run_boot_self_test() {
    selftest::run();
}
