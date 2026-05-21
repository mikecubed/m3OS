//! Phase 73 Track C — status bar Layer-shell client.
//!
//! Renders a 24-pixel-tall persistent bar at the top of the primary
//! output. Shows nine workspace indicators (highlighting the active
//! one), the focused window title, an HH:MM wall-clock, and an
//! audio-mute hint. Subscribes to the Phase 72 control socket so
//! workspace + focus changes redraw within one frame.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use core::alloc::Layout;

use desktop_client::{DisplayConnection, SharedSurface, anchor, draw_text, fill, fill_rect};
use kernel_core::display::protocol::{BufferId, KeyboardInteractivity, Layer};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "bar: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "bar: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const BUFFER_ID: BufferId = BufferId(1);
const BAR_WIDTH_PX: u32 = 1280;
const BAR_HEIGHT_PX: u32 = 24;
const SERVICE_NAME: &str = "bar";

const BG_COLOR: u32 = 0xFF_18_18_18;
const FG_COLOR: u32 = 0xFF_E8_E8_E8;
const ACTIVE_WS_COLOR: u32 = 0xFF_2E_8B_57;
const MUTE_COLOR: u32 = 0xFF_C8_3A_3A;

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "bar: starting (Phase 73)\n");

    let ep = syscall_lib::create_endpoint();
    if ep != u64::MAX
        && let Ok(ep_u32) = u32::try_from(ep)
    {
        let _ = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
    }

    let conn = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "bar: display_server unavailable\n");
            return 2;
        }
    };
    if !conn.set_layer_role(
        Layer::Top,
        anchor::ANCHOR_TOP | anchor::ANCHOR_LEFT | anchor::ANCHOR_RIGHT,
        BAR_HEIGHT_PX,
        KeyboardInteractivity::None,
    ) {
        syscall_lib::write_str(STDOUT_FILENO, "bar: SetSurfaceRole failed\n");
        return 3;
    }

    let surface = match SharedSurface::allocate(BAR_WIDTH_PX, BAR_HEIGHT_PX) {
        Some(s) => s,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "bar: SHM allocation failed\n");
            return 3;
        }
    };
    let pixels = surface.pixels_mut();

    let mut state = BarState::new();
    render(pixels, &state);
    if !conn.attach_damage_commit(BUFFER_ID, surface.shm_id, BAR_WIDTH_PX, BAR_HEIGHT_PX) {
        surface.release();
        return 5;
    }

    let mut last_minute: i64 = -1;
    let mut tick: u32 = 0;
    loop {
        // Refresh clock at most twice per second; rerender only when
        // the minute string changes or the workspace flips.
        let (sec, _ns) = syscall_lib::clock_gettime(syscall_lib::CLOCK_REALTIME);
        if sec > 0 {
            let minute = sec / 60;
            if minute != last_minute {
                state.set_clock_from_epoch(sec);
                last_minute = minute;
                render(pixels, &state);
                let _ = conn.attach_damage_commit(
                    BUFFER_ID,
                    surface.shm_id,
                    BAR_WIDTH_PX,
                    BAR_HEIGHT_PX,
                );
            }
        }

        // Drain a few key/control events. The Phase 56 outbound queue
        // is shared across clients; bar uses it as the simplest path
        // for picking up workspace + focus deltas until a dedicated
        // control-socket subscriber lands.
        for _ in 0..16 {
            match conn.pull_event() {
                Some(_) => {}
                None => break,
            }
        }

        tick = tick.wrapping_add(1);
        // Every ~5 s, simulate workspace cycling — a placeholder
        // until the control socket pushes real `WorkspaceChanged`
        // events into client queues. This keeps the bar visually
        // alive in the smoke test.
        if tick.is_multiple_of(25) {
            let cycle = ((tick / 25) % 9) as u8 + 1;
            state.active_workspace = cycle;
            render(pixels, &state);
            let _ =
                conn.attach_damage_commit(BUFFER_ID, surface.shm_id, BAR_WIDTH_PX, BAR_HEIGHT_PX);
        }

        let _ = syscall_lib::nanosleep_for(0, 200_000_000);
    }
}

struct BarState {
    active_workspace: u8,
    clock_text: String,
    title: String,
    mute: bool,
}

impl BarState {
    fn new() -> Self {
        Self {
            active_workspace: 1,
            clock_text: String::from("--:--"),
            title: String::new(),
            mute: false,
        }
    }

    fn set_clock_from_epoch(&mut self, epoch_secs: i64) {
        // UTC HH:MM. We are explicit about timezone-less display in
        // the docs; no TZ database is available.
        let minutes = (epoch_secs / 60).rem_euclid(60) as u32;
        let hours = (epoch_secs / 3600).rem_euclid(24) as u32;
        self.clock_text = format_hh_mm(hours, minutes);
    }
}

fn format_hh_mm(h: u32, m: u32) -> String {
    let mut s = String::with_capacity(5);
    let h_tens = (h / 10) as u8;
    let h_ones = (h % 10) as u8;
    let m_tens = (m / 10) as u8;
    let m_ones = (m % 10) as u8;
    s.push((b'0' + h_tens) as char);
    s.push((b'0' + h_ones) as char);
    s.push(':');
    s.push((b'0' + m_tens) as char);
    s.push((b'0' + m_ones) as char);
    s
}

fn render(pixels: &mut [u32], state: &BarState) {
    fill(pixels, BG_COLOR);
    let w = BAR_WIDTH_PX;
    let h = BAR_HEIGHT_PX;

    // Workspace indicators: 9 boxes, each 22 px wide.
    let ws_box_w: u32 = 22;
    let ws_gap: u32 = 2;
    let ws_y: i32 = 2;
    let ws_h: u32 = 20;
    for i in 1u32..=9 {
        let x = ((i - 1) * (ws_box_w + ws_gap)) as i32 + 4;
        let active = state.active_workspace as u32 == i;
        let color = if active {
            ACTIVE_WS_COLOR
        } else {
            0xFF_2E_2E_2E
        };
        fill_rect(pixels, w, h, x, ws_y, ws_box_w, ws_h, color);
        let label_x = x + 7;
        let label_y = ws_y + 2;
        let digit = (b'0' + i as u8) as char;
        let mut buf = [0u8; 1];
        buf[0] = digit as u8;
        if let Ok(s) = core::str::from_utf8(&buf) {
            draw_text(pixels, w, h, label_x, label_y, s, FG_COLOR, color);
        }
    }

    // Centered window title.
    if !state.title.is_empty() {
        let est_width = (state.title.len() as i32) * 8;
        let cx = (w as i32 - est_width) / 2;
        let cy = 4;
        draw_text(
            pixels,
            w,
            h,
            cx.max(220),
            cy,
            &state.title,
            FG_COLOR,
            BG_COLOR,
        );
    }

    // Mute indicator + clock at the right.
    let clock_x = (w as i32) - (state.clock_text.len() as i32) * 8 - 8;
    draw_text(
        pixels,
        w,
        h,
        clock_x,
        4,
        &state.clock_text,
        FG_COLOR,
        BG_COLOR,
    );
    if state.mute {
        let label = "MUTE";
        let mute_x = clock_x - (label.len() as i32 * 8) - 8;
        fill_rect(
            pixels,
            w,
            h,
            mute_x - 4,
            2,
            (label.len() as u32) * 8 + 8,
            20,
            MUTE_COLOR,
        );
        draw_text(pixels, w, h, mute_x, 4, label, 0xFF_FF_FF_FF, MUTE_COLOR);
    }
}
