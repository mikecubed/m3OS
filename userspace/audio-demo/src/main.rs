//! `audio-demo` — Phase 57 Track E.2 audio reference client.
//!
//! On run, the demo opens a stream against `audio_server`, submits a
//! one-second 440 Hz sine wave (16-bit signed LE, stereo, 48 kHz),
//! drains, closes, and exits 0. On any [`audio_client::AudioClientError`]
//! the demo logs a structured line containing the variant name and
//! exits non-zero. The demo doubles as the audio smoke harness for
//! Track H.1.
//!
//! ## Why this binary is *not* a daemon
//!
//! The four-step new-binary convention covers the four wiring sites:
//! workspace `members`, xtask `bins`, kernel ramdisk `BIN_ENTRIES`,
//! and (only for daemons) `etc/services.d/<name>.conf` +
//! `KNOWN_CONFIGS` in `userspace/init/src/main.rs`.
//!
//! `audio-demo` is a one-shot — it opens, plays, closes, exits — so
//! the service-config step is intentionally skipped. The demo runs
//! either by manual invocation from the shell (`/bin/audio-demo`) or
//! by H.1 driving it as a smoke client. Adding a daemon manifest
//! here would invite the service supervisor to relaunch it on every
//! exit, which is the wrong semantics for a one-shot.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "audio-demo: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "audio-demo: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    // E.2 scaffold: the open / submit / drain / close run lands in
    // the next commit. This stub keeps the four-step convention
    // verifiable (workspace member + xtask bin + ramdisk entry are
    // all wired) before the sine-wave generator is implemented.
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:scaffold\n");
    0
}
