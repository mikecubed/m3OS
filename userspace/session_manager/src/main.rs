//! Phase 57 Track F.2 — `session_manager` daemon (scaffold).
//!
//! This commit lands only the four-step new-binary convention so the
//! crate is buildable and the binary is embedded in the kernel image:
//!
//! 1. Workspace member added to `Cargo.toml` `members`.
//! 2. `xtask` `bins` array gains the entry (with `needs_alloc =
//!    true`).
//! 3. `kernel/src/fs/ramdisk.rs` embeds the ELF and lists it in
//!    `BIN_ENTRIES` so `execve` can find it under `/bin/`.
//! 4. `etc/services.d/session_manager.conf` declares the service and
//!    init's `KNOWN_CONFIGS` fallback list references it.
//!
//! The boot-ordering loop, control-socket stub, and supervisor adapter
//! land in the next commit ("implement session_manager boot loop"),
//! consuming the F.1 sequencer + F.3 codec.

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
    syscall_lib::write_str(STDOUT_FILENO, "session_manager: alloc error\n");
    syscall_lib::exit(99)
}

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(
        STDOUT_FILENO,
        "session_manager: scaffold (Phase 57 F.2 — boot loop in next commit)\n",
    );
    // Idle so init's supervisor sees a healthy daemon. The next
    // commit replaces this loop with the F.1 boot sequencer +
    // event-loop multiplexer.
    loop {
        let _ = syscall_lib::nanosleep_for(1, 0);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "session_manager: PANIC\n");
    syscall_lib::exit(101)
}
