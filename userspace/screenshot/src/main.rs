//! `screenshot` — capture the composited screen to a PNG file
//! (Phase 105 Track C.5).
//!
//! The compositor owns the framebuffer, so a screenshot has to originate
//! there: this tool connects to `display_server` as an ordinary client,
//! asks it to blit the current frame into a shared-memory region
//! (`DisplayConnection::capture_output` → the `CaptureOutput` verb), then
//! PNG-encodes the returned pixels (`imagefmt::encode_png`) and writes the
//! file.
//!
//! Usage: `screenshot [PATH]` (default `/tmp/screenshot.png`).
//!
//! After writing, the tool re-reads and decodes its own file to prove the
//! encode → write → read → decode round-trip is lossless, and counts the
//! pixels that differ from the top-left corner so a silently-blank capture
//! is caught. It prints one greppable sentinel line for the
//! `screenshot-smoke` gate:
//!
//! ```text
//! SCREENSHOT_OK <w>x<h> nonblank=<N> bytes=<B> path=<PATH>
//! ```
//!
//! or `SCREENSHOT_FAIL reason=<why>` on any failure.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::Layout;

use desktop_client::DisplayConnection;
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{O_CREAT, O_WRONLY, STDOUT_FILENO, write_str};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "screenshot: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "screenshot: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const DEFAULT_PATH: &str = "/tmp/screenshot.png";

fn log(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    syscall_lib::serial_print(msg);
}

fn fail(reason: &str) -> i32 {
    log("SCREENSHOT_FAIL reason=");
    log(reason);
    log("\n");
    1
}

fn write_u32_dec(n: u32) {
    // Small stack itoa; avoids pulling in formatting machinery.
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    let mut v = n;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    // SAFETY: buf[i..] is ASCII digits.
    log(core::str::from_utf8(&buf[i..]).unwrap_or("?"));
}

/// Write the whole buffer to `path`, creating/truncating it. Returns
/// `true` on success.
fn write_file(path: &str, bytes: &[u8]) -> bool {
    // Null-terminate the path for the C-style `open` ABI.
    let mut cpath: Vec<u8> = Vec::with_capacity(path.len() + 1);
    cpath.extend_from_slice(path.as_bytes());
    cpath.push(0);
    let fd = syscall_lib::open(&cpath, O_WRONLY | O_CREAT, 0o644);
    if fd < 0 {
        return false;
    }
    let fd = fd as i32;
    let mut off = 0usize;
    let mut ok = true;
    while off < bytes.len() {
        let n = syscall_lib::write(fd, &bytes[off..]);
        if n <= 0 {
            ok = false;
            break;
        }
        off += n as usize;
    }
    let _ = syscall_lib::fsync(fd);
    let _ = syscall_lib::close(fd);
    ok
}

/// Read the entire file at `path` into a `Vec`. Returns `None` on error.
fn read_file(path: &str) -> Option<Vec<u8>> {
    let mut cpath: Vec<u8> = Vec::with_capacity(path.len() + 1);
    cpath.extend_from_slice(path.as_bytes());
    cpath.push(0);
    let fd = syscall_lib::open(&cpath, syscall_lib::O_RDONLY, 0);
    if fd < 0 {
        return None;
    }
    let fd = fd as i32;
    let mut out: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = syscall_lib::read(fd, &mut chunk);
        if n < 0 {
            let _ = syscall_lib::close(fd);
            return None;
        }
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n as usize]);
    }
    let _ = syscall_lib::close(fd);
    Some(out)
}

/// Count the pixels that differ from the top-left corner. A fully uniform
/// capture (e.g. an all-black blit that silently failed) yields 0.
fn count_nonblank(pixels: &[u32]) -> u32 {
    if pixels.is_empty() {
        return 0;
    }
    let corner = pixels[0];
    let mut n = 0u32;
    for &p in pixels {
        if p != corner {
            n = n.saturating_add(1);
        }
    }
    n
}

fn program_main(args: &[&str]) -> i32 {
    let path = args.get(1).copied().unwrap_or(DEFAULT_PATH);

    let conn = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => return fail("connect"),
    };

    let (w, h, pixels) = match conn.capture_output() {
        Some(t) => t,
        None => {
            conn.goodbye();
            return fail("capture");
        }
    };
    conn.goodbye();

    if pixels.len() != (w as usize).saturating_mul(h as usize) || pixels.is_empty() {
        return fail("dims");
    }
    let nonblank = count_nonblank(&pixels);
    if nonblank == 0 {
        // A valid capture of a rendered desktop is never uniform; treat an
        // all-one-color frame as a failed blit rather than a real shot.
        return fail("blank");
    }

    let png = imagefmt::encode_png(w, h, &pixels);
    if png.is_empty() {
        return fail("encode");
    }

    if !write_file(path, &png) {
        return fail("write");
    }

    // Re-read and decode our own file to prove the round-trip is lossless.
    let back = match read_file(path) {
        Some(b) => b,
        None => return fail("read"),
    };
    match imagefmt::decode_png(&back) {
        Ok((dw, dh, dpix)) if dw == w && dh == h && dpix == pixels => {}
        Ok(_) => return fail("roundtrip"),
        Err(_) => return fail("decode"),
    }

    // One greppable success line for the gate.
    log("SCREENSHOT_OK ");
    write_u32_dec(w);
    log("x");
    write_u32_dec(h);
    log(" nonblank=");
    write_u32_dec(nonblank);
    log(" bytes=");
    write_u32_dec(png.len() as u32);
    log(" path=");
    log(path);
    log("\n");
    0
}
