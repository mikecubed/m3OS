//! In-kernel GDB Remote Serial Protocol stub (Phase 111 Track C.2/C.4).
//!
//! An **all-stop** debugger for the kernel itself. On a ring-0 `#BP`/`#DB` trap
//! (a planted software breakpoint, a hardware breakpoint, a single-step, or the
//! wait-for-debugger `int3` in [`kgdb_break`]) the machine freezes — every
//! other core parks in its NMI handler ([`crate::smp::kgdb_stop_all_aps`]) — and
//! this loop owns the CPU, servicing GDB packets over the polled COM2 transport
//! ([`super::com2`]) until the developer continues, steps, detaches, or kills.
//!
//! The protocol layer (framing, checksums, hex) is the host-tested
//! [`kernel_core::gdb_rsp`] codec; this module is the command dispatch + the
//! amd64 register mapping onto [`DebugTrapFrame`]. FPU/XSAVE state is deferred
//! (charter): the register file is the 16 GPRs + RIP + EFLAGS + the six segment
//! selectors, in GDB's amd64 order.
//!
//! Compiled only under the `kgdb` feature — the stub is arbitrary kernel
//! peek/poke and is OFF in production (see `kernel/Cargo.toml`).

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use kernel_core::gdb_rsp::{self, PacketReader, RspEvent};
use spin::Mutex;

use crate::arch::x86_64::debug::{
    self, DebugTrapFrame, insert_sw_breakpoint, read_kernel_bytes, remove_sw_breakpoint,
    write_kernel_bytes,
};
use crate::arch::x86_64::interrupts;
use crate::smp;

/// Max bytes serviced by a single `m`/`M`. Bounds the reply/scratch buffers.
const MAX_MEM: usize = 1024;
/// Register block wire size: 16×u64 GPR (incl. rsp) + rip(u64) + eflags(u32) +
/// 6 segment selectors (u32 each) = 164 bytes.
const REG_BYTES: usize = 16 * 8 + 8 + 4 + 6 * 4;
/// GDB `PacketSize` we advertise (hex): 0x400 = 1024, matching `MAX_MEM`.
const PACKET_SIZE_HEX: &str = "400";

/// Software-breakpoint slots planted via `Z0`. Small fixed table — a kernel
/// debug session rarely needs many, and this keeps the stub alloc-free.
const MAX_SW_BP: usize = 32;

#[derive(Clone, Copy)]
struct SwBreak {
    addr: u64,
    orig: u8,
    active: bool,
}

/// Stub state. All access is from the single owner core while the machine is
/// all-stopped, so the `Mutex` is uncontended — it exists only to satisfy the
/// borrow checker for the static, never to arbitrate real concurrency.
struct StubState {
    sw_bp: [SwBreak; MAX_SW_BP],
    reply: [u8; 2 * MAX_MEM + 16],
    scratch: [u8; MAX_MEM],
    last_reply_len: usize,
}

static STATE: Mutex<StubState> = Mutex::new(StubState {
    sw_bp: [SwBreak {
        addr: 0,
        orig: 0,
        active: false,
    }; MAX_SW_BP],
    reply: [0; 2 * MAX_MEM + 16],
    scratch: [0; MAX_MEM],
    last_reply_len: 0,
});

/// True once COM2 is initialized. Guards re-entry of `init`.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Set after a `c`/`s` so the *next* trap entry sends an unsolicited stop reply
/// (GDB is waiting for it). Clear on the initial wait-for-debugger entry, where
/// GDB drives with `?`.
static AWAITING_STOP_REPLY: AtomicBool = AtomicBool::new(false);

/// Sentinel: total timer ticks sampled at the last stop entry (for the all-stop
/// no-advance proof logged on release).
static PROGRESS_AT_STOP: AtomicUsize = AtomicUsize::new(0);

/// Initialize the polled COM2 transport. Idempotent.
pub fn init() {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return;
    }
    super::com2::init();
}

/// Non-inlined, deterministic breakpoint target for the `kgdb-smoke` gate: the
/// driver sets `Z0` at this symbol's address (`nm` + the 1 TiB PIE base) and
/// asserts the hit. Kept `#[inline(never)]` so the address is a real,
/// resolvable symbol (a raw-addr probe cannot follow DWARF inline sites).
#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn kgdb_probe_target() {
    // Touch KGDB_PROBE_MAGIC through a volatile read so neither the static nor
    // this call is optimized away, and so the gate has a live known value at a
    // resolvable address to read back over `m`.
    let v = unsafe { core::ptr::read_volatile(&raw const KGDB_PROBE_MAGIC) };
    core::hint::black_box(v);
}

/// Known-value the gate reads back over `m` to prove memory inspection works.
/// `#[used]` keeps the symbol in the ELF (it is only referenced externally, by
/// the gate's `m` read, so the optimizer would otherwise drop it).
#[used]
#[unsafe(no_mangle)]
pub static KGDB_PROBE_MAGIC: u64 = 0xB16B_00B5_CAFE_F00D;

/// Wait-for-debugger entry: freeze here at a known boot point until a GDB
/// client attaches over COM2 and continues. Prints a COM1 breadcrumb (the live
/// console) first so an operator without a debugger sees why the boot paused.
/// The `int3` is a compiled-in trap (not a planted breakpoint), so the stub
/// resumes *past* it on continue — no rewind, no loop.
pub fn kgdb_break() {
    init();
    crate::serial::_print(format_args!(
        "KGDB:waiting for debugger on COM2 (0x2F8 → host TCP)\n"
    ));
    // SAFETY: int3 raises #BP → naked entry → on_breakpoint → this stub.
    unsafe { core::arch::asm!("int3", options(nostack)) };
    crate::serial::_print(format_args!("KGDB:resumed\n"));
}

// ---------------------------------------------------------------------------
// Trap entry points (called from the Track B seam in arch::x86_64::debug)
// ---------------------------------------------------------------------------

/// Ring-0 `#BP` consumer. Returns `true` (event consumed) — the kgdb stub owns
/// every ring-0 breakpoint while the feature is live. Rewinds RIP to the
/// breakpoint address for a *planted* `Z0` breakpoint (so `g` reports the bp
/// location and continue re-executes the restored instruction); a compiled-in
/// `int3` (e.g. [`kgdb_break`]) is left pointing past the byte so continue does
/// not loop.
pub fn on_breakpoint(bp_addr: u64, frame: &mut DebugTrapFrame) -> bool {
    if is_planted_bp(bp_addr) {
        frame.rip = bp_addr;
    }
    session(StopReason::SwBreak, frame);
    true
}

/// Ring-0 `#DB` consumer (single-step completion / hardware breakpoint hit).
pub fn on_debug_exception(
    status: &kernel_core::debug_regs::Dr6Status,
    _rip: u64,
    frame: &mut DebugTrapFrame,
) -> bool {
    // Stop stepping; the stub re-arms TF if the user steps again.
    if status.single_step {
        debug::clear_single_step(frame);
    }
    session(StopReason::Trap, frame);
    true
}

/// Panic hook (Track C.4): drop a bare-metal panic into the stub instead of
/// halting, so a crashed machine becomes an interactive post-mortem. Enters via
/// a fresh `int3` so the stub gets a real [`DebugTrapFrame`] through the normal
/// naked-entry path; returns after the developer detaches/continues (the panic
/// path then halts as usual). By the time this runs the panic AP-quiesce has
/// already parked the sibling cores, so the stub's own all-stop finds none
/// online and no-ops.
pub fn enter_from_panic() {
    init();
    crate::serial::_print(format_args!("KGDB:panic — entering stub (int3 on COM2)\n"));
    // SAFETY: int3 → #BP → naked entry → on_breakpoint → session.
    unsafe { core::arch::asm!("int3", options(nostack)) };
}

/// True if an async break is pending on COM2: the stub has been attached
/// (`init` ran) and a byte is waiting. The timer poll uses this as a cheap
/// per-tick guard so it only builds a trap frame when there is actually
/// something to service.
pub fn async_break_pending() -> bool {
    INITIALIZED.load(Ordering::Acquire) && super::com2::rx_pending()
}

/// Async-break entry (Track C.4): GDB `Ctrl-C` sends a lone `0x03` on the link
/// while the guest is *running*. Called from the timer tick (the only reliable
/// poll point for a busy guest) with the interrupted context as `frame`. If the
/// pending byte is the `0x03` interrupt, break into the stub at the interrupted
/// RIP — send the stop reply GDB is waiting for, then serve the session and
/// resume on continue. Any other stray byte is consumed and ignored.
///
/// Returns `true` if it entered the stub (so the caller writes the possibly
/// register-modified `frame` back to the interrupted context).
pub fn poll_async_break(frame: &mut DebugTrapFrame) -> bool {
    if !INITIALIZED.load(Ordering::Acquire) {
        return false;
    }
    match super::com2::try_read_byte() {
        Some(b) if b == gdb_rsp::INTERRUPT => {
            // GDB sent 0x03 and is waiting for a stop reply.
            AWAITING_STOP_REPLY.store(true, Ordering::Release);
            session(StopReason::Trap, frame);
            true
        }
        _ => false,
    }
}

#[derive(Clone, Copy)]
enum StopReason {
    SwBreak,
    Trap,
}

impl StopReason {
    /// GDB stop-reply signal number (SIGTRAP = 5 for all debug stops).
    fn signal(self) -> u8 {
        5
    }
}

// ---------------------------------------------------------------------------
// Session loop
// ---------------------------------------------------------------------------

/// Own the machine: all-stop the other cores, send the stop reply if one is
/// owed, then service packets until the developer resumes. Releases the cores
/// on the way out.
fn session(reason: StopReason, frame: &mut DebugTrapFrame) {
    let parked_mask = smp::kgdb_stop_all_aps();
    PROGRESS_AT_STOP.store(interrupts::total_timer_ticks() as usize, Ordering::Relaxed);

    // If we were resumed from a c/s, GDB is waiting for an unsolicited stop
    // reply; send it. On the first (wait-for-debugger) entry GDB drives with
    // `?`, so stay quiet.
    if AWAITING_STOP_REPLY.swap(false, Ordering::AcqRel) {
        send_stop_reply(reason);
    }

    loop {
        let payload = read_packet();
        match dispatch(payload, frame, reason) {
            Flow::Reply => {}
            Flow::Resume => break,
        }
    }

    // Sentinel: prove the parked cores did not advance while stopped.
    let before = PROGRESS_AT_STOP.load(Ordering::Relaxed) as u64;
    let after = interrupts::total_timer_ticks();
    crate::serial::_print(format_args!(
        "KGDB:release parked_mask={parked_mask:#x} ticks_before={before} ticks_after={after}\n"
    ));
    smp::kgdb_release_aps();
}

enum Flow {
    Reply,
    Resume,
}

/// A fresh `PacketReader` per session-read. Kept local (not static) so a
/// re-entrant single-step trap during a session gets a clean reader.
fn read_packet() -> PacketBuf {
    let mut reader = PacketReader::new();
    loop {
        let Some(b) = super::com2::try_read_byte() else {
            core::hint::spin_loop();
            continue;
        };
        match reader.feed(b) {
            Some(RspEvent::Packet(len)) => {
                super::com2::write_byte(gdb_rsp::ACK);
                let mut buf = PacketBuf::new();
                buf.copy_from(&reader.payload()[..len]);
                return buf;
            }
            Some(RspEvent::BadChecksum) => {
                super::com2::write_byte(gdb_rsp::NAK);
            }
            Some(RspEvent::Nak) => {
                // GDB rejected our last reply — retransmit it.
                retransmit_last();
            }
            Some(RspEvent::Ack) | Some(RspEvent::Interrupt) | None => {}
        }
    }
}

/// Owned copy of a decoded packet payload (so we drop the reader's borrow).
struct PacketBuf {
    buf: [u8; gdb_rsp::MAX_PACKET],
    len: usize,
}

impl PacketBuf {
    fn new() -> Self {
        PacketBuf {
            buf: [0; gdb_rsp::MAX_PACKET],
            len: 0,
        }
    }
    fn copy_from(&mut self, src: &[u8]) {
        let n = src.len().min(self.buf.len());
        self.buf[..n].copy_from_slice(&src[..n]);
        self.len = n;
    }
    fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

fn dispatch(pkt: PacketBuf, frame: &mut DebugTrapFrame, reason: StopReason) -> Flow {
    let p = pkt.as_slice();
    let Some(&first) = p.first() else {
        // Empty packet — GDB's `vMustReplyEmpty` probe path; ack with empty.
        send_packet(b"");
        return Flow::Reply;
    };
    match first {
        b'?' => {
            send_stop_reply(reason);
            Flow::Reply
        }
        b'g' => {
            reply_registers(frame);
            Flow::Reply
        }
        b'G' => {
            write_registers(&p[1..], frame);
            send_packet(b"OK");
            Flow::Reply
        }
        b'm' => {
            reply_mem_read(&p[1..]);
            Flow::Reply
        }
        b'M' => {
            reply_mem_write(&p[1..]);
            Flow::Reply
        }
        b'c' => {
            AWAITING_STOP_REPLY.store(true, Ordering::Release);
            Flow::Resume
        }
        b's' => {
            debug::set_single_step(frame);
            AWAITING_STOP_REPLY.store(true, Ordering::Release);
            Flow::Resume
        }
        b'Z' => {
            handle_insert_bp(&p[1..]);
            Flow::Reply
        }
        b'z' => {
            handle_remove_bp(&p[1..]);
            Flow::Reply
        }
        b'D' => {
            // Detach — remove all planted breakpoints, ack, resume.
            remove_all_sw_bp();
            send_packet(b"OK");
            AWAITING_STOP_REPLY.store(false, Ordering::Release);
            Flow::Resume
        }
        b'k' => {
            // Kill — a kernel cannot be killed; treat as detach-and-continue.
            remove_all_sw_bp();
            AWAITING_STOP_REPLY.store(false, Ordering::Release);
            Flow::Resume
        }
        b'q' => {
            handle_query(p);
            Flow::Reply
        }
        b'H' => {
            // Thread selection — single "thread"; accept any.
            send_packet(b"OK");
            Flow::Reply
        }
        _ => {
            // Unknown command → empty reply ("unsupported").
            send_packet(b"");
            Flow::Reply
        }
    }
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

fn handle_query(p: &[u8]) {
    if p.starts_with(b"qSupported") {
        let mut body = [0u8; 32];
        let prefix = b"PacketSize=";
        body[..prefix.len()].copy_from_slice(prefix);
        let n = prefix.len();
        let hex = PACKET_SIZE_HEX.as_bytes();
        body[n..n + hex.len()].copy_from_slice(hex);
        send_packet(&body[..n + hex.len()]);
    } else if p.starts_with(b"qAttached") {
        // 1 = attached to an existing process (do not kill on detach).
        send_packet(b"1");
    } else if p.starts_with(b"qC") {
        // Current thread id — we model a single thread "1".
        send_packet(b"QC1");
    } else if p.starts_with(b"qfThreadInfo") {
        send_packet(b"m1");
    } else if p.starts_with(b"qsThreadInfo") {
        send_packet(b"l");
    } else {
        send_packet(b"");
    }
}

/// `Z<type>,<addr>,<kind>` — insert a breakpoint. Type 0 = software (int3),
/// type 1 = hardware execute (DR). Others unsupported (empty reply).
fn handle_insert_bp(rest: &[u8]) {
    let Some((ty, addr)) = parse_bp(rest) else {
        send_packet(b"");
        return;
    };
    match ty {
        b'0' => {
            if insert_planted_bp(addr) {
                send_packet(b"OK");
            } else {
                send_packet(b"E01"); // table full
            }
        }
        b'1' => {
            use kernel_core::debug_regs::{BreakCondition, BreakLength, SlotConfig};
            debug::DebugRegs::arm(
                0,
                addr,
                SlotConfig {
                    condition: BreakCondition::Execute,
                    length: BreakLength::One,
                },
            );
            send_packet(b"OK");
        }
        _ => send_packet(b""),
    }
}

fn handle_remove_bp(rest: &[u8]) {
    let Some((ty, addr)) = parse_bp(rest) else {
        send_packet(b"");
        return;
    };
    match ty {
        b'0' => {
            remove_planted_bp(addr);
            send_packet(b"OK");
        }
        b'1' => {
            debug::DebugRegs::disarm(0);
            send_packet(b"OK");
        }
        _ => send_packet(b""),
    }
}

/// Parse `<type>,<addr>,<kind>` → (type_byte, addr). Ignores kind.
fn parse_bp(rest: &[u8]) -> Option<(u8, u64)> {
    let ty = *rest.first()?;
    // rest = "0,addr,kind" — skip type + comma.
    let after_comma = rest.get(2..)?;
    let (addr, n) = gdb_rsp::parse_hex_prefix(after_comma);
    if n == 0 {
        return None;
    }
    Some((ty, addr))
}

// ---------------------------------------------------------------------------
// Register (de)serialization — GDB amd64 order, little-endian hex
// ---------------------------------------------------------------------------
//
// Order: rax rbx rcx rdx rsi rdi rbp rsp r8..r15 rip eflags cs ss ds es fs gs.
// Our DebugTrapFrame.gprs = [rax rbx rcx rdx rsi rdi rbp r8..r15]; rsp/rip/etc
// are separate fields. ds/es/fs/gs are not tracked (0).

/// Emit each register value as little-endian hex into `out`, returning bytes
/// written.
fn registers_to_hex(frame: &DebugTrapFrame, out: &mut [u8]) -> usize {
    let mut pos = 0;
    let put_u64 = |v: u64, out: &mut [u8], pos: &mut usize| {
        let n = gdb_rsp::hex_encode(&v.to_le_bytes(), &mut out[*pos..]).unwrap_or(0);
        *pos += n;
    };
    // GPRs in GDB order (rsp inserted between rbp and r8).
    put_u64(frame.gprs[0], out, &mut pos); // rax
    put_u64(frame.gprs[1], out, &mut pos); // rbx
    put_u64(frame.gprs[2], out, &mut pos); // rcx
    put_u64(frame.gprs[3], out, &mut pos); // rdx
    put_u64(frame.gprs[4], out, &mut pos); // rsi
    put_u64(frame.gprs[5], out, &mut pos); // rdi
    put_u64(frame.gprs[6], out, &mut pos); // rbp
    put_u64(frame.rsp, out, &mut pos); // rsp
    for i in 7..15 {
        put_u64(frame.gprs[i], out, &mut pos); // r8..r15
    }
    put_u64(frame.rip, out, &mut pos); // rip
    // eflags is 32-bit.
    let n = gdb_rsp::hex_encode(&(frame.rflags as u32).to_le_bytes(), &mut out[pos..]).unwrap_or(0);
    pos += n;
    // Segment selectors (32-bit each): cs, ss, ds, es, fs, gs.
    let put_u32 = |v: u32, out: &mut [u8], pos: &mut usize| {
        let n = gdb_rsp::hex_encode(&v.to_le_bytes(), &mut out[*pos..]).unwrap_or(0);
        *pos += n;
    };
    put_u32(frame.cs as u32, out, &mut pos);
    put_u32(frame.ss as u32, out, &mut pos);
    put_u32(0, out, &mut pos); // ds
    put_u32(0, out, &mut pos); // es
    put_u32(0, out, &mut pos); // fs
    put_u32(0, out, &mut pos); // gs
    pos
}

fn reply_registers(frame: &DebugTrapFrame) {
    let mut hex = [0u8; REG_BYTES * 2];
    let n = registers_to_hex(frame, &mut hex);
    send_packet(&hex[..n]);
}

/// Parse a `G` register block (little-endian hex) back into the frame. Writes
/// only the fields the CPU reloads (GPRs / rip / eflags); segment selectors are
/// accepted but ignored (we do not reload segments from the debugger).
fn write_registers(hex: &[u8], frame: &mut DebugTrapFrame) {
    let mut bytes = [0u8; REG_BYTES];
    let n = gdb_rsp::hex_decode(hex, &mut bytes).unwrap_or(0);
    if n < 17 * 8 + 4 {
        return; // too short to hold GPRs + rip + eflags
    }
    let rd_u64 = |off: usize, b: &[u8]| -> u64 {
        let mut a = [0u8; 8];
        a.copy_from_slice(&b[off..off + 8]);
        u64::from_le_bytes(a)
    };
    frame.gprs[0] = rd_u64(0, &bytes); // rax
    frame.gprs[1] = rd_u64(8, &bytes); // rbx
    frame.gprs[2] = rd_u64(16, &bytes); // rcx
    frame.gprs[3] = rd_u64(24, &bytes); // rdx
    frame.gprs[4] = rd_u64(32, &bytes); // rsi
    frame.gprs[5] = rd_u64(40, &bytes); // rdi
    frame.gprs[6] = rd_u64(48, &bytes); // rbp
    frame.rsp = rd_u64(56, &bytes); // rsp
    for i in 0..8 {
        frame.gprs[7 + i] = rd_u64(64 + i * 8, &bytes); // r8..r15
    }
    frame.rip = rd_u64(128, &bytes); // rip
    let mut ef = [0u8; 4];
    ef.copy_from_slice(&bytes[136..140]);
    // Preserve the high 32 bits of rflags; overwrite the low 32 from the debugger.
    frame.rflags = (frame.rflags & !0xFFFF_FFFF) | u32::from_le_bytes(ef) as u64;
}

// ---------------------------------------------------------------------------
// Memory read/write
// ---------------------------------------------------------------------------

/// `m<addr>,<len>` → hex bytes. Rejects non-canonical addresses and oversize
/// lengths.
fn reply_mem_read(rest: &[u8]) {
    let (addr, n1) = gdb_rsp::parse_hex_prefix(rest);
    if n1 == 0 || rest.get(n1) != Some(&b',') {
        send_packet(b"E22");
        return;
    }
    let (len, n2) = gdb_rsp::parse_hex_prefix(&rest[n1 + 1..]);
    if n2 == 0 {
        send_packet(b"E22");
        return;
    }
    let len = (len as usize).min(MAX_MEM);
    if !is_canonical(addr) || !is_canonical(addr.wrapping_add(len as u64).wrapping_sub(1)) {
        send_packet(b"E14");
        return;
    }
    let mut hex = [0u8; 2 * MAX_MEM];
    let hn = {
        let mut st = STATE.lock();
        // SAFETY: canonical kernel range; the gate reads only mapped symbols. A
        // truly-unmapped read would fault — acceptable for a debug-only feature.
        unsafe { read_kernel_bytes(addr, &mut st.scratch[..len]) };
        gdb_rsp::hex_encode(&st.scratch[..len], &mut hex).unwrap_or(0)
    };
    send_packet(&hex[..hn]);
}

/// `M<addr>,<len>:<hex>` → write bytes.
fn reply_mem_write(rest: &[u8]) {
    let (addr, n1) = gdb_rsp::parse_hex_prefix(rest);
    if n1 == 0 || rest.get(n1) != Some(&b',') {
        send_packet(b"E22");
        return;
    }
    let (len, n2) = gdb_rsp::parse_hex_prefix(&rest[n1 + 1..]);
    let colon = n1 + 1 + n2;
    if n2 == 0 || rest.get(colon) != Some(&b':') {
        send_packet(b"E22");
        return;
    }
    let len = (len as usize).min(MAX_MEM);
    if !is_canonical(addr) {
        send_packet(b"E14");
        return;
    }
    let hex = &rest[colon + 1..];
    let ok = {
        let mut st = STATE.lock();
        if gdb_rsp::hex_decode(&hex[..(len * 2).min(hex.len())], &mut st.scratch[..len]).is_none() {
            false
        } else {
            // SAFETY: canonical kernel range; WP-off write is restored by the
            // helper, and the machine is all-stopped so no other core observes
            // the window.
            unsafe { write_kernel_bytes(addr, &st.scratch[..len]) };
            true
        }
    };
    send_packet(if ok { b"OK" } else { b"E22" });
}

/// x86-64 canonical-address test (bits 47..63 must equal bit 47).
fn is_canonical(addr: u64) -> bool {
    let top = addr >> 47;
    top == 0 || top == 0x1FFFF
}

// ---------------------------------------------------------------------------
// Software-breakpoint table
// ---------------------------------------------------------------------------

fn insert_planted_bp(addr: u64) -> bool {
    let mut st = STATE.lock();
    // Already planted? Idempotent success.
    if st.sw_bp.iter().any(|b| b.active && b.addr == addr) {
        return true;
    }
    let Some(slot) = st.sw_bp.iter().position(|b| !b.active) else {
        return false;
    };
    // SAFETY: `addr` is a kernel code byte; the machine is all-stopped, so the
    // patch (WP-off inside the helper) is race-free.
    let orig = unsafe { insert_sw_breakpoint(addr) };
    st.sw_bp[slot] = SwBreak {
        addr,
        orig,
        active: true,
    };
    true
}

fn remove_planted_bp(addr: u64) {
    let mut st = STATE.lock();
    if let Some(slot) = st.sw_bp.iter().position(|b| b.active && b.addr == addr) {
        let orig = st.sw_bp[slot].orig;
        // SAFETY: restoring the exact byte we saved at `addr`.
        unsafe { remove_sw_breakpoint(addr, orig) };
        st.sw_bp[slot].active = false;
    }
}

fn remove_all_sw_bp() {
    let mut st = STATE.lock();
    for i in 0..MAX_SW_BP {
        if st.sw_bp[i].active {
            let (addr, orig) = (st.sw_bp[i].addr, st.sw_bp[i].orig);
            // SAFETY: restoring a byte we planted.
            unsafe { remove_sw_breakpoint(addr, orig) };
            st.sw_bp[i].active = false;
        }
    }
}

fn is_planted_bp(addr: u64) -> bool {
    STATE
        .lock()
        .sw_bp
        .iter()
        .any(|b| b.active && b.addr == addr)
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

fn send_stop_reply(reason: StopReason) {
    // Minimal stop reply: `S<sig>` (two hex digits).
    let sig = reason.signal();
    let body = [b'S', hex_hi(sig), hex_lo(sig)];
    send_packet(&body);
}

/// Frame `payload` as `$payload#cc`, store it (for retransmit-on-nak), and
/// transmit over COM2. The `STATE` lock is held across the polled write — the
/// machine is all-stopped, so it is uncontended.
fn send_packet(payload: &[u8]) {
    let mut st = STATE.lock();
    let Some(n) = gdb_rsp::encode_packet(payload, &mut st.reply) else {
        return;
    };
    st.last_reply_len = n;
    super::com2::write_all(&st.reply[..n]);
}

fn retransmit_last() {
    let st = STATE.lock();
    let n = st.last_reply_len;
    if n > 0 {
        super::com2::write_all(&st.reply[..n]);
    }
}

#[inline]
fn hex_hi(b: u8) -> u8 {
    hex_nibble(b >> 4)
}
#[inline]
fn hex_lo(b: u8) -> u8 {
    hex_nibble(b & 0xf)
}
#[inline]
fn hex_nibble(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'a' + (n - 10) }
}
