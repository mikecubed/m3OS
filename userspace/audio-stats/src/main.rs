//! `audio-stats` — Phase 63 Track E.1 one-shot audio stats CLI.
//!
//! Opens the `audio.cmd` control socket (no PCM stream — stats are
//! available regardless of stream state), sends a
//! `ControlCommand(GetStats)` request, decodes the `AudioControlEvent::Stats`
//! reply, and emits two lines to stdout:
//!
//! ```text
//! AUDIO_STATS:consumed=<N> underruns=<M>
//! AUDIO_STATS:PASS    (when frames_consumed > 0)
//! ```
//!
//! or:
//!
//! ```text
//! AUDIO_STATS:consumed=0 underruns=<M>
//! AUDIO_STATS:FAIL:consumed=0
//! ```
//!
//! The detail line (`consumed=<N>`) is always emitted first for CI log
//! diagnostics.
//!
//! ## Why no Open
//!
//! `GetStats` is a control-plane verb: `audio_server` handles it via
//! `AudioControlCommand::GetStats` independently of any open PCM stream.
//! Calling `Open` before `GetStats` is unnecessary and perturbs the
//! server's single-slot state — it temporarily occupies the device slot,
//! preventing any other client from opening while we are querying. The
//! fix uses `AudioClient::connect()` to bind only the control socket.
//!
//! ## Usage
//!
//! Invoke from the shell prompt inside the QEMU guest:
//!
//! ```text
//! /bin/audio-stats
//! ```
//!
//! The binary exits 0 on success, 1 on `frames_consumed == 0`, 2 on
//! connection / protocol error.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use audio_client::{AudioClient, AudioClientError};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "audio-stats: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "audio-stats: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    // Connect to the control socket only — no PCM stream is opened.
    // `AudioClient::connect()` binds the IPC endpoint without issuing
    // `Open`, so the server's single-slot state is not perturbed and
    // any other client (e.g. term's AudioClientBellSink) can continue
    // to hold the slot while we query stats.
    let mut client = match AudioClient::connect() {
        Ok(c) => c,
        Err(err) => {
            write_str("audio-stats: error: ");
            write_error(err);
            write_str("\n");
            return 2;
        }
    };

    let stats = match client.get_stats() {
        Ok(s) => s,
        Err(err) => {
            write_str("audio-stats: get_stats error: ");
            write_error(err);
            write_str("\n");
            return 2;
        }
    };

    // Always emit the detail line first — CI logs preserve it for
    // post-mortem inspection regardless of the PASS/FAIL outcome.
    write_str("AUDIO_STATS:consumed=");
    write_u64(stats.frames_consumed);
    write_str(" underruns=");
    write_u32(stats.underrun_count);
    write_str("\n");

    if stats.frames_consumed > 0 {
        syscall_lib::write_str(STDOUT_FILENO, "AUDIO_STATS:PASS\n");
        0
    } else {
        syscall_lib::write_str(STDOUT_FILENO, "AUDIO_STATS:FAIL:consumed=0\n");
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
