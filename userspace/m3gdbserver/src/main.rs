//! `m3gdbserver` — a native GDB stub server for ring-3 programs (Phase 111
//! Track D.3).
//!
//! Debugs the OS's own userspace programs the Linux `gdbserver` way: it
//! `fork`s, has the child `PTRACE_TRACEME` + `execve` the target (which
//! exec-stops before its first instruction — see the kernel exec-stop), then
//! translates the **GDB Remote Serial Protocol** it speaks with a host GDB over
//! TCP into `sys_ptrace` requests against the tracee. The kernel is alive during
//! userspace debugging, so ordinary IRQ-driven TCP works — no polled link (that
//! is the kgdb stub's frozen-kernel constraint, not this).
//!
//! Usage: `m3gdbserver <port> <program> [args...]`. Listens on `0.0.0.0:<port>`,
//! accepts one GDB connection, and serves it. The RSP wire codec is the same
//! host-tested `kernel_core::gdb_rsp` the in-kernel kgdb stub uses.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::gdb_rsp::{self, PacketReader, RspEvent};
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{
    AF_INET, SO_REUSEADDR, SOCK_STREAM, SOL_SOCKET, SockaddrIn, accept, bind, close, execve, fork,
    listen, read, setsockopt, socket, syscall4, waitpid, write, write_str,
};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! {
    write(2, b"m3gdbserver: OOM\n");
    syscall_lib::exit(1)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write(2, b"m3gdbserver: PANIC\n");
    syscall_lib::exit(1)
}

syscall_lib::entry_point!(program_main);

/// m3OS ptrace syscall (feature-gated `0x1152`).
const SYS_PTRACE: u64 = 0x1152;
// Linux ptrace request numbers.
const PTRACE_PEEKTEXT: u64 = 1;
const PTRACE_POKETEXT: u64 = 4;
const PTRACE_CONT: u64 = 7;
const PTRACE_KILL: u64 = 8;
const PTRACE_SINGLESTEP: u64 = 9;
const PTRACE_GETREGS: u64 = 12;
const PTRACE_SETREGS: u64 = 13;
const PTRACE_TRACEME: u64 = 0;

/// `SavedUserRegs` layout (18 × u64): rax,rbx,rcx,rdx,rsi,rdi,rbp,rsp,r8..r15,
/// rip,rflags — already GDB amd64 GPR order for the first 17.
const REG_COUNT: usize = 18;
const REG_RIP: usize = 16;
const REG_RFLAGS: usize = 17;

#[inline]
fn ptrace(req: u64, pid: u64, addr: u64, data: u64) -> u64 {
    // SAFETY: raw ptrace syscall — the kernel validates every argument.
    unsafe { syscall4(SYS_PTRACE, req, pid, addr, data) }
}

fn program_main(args: &[&str]) -> i32 {
    if args.len() < 3 {
        write_str(2, "usage: m3gdbserver <port> <program> [args...]\n");
        return 2;
    }
    let Some(port) = parse_u16(args[1]) else {
        write_str(2, "m3gdbserver: bad port\n");
        return 2;
    };
    let program = args[2];

    let lfd = match setup_listener(port) {
        Ok(fd) => fd,
        Err(()) => {
            write_str(2, "m3gdbserver: failed to bind/listen\n");
            return 1;
        }
    };
    write_str(1, "M3GDBSERVER:listening on ");
    write_dec(1, port as u64);
    write_str(1, "\n");

    // Accept one GDB connection.
    let cfd = accept(lfd, None);
    if cfd < 0 {
        write_str(2, "m3gdbserver: accept failed\n");
        return 1;
    }
    let cfd = cfd as i32;
    write_str(1, "M3GDBSERVER:client connected\n");

    // fork + TRACEME + execve the tracee (it exec-stops before running).
    let pid = fork();
    if pid == 0 {
        close(cfd);
        close(lfd);
        ptrace(PTRACE_TRACEME, 0, 0, 0);
        exec_program(program); // -> ! (execve, or exit(127) on failure)
    }
    close(lfd);
    let tracee = pid as i32;

    // Reap the exec-stop so the tracee is parked and inspectable.
    let mut status: i32 = 0;
    waitpid(tracee, &mut status, 0);
    write_str(1, "M3GDBSERVER:tracee exec-stopped\n");

    serve(cfd, tracee);
    close(cfd);
    write_str(1, "M3GDBSERVER:session closed\n");
    0
}

/// `execve(program, [program, NULL], [NULL])`. Only returns (via `exit`) if
/// `execve` fails.
fn exec_program(program: &str) -> ! {
    let mut path = Vec::with_capacity(program.len() + 1);
    path.extend_from_slice(program.as_bytes());
    path.push(0);
    let argv: [*const u8; 2] = [path.as_ptr(), core::ptr::null()];
    let envp: [*const u8; 1] = [core::ptr::null()];
    execve(&path, &argv, &envp);
    write_str(2, "m3gdbserver: execve failed\n");
    syscall_lib::exit(127)
}

fn setup_listener(port: u16) -> Result<i32, ()> {
    let fd = socket(AF_INET as i32, SOCK_STREAM as i32, 0);
    if fd < 0 {
        return Err(());
    }
    let fd = fd as i32;
    let one: i32 = 1;
    // SAFETY: reinterpret the i32 as its 4 bytes for setsockopt.
    let optval =
        unsafe { core::slice::from_raw_parts(&one as *const i32 as *const u8, size_of::<i32>()) };
    setsockopt(fd, SOL_SOCKET as i32, SO_REUSEADDR as i32, optval);
    let addr = SockaddrIn::new([0, 0, 0, 0], port);
    if bind(fd, &addr) < 0 {
        close(fd);
        return Err(());
    }
    if listen(fd, 1) < 0 {
        close(fd);
        return Err(());
    }
    Ok(fd)
}

// ---------------------------------------------------------------------------
// RSP session
// ---------------------------------------------------------------------------

/// A planted software breakpoint: `(addr, original_byte)`.
struct SwBreak {
    addr: u64,
    orig: u8,
}

/// Serve the RSP session until the client detaches/kills or the socket closes.
fn serve(sock: i32, tracee: i32) {
    let mut reader = PacketReader::new();
    let mut breaks: Vec<SwBreak> = Vec::new();
    let mut rxbuf = [0u8; 1024];
    loop {
        let n = read(sock, &mut rxbuf);
        if n <= 0 {
            return;
        }
        for i in 0..n as usize {
            match reader.feed(rxbuf[i]) {
                Some(RspEvent::Packet(len)) => {
                    // ACK the packet, then dispatch its (owned) payload.
                    let _ = write(sock, b"+");
                    let payload: Vec<u8> = reader.payload()[..len].to_vec();
                    if dispatch(sock, tracee, &payload, &mut breaks) {
                        return; // detach / kill
                    }
                }
                Some(RspEvent::BadChecksum) => {
                    let _ = write(sock, b"-");
                }
                Some(RspEvent::Interrupt) => {
                    // GDB Ctrl-C on a running target. We only reach `read` while
                    // stopped (c/s block in waitpid), so treat as a no-op here.
                }
                _ => {}
            }
        }
    }
}

/// Handle one RSP packet. Returns `true` if the session should end.
fn dispatch(sock: i32, tracee: i32, pkt: &[u8], breaks: &mut Vec<SwBreak>) -> bool {
    let Some(&first) = pkt.first() else {
        send_packet(sock, b"");
        return false;
    };
    match first {
        b'?' => send_packet(sock, b"S05"),
        b'g' => reply_registers(sock, tracee),
        b'G' => {
            write_registers(tracee, &pkt[1..]);
            send_packet(sock, b"OK");
        }
        b'P' => {
            write_one_register(tracee, &pkt[1..]);
            send_packet(sock, b"OK");
        }
        b'm' => reply_mem_read(sock, tracee, &pkt[1..]),
        b'M' => reply_mem_write(sock, tracee, &pkt[1..]),
        b'Z' => handle_insert_bp(sock, tracee, &pkt[1..], breaks),
        b'z' => handle_remove_bp(sock, tracee, &pkt[1..], breaks),
        b'c' => {
            resume_and_report(sock, tracee, PTRACE_CONT);
        }
        b's' => {
            resume_and_report(sock, tracee, PTRACE_SINGLESTEP);
        }
        b'k' => {
            ptrace(PTRACE_KILL, tracee as u64, 0, 0);
            return true;
        }
        b'D' => {
            // Detach — remove breakpoints, resume, end the session.
            for b in breaks.iter() {
                poke_byte(tracee, b.addr, b.orig);
            }
            ptrace(PTRACE_CONT, tracee as u64, 0, 0);
            send_packet(sock, b"OK");
            return true;
        }
        b'H' => send_packet(sock, b"OK"),
        b'q' => handle_query(sock, pkt),
        _ => send_packet(sock, b""),
    }
    false
}

fn handle_query(sock: i32, pkt: &[u8]) {
    if pkt.starts_with(b"qSupported") {
        send_packet(sock, b"PacketSize=400");
    } else if pkt.starts_with(b"qAttached") {
        send_packet(sock, b"0"); // we created the process
    } else if pkt.starts_with(b"qC") {
        send_packet(sock, b"QC1");
    } else if pkt.starts_with(b"qfThreadInfo") {
        send_packet(sock, b"m1");
    } else if pkt.starts_with(b"qsThreadInfo") {
        send_packet(sock, b"l");
    } else {
        send_packet(sock, b"");
    }
}

// ---------------------------------------------------------------------------
// Registers (GDB amd64 order)
// ---------------------------------------------------------------------------

fn getregs(tracee: i32) -> [u64; REG_COUNT] {
    let mut regs = [0u64; REG_COUNT];
    ptrace(PTRACE_GETREGS, tracee as u64, 0, regs.as_mut_ptr() as u64);
    regs
}

fn setregs(tracee: i32, regs: &[u64; REG_COUNT]) {
    ptrace(PTRACE_SETREGS, tracee as u64, 0, regs.as_ptr() as u64);
}

/// `g` — all registers in GDB amd64 order: the 16 GPRs + rip (SavedUserRegs
/// indices 0..17), then eflags (u32), then cs/ss/ds/es/fs/gs (u32 each).
fn reply_registers(sock: i32, tracee: i32) {
    let regs = getregs(tracee);
    let mut out: Vec<u8> = Vec::with_capacity(2 * (17 * 8 + 7 * 4));
    for &r in regs.iter().take(REG_RFLAGS) {
        push_u64_le_hex(&mut out, r);
    }
    push_u32_le_hex(&mut out, regs[REG_RFLAGS] as u32); // eflags
    push_u32_le_hex(&mut out, 0x33); // cs (user code)
    push_u32_le_hex(&mut out, 0x2b); // ss (user data)
    for _ in 0..4 {
        push_u32_le_hex(&mut out, 0); // ds, es, fs, gs
    }
    send_packet(sock, &out);
}

/// `G` — write the register block back (GPRs + rip + eflags; segments ignored).
fn write_registers(tracee: i32, hex: &[u8]) {
    let mut regs = getregs(tracee);
    // 17 × u64 (rax..rip) then eflags (u32).
    for (i, reg) in regs.iter_mut().enumerate().take(REG_RFLAGS) {
        if let Some(v) = read_u64_le_hex(hex, i * 16) {
            *reg = v;
        }
    }
    if let Some(ef) = read_u32_le_hex(hex, 17 * 16) {
        regs[REG_RFLAGS] = (regs[REG_RFLAGS] & !0xFFFF_FFFF) | ef as u64;
    }
    setregs(tracee, &regs);
}

/// `P<n>=<value>` — write one register (LE hex). `n` is the GDB register number.
fn write_one_register(tracee: i32, rest: &[u8]) {
    let (n, consumed) = gdb_rsp::parse_hex_prefix(rest);
    if consumed == 0 || rest.get(consumed) != Some(&b'=') {
        return;
    }
    let val_hex = &rest[consumed + 1..];
    let mut regs = getregs(tracee);
    let idx = n as usize;
    if idx < REG_RIP + 1 {
        if let Some(v) = read_u64_le_hex(val_hex, 0) {
            regs[idx] = v;
        }
    } else if idx == 17 {
        if let Some(ef) = read_u32_le_hex(val_hex, 0) {
            regs[REG_RFLAGS] = (regs[REG_RFLAGS] & !0xFFFF_FFFF) | ef as u64;
        }
    }
    setregs(tracee, &regs);
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// `m<addr>,<len>` — read tracee memory (word-at-a-time via PEEKTEXT).
fn reply_mem_read(sock: i32, tracee: i32, rest: &[u8]) {
    let (addr, n1) = gdb_rsp::parse_hex_prefix(rest);
    if n1 == 0 || rest.get(n1) != Some(&b',') {
        send_packet(sock, b"E22");
        return;
    }
    let (len, n2) = gdb_rsp::parse_hex_prefix(&rest[n1 + 1..]);
    if n2 == 0 {
        send_packet(sock, b"E22");
        return;
    }
    let len = (len as usize).min(512);
    let mut out: Vec<u8> = Vec::with_capacity(len * 2);
    for i in 0..len {
        let b = peek_byte(tracee, addr + i as u64);
        push_u8_hex(&mut out, b);
    }
    send_packet(sock, &out);
}

/// `M<addr>,<len>:<hex>` — write tracee memory.
fn reply_mem_write(sock: i32, tracee: i32, rest: &[u8]) {
    let (addr, n1) = gdb_rsp::parse_hex_prefix(rest);
    if n1 == 0 || rest.get(n1) != Some(&b',') {
        send_packet(sock, b"E22");
        return;
    }
    let (len, n2) = gdb_rsp::parse_hex_prefix(&rest[n1 + 1..]);
    let colon = n1 + 1 + n2;
    if n2 == 0 || rest.get(colon) != Some(&b':') {
        send_packet(sock, b"E22");
        return;
    }
    let hex = &rest[colon + 1..];
    let len = (len as usize).min(512);
    for i in 0..len {
        let Some(b) = read_u8_hex(hex, i * 2) else {
            send_packet(sock, b"E22");
            return;
        };
        poke_byte(tracee, addr + i as u64, b);
    }
    send_packet(sock, b"OK");
}

/// Read one byte from the tracee (PEEKTEXT reads a word; take the low byte).
fn peek_byte(tracee: i32, addr: u64) -> u8 {
    (ptrace(PTRACE_PEEKTEXT, tracee as u64, addr, 0) & 0xff) as u8
}

/// Write one byte into the tracee (read-modify-write the containing word).
fn poke_byte(tracee: i32, addr: u64, val: u8) {
    let word = ptrace(PTRACE_PEEKTEXT, tracee as u64, addr, 0);
    let new = (word & !0xff) | val as u64;
    ptrace(PTRACE_POKETEXT, tracee as u64, addr, new);
}

// ---------------------------------------------------------------------------
// Breakpoints
// ---------------------------------------------------------------------------

/// `Z<type>,<addr>,<kind>` — insert. Type 0 = software (int3 patch).
fn handle_insert_bp(sock: i32, tracee: i32, rest: &[u8], breaks: &mut Vec<SwBreak>) {
    let Some((ty, addr)) = parse_bp(rest) else {
        send_packet(sock, b"");
        return;
    };
    if ty != b'0' {
        send_packet(sock, b""); // only software breakpoints
        return;
    }
    if !breaks.iter().any(|b| b.addr == addr) {
        let orig = peek_byte(tracee, addr);
        poke_byte(tracee, addr, 0xCC);
        breaks.push(SwBreak { addr, orig });
    }
    send_packet(sock, b"OK");
}

/// `z<type>,<addr>,<kind>` — remove.
fn handle_remove_bp(sock: i32, tracee: i32, rest: &[u8], breaks: &mut Vec<SwBreak>) {
    let Some((ty, addr)) = parse_bp(rest) else {
        send_packet(sock, b"");
        return;
    };
    if ty != b'0' {
        send_packet(sock, b"");
        return;
    }
    if let Some(pos) = breaks.iter().position(|b| b.addr == addr) {
        poke_byte(tracee, addr, breaks[pos].orig);
        breaks.swap_remove(pos);
    }
    send_packet(sock, b"OK");
}

fn parse_bp(rest: &[u8]) -> Option<(u8, u64)> {
    let ty = *rest.first()?;
    let after = rest.get(2..)?; // skip "<ty>,"
    let (addr, n) = gdb_rsp::parse_hex_prefix(after);
    if n == 0 {
        return None;
    }
    Some((ty, addr))
}

// ---------------------------------------------------------------------------
// Resume + stop reply
// ---------------------------------------------------------------------------

/// `c`/`s` — resume (or single-step) the tracee, wait for its next stop, and
/// send the stop reply (`S<sig>` for a trap, `W<code>` for exit, `X<sig>` for a
/// terminating signal).
fn resume_and_report(sock: i32, tracee: i32, req: u64) {
    ptrace(req, tracee as u64, 0, 0);
    let mut status: i32 = 0;
    waitpid(tracee, &mut status, 0);
    send_stop_reply(sock, status);
}

fn send_stop_reply(sock: i32, status: i32) {
    if (status & 0xff) == 0x7f {
        // WIFSTOPPED: signal in bits 8..15.
        let sig = ((status >> 8) & 0xff) as u8;
        let body = [b'S', hex_hi(sig), hex_lo(sig)];
        send_packet(sock, &body);
    } else if (status & 0x7f) == 0 {
        // WIFEXITED: exit code in bits 8..15.
        let code = ((status >> 8) & 0xff) as u8;
        let body = [b'W', hex_hi(code), hex_lo(code)];
        send_packet(sock, &body);
    } else {
        // WIFSIGNALED: terminating signal in low 7 bits.
        let sig = (status & 0x7f) as u8;
        let body = [b'X', hex_hi(sig), hex_lo(sig)];
        send_packet(sock, &body);
    }
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

fn send_packet(sock: i32, payload: &[u8]) {
    let mut out = vec![0u8; payload.len() + 4];
    if let Some(n) = gdb_rsp::encode_packet(payload, &mut out) {
        write_all(sock, &out[..n]);
    }
}

fn write_all(sock: i32, mut buf: &[u8]) {
    while !buf.is_empty() {
        let n = write(sock, buf);
        if n <= 0 {
            return;
        }
        buf = &buf[n as usize..];
    }
}

fn push_u64_le_hex(out: &mut Vec<u8>, v: u64) {
    for b in v.to_le_bytes() {
        push_u8_hex(out, b);
    }
}
fn push_u32_le_hex(out: &mut Vec<u8>, v: u32) {
    for b in v.to_le_bytes() {
        push_u8_hex(out, b);
    }
}
fn push_u8_hex(out: &mut Vec<u8>, b: u8) {
    out.push(hex_hi(b));
    out.push(hex_lo(b));
}

/// Read `count` little-endian hex bytes starting at char offset `off`.
fn read_u64_le_hex(hex: &[u8], off: usize) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..8 {
        v |= (read_u8_hex(hex, off + i * 2)? as u64) << (8 * i);
    }
    Some(v)
}
fn read_u32_le_hex(hex: &[u8], off: usize) -> Option<u32> {
    let mut v = 0u32;
    for i in 0..4 {
        v |= (read_u8_hex(hex, off + i * 2)? as u32) << (8 * i);
    }
    Some(v)
}
fn read_u8_hex(hex: &[u8], off: usize) -> Option<u8> {
    let hi = gdb_rsp::parse_hex_digit(*hex.get(off)?)?;
    let lo = gdb_rsp::parse_hex_digit(*hex.get(off + 1)?)?;
    Some((hi << 4) | lo)
}

#[inline]
fn hex_hi(b: u8) -> u8 {
    hex_nib(b >> 4)
}
#[inline]
fn hex_lo(b: u8) -> u8 {
    hex_nib(b & 0xf)
}
#[inline]
fn hex_nib(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'a' + (n - 10) }
}

fn parse_u16(s: &str) -> Option<u16> {
    let mut v: u32 = 0;
    if s.is_empty() {
        return None;
    }
    for &b in s.as_bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (b - b'0') as u32;
        if v > 65535 {
            return None;
        }
    }
    Some(v as u16)
}

fn write_dec(fd: i32, mut n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if n == 0 {
        write(fd, b"0");
        return;
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    write(fd, &buf[i..]);
}
