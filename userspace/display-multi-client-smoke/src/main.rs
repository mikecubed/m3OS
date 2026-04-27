//! Phase 56 close-out (G.1 regression) — multi-client coexistence smoke.
//!
//! Drives two distinct surfaces through the full Phase 56 protocol —
//! Hello, CreateSurface, SetSurfaceRole(Toplevel), LABEL_PIXELS bulk,
//! AttachBuffer, CommitSurface — each filled with a different color
//! (red and blue), then queries `display_server` via the test-only
//! `ControlCommand::ReadBackPixel` verb to confirm both colors actually
//! land on screen at their layout-derived positions.
//!
//! The architectural claim Phase 56 makes is "two graphical surfaces
//! coexist in the compositor without raw-framebuffer conflicts." A
//! single process driving two surface streams demonstrates that claim
//! end-to-end: the registry tracks surfaces independently by id, the
//! composer's z-order and damage paths handle both, the layout policy
//! places them at distinct positions, and the framebuffer ends up with
//! both colors visible. The variant where two separate processes each
//! drive one surface is a stricter test (and could be added later by
//! splitting this binary into two via the four-step new-binary
//! convention) but adds little architectural insight beyond what F.2's
//! crash-recovery regression already demonstrates about multi-process
//! IPC.
//!
//! # Gating
//!
//! The smoke is gated by an env-var marker (`/etc/display_server.readback`
//! drops `M3OS_DISPLAY_SERVER_READBACK=1` into display_server's envp),
//! mirroring F.2's debug-crash gate. Production boots leave the verb
//! disabled and `ReadBackPixel` returns `Error { UnknownVerb }`.
//!
//! # Output signals
//!
//! - `MULTI_CLIENT_SMOKE:BEGIN` — entry point reached
//! - `MULTI_CLIENT_SMOKE:surfaces-committed` — both protocol round-trips done
//! - `MULTI_CLIENT_SMOKE:red-visible-at-(x,y)` — readback at surface 1
//! - `MULTI_CLIENT_SMOKE:blue-visible-at-(x,y)` — readback at surface 2
//! - `MULTI_CLIENT_SMOKE:PASS` — both colors observed at expected positions
//! - `MULTI_CLIENT_SMOKE:FAIL step=N` — failure at the named step

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::display::control::{ControlCommand, ControlEvent, decode_event, encode_command};
use kernel_core::display::protocol::{
    BufferId, ClientMessage, PROTOCOL_VERSION, SurfaceId, SurfaceRole,
};
use surface_buffer::{PixelFormat, SurfaceBuffer, SurfaceBufferId};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "multi-client-smoke: alloc error\n");
    syscall_lib::exit(99)
}

// ---------------------------------------------------------------------------
// Wire constants
// ---------------------------------------------------------------------------

const DISPLAY_SERVICE_NAME: &str = "display";
const CONTROL_SERVICE_NAME: &str = "display-control";
const LABEL_PROTOCOL: u64 = 1;
const LABEL_PIXELS: u64 = 2;
const LABEL_CTL_CMD: u64 = 1;
const PIXEL_BULK_HEADER_LEN: usize = 8;
const ENCODE_BUF_LEN: usize = 128;
const REPLY_BUF_LEN: usize = 256;

/// 16×16 BGRA surfaces — small enough that the cascade offset cleanly
/// separates them on a 1280×800 framebuffer.
const SURFACE_W: u32 = 16;
const SURFACE_H: u32 = 16;

/// Surface 1: pure red, BGRA8888 little-endian (0xAARRGGBB).
const COLOR_RED: u32 = 0x00FF_0000;
/// Surface 2: pure blue, BGRA8888 little-endian.
const COLOR_BLUE: u32 = 0x0000_00FF;

/// Service-lookup retry budget for post-startup connections. 8 × 5 ms
/// is fine because display_server is already running when this smoke
/// is invoked from the post-login shell.
const SERVICE_LOOKUP_ATTEMPTS: u32 = 8;
const SERVICE_LOOKUP_BACKOFF_NS: u32 = 5_000_000;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "MULTI_CLIENT_SMOKE:BEGIN\n");

    let display_handle = match lookup_with_backoff(DISPLAY_SERVICE_NAME) {
        Some(h) => h,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "MULTI_CLIENT_SMOKE:FAIL step=1 lookup display\n",
            );
            return 1;
        }
    };

    let control_handle = match lookup_with_backoff(CONTROL_SERVICE_NAME) {
        Some(h) => h,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "MULTI_CLIENT_SMOKE:FAIL step=1 lookup display-control\n",
            );
            return 1;
        }
    };

    // ------------------------------------------------------------------
    // Step 2: drive surface (red) end-to-end through the protocol.
    // Use surface IDs in the 100+ range so they don't collide with
    // `gfx-demo`'s id=1 (which is already mapped at boot per the F.1
    // service manifest).
    // ------------------------------------------------------------------
    if !drive_surface(display_handle, SurfaceId(100), BufferId(100), COLOR_RED) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "MULTI_CLIENT_SMOKE:FAIL step=2 drive surface 1 (red)\n",
        );
        return 2;
    }

    // ------------------------------------------------------------------
    // Step 3: drive surface (blue) end-to-end. Same display endpoint;
    // the registry tracks both by id.
    // ------------------------------------------------------------------
    if !drive_surface(display_handle, SurfaceId(101), BufferId(101), COLOR_BLUE) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "MULTI_CLIENT_SMOKE:FAIL step=3 drive surface 2 (blue)\n",
        );
        return 3;
    }
    syscall_lib::write_str(STDOUT_FILENO, "MULTI_CLIENT_SMOKE:surfaces-committed\n");

    // ------------------------------------------------------------------
    // Step 4: wait briefly so display_server has a frame-tick to compose
    // both surfaces. The compose loop runs every ~16 ms (60 Hz); a 200 ms
    // sleep gives ~12 frames of headroom.
    // ------------------------------------------------------------------
    let _ = syscall_lib::nanosleep_for(0, 200_000_000);

    // ------------------------------------------------------------------
    // Step 5: query ReadBackPixel for the red surface.
    // ------------------------------------------------------------------
    // FloatingLayout assigns cascade slots in order. `gfx-demo` claims
    // slot 0 (centered, no offset) at boot, so the smoke's surfaces
    // land at slot 1 (red) and slot 2 (blue). CASCADE_OFFSET = 32 px
    // per `kernel_core::display::layout`. For a 1280x800 output with
    // 16x16 surfaces:
    //   slot 0 (gfx-demo): origin (632, 392), center (640, 400)
    //   slot 1 (red):      origin (664, 424), center (672, 432)
    //   slot 2 (blue):     origin (696, 456), center (704, 464)
    let s1_center = (672u32, 432u32);
    let s2_center = (704u32, 464u32);

    let red_color = match readback_pixel(control_handle, s1_center.0, s1_center.1) {
        Some(c) => c,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "MULTI_CLIENT_SMOKE:FAIL step=5 readback surface 1\n",
            );
            return 5;
        }
    };
    print_pixel("red-visible-at", s1_center.0, s1_center.1, red_color);

    if !color_matches(red_color, COLOR_RED) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "MULTI_CLIENT_SMOKE:FAIL step=5 wrong color\n",
        );
        print_pixel("expected-red", s1_center.0, s1_center.1, COLOR_RED);
        return 5;
    }

    // ------------------------------------------------------------------
    // Step 6: query ReadBackPixel for surface 2 (blue).
    // ------------------------------------------------------------------
    let blue_color = match readback_pixel(control_handle, s2_center.0, s2_center.1) {
        Some(c) => c,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "MULTI_CLIENT_SMOKE:FAIL step=6 readback surface 2\n",
            );
            return 6;
        }
    };
    print_pixel("blue-visible-at", s2_center.0, s2_center.1, blue_color);

    if !color_matches(blue_color, COLOR_BLUE) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "MULTI_CLIENT_SMOKE:FAIL step=6 wrong color\n",
        );
        print_pixel("expected-blue", s2_center.0, s2_center.1, COLOR_BLUE);
        return 6;
    }

    syscall_lib::write_str(STDOUT_FILENO, "MULTI_CLIENT_SMOKE:PASS\n");
    0
}

// ---------------------------------------------------------------------------
// Surface-driving helpers
// ---------------------------------------------------------------------------

fn drive_surface(
    server_handle: u32,
    surface_id: SurfaceId,
    buffer_id: BufferId,
    color: u32,
) -> bool {
    let mut buf = [0u8; ENCODE_BUF_LEN];

    // Hello — required once per connection. We send it on every drive
    // call for simplicity; display_server's dispatcher tolerates it.
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        capabilities: 0,
    };
    if !send_message(server_handle, &hello, &mut buf, "Hello") {
        return false;
    }

    let mut surface_buf = match SurfaceBuffer::new(
        SurfaceBufferId(buffer_id.0),
        SURFACE_W,
        SURFACE_H,
        PixelFormat::Bgra8888,
    ) {
        Ok(b) => b,
        Err(_) => return false,
    };
    surface_buf.fill(color);

    let create = ClientMessage::CreateSurface { surface_id };
    if !send_message(server_handle, &create, &mut buf, "CreateSurface") {
        return false;
    }

    let role = ClientMessage::SetSurfaceRole {
        surface_id,
        role: SurfaceRole::Toplevel,
    };
    if !send_message(server_handle, &role, &mut buf, "SetSurfaceRole") {
        return false;
    }

    if !send_pixels(
        server_handle,
        buffer_id,
        SURFACE_W,
        SURFACE_H,
        surface_buf.pixels(),
    ) {
        return false;
    }

    let attach = ClientMessage::AttachBuffer {
        surface_id,
        buffer_id,
    };
    if !send_message(server_handle, &attach, &mut buf, "AttachBuffer") {
        return false;
    }

    let commit = ClientMessage::CommitSurface { surface_id };
    if !send_message(server_handle, &commit, &mut buf, "CommitSurface") {
        return false;
    }

    true
}

fn send_message(server_handle: u32, msg: &ClientMessage, buf: &mut [u8], _step: &str) -> bool {
    let len = match msg.encode(buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let reply = syscall_lib::ipc_call_buf(server_handle, LABEL_PROTOCOL, 0, &buf[..len]);
    reply != u64::MAX
}

fn send_pixels(server_handle: u32, buffer_id: BufferId, w: u32, h: u32, pixels: &[u8]) -> bool {
    let mut payload: Vec<u8> = Vec::with_capacity(PIXEL_BULK_HEADER_LEN + pixels.len());
    payload.extend_from_slice(&w.to_le_bytes());
    payload.extend_from_slice(&h.to_le_bytes());
    payload.extend_from_slice(pixels);
    let reply =
        syscall_lib::ipc_call_buf(server_handle, LABEL_PIXELS, buffer_id.0 as u64, &payload);
    reply != u64::MAX
}

// ---------------------------------------------------------------------------
// ReadBackPixel helpers
// ---------------------------------------------------------------------------

fn readback_pixel(control_handle: u32, x: u32, y: u32) -> Option<u32> {
    let mut req_buf = [0u8; ENCODE_BUF_LEN];
    let req_len = encode_command(&ControlCommand::ReadBackPixel { x, y }, &mut req_buf).ok()?;
    let reply_label =
        syscall_lib::ipc_call_buf(control_handle, LABEL_CTL_CMD, 0, &req_buf[..req_len]);
    if reply_label == u64::MAX {
        return None;
    }

    let mut reply_buf = [0u8; REPLY_BUF_LEN];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX || n == 0 {
        return None;
    }
    let used = n as usize;
    let (evt, _) = decode_event(&reply_buf[..used]).ok()?;
    match evt {
        ControlEvent::PixelReply { color } => Some(color),
        _ => None,
    }
}

/// Loose color match — the framebuffer may be BGRA or RGBA depending
/// on backend, and the alpha channel can be 0 or 0xFF. Compare only the
/// 24 RGB bits and accept either channel order.
fn color_matches(observed: u32, expected: u32) -> bool {
    let obs_rgb = observed & 0x00FF_FFFF;
    let exp_rgb = expected & 0x00FF_FFFF;
    if obs_rgb == exp_rgb {
        return true;
    }
    // Try the swapped channel order (BGRA vs RGBA).
    let swapped = ((expected & 0x00FF_0000) >> 16)
        | (expected & 0x0000_FF00)
        | ((expected & 0x0000_00FF) << 16);
    obs_rgb == swapped
}

fn print_pixel(label: &str, x: u32, y: u32, color: u32) {
    syscall_lib::write_str(STDOUT_FILENO, "MULTI_CLIENT_SMOKE:");
    syscall_lib::write_str(STDOUT_FILENO, label);
    syscall_lib::write_str(STDOUT_FILENO, &format!(":({},{})=0x{:08X}\n", x, y, color));
}

// ---------------------------------------------------------------------------
// Service lookup
// ---------------------------------------------------------------------------

fn lookup_with_backoff(name: &str) -> Option<u32> {
    for attempt in 0..SERVICE_LOOKUP_ATTEMPTS {
        let raw = syscall_lib::ipc_lookup_service(name);
        if raw != u64::MAX {
            return Some(raw as u32);
        }
        if attempt + 1 == SERVICE_LOOKUP_ATTEMPTS {
            return None;
        }
        let _ = syscall_lib::nanosleep_for(0, SERVICE_LOOKUP_BACKOFF_NS);
    }
    None
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "MULTI_CLIENT_SMOKE:PANIC\n");
    syscall_lib::exit(101)
}
