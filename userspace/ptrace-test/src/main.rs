//! `ptrace-test` — the ptrace-smoke tracer/tracee proof (Phase 111 Track D).
//!
//! A self-contained end-to-end exercise of the kernel ptrace substrate, needing
//! no `m3gdbserver` or host GDB: this one binary forks, the child (tracee) calls
//! `PTRACE_TRACEME` and executes an `int3`, and the parent (tracer) drives the
//! full cycle — `waitpid` sees the stop as `WIFSTOPPED` with `SIGTRAP`,
//! `GETREGS` reads the tracee's registers (the `0xCAFE` marker the child put in
//! `rbx`), `PEEKTEXT` reads the `0xCC` int3 byte from the tracee's code,
//! `POKETEXT`/`PEEKTEXT` round-trip a word through the tracee's stack scratch,
//! `SETREGS` rewrites `rbx` to `42`, and `CONT` resumes the tracee — which then
//! exits with `rbx`, so the parent's second `waitpid` sees exit code `42`,
//! proving the modified register flowed back into the resumed tracee.
//!
//! Each step prints a `PTRACE_SMOKE:<step> ok` sentinel the `ptrace-smoke` gate
//! asserts on; any failure prints `... FAIL`.
#![no_std]
#![no_main]

use syscall_lib::{fork, syscall4, waitpid, write};

/// Linux ptrace request numbers (the subset the kernel implements).
// `PTRACE_TRACEME` (request 0) is issued by the tracee's naked asm stub, which
// passes the request in `edi` as a bare `xor edi, edi` (see `child_traced`) and
// so cannot reference this symbol. Kept to document the full request-number ABI
// alongside its siblings.
#[allow(dead_code)]
const PTRACE_TRACEME: u64 = 0;
const PTRACE_PEEKTEXT: u64 = 1;
const PTRACE_POKETEXT: u64 = 4;
const PTRACE_CONT: u64 = 7;
const PTRACE_GETREGS: u64 = 12;
const PTRACE_SETREGS: u64 = 13;

/// m3OS syscall number for ptrace (feature-gated `0x1152`).
const SYS_PTRACE: u64 = 0x1152;
const SYS_EXIT: u64 = 60;

/// `SavedUserRegs` field indices (18 × u64): rax,rbx,rcx,rdx,rsi,rdi,rbp,rsp,
/// r8..r15, rip, rflags.
const RBX: usize = 1;
const RSP: usize = 7;
const RIP: usize = 16;

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!("xor rbp, rbp", "call {f}", f = sym main_trampoline);
}

extern "C" fn main_trampoline() -> ! {
    let pid = fork();
    if pid == 0 {
        child_traced();
    }
    tracer(pid as i32);
    syscall_lib::exit(0)
}

/// The tracee: request tracing, mark `rbx`, `int3`, then exit with `rbx`.
/// Written entirely in asm so `rbx` is under our control across the trap and the
/// parent's `SETREGS` is observable in the exit code.
fn child_traced() -> ! {
    // SAFETY: raw syscalls + int3; `noreturn` — the compiler relies on nothing
    // after this block, so clobbering rbx is fine.
    unsafe {
        core::arch::asm!(
            // PTRACE_TRACEME: syscall(SYS_PTRACE, 0, 0, 0, 0)
            "mov rax, {sys_ptrace}",
            "xor edi, edi",
            "xor esi, esi",
            "xor edx, edx",
            "xor r10d, r10d",
            "syscall",
            // Marker the parent's GETREGS verifies, then trap.
            "mov rbx, 0xCAFE",
            "int3",
            // Resumed here (RIP is past the int3). rbx was overwritten to 42 by
            // the parent's SETREGS; exit with it.
            "mov rdi, rbx",
            "mov rax, {sys_exit}",
            "syscall",
            sys_ptrace = const SYS_PTRACE,
            sys_exit = const SYS_EXIT,
            options(noreturn),
        )
    }
}

fn tracer(child: i32) {
    // 1. Wait for the tracee's first stop.
    let mut status: i32 = 0;
    waitpid(child, &mut status, 0);
    let wifstopped = (status & 0xff) == 0x7f;
    let stopsig = (status >> 8) & 0xff;
    if wifstopped && stopsig == 5 {
        p("PTRACE_SMOKE:stop-sigtrap ok\n");
    } else {
        phex("PTRACE_SMOKE:stop FAIL status=", status as u64);
    }

    // 2. GETREGS — read the tracee's register file.
    let mut regs = [0u64; 18];
    // SAFETY: kernel writes 144 bytes into `regs` on success.
    let g = unsafe {
        syscall4(
            SYS_PTRACE,
            PTRACE_GETREGS,
            child as u64,
            0,
            regs.as_mut_ptr() as u64,
        )
    };
    if g == 0 && regs[RBX] == 0xCAFE {
        p("PTRACE_SMOKE:getregs-rbx ok\n");
    } else {
        phex("PTRACE_SMOKE:getregs-rbx FAIL rbx=", regs[RBX]);
    }
    let rip = regs[RIP];
    let rsp = regs[RSP];

    // 3. PEEKTEXT — the int3 byte (0xCC) sits at RIP-1 (RIP is past it).
    // SAFETY: reads one word from the tracee's address space.
    let word = unsafe {
        syscall4(
            SYS_PTRACE,
            PTRACE_PEEKTEXT,
            child as u64,
            rip.wrapping_sub(1),
            0,
        )
    };
    if (word & 0xff) == 0xcc {
        p("PTRACE_SMOKE:peek-int3 ok\n");
    } else {
        phex("PTRACE_SMOKE:peek-int3 FAIL word=", word);
    }

    // 4. POKETEXT + PEEKTEXT round-trip through the tracee's stack scratch (the
    //    128-byte red zone below RSP is safe to clobber while stopped).
    let scratch = rsp.wrapping_sub(16);
    const MAGIC: u64 = 0xABCD_1234_5678_9AB0;
    // SAFETY: writes/reads one word in the tracee's mapped stack page.
    let poke = unsafe { syscall4(SYS_PTRACE, PTRACE_POKETEXT, child as u64, scratch, MAGIC) };
    let back = unsafe { syscall4(SYS_PTRACE, PTRACE_PEEKTEXT, child as u64, scratch, 0) };
    if poke == 0 && back == MAGIC {
        p("PTRACE_SMOKE:poke-roundtrip ok\n");
    } else {
        phex("PTRACE_SMOKE:poke-roundtrip FAIL back=", back);
    }

    // 5. SETREGS — rewrite rbx to 42 so the resumed tracee exits with it.
    regs[RBX] = 42;
    // SAFETY: kernel reads 144 bytes from `regs`.
    let s = unsafe {
        syscall4(
            SYS_PTRACE,
            PTRACE_SETREGS,
            child as u64,
            0,
            regs.as_ptr() as u64,
        )
    };
    if s == 0 {
        p("PTRACE_SMOKE:setregs ok\n");
    } else {
        p("PTRACE_SMOKE:setregs FAIL\n");
    }

    // 6. CONT — resume the tracee.
    // SAFETY: raw ptrace syscall.
    let _ = unsafe { syscall4(SYS_PTRACE, PTRACE_CONT, child as u64, 0, 0) };

    // 7. Second wait — the tracee exits with the SETREGS'd rbx (42).
    let mut status2: i32 = 0;
    waitpid(child, &mut status2, 0);
    let code = (status2 >> 8) & 0xff;
    if code == 42 {
        p("PTRACE_SMOKE:setregs-effect ok (exit=42)\n");
    } else {
        phex("PTRACE_SMOKE:setregs-effect FAIL code=", code as u64);
    }

    p("PTRACE_SMOKE:done\n");
}

fn p(s: &str) {
    write(1, s.as_bytes());
}

/// Print `label` followed by `0x…` hex and a newline.
fn phex(label: &str, n: u64) {
    p(label);
    let mut buf = [0u8; 18];
    buf[0] = b'0';
    buf[1] = b'x';
    for i in 0..16 {
        let nib = ((n >> ((15 - i) * 4)) & 0xf) as u8;
        buf[2 + i] = if nib < 10 {
            b'0' + nib
        } else {
            b'a' + nib - 10
        };
    }
    write(1, &buf);
    p("\n");
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    p("PTRACE_SMOKE:PANIC\n");
    syscall_lib::exit(101)
}
