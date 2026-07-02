//! `m3ui-demo` — a Toplevel that exercises every `m3ui` widget, and the
//! target of the `toolkit-render-probe` gate (Phase 105 Track A.7).
//!
//! Each frame it folds the compositor's input into `m3ui`'s `InputState`,
//! builds a `Ui`, declares a button / counter label / checkbox / text
//! field / selectable list / slider, and commits the surface. It prints
//! serial sentinels the render probe keys off:
//!
//! - `M3UI_DEMO:ready`          — the first frame composed + committed.
//! - `M3UI_DEMO:focus`          — the compositor gave the window keyboard focus.
//! - `M3UI_DEMO:geom ...`       — window + button screen rects (for the pointer arm).
//! - `M3UI_DEMO:count=<n>`      — the `+1` button was activated (keyboard or pointer).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use core::alloc::Layout;

use desktop_client::{DisplayConnection, SharedSurface};
use kernel_core::display::protocol::{BufferId, ServerMessage};
use m3ui::{
    Focus, InputState, Rect, SurfacePainter, TextBuffer, Theme, Ui, apply_pointer, decode_key,
};
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{STDOUT_FILENO, write_str};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "m3ui-demo: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "m3ui-demo: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const BUFFER_ID: BufferId = BufferId(1);
const WIN_W: u32 = 460;
const WIN_H: u32 = 380;
const SCALE: u32 = 1;

/// Mirror to serial (the render probe's oracle) AND dmesg.
fn log(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    syscall_lib::serial_print(msg);
}

struct AppState {
    count: i32,
    enabled: bool,
    text: TextBuffer,
    volume: i32,
    selected: usize,
}

fn program_main(_args: &[&str]) -> i32 {
    let conn = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => {
            log("m3ui-demo: cannot connect to display_server\n");
            return 1;
        }
    };
    conn.set_toplevel_role();

    let mut win_w = WIN_W;
    let mut win_h = WIN_H;
    let mut surface = match SharedSurface::allocate(win_w, win_h) {
        Some(s) => s,
        None => {
            log("m3ui-demo: surface allocate failed\n");
            return 1;
        }
    };

    let mut input = InputState::new();
    let mut focus = Focus::new();
    let theme = Theme::dark();
    let mut state = AppState {
        count: 0,
        enabled: true,
        text: TextBuffer::from_str("edit me"),
        volume: 50,
        selected: 0,
    };
    // Window origin in screen space (from SurfaceConfigured), for the
    // pointer-injection arm's coordinate math.
    let mut win_origin = (0i32, 0i32);
    let mut announced_ready = false;
    let mut geom_printed = false;

    // The button's local rect within the window (stable given the fixed
    // layout below) so we can report its screen position once configured.
    let button_local = button_rect(&theme);

    loop {
        input.begin_frame();
        focus.begin_frame();

        // ---- fold compositor events ----------------------------------
        let mut running = true;
        let mut resized: Option<(u32, u32)> = None;
        for _ in 0..64 {
            match conn.pull_event() {
                Some(ServerMessage::Key(ev)) => {
                    if let Some(kp) = decode_key(&ev) {
                        input.set_mods(kp.mods);
                        input.push_key(kp);
                    }
                }
                Some(ServerMessage::Pointer(ev)) => apply_pointer(&mut input, &ev),
                Some(ServerMessage::FocusIn { .. }) => log("M3UI_DEMO:focus\n"),
                Some(ServerMessage::SurfaceConfigured { rect, .. }) => {
                    win_origin = (rect.x, rect.y);
                }
                Some(ServerMessage::SurfaceResized { width, height, .. }) => {
                    resized = Some((width.max(64), height.max(64)));
                }
                Some(ServerMessage::CloseRequest { .. }) => running = false,
                Some(_) => {}
                None => break,
            }
        }
        if !running {
            break;
        }
        if let Some((w, h)) = resized
            && (w != win_w || h != win_h)
        {
            surface.release();
            win_w = w;
            win_h = h;
            surface = match SharedSurface::allocate(win_w, win_h) {
                Some(s) => s,
                None => {
                    log("m3ui-demo: realloc failed\n");
                    return 1;
                }
            };
        }

        // ---- build + draw the frame ----------------------------------
        let count_before = state.count;
        {
            let pixels = surface.pixels_mut();
            let mut painter = SurfacePainter::new(pixels, win_w, win_h, SCALE);
            let bounds = Rect::new(0, 0, win_w as i32, win_h as i32);
            build_ui(&mut painter, &input, &mut focus, &theme, bounds, &mut state);
        }
        if state.count != count_before {
            log(&format!("M3UI_DEMO:count={}\n", state.count));
        }

        // ---- present --------------------------------------------------
        conn.attach_damage_commit(BUFFER_ID, surface.shm_id, win_w, win_h);
        if !announced_ready {
            announced_ready = true;
            log("M3UI_DEMO:ready\n");
        }
        if !geom_printed {
            geom_printed = true;
            let bx = win_origin.0 + button_local.x;
            let by = win_origin.1 + button_local.y;
            let (sw, sh) = desktop_client::output_size();
            log(&format!(
                "M3UI_DEMO:geom win=({},{},{},{}) screen=({},{}) btn=({},{},{},{})\n",
                win_origin.0,
                win_origin.1,
                win_w,
                win_h,
                sw,
                sh,
                bx,
                by,
                button_local.w,
                button_local.h
            ));
        }

        let _ = syscall_lib::nanosleep_for(0, 33_000_000); // ~30 Hz
    }

    surface.release();
    conn.goodbye();
    0
}

/// The button occupies the third row (after title + count labels). Kept
/// in one place so `main` can report its screen position for the pointer
/// arm and `build_ui` places it identically.
fn button_rect(theme: &Theme) -> Rect {
    let x = theme.pad_x;
    // Rows: title, count label, then the button — each row_height + spacing.
    let row = |i: i32| theme.pad_y + i * (theme.row_height + theme.spacing);
    Rect::new(x, row(2), WIN_W as i32 - 2 * theme.pad_x, theme.row_height)
}

fn build_ui<P: m3ui::Painter>(
    painter: &mut P,
    input: &InputState,
    focus: &mut Focus,
    theme: &Theme,
    bounds: Rect,
    state: &mut AppState,
) {
    let mut ui = Ui::new(painter, input, focus, theme, bounds);
    ui.label("m3ui demo");
    let count_line = format!("count: {}", state.count);
    ui.label(&count_line);
    // The button is the first focusable widget → default keyboard focus,
    // so a single Enter (or a click) increments the counter.
    if ui.button("+1").clicked {
        state.count += 1;
    }
    ui.checkbox("enabled", &mut state.enabled);
    ui.text_field(&mut state.text);
    ui.separator();
    for (i, name) in ["Alpha", "Bravo", "Charlie"].iter().enumerate() {
        if ui.selectable(name, state.selected == i).clicked {
            state.selected = i;
        }
    }
    ui.separator();
    ui.slider(&mut state.volume, 0, 100);
    ui.end();
}
