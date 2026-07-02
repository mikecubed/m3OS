//! `settings` — Phase 105 Track D.3 settings/control-panel Toplevel.
//!
//! An `m3ui` client with the phase's four sections. Only **Sound** is
//! backend-wired in this slice: the master-volume slider issues
//! `SetMasterVolume` to `audio_server` over `audio_client` (Track D.2's
//! verb). **Network** waits on the Phase 104 Wi-Fi backend and
//! **Display**/**Power** on the Phase 103 brightness/battery surface —
//! each renders a placeholder row naming its dependency until then.
//!
//! Serial sentinels (`settings-smoke`'s oracle — mirrored to serial
//! because a term-launched Toplevel's stdout is the term PTY, not COM1):
//!
//! - `SETTINGS:ready`                        — first frame composed + committed.
//! - `SETTINGS:audio=ok|unavailable`         — control-plane connect result.
//! - `SETTINGS:volume=<pct> q15=<q> ack=<r>` — slider change pushed to
//!   `audio_server`; `<r>` is `ok`, `err`, or `none` (no audio client).
//!
//! The volume slider is the panel's only focusable widget, so it holds
//! default keyboard focus from the first frame: Left/Right nudge it by
//! 1% per press (the gate drives exactly this path).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use core::alloc::Layout;

use audio_client::AudioClient;
use desktop_client::{DisplayConnection, SharedSurface};
use kernel_core::display::protocol::{BufferId, ServerMessage};
use m3ui::{Focus, InputState, Rect, SurfacePainter, Theme, Ui, apply_pointer, decode_key};
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{STDOUT_FILENO, write_str};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "settings: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "settings: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const BUFFER_ID: BufferId = BufferId(1);
const WIN_W: u32 = 460;
const WIN_H: u32 = 470;
const SCALE: u32 = 1;

/// Mirror to serial (the smoke gate's oracle) AND the term PTY.
fn log(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    syscall_lib::serial_print(msg);
}

/// Map a 0–100 volume percentage onto the wire's Q15 gain.
///
/// 100% is exactly `0x8000` (unity — `MASTER_GAIN_UNITY_Q15`), 0% mutes;
/// intermediate values truncate downward so the sent gain never exceeds
/// what the percentage promises.
fn pct_to_q15(pct: i32) -> u16 {
    ((pct.clamp(0, 100) as u32 * 0x8000) / 100) as u16
}

struct AppState {
    volume: i32,
}

fn program_main(_args: &[&str]) -> i32 {
    let conn = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => {
            log("settings: cannot connect to display_server\n");
            return 1;
        }
    };
    conn.set_toplevel_role();

    let mut win_w = WIN_W;
    let mut win_h = WIN_H;
    let mut surface = match SharedSurface::allocate(win_w, win_h) {
        Some(s) => s,
        None => {
            log("settings: surface allocate failed\n");
            return 1;
        }
    };

    // Control-plane audio connection (no PCM stream is opened, so the
    // server's single-client slot stays free for a real player). The
    // panel stays usable without audio — the slider then reports
    // `ack=none` instead of pushing gains.
    let mut audio = match AudioClient::connect() {
        Ok(c) => {
            log("SETTINGS:audio=ok\n");
            Some(c)
        }
        Err(_) => {
            log("SETTINGS:audio=unavailable\n");
            None
        }
    };

    let mut input = InputState::new();
    let mut focus = Focus::new();
    let theme = Theme::dark();
    // Start at 100% — `audio_server` boots at unity gain, so the UI
    // reflects the real server state without a startup write.
    let mut state = AppState { volume: 100 };
    let mut announced_ready = false;

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
                    log("settings: realloc failed\n");
                    return 1;
                }
            };
        }

        // ---- build + draw the frame ----------------------------------
        let volume_before = state.volume;
        {
            let pixels = surface.pixels_mut();
            let mut painter = SurfacePainter::new(pixels, win_w, win_h, SCALE);
            let bounds = Rect::new(0, 0, win_w as i32, win_h as i32);
            build_ui(&mut painter, &input, &mut focus, &theme, bounds, &mut state);
        }
        // Push a changed volume to the server once per frame (the slider
        // may fold several ±1 key steps into one frame — one IPC covers
        // them all).
        if state.volume != volume_before {
            let q15 = pct_to_q15(state.volume);
            let ack = match audio.as_mut() {
                Some(client) => {
                    if client.set_master_volume(q15).is_ok() {
                        "ok"
                    } else {
                        "err"
                    }
                }
                None => "none",
            };
            log(&format!(
                "SETTINGS:volume={} q15={} ack={}\n",
                state.volume, q15, ack
            ));
        }

        // ---- present --------------------------------------------------
        conn.attach_damage_commit(BUFFER_ID, surface.shm_id, win_w, win_h);
        if !announced_ready {
            announced_ready = true;
            log("SETTINGS:ready\n");
        }

        let _ = syscall_lib::nanosleep_for(0, 33_000_000); // ~30 Hz
    }

    surface.release();
    conn.goodbye();
    0
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
    ui.label("Settings");
    ui.separator();

    ui.label("Network");
    ui.label("  Wi-Fi setup arrives with Phase 104");
    ui.separator();

    ui.label("Display");
    ui.label("  Brightness control arrives with Phase 103");
    ui.separator();

    ui.label("Sound");
    let volume_line = format!("  Master volume: {}%", state.volume);
    ui.label(&volume_line);
    // The panel's only focusable widget — it holds default keyboard
    // focus, so Left/Right adjust the volume from the first frame.
    ui.slider(&mut state.volume, 0, 100);
    ui.separator();

    ui.label("Power");
    ui.label("  Battery status arrives with Phase 103");
    ui.end();
}
