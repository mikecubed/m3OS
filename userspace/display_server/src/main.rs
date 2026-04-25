//! Phase 56 — userspace display server (compositor).
//!
//! This binary owns presentation: it claims the primary framebuffer from
//! the kernel via the Phase 47/56 syscall surface, fills it with a known
//! background color so the ownership transfer is visually unambiguous,
//! registers itself in the service registry as `"display"`, and idles on
//! its IPC endpoint so init's supervisor sees a healthy daemon.
//!
//! Tracks landed in this PR:
//!   * **C.1** — crate scaffolding + four-place new-binary wiring.
//!   * **C.2** — framebuffer acquisition through the [`KernelFramebufferOwner`]
//!     impl of the `kernel-core::display::fb_owner::FramebufferOwner` trait,
//!     plus initial background fill.
//!
//! Tracks deferred to follow-up PRs (foundation in `kernel-core`):
//!   * **C.3 / C.4** — surface state machine + damage-tracked composer.
//!   * **C.5** — AF_UNIX / IPC client-protocol dispatcher.
//!   * **C.6** — `gfx-demo` reference client.
//!   * **B.2 / B.3 / B.4** — kernel-side wiring for mouse, frame-tick,
//!     and surface-buffer transport (pure-logic cores already in
//!     `kernel-core::input::mouse`, `kernel-core::display::frame_tick`,
//!     `kernel-core::display::buffer`).
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

mod fb;

use core::alloc::Layout;
use kernel_core::display::fb_owner::{FbError, FramebufferOwner};
use kernel_core::display::protocol::Rect;
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

use crate::fb::KernelFramebufferOwner;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "display_server: alloc error\n");
    syscall_lib::exit(99)
}

/// Phase 56 startup background colour (encoded BGRA8888 / RGBA8888 — both
/// formats happen to render this byte order as a uniform deep teal). The
/// learning doc records this so manual smoke validation knows what to
/// expect on `cargo xtask run-gui --fresh`.
const BG_PIXEL: u32 = 0x002B5_5A4Bu32;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(
        STDOUT_FILENO,
        "display_server: starting (Phase 56 — C.1+C.2)\n",
    );

    // ----- Service registration -------------------------------------------
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "display_server: failed to create endpoint\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    let reg = syscall_lib::ipc_register_service(ep_handle, "display");
    if reg == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: failed to register 'display'\n",
        );
        return 1;
    }
    syscall_lib::write_str(STDOUT_FILENO, "display_server: registered as 'display'\n");

    // ----- Framebuffer acquisition (C.2) ---------------------------------
    let mut owner = match acquire_framebuffer_with_backoff() {
        Ok(o) => o,
        Err(reason) => {
            syscall_lib::write_str(STDOUT_FILENO, "display_server: ");
            syscall_lib::write_str(STDOUT_FILENO, reason);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
            return 1;
        }
    };
    let meta = owner.metadata();

    // Initial fill across the whole framebuffer so the ownership handoff
    // is visually unambiguous during bring-up.
    if let Err(err) = paint_solid(&mut owner, BG_PIXEL) {
        report_fb_error("initial fill", err);
        // Best-effort release; we still exit nonzero so init notices.
        let _ = syscall_lib::framebuffer_release();
        return 1;
    }
    let _ = owner.present();

    syscall_lib::write_str(STDOUT_FILENO, "display_server: framebuffer acquired\n");
    log_fb_meta(meta.width, meta.height, meta.stride_bytes);

    // ----- Idle loop ------------------------------------------------------
    //
    // Tracks C.3–C.5 replace this with a real protocol dispatcher driven by
    // a frame-tick notification, the input endpoint, the client listening
    // socket, and the control socket — see the engineering-discipline note
    // in the Phase 56 task doc on the single-threaded event loop.
    loop {
        let _label = syscall_lib::ipc_recv(ep_handle);
        // No client protocol yet — silently drop. The reply capability is
        // overwritten on the next ipc_recv, so no resource leaks.
    }
}

/// Try to acquire the framebuffer with bounded retry, in case another
/// short-lived process is still releasing ownership at boot.
fn acquire_framebuffer_with_backoff() -> Result<KernelFramebufferOwner, &'static str> {
    const MAX_ATTEMPTS: u32 = 8;
    const BACKOFF_NS: u32 = 5_000_000; // 5 ms

    for attempt in 0..MAX_ATTEMPTS {
        match KernelFramebufferOwner::acquire() {
            Ok(o) => return Ok(o),
            Err(fb::AcquireError::FbBusy) => {
                if attempt + 1 == MAX_ATTEMPTS {
                    return Err("framebuffer busy after retry budget");
                }
                syscall_lib::nanosleep_for(0, BACKOFF_NS);
            }
            Err(fb::AcquireError::FbInfoFailed) => return Err("FB info syscall failed"),
            Err(fb::AcquireError::FbMmapFailed) => return Err("FB mmap syscall failed"),
            Err(fb::AcquireError::UnsupportedPixelFormat) => {
                return Err("FB pixel format not supported");
            }
        }
    }
    Err("framebuffer busy after retry budget")
}

/// Fill the entire framebuffer rectangle with `pixel` (packed 32-bit).
fn paint_solid(owner: &mut KernelFramebufferOwner, pixel: u32) -> Result<(), FbError> {
    let meta = owner.metadata();
    let w = meta.width;
    let h = meta.height;
    if w == 0 || h == 0 {
        return Ok(());
    }
    // Build one full row of pixel data, then write each row in turn. Avoids
    // allocating a width*height*4 staging buffer on the heap.
    let row_bytes_len = (w as usize) * 4;
    let mut row: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(row_bytes_len);
    let bytes = pixel.to_le_bytes();
    for _ in 0..w {
        row.extend_from_slice(&bytes);
    }
    for y in 0..h {
        owner.write_pixels(
            Rect {
                x: 0,
                y: y as i32,
                w,
                h: 1,
            },
            &row,
            row_bytes_len as u32,
        )?;
    }
    Ok(())
}

fn log_fb_meta(w: u32, h: u32, stride: u32) {
    syscall_lib::write_str(STDOUT_FILENO, "display_server: fb metadata: ");
    write_u32(w);
    syscall_lib::write_str(STDOUT_FILENO, "x");
    write_u32(h);
    syscall_lib::write_str(STDOUT_FILENO, " stride=");
    write_u32(stride);
    syscall_lib::write_str(STDOUT_FILENO, "\n");
}

fn write_u32(mut value: u32) {
    let mut buf = [0u8; 10];
    let mut idx = buf.len();
    if value == 0 {
        idx -= 1;
        buf[idx] = b'0';
    } else {
        while value != 0 {
            idx -= 1;
            buf[idx] = b'0' + (value % 10) as u8;
            value /= 10;
        }
    }
    if let Ok(s) = core::str::from_utf8(&buf[idx..]) {
        syscall_lib::write_str(STDOUT_FILENO, s);
    }
}

fn report_fb_error(stage: &str, err: FbError) {
    syscall_lib::write_str(STDOUT_FILENO, "display_server: fb error in ");
    syscall_lib::write_str(STDOUT_FILENO, stage);
    let suffix = match err {
        FbError::OutOfBounds => " (OutOfBounds)\n",
        FbError::Truncated => " (Truncated)\n",
        FbError::InvalidStride => " (InvalidStride)\n",
        FbError::Unsupported => " (Unsupported)\n",
    };
    syscall_lib::write_str(STDOUT_FILENO, suffix);
}

/// Map the kernel's reported pixel-format tag onto
/// `kernel-core::display::fb_owner::PixelFormat`.
pub(crate) fn pixel_format_from_kernel_tag(
    tag: u32,
) -> Option<kernel_core::display::fb_owner::PixelFormat> {
    use kernel_core::display::fb_owner::PixelFormat;
    match tag {
        0 => Some(PixelFormat::Rgba8888), // bootloader_api::PixelFormat::Rgb
        1 => Some(PixelFormat::Bgra8888), // bootloader_api::PixelFormat::Bgr
        _ => None,
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "display_server: PANIC\n");
    let _ = syscall_lib::framebuffer_release();
    syscall_lib::exit(101)
}
