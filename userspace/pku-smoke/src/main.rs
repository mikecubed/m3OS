//! Phase 90a Track D.1 - `pku-smoke`.
//!
//! Kernel-level regression for the Memory Protection Keys (PKU) substrate
//! (Track B) and the W^X v2 pkey-guarded exception (Track C.1), independent
//! of V8/Node. Each case emits a distinct `PKU_SMOKE:<case>:ok` serial
//! sentinel (or `PKU_SMOKE:<case>:SKIP ...` when PKU is absent on this CPU)
//! so a regression names the broken layer rather than surfacing three layers
//! up. A final `PKU_SMOKE:done` summary sentinel marks a clean run; an
//! assertion failure prints `PKU_SMOKE:<case>:FAIL <reason>` and exits 2; a
//! panic prints `PKU_SMOKE:panic` and exits 101.
//!
//! ## Cases
//!
//! - `alloc` - alloc/free lifecycle: `pkey_alloc(0, PKEY_DISABLE_WRITE)` ≥ 1,
//!   `pkey_free`, re-alloc reuses the freed slot, `pkey_free(0)` → EINVAL,
//!   `pkey_free(unallocated)` → EINVAL.
//! - `exhaust` - alloc until ENOSPC (15 allocatable keys; key 0 reserved).
//! - `deny_fault` - a write to a page tagged with a write-deny key faults; the
//!   write happens in a forked child, and the parent asserts the child was
//!   killed by SIGSEGV (signal 11). This is the core PKU proof: a denied write
//!   must trap. (m3OS delivers an unhandled userspace page fault by killing the
//!   process - no in-process SIGSEGV is catchable for a fault - so a child + a
//!   `waitpid` status check is the recovery mechanism: the parent reports `ok`
//!   and continues.)
//! - `asym` - per-thread (per-context) register asymmetry: context A opens a
//!   write window (WRPKRU clearing the key's write-disable bit) and writes the
//!   tagged page successfully; context B (a forked child) closes the window
//!   (WRPKRU setting the write-disable bit) and faults writing the SAME page.
//!   Two independent PKRU registers, same tagged page, opposite outcomes —
//!   the PKU model. (Real two-process contexts, not a one-thread two-pass.)
//! - `sigframe` - signal-frame PKRU preservation (B.4): open a write window,
//!   raise SIGUSR1, the handler RDPKRUs and confirms the window is still open,
//!   and after the handler returns the window persists (a write still
//!   succeeds). Proves PKRU rides the signal frame.
//! - `wx_v2` - the W^X v2 matrix: `pkey_mprotect(RWX, write-deny-key)` SUCCEEDS
//!   (the v2 grant); `pkey_mprotect(RWX, key=0)` → EINVAL; plain
//!   `mprotect(RWX)` → EINVAL; `mmap(RWX)` → EINVAL. The two reject arms hold
//!   on a no-PKU CPU too (they are the unchanged Phase 75 v1 rule) and are
//!   asserted there as well.
//!
//! Uses raw syscalls (no libc) like `wx-violation`, plus direct `RDPKRU` /
//! `WRPKRU` (legal in ring 3 once `CR4.PKE` is set - which the kernel does
//! when PKU is usable). PKU presence is probed via `pkey_alloc`: ENOSPC ⇒ no
//! PKU on this CPU ⇒ the hardware-dependent arms print SKIP.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use syscall_lib::{
    __syscall_lib_sigrestorer, STDOUT_FILENO, SYS_MMAP, SYS_MPROTECT, SYS_MUNMAP, SigAction, exit,
    syscall0, syscall2, syscall3, syscall4, syscall6, write,
};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    let _ = write(STDOUT_FILENO, b"PKU_SMOKE:panic\n");
    exit(101)
}

// --- Linux-compatible syscall numbers / errnos / prot+map flags ------------

const SYS_FORK: u64 = 57;
const SYS_WAITPID: u64 = 61;
const SYS_GETPID: u64 = 39;
const SYS_KILL: u64 = 62;
const SYS_RT_SIGACTION: u64 = 13;

const SYS_PKEY_MPROTECT: u64 = 329;
const SYS_PKEY_ALLOC: u64 = 330;
const SYS_PKEY_FREE: u64 = 331;

const PROT_READ: u64 = 0x1;
const PROT_WRITE: u64 = 0x2;
const PROT_EXEC: u64 = 0x4;

const MAP_PRIVATE: u64 = 0x02;
const MAP_ANONYMOUS: u64 = 0x20;

const EINVAL_NEG: i64 = -22;
const ENOSPC_NEG: i64 = -28;

const PAGE: u64 = 4096;

const PKEY_DISABLE_ACCESS: u64 = 0x1;
const PKEY_DISABLE_WRITE: u64 = 0x2;

const SA_RESTORER: u64 = 0x0400_0000;
const SIGUSR1: u64 = 10;
const SIGSEGV_SIGNALLED: i32 = 11;

// --- thin syscall helpers ---------------------------------------------------

fn pkey_alloc(flags: u64, rights: u64) -> i64 {
    unsafe { syscall2(SYS_PKEY_ALLOC, flags, rights) as i64 }
}
fn pkey_free(key: u64) -> i64 {
    unsafe { syscall2(SYS_PKEY_FREE, key, 0) as i64 }
}
fn pkey_mprotect(addr: u64, len: u64, prot: u64, pkey: u64) -> i64 {
    unsafe { syscall4(SYS_PKEY_MPROTECT, addr, len, prot, pkey) as i64 }
}
fn mprotect(addr: u64, len: u64, prot: u64) -> i64 {
    unsafe { syscall3(SYS_MPROTECT, addr, len, prot) as i64 }
}
fn mmap_anon(prot: u64) -> i64 {
    unsafe {
        syscall6(
            SYS_MMAP,
            0,
            PAGE,
            prot,
            MAP_PRIVATE | MAP_ANONYMOUS,
            u64::MAX,
            0,
        ) as i64
    }
}
fn munmap(addr: u64) {
    let _ = unsafe { syscall2(SYS_MUNMAP, addr, PAGE) };
}
fn fork() -> i64 {
    unsafe { syscall0(SYS_FORK) as i64 }
}
fn waitpid(pid: i64) -> i32 {
    let mut status: i32 = 0;
    // SYS_WAITPID (61) is Linux `wait4(pid, status, options, rusage)` — a
    // 4-argument syscall. Use `syscall4` with `rusage_ptr = 0`: a `syscall3`
    // would leave `r10` (the rusage pointer) uninitialized, and the kernel's
    // `sys_wait4` reads `r10` and — on a successful reap with a non-zero
    // pointer — writes a 144-byte `struct rusage` to it, a stray write to a
    // garbage user address. Passing 0 makes the kernel skip that write.
    let _ = unsafe {
        syscall4(
            SYS_WAITPID,
            pid as u64,
            &mut status as *mut i32 as u64,
            0,
            0,
        )
    };
    status
}
fn getpid() -> i64 {
    unsafe { syscall0(SYS_GETPID) as i64 }
}
fn raise(sig: u64) {
    let _ = unsafe { syscall2(SYS_KILL, getpid() as u64, sig) };
}

// --- RDPKRU / WRPKRU (ring 3, legal once CR4.PKE is set) --------------------

#[inline]
fn rdpkru() -> u32 {
    let pkru: u32;
    unsafe {
        core::arch::asm!(
            "rdpkru",
            in("ecx") 0u32,
            out("eax") pkru,
            out("edx") _,
            options(nomem, nostack, preserves_flags),
        );
    }
    pkru
}

#[inline]
fn wrpkru(value: u32) {
    unsafe {
        core::arch::asm!(
            "wrpkru",
            in("eax") value,
            in("ecx") 0u32,
            in("edx") 0u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Clear key `k`'s write-disable bit in PKRU (open a write window for `k`).
fn pkru_open_write(k: u8) {
    let wd = 1u32 << (2 * k as u32 + 1);
    wrpkru(rdpkru() & !wd);
}

/// Set key `k`'s write-disable bit in PKRU (deny writes through `k`).
fn pkru_deny_write(k: u8) {
    let wd = 1u32 << (2 * k as u32 + 1);
    wrpkru(rdpkru() | wd);
}

/// Read key `k`'s write-disable bit (true ⇒ writes denied).
fn pkru_write_denied(k: u8) -> bool {
    let wd = 1u32 << (2 * k as u32 + 1);
    (rdpkru() & wd) != 0
}

// --- output helpers ---------------------------------------------------------

fn ok(case: &[u8]) {
    let _ = write(STDOUT_FILENO, b"PKU_SMOKE:");
    let _ = write(STDOUT_FILENO, case);
    let _ = write(STDOUT_FILENO, b":ok\n");
}

fn skip(case: &[u8], reason: &[u8]) {
    let _ = write(STDOUT_FILENO, b"PKU_SMOKE:");
    let _ = write(STDOUT_FILENO, case);
    let _ = write(STDOUT_FILENO, b":SKIP (reason: ");
    let _ = write(STDOUT_FILENO, reason);
    let _ = write(STDOUT_FILENO, b")\n");
}

fn fail(case: &[u8], reason: &[u8]) -> ! {
    let _ = write(STDOUT_FILENO, b"PKU_SMOKE:");
    let _ = write(STDOUT_FILENO, case);
    let _ = write(STDOUT_FILENO, b":FAIL ");
    let _ = write(STDOUT_FILENO, reason);
    let _ = write(STDOUT_FILENO, b"\n");
    exit(2)
}

// Phase 86f FIX 2: naked _start trampoline. This binary ignores argv/envp.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym pku_smoke_main,
    );
}

/// Probe PKU presence. A success (key ≥ 1) means PKU is usable; free the probe
/// key so the exhaustion case starts from a clean table. On a no-PKU CPU
/// `sys_pkey_alloc` returns -ENOSPC up front (it reports zero allocatable keys
/// without touching the table) — that is the *one* benign non-success, after
/// which the hardware-dependent arms legitimately SKIP. Any other return (a
/// different negative errno such as EINVAL/ENOSYS, or a spurious 0) is a
/// regression in the `pkey_alloc` path itself; a regression gate must not let
/// that masquerade as "no PKU" and silently SKIP every case, so we fail loud.
fn pku_present() -> bool {
    let k = pkey_alloc(0, 0);
    if k >= 1 {
        let _ = pkey_free(k as u64);
        return true;
    }
    if k != ENOSPC_NEG {
        fail(
            b"probe",
            b"pkey_alloc(0,0) returned neither a key (>=1) nor -ENOSPC; regression, not a no-PKU CPU",
        );
    }
    false
}

fn pku_smoke_main() -> ! {
    let _ = write(STDOUT_FILENO, b"PKU_SMOKE:begin\n");

    let has_pku = pku_present();

    // The v1 W^X rejections hold with or without PKU - assert them always.
    case_wx_v1_rejects();

    if !has_pku {
        skip(b"alloc", b"no PKU - pkey_alloc returns ENOSPC on this CPU");
        skip(b"exhaust", b"no PKU - no allocatable keys");
        skip(b"deny_fault", b"no PKU - no key-gated write fault");
        skip(b"asym", b"no PKU - no per-thread PKRU asymmetry");
        skip(b"sigframe", b"no PKU - no PKRU to ride the signal frame");
        // wx_v2 accept arm needs a write-deny key (PKU); the reject arms were
        // already asserted in case_wx_v1_rejects above.
        skip(
            b"wx_v2",
            b"no PKU - v2 grant unavailable; v1 rejects asserted",
        );
        let _ = write(STDOUT_FILENO, b"PKU_SMOKE:done\n");
        exit(0)
    }

    case_alloc();
    case_exhaust();
    case_deny_fault();
    case_asym();
    case_sigframe();
    case_wx_v2_accept();

    let _ = write(STDOUT_FILENO, b"PKU_SMOKE:done\n");
    exit(0)
}

// ===========================================================================
// alloc/free lifecycle
// ===========================================================================

fn case_alloc() {
    // alloc with write-deny rights → key ≥ 1
    let k = pkey_alloc(0, PKEY_DISABLE_WRITE);
    if k < 1 {
        fail(b"alloc", b"pkey_alloc returned key < 1");
    }
    // free it
    if pkey_free(k as u64) != 0 {
        fail(b"alloc", b"pkey_free of allocated key failed");
    }
    // re-alloc reuses the just-freed slot (lowest free key)
    let k2 = pkey_alloc(0, PKEY_DISABLE_WRITE);
    if k2 != k {
        fail(b"alloc", b"re-alloc did not reuse freed key");
    }
    // pkey_free(0) → EINVAL (key 0 reserved)
    if pkey_free(0) != EINVAL_NEG {
        fail(b"alloc", b"pkey_free(0) was not EINVAL");
    }
    // pkey_free(unallocated) → EINVAL (pick a high, certainly-unallocated key)
    if pkey_free(14) != EINVAL_NEG {
        fail(b"alloc", b"pkey_free(unallocated) was not EINVAL");
    }
    // alloc with an unknown rights bit → EINVAL
    if pkey_alloc(0, 0x8) != EINVAL_NEG {
        fail(b"alloc", b"pkey_alloc(bad-rights) was not EINVAL");
    }
    // tidy up the slot we re-allocated
    let _ = pkey_free(k2 as u64);
    ok(b"alloc");
}

// ===========================================================================
// exhaustion → ENOSPC
// ===========================================================================

fn case_exhaust() {
    // 15 allocatable keys (1..=15); key 0 reserved.
    let mut keys = [0i64; 15];
    let mut n = 0usize;
    loop {
        let k = pkey_alloc(0, PKEY_DISABLE_WRITE);
        if k >= 1 {
            if n >= keys.len() {
                // More than 15 keys allocated - wrong key count.
                fail(b"exhaust", b"allocated more than 15 keys before ENOSPC");
            }
            keys[n] = k;
            n += 1;
        } else if k == ENOSPC_NEG {
            break;
        } else {
            fail(b"exhaust", b"alloc returned neither a key nor ENOSPC");
        }
    }
    if n != 15 {
        fail(b"exhaust", b"did not get exactly 15 allocatable keys");
    }
    // Release them so later cases have a clean table.
    for &k in keys.iter().take(n) {
        let _ = pkey_free(k as u64);
    }
    ok(b"exhaust");
}

// ===========================================================================
// PKRU-denied write fault (core PKU proof) - in a forked child
// ===========================================================================

fn case_deny_fault() {
    // Allocate a write-deny key; alloc-time PKRU now denies write for it.
    let k = pkey_alloc(0, PKEY_DISABLE_WRITE);
    if k < 1 {
        fail(b"deny_fault", b"pkey_alloc failed");
    }
    let key = k as u8;

    // Map a RW page and fault it in (untagged, key 0 - write succeeds).
    let base = mmap_anon(PROT_READ | PROT_WRITE);
    if base < 0 {
        fail(b"deny_fault", b"mmap(RW) failed");
    }
    let base = base as u64;
    unsafe { (base as *mut u8).write_volatile(0x11) };

    // Tag the (already faulted-in) page with the write-deny key. Still RW in
    // the PTE - the deny is enforced by PKRU, not the PTE permission bits.
    if pkey_mprotect(base, PAGE, PROT_READ | PROT_WRITE, k as u64) != 0 {
        fail(b"deny_fault", b"pkey_mprotect(RW, key) failed");
    }

    // Sanity: PKRU denies write for this key (alloc-time init rights).
    if !pkru_write_denied(key) {
        fail(b"deny_fault", b"PKRU did not deny write after alloc");
    }

    // The write must trap. m3OS kills the process on an unhandled fault, so do
    // the write in a child and observe the SIGSEGV via waitpid.
    let child = fork();
    if child == 0 {
        // Child: writing the write-deny-tagged page must fault → kill.
        unsafe { (base as *mut u8).write_volatile(0x22) };
        // If we reach here the write was NOT trapped - exit with a sentinel
        // code (7) the parent will distinguish from a SIGSEGV kill.
        exit(7);
    }
    if child < 0 {
        fail(b"deny_fault", b"fork failed");
    }
    let status = waitpid(child);
    // WIFEXITED ⇒ (code & 0xff) << 8; WIFSIGNALED ⇒ sig & 0x7f.
    let termsig = status & 0x7f;
    if termsig != SIGSEGV_SIGNALLED {
        fail(
            b"deny_fault",
            b"denied write did not kill child with SIGSEGV",
        );
    }

    let _ = pkey_free(k as u64);
    munmap(base);
    ok(b"deny_fault");
}

// ===========================================================================
// per-thread (per-context) PKRU asymmetry - same tagged page, two PKRUs
// ===========================================================================

fn case_asym() {
    let k = pkey_alloc(0, PKEY_DISABLE_WRITE);
    if k < 1 {
        fail(b"asym", b"pkey_alloc failed");
    }
    let key = k as u8;

    let base = mmap_anon(PROT_READ | PROT_WRITE);
    if base < 0 {
        fail(b"asym", b"mmap(RW) failed");
    }
    let base = base as u64;
    unsafe { (base as *mut u8).write_volatile(0x33) };
    if pkey_mprotect(base, PAGE, PROT_READ | PROT_WRITE, k as u64) != 0 {
        fail(b"asym", b"pkey_mprotect(RW, key) failed");
    }

    // Context A (this process): OPEN the write window for the key and write
    // the tagged page successfully.
    pkru_open_write(key);
    unsafe { (base as *mut u8).write_volatile(0x44) };
    if unsafe { (base as *const u8).read_volatile() } != 0x44 {
        fail(b"asym", b"open-window write did not land");
    }

    // Context B (forked child): the child inherits the open window (so its
    // first touch resolves CoW to a private writable frame keeping the tag),
    // then CLOSES the window and writes again - a pure PKRU-gated fault, no
    // CoW interaction left. The denied write must trap → child killed.
    let child = fork();
    if child == 0 {
        // Resolve CoW privately while the window is still open.
        unsafe { (base as *mut u8).write_volatile(0x55) };
        // Now deny writes for the key in THIS context's PKRU only.
        pkru_deny_write(key);
        // This write must fault despite the page being writable in the PTE —
        // the per-context PKRU register denies it.
        unsafe { (base as *mut u8).write_volatile(0x66) };
        exit(7); // unreached if the write trapped
    }
    if child < 0 {
        fail(b"asym", b"fork failed");
    }
    let status = waitpid(child);
    if (status & 0x7f) != SIGSEGV_SIGNALLED {
        fail(
            b"asym",
            b"deny-window child write did not trap (no asymmetry)",
        );
    }

    // Context A still has the window open - write again to prove the child's
    // PKRU change did not leak back into the parent.
    unsafe { (base as *mut u8).write_volatile(0x77) };
    if unsafe { (base as *const u8).read_volatile() } != 0x77 {
        fail(b"asym", b"parent window closed after child (PKRU leak)");
    }

    let _ = pkey_free(k as u64);
    munmap(base);
    ok(b"asym");
}

// ===========================================================================
// signal-frame PKRU preservation (B.4)
// ===========================================================================

// Shared between the SIGUSR1 handler and the main flow. The handler runs
// asynchronously w.r.t. the main thread, so non-atomic access (even through
// raw pointers) would be a data race / UB under Rust's memory model; atomics
// make every access well-defined.
static SIG_BASE: AtomicU64 = AtomicU64::new(0);
static SIG_KEY: AtomicU8 = AtomicU8::new(0);
static SIG_HANDLER_SAW_OPEN: AtomicBool = AtomicBool::new(false);
static SIG_HANDLER_WROTE: AtomicBool = AtomicBool::new(false);

extern "C" fn sigusr1_handler(_sig: i32) {
    // The window opened before raising the signal must still be open inside
    // the handler - PKRU rides the signal frame (B.4). RDPKRU here reads the
    // delivered-frame PKRU.
    let key = SIG_KEY.load(Ordering::SeqCst);
    let open = !pkru_write_denied(key);
    SIG_HANDLER_SAW_OPEN.store(open, Ordering::SeqCst);
    if open {
        // And a write through the tagged page succeeds inside the handler.
        let base = SIG_BASE.load(Ordering::SeqCst);
        unsafe { (base as *mut u8).write_volatile(0xBB) };
        let wrote = unsafe { (base as *const u8).read_volatile() } == 0xBB;
        SIG_HANDLER_WROTE.store(wrote, Ordering::SeqCst);
    }
}

fn case_sigframe() {
    let k = pkey_alloc(0, PKEY_DISABLE_WRITE);
    if k < 1 {
        fail(b"sigframe", b"pkey_alloc failed");
    }
    let key = k as u8;

    let base = mmap_anon(PROT_READ | PROT_WRITE);
    if base < 0 {
        fail(b"sigframe", b"mmap(RW) failed");
    }
    let base = base as u64;
    unsafe { (base as *mut u8).write_volatile(0xAA) };
    if pkey_mprotect(base, PAGE, PROT_READ | PROT_WRITE, k as u64) != 0 {
        fail(b"sigframe", b"pkey_mprotect(RW, key) failed");
    }

    SIG_BASE.store(base, Ordering::SeqCst);
    SIG_KEY.store(key, Ordering::SeqCst);
    SIG_HANDLER_SAW_OPEN.store(false, Ordering::SeqCst);
    SIG_HANDLER_WROTE.store(false, Ordering::SeqCst);

    // Install the SIGUSR1 handler (with the shared restorer trampoline).
    let act = SigAction {
        sa_handler: sigusr1_handler as *const () as u64,
        sa_flags: SA_RESTORER,
        sa_restorer: __syscall_lib_sigrestorer as *const () as u64,
        sa_mask: 0,
    };
    let rc = unsafe {
        syscall3(
            SYS_RT_SIGACTION,
            SIGUSR1,
            &act as *const SigAction as u64,
            0,
        ) as i64
    };
    if rc != 0 {
        fail(b"sigframe", b"rt_sigaction(SIGUSR1) failed");
    }

    // Open the write window, then raise the signal.
    pkru_open_write(key);
    raise(SIGUSR1);

    // Handler must have observed the window open and written through it.
    let saw_open = SIG_HANDLER_SAW_OPEN.load(Ordering::SeqCst);
    let wrote = SIG_HANDLER_WROTE.load(Ordering::SeqCst);
    if !saw_open {
        fail(
            b"sigframe",
            b"handler saw window CLOSED - PKRU not on signal frame",
        );
    }
    if !wrote {
        fail(b"sigframe", b"handler write through open window failed");
    }

    // After the handler returns, the window must persist (restore-on-sigreturn).
    if pkru_write_denied(key) {
        fail(
            b"sigframe",
            b"window closed after handler return - PKRU not restored",
        );
    }
    unsafe { (base as *mut u8).write_volatile(0xCC) };
    if unsafe { (base as *const u8).read_volatile() } != 0xCC {
        fail(
            b"sigframe",
            b"post-handler write through open window failed",
        );
    }

    let _ = pkey_free(k as u64);
    munmap(base);
    ok(b"sigframe");
}

// ===========================================================================
// W^X v2 matrix - reject arms (v1, hold with or without PKU)
// ===========================================================================

fn case_wx_v1_rejects() {
    // plain mprotect(RWX) → EINVAL (Phase 75 v1 rule, unchanged).
    let base = mmap_anon(PROT_READ | PROT_WRITE);
    if base < 0 {
        fail(b"wx_v2", b"setup mmap(RW) failed");
    }
    let base = base as u64;
    if mprotect(base, PAGE, PROT_READ | PROT_WRITE | PROT_EXEC) != EINVAL_NEG {
        fail(b"wx_v2", b"plain mprotect(RWX) was not EINVAL");
    }
    munmap(base);

    // mmap(RWX) → EINVAL at mmap entry (C.1 contract clause 1).
    let m = mmap_anon(PROT_READ | PROT_WRITE | PROT_EXEC);
    if m >= 0 {
        munmap(m as u64);
        fail(b"wx_v2", b"mmap(RWX) was not rejected");
    }
    if m != EINVAL_NEG {
        fail(b"wx_v2", b"mmap(RWX) reject errno was not EINVAL");
    }
}

// ===========================================================================
// W^X v2 matrix - accept arm + the pkey-keyed reject arms (need PKU)
// ===========================================================================

fn case_wx_v2_accept() {
    // pkey_mprotect(RWX, key=0) → EINVAL (clause 3.a: key 0 behaves like
    // plain mprotect - W+X rejected).
    let base0 = mmap_anon(PROT_READ | PROT_WRITE);
    if base0 < 0 {
        fail(b"wx_v2", b"setup mmap(RW) failed");
    }
    let base0 = base0 as u64;
    unsafe { (base0 as *mut u8).write_volatile(0x01) };
    if pkey_mprotect(base0, PAGE, PROT_READ | PROT_WRITE | PROT_EXEC, 0) != EINVAL_NEG {
        fail(b"wx_v2", b"pkey_mprotect(RWX, key=0) was not EINVAL");
    }
    munmap(base0);

    // pkey_mprotect(RWX, write-deny-key) → SUCCEEDS (clause 3.b: the v2 grant).
    let k = pkey_alloc(0, PKEY_DISABLE_WRITE);
    if k < 1 {
        fail(b"wx_v2", b"pkey_alloc(write-deny) failed");
    }
    let base = mmap_anon(PROT_READ | PROT_WRITE);
    if base < 0 {
        fail(b"wx_v2", b"setup mmap(RW) for grant failed");
    }
    let base = base as u64;
    unsafe { (base as *mut u8).write_volatile(0x02) };
    if pkey_mprotect(base, PAGE, PROT_READ | PROT_WRITE | PROT_EXEC, k as u64) != 0 {
        fail(
            b"wx_v2",
            b"pkey_mprotect(RWX, write-deny-key) was REJECTED (v2 grant denied)",
        );
    }

    // A permissive (no-deny) key must NOT get the grant (clause 3.c).
    let kp = pkey_alloc(0, 0);
    if kp < 1 {
        fail(b"wx_v2", b"pkey_alloc(permissive) failed");
    }
    let base2 = mmap_anon(PROT_READ | PROT_WRITE);
    if base2 < 0 {
        fail(b"wx_v2", b"setup mmap(RW) for permissive-key failed");
    }
    let base2 = base2 as u64;
    unsafe { (base2 as *mut u8).write_volatile(0x03) };
    if pkey_mprotect(base2, PAGE, PROT_READ | PROT_WRITE | PROT_EXEC, kp as u64) != EINVAL_NEG {
        fail(
            b"wx_v2",
            b"pkey_mprotect(RWX, permissive-key) was not EINVAL",
        );
    }

    // An access-disable key (PKEY_DISABLE_ACCESS implies write-deny) also
    // qualifies for the grant.
    let ka = pkey_alloc(0, PKEY_DISABLE_ACCESS);
    if ka < 1 {
        fail(b"wx_v2", b"pkey_alloc(access-deny) failed");
    }
    let base3 = mmap_anon(PROT_READ | PROT_WRITE);
    if base3 < 0 {
        fail(b"wx_v2", b"setup mmap(RW) for access-deny key failed");
    }
    let base3 = base3 as u64;
    unsafe { (base3 as *mut u8).write_volatile(0x04) };
    if pkey_mprotect(base3, PAGE, PROT_READ | PROT_WRITE | PROT_EXEC, ka as u64) != 0 {
        fail(
            b"wx_v2",
            b"pkey_mprotect(RWX, access-deny-key) was REJECTED",
        );
    }

    let _ = pkey_free(k as u64);
    let _ = pkey_free(kp as u64);
    let _ = pkey_free(ka as u64);
    munmap(base);
    munmap(base2);
    munmap(base3);
    ok(b"wx_v2");
}
