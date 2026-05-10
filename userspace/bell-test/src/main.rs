//! `bell-test` — Phase 63 Track E.1 bell path exerciser.
//!
//! This binary exercises the exact same code path that `term`'s ANSI
//! parser takes when it processes a BEL byte (0x07):
//!
//!   BEL → `RenderCommand::Bell` → `Bell::ring` → `AudioClientBellSink::play`
//!       → `audio_client::AudioClient::submit_frames`
//!
//! It constructs an `AudioClientBellSink` from `term::bell`, wraps it in a
//! `Bell`, and calls `Bell::ring()` — the same call site as
//! `term/src/main.rs` line ~276. This sidesteps the kbd_server routing
//! problem: serial writes go to `sh0`, not to `term`'s ANSI parser, so
//! a `printf '\x07'` injection via QEMU stdin would never reach `term`.
//! Running `bell-test` from the shell (sh0) directly calls the bell
//! library, bypassing the routing gap entirely.
//!
//! ## Protocol
//!
//! After calling `Bell::ring`, the binary sleeps 200 ms (enough for the
//! AC'97 backend to DMA the 30 ms, 5760-byte tone through the BDL ring),
//! then calls `AudioClient::connect().get_stats()` to read
//! `frames_consumed`. It emits:
//!
//! ```text
//! BELL_TEST:consumed=<N> underruns=<M>
//! BELL_TEST:PASS   (frames_consumed > 0)
//! ```
//!
//! or:
//!
//! ```text
//! BELL_TEST:consumed=0 underruns=<M>
//! BELL_TEST:FAIL:consumed=0
//! ```
//!
//! The `BELL_TEST:PASS` / `BELL_TEST:FAIL:consumed=0` sentinels are the
//! patterns the `bell-smoke` xtask step waits for.
//!
//! ## Routing fix (why this exists)
//!
//! The original bell-smoke approach injected `printf '\x07'\n` via
//! `SmokeStep::Send` which writes to QEMU's serial stdin. That goes to
//! the kernel serial shell (`sh0`), never to `term`'s ANSI parser.
//! `bell-test` is run from `sh0`; it directly invokes the bell library
//! that `term` uses, so the routing problem is irrelevant.
//!
//! ## Binary convention (AGENTS.md §"Adding a New Userspace Binary")
//!
//! Registered in four places:
//! 1. Workspace `Cargo.toml` members list.
//! 2. `xtask/src/main.rs` `bins` array (`needs_alloc = true`).
//! 3. `kernel/src/fs/ramdisk.rs` `BIN_ENTRIES`.
//! 4. No `.conf` — one-shot, not a daemon.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use audio_client::{AudioClient, AudioClientError};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;
use term::bell::{AudioClientBellSink, Bell};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "bell-test: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "bell-test: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    // Construct the same bell sink that term uses in its event loop.
    // AudioClientBellSink::new() pre-computes the 30 ms 880 Hz square-wave
    // tone and lazily opens the audio stream on the first play() call.
    let sink = AudioClientBellSink::new();
    let mut bell = Bell::new(sink);

    // Ring the bell at t=0. Any non-MAX timestamp works for the first ring
    // because Bell::ring treats last_played_ms == u64::MAX as "never rung".
    // We use 0 here for simplicity; the coalescing window cannot fire on the
    // very first call.
    match bell.ring(0) {
        Ok(true) => {
            // play() was called — good.
        }
        Ok(false) => {
            // Coalesced (impossible on the first ring, but be defensive).
            write_str("bell-test: warn: first ring was coalesced (unexpected)\n");
        }
        Err(_) => {
            // AudioClientBellSink could not open the stream — audio_server
            // is absent or busy.  Report and bail.
            write_str("bell-test: error: Bell::ring failed (audio unavailable)\n");
            write_str("BELL_TEST:consumed=0 underruns=0\n");
            write_str("BELL_TEST:FAIL:audio_unavailable\n");
            return 1;
        }
    }

    // Sleep 200 ms to give the AC'97 DMA engine time to consume the
    // 5760-byte (30 ms at 48 kHz stereo 16-bit) bell tone through the BDL
    // ring before querying stats. The tone is 30 ms; 200 ms gives 6.7×
    // headroom for scheduling jitter under QEMU/TCG.
    syscall_lib::nanosleep_for(0, 200_000_000);

    // Query stats via a control-only client — no Open, no perturbing the
    // device slot that AudioClientBellSink already holds (or had held).
    let mut stats_client = match AudioClient::connect() {
        Ok(c) => c,
        Err(err) => {
            write_str("bell-test: stats connect error: ");
            write_error(err);
            write_str("\n");
            write_str("BELL_TEST:consumed=0 underruns=0\n");
            write_str("BELL_TEST:FAIL:stats_connect_error\n");
            return 2;
        }
    };

    let stats = match stats_client.get_stats() {
        Ok(s) => s,
        Err(err) => {
            write_str("bell-test: get_stats error: ");
            write_error(err);
            write_str("\n");
            write_str("BELL_TEST:consumed=0 underruns=0\n");
            write_str("BELL_TEST:FAIL:get_stats_error\n");
            return 2;
        }
    };

    // Detail line — always emitted before the PASS/FAIL sentinel so CI
    // logs retain the consumed/underrun values for post-mortem inspection.
    write_str("BELL_TEST:consumed=");
    write_u64(stats.frames_consumed);
    write_str(" underruns=");
    write_u32(stats.underrun_count);
    write_str("\n");

    if stats.frames_consumed > 0 {
        syscall_lib::write_str(STDOUT_FILENO, "BELL_TEST:PASS\n");
        0
    } else {
        syscall_lib::write_str(STDOUT_FILENO, "BELL_TEST:FAIL:consumed=0\n");
        1
    }
}

// ---------------------------------------------------------------------------
// Minimal write helpers — no format! / alloc needed for these fixed
// strings and decimal integers.
// ---------------------------------------------------------------------------

fn write_str(s: &str) {
    syscall_lib::write_str(STDOUT_FILENO, s);
}

fn write_u32(n: u32) {
    write_decimal(n as u64);
}

fn write_u64(n: u64) {
    write_decimal(n);
}

/// Write a decimal integer to stdout without heap allocation.
fn write_decimal(mut n: u64) {
    let mut buf = [0u8; 20]; // log10(u64::MAX) < 20
    let mut pos = buf.len();
    if n == 0 {
        write_str("0");
        return;
    }
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    // SAFETY: buf[pos..] contains only ASCII digits written above.
    let s = unsafe { core::str::from_utf8_unchecked(&buf[pos..]) };
    write_str(s);
}

fn write_error(err: AudioClientError) {
    match err {
        AudioClientError::Io(code) => {
            write_str("Io(");
            let abs = if code < 0 {
                -(code as i64) as u64
            } else {
                code as u64
            };
            write_decimal(abs);
            write_str(")");
        }
        AudioClientError::Protocol(_) => write_str("Protocol"),
        AudioClientError::Server(_) => write_str("Server"),
        AudioClientError::UnexpectedReply => write_str("UnexpectedReply"),
        AudioClientError::NotOpen => write_str("NotOpen"),
        _ => write_str("Unknown"),
    }
}
