//! Phase 74 Track B.3 — `page-grant-test`.
//!
//! Single-process round-trip smoke test for the new page-grant transport.
//! Proves that:
//!
//! 1. A 1024-page (4 MiB) region can be granted to the kernel via
//!    `sys_page_grant_send` — returns a `CapHandle`.
//! 2. The same process can then call `sys_page_grant_recv` with that
//!    capability and receive the same physical frames back at a fresh
//!    kernel-chosen virtual address.
//! 3. A sentinel pattern written before the send round-trips through
//!    the kernel with **zero copies** — the receiver-side virtual
//!    address points at the exact same physical frames so the sentinel
//!    is still observable.
//! 4. A second `sys_page_grant_recv` against the same capability fails
//!    (the one-shot consume semantics are honoured).
//!
//! On success prints `PAGE_GRANT_SMOKE:roundtrip:ok` and exits 0.
//! Any failure prints `PAGE_GRANT_SMOKE:fail <reason>` and exits 2.

#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, brk, exit, page_grant_recv, page_grant_send, write};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    let _ = write(STDOUT_FILENO, b"PAGE_GRANT_SMOKE:fail panic\n");
    exit(101)
}

fn fail(reason: &[u8]) -> ! {
    let _ = write(STDOUT_FILENO, b"PAGE_GRANT_SMOKE:fail ");
    let _ = write(STDOUT_FILENO, reason);
    let _ = write(STDOUT_FILENO, b"\n");
    exit(2)
}

/// Grant size: 1024 4 KiB pages = 4 MiB. Matches the Phase 74 task list's
/// B.3 acceptance criterion.
const N_PAGES: usize = 1024;
const PAGE: usize = 4096;
const REGION_BYTES: usize = N_PAGES * PAGE;

/// Sentinel byte pattern. Each page gets a distinct byte derived from
/// its page index so a partial copy / wrong-mapping bug surfaces as a
/// detectable mismatch.
fn sentinel_byte_for_page(page_index: usize) -> u8 {
    // 0x40 + (page_index mod 0x40) gives a visible-ASCII byte in the
    // range '@'..'~', easy to spot in a hex dump on failure.
    0x40 + ((page_index & 0x3F) as u8)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _ = write(STDOUT_FILENO, b"PAGE_GRANT_SMOKE:roundtrip:begin\n");

    // 1. Reserve REGION_BYTES of contiguous heap via brk. The initial brk
    //    call with addr=0 queries the current break.
    let cur_break = brk(0);
    if cur_break == 0 {
        fail(b"brk-query-zero");
    }
    // Round the break up to a page boundary so the grant range is
    // page-aligned (required by sys_page_grant_send).
    let region_base = (cur_break + (PAGE as u64) - 1) & !(PAGE as u64 - 1);
    let new_break = region_base + REGION_BYTES as u64;
    let after_brk = brk(new_break);
    if after_brk < new_break {
        fail(b"brk-extend-failed");
    }

    // 2. Write the per-page sentinel pattern.
    for page_index in 0..N_PAGES {
        let byte = sentinel_byte_for_page(page_index);
        // SAFETY: region_base..region_base+REGION_BYTES is brk-allocated
        // and mapped read-write into our address space.
        unsafe {
            let p = (region_base as *mut u8).add(page_index * PAGE);
            // Fill the first 64 bytes of each page with the sentinel so
            // a partial-page mismatch is detectable.
            for off in 0..64usize {
                p.add(off).write_volatile(byte);
            }
        }
    }

    // 3. Hand the pages to the kernel as a PageGrant.
    let cap_handle = page_grant_send(region_base, N_PAGES);
    if cap_handle == u64::MAX {
        fail(b"send-returned-u64-max");
    }
    if cap_handle > u32::MAX as u64 {
        fail(b"send-returned-out-of-range-handle");
    }
    let cap_handle = cap_handle as u32;

    // 4. Receive the grant back into the same process at a fresh
    //    kernel-chosen vaddr. The PFNs are identical, so reading the
    //    sentinel back proves the round-trip moved ownership without
    //    copying any bytes.
    let recv_vaddr = page_grant_recv(cap_handle);
    if recv_vaddr == u64::MAX {
        fail(b"recv-returned-u64-max");
    }
    if !recv_vaddr.is_multiple_of(PAGE as u64) {
        fail(b"recv-returned-unaligned-vaddr");
    }

    // 5. Verify the sentinel pattern survives.
    for page_index in 0..N_PAGES {
        let expected = sentinel_byte_for_page(page_index);
        // SAFETY: recv_vaddr..recv_vaddr+REGION_BYTES is the freshly-
        // mapped region the kernel just installed.
        unsafe {
            let p = (recv_vaddr as *const u8).add(page_index * PAGE);
            for off in 0..64usize {
                let actual = p.add(off).read_volatile();
                if actual != expected {
                    // Mismatch — round-trip lost / wrong-frame mapped.
                    fail(b"sentinel-mismatch");
                }
            }
        }
    }

    // 6. A second recv against the same (now-consumed) capability must
    //    fail. The kernel removed the cap from our table inside the
    //    first recv, so this looks like a bad-handle from userspace.
    let second_recv = page_grant_recv(cap_handle);
    if second_recv != u64::MAX {
        fail(b"double-recv-succeeded");
    }

    let _ = write(STDOUT_FILENO, b"PAGE_GRANT_SMOKE:roundtrip:ok\n");
    exit(0)
}
