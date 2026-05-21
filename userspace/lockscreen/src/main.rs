//! Phase 73 Track F — lockscreen stub.
//!
//! Maps a full-output Layer surface with `keyboard-interactivity:
//! exclusive` so no keystroke reaches any other surface while it is
//! running. Solid-black background with centred "Locked — press
//! Enter to unlock" text. Pressing Enter exits the process; the
//! `display_server` supervisor releases the input grab and routes
//! focus back to the previous surface.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use desktop_client::{DisplayConnection, SharedSurface, anchor, draw_text, fill, fill_rect};
use kernel_core::display::protocol::{
    BufferId, KeyboardInteractivity, Layer, ServerMessage, SurfaceId,
};
use kernel_core::input::events::KeyEventKind;
use kernel_core::input::keymap::KEY_ENTER;
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "lockscreen: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "lockscreen: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const SURFACE_ID: SurfaceId = SurfaceId(1);
const BUFFER_ID: BufferId = BufferId(1);
const WIDTH_PX: u32 = 1280;
const HEIGHT_PX: u32 = 800;
const SERVICE_NAME: &str = "lockscreen";

const BG_COLOR: u32 = 0xFF_00_00_00;
const FG_COLOR: u32 = 0xFF_E0_E0_E0;
const ACCENT_COLOR: u32 = 0xFF_60_C0_FF;

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "lockscreen: starting (Phase 73)\n");

    let ep = syscall_lib::create_endpoint();
    if ep != u64::MAX
        && let Ok(ep_u32) = u32::try_from(ep)
    {
        let _ = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
    }

    let conn = match DisplayConnection::connect(SURFACE_ID) {
        Some(c) => c,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "lockscreen: display_server unavailable\n");
            return 2;
        }
    };
    if !conn.set_layer_role(
        Layer::Overlay,
        anchor::ANCHOR_TOP | anchor::ANCHOR_BOTTOM | anchor::ANCHOR_LEFT | anchor::ANCHOR_RIGHT,
        0,
        KeyboardInteractivity::Exclusive,
    ) {
        return 3;
    }

    let surface = match SharedSurface::allocate(WIDTH_PX, HEIGHT_PX) {
        Some(s) => s,
        None => return 3,
    };
    let pixels = surface.pixels_mut();
    render(pixels);
    let _ = conn.attach_damage_commit(BUFFER_ID, surface.shm_id, WIDTH_PX, HEIGHT_PX);

    loop {
        match conn.pull_event() {
            Some(ServerMessage::Key(ev)) => {
                if ev.kind == KeyEventKind::Up {
                    continue;
                }
                if ev.keycode == KEY_ENTER.0 {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "lockscreen: Enter pressed; dismissing\n",
                    );
                    break;
                }
                // Any other key while lockscreen is active is dropped —
                // the spec mandates "no keystroke (except Enter) is
                // delivered to any other surface". The `exclusive`
                // keyboard-interactivity grant achieves this on the
                // compositor side; we still receive the event because
                // the surface has focus, and explicitly discarding it
                // here means the lockscreen UI never repaints with
                // typed characters.
                continue;
            }
            Some(ServerMessage::CloseRequest { .. }) => break,
            Some(ServerMessage::Disconnect { .. }) => break,
            Some(_) => {}
            None => {
                let _ = syscall_lib::nanosleep_for(0, 20_000_000);
            }
        }
    }

    conn.goodbye();
    surface.release();
    0
}

fn render(pixels: &mut [u32]) {
    fill(pixels, BG_COLOR);
    // Soft accent block in the middle for a sense of depth.
    let cx: i32 = (WIDTH_PX as i32 - 360) / 2;
    let cy: i32 = (HEIGHT_PX as i32 - 80) / 2;
    fill_rect(pixels, WIDTH_PX, HEIGHT_PX, cx, cy, 360, 80, 0xFF_10_10_18);
    let msg = "Locked - press Enter to unlock";
    let msg_w = (msg.len() as i32) * 8;
    let mx = (WIDTH_PX as i32 - msg_w) / 2;
    let my = cy + 32;
    draw_text(
        pixels,
        WIDTH_PX,
        HEIGHT_PX,
        mx,
        my,
        msg,
        FG_COLOR,
        0xFF_10_10_18,
    );
    // Accent line above the text.
    fill_rect(
        pixels,
        WIDTH_PX,
        HEIGHT_PX,
        cx + 20,
        cy + 20,
        320,
        2,
        ACCENT_COLOR,
    );
}
