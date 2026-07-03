//! `settings` — Phase 105 Track D.3/D.4 settings/control-panel Toplevel.
//!
//! An `m3ui` client with the phase's four sections. **Sound** drives
//! `SetMasterVolume` through `audio_client` (Track D.2); **Display**
//! and **Power** consume Phase 103's `power` IPC service (battery,
//! thermal, sleep posture, brightness via `POWER_SET_BRIGHTNESS`, and a
//! Suspend button riding `POWER_SUSPEND` — the D.3 power-menu surface).
//! **Network** still waits on the Phase 104 Wi-Fi backend.
//!
//! On QEMU the power backends serve the platform posture (no battery,
//! no backlight, S3+S4 declared): the Display section renders the
//! honest "no backlight device" row instead of a slider, so the panel's
//! focus order — and therefore `settings-smoke`'s keyboard arms — stays
//! deterministic in CI while the slider path lights up on hardware.
//!
//! Serial sentinels (`settings-smoke`'s oracle — mirrored to serial
//! because a term-launched Toplevel's stdout is the term PTY, not COM1):
//!
//! - `SETTINGS:ready`                        — first frame composed + committed.
//! - `SETTINGS:audio=ok|unavailable`         — audio control-plane connect.
//! - `SETTINGS:power=ok battery=<b> backlight=<pct|none> sleep=<s>` —
//!   power service connected + first status decoded (`unavailable` when
//!   powerd is missing; the panel stays usable).
//! - `SETTINGS:volume=<pct> q15=<q> ack=<r>` — slider change pushed to
//!   `audio_server`; `<r>` is `ok`, `err`, or `none` (no audio client).
//! - `SETTINGS:brightness=<pct> ack=<r>`     — brightness slider change
//!   pushed through `POWER_SET_BRIGHTNESS` (hardware-only path).
//! - `SETTINGS:suspend=requested` / `=resumed|failed` — the Suspend
//!   button's `POWER_SUSPEND` round trip (blocks across the sleep).
//!
//! The volume slider is declared first, so it holds default keyboard
//! focus from the first frame: Left/Right nudge it by 1% per press (the
//! gate drives exactly this path). Tab walks to the Suspend button.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use alloc::vec;
use core::alloc::Layout;

use audio_client::AudioClient;
use desktop_client::{DisplayConnection, SharedSurface};
use kernel_core::display::protocol::{BufferId, ServerMessage};
use kernel_core::power::backlight::BACKLIGHT_UNKNOWN;
use kernel_core::power::control::{
    POWER_SERVICE_NAME, POWER_SET_BRIGHTNESS, POWER_STATUS, POWER_SUSPEND, PowerStatusWire,
    SLEEP_S0IX, SLEEP_S3, SLEEP_S4, ThermalWire,
};
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

/// Frames between `POWER_STATUS` refreshes (~2 s at the 30 Hz cadence).
const POWER_REFRESH_FRAMES: u32 = 60;

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

// ---------------------------------------------------------------------
// Phase 103 `power` service client (the m3ctl dispatch shape)
// ---------------------------------------------------------------------

fn lookup_power() -> Option<u32> {
    for _ in 0..8 {
        let h = syscall_lib::ipc_lookup_service(POWER_SERVICE_NAME);
        if h != u64::MAX {
            return u32::try_from(h).ok();
        }
        let _ = syscall_lib::nanosleep_for(0, 5_000_000);
    }
    None
}

fn query_power(handle: u32) -> Option<PowerStatusWire> {
    // Plain (non-bulk) call — the request has no body and the kernel's
    // bulk path rejects zero-length bodies (the slice-1 lesson).
    if syscall_lib::ipc_call(handle, u64::from(POWER_STATUS), 0) != 0 {
        return None;
    }
    let mut buf = vec![0u8; 64];
    let n = syscall_lib::ipc_take_pending_bulk(&mut buf);
    if n == u64::MAX || n == 0 {
        return None;
    }
    PowerStatusWire::decode(&buf[..(n as usize).min(buf.len())])
}

fn sleep_str(bits: u8) -> alloc::string::String {
    if bits == 0 {
        return alloc::string::String::from("none");
    }
    let mut s = alloc::string::String::new();
    for (bit, name) in [(SLEEP_S3, "S3"), (SLEEP_S4, "S4"), (SLEEP_S0IX, "S0ix")] {
        if bits & bit != 0 {
            if !s.is_empty() {
                s.push('+');
            }
            s.push_str(name);
        }
    }
    s
}

struct AppState {
    volume: i32,
    /// Brightness slider value (hardware path; absent widget on QEMU).
    brightness: i32,
    power: Option<u32>,
    status: Option<PowerStatusWire>,
    frames_since_refresh: u32,
    suspend_clicked: bool,
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

    // Phase 103 power service: battery/thermal/sleep + brightness.
    let power = lookup_power();
    let status = power.and_then(query_power);
    match (&power, &status) {
        (Some(_), Some(st)) => {
            let backlight = if st.backlight_pct == BACKLIGHT_UNKNOWN {
                alloc::string::String::from("none")
            } else {
                format!("{}", st.backlight_pct)
            };
            let battery = if st.battery_present {
                format!("{}%", st.percent)
            } else {
                alloc::string::String::from("none")
            };
            log(&format!(
                "SETTINGS:power=ok battery={battery} backlight={backlight} sleep={}\n",
                sleep_str(st.sleep_bits)
            ));
        }
        _ => log("SETTINGS:power=unavailable\n"),
    }

    let mut input = InputState::new();
    let mut focus = Focus::new();
    let theme = Theme::dark();
    // Start at 100% — `audio_server` boots at unity gain, so the UI
    // reflects the real server state without a startup write.
    let mut state = AppState {
        volume: 100,
        brightness: status
            .as_ref()
            .map(|s| s.backlight_pct)
            .filter(|&p| p != BACKLIGHT_UNKNOWN)
            .map(|p| p as i32)
            .unwrap_or(50),
        power,
        status,
        frames_since_refresh: 0,
        suspend_clicked: false,
    };
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

        // ---- periodic power refresh -----------------------------------
        state.frames_since_refresh += 1;
        if state.frames_since_refresh >= POWER_REFRESH_FRAMES {
            state.frames_since_refresh = 0;
            if let Some(h) = state.power {
                state.status = query_power(h);
            }
        }

        // ---- build + draw the frame ----------------------------------
        let volume_before = state.volume;
        let brightness_before = state.brightness;
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
        // Push a changed brightness through the power service (hardware
        // path — the slider only exists when a backlight device does).
        if state.brightness != brightness_before {
            let ack = match state.power {
                Some(h) => {
                    let r = syscall_lib::ipc_call(
                        h,
                        u64::from(POWER_SET_BRIGHTNESS),
                        state.brightness.clamp(0, 100) as u64,
                    );
                    if r == 0 { "ok" } else { "err" }
                }
                None => "none",
            };
            log(&format!(
                "SETTINGS:brightness={} ack={}\n",
                state.brightness, ack
            ));
        }
        // Suspend button: the call BLOCKS across the whole sleep/resume
        // cycle (powerd replies after \_WAK on the far side).
        if state.suspend_clicked {
            state.suspend_clicked = false;
            if let Some(h) = state.power {
                log("SETTINGS:suspend=requested\n");
                let r = syscall_lib::ipc_call(h, u64::from(POWER_SUSPEND), 0);
                log(if r == 0 {
                    "SETTINGS:suspend=resumed\n"
                } else {
                    "SETTINGS:suspend=failed\n"
                });
                // The world changed across a suspend — refresh eagerly.
                state.status = query_power(h);
            }
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

    ui.label("Sound");
    let volume_line = format!("  Master volume: {}%", state.volume);
    ui.label(&volume_line);
    // Declared first among focusables — holds default keyboard focus,
    // so Left/Right adjust the volume from the first frame (the gate
    // drives exactly this path).
    ui.slider(&mut state.volume, 0, 100);
    ui.separator();

    ui.label("Display");
    let has_backlight = state
        .status
        .as_ref()
        .map(|s| s.backlight_pct != BACKLIGHT_UNKNOWN)
        .unwrap_or(false);
    if has_backlight {
        let line = format!("  Brightness: {}%", state.brightness);
        ui.label(&line);
        ui.slider(&mut state.brightness, 0, 100);
    } else {
        ui.label("  No backlight device");
    }
    ui.separator();

    ui.label("Power");
    match state.status.as_ref() {
        Some(st) => {
            if st.battery_present {
                let mut line = format!("  Battery: {}%", st.percent);
                if st.state & kernel_core::power::battery::BST_STATE_CHARGING != 0 {
                    line.push_str(" (charging)");
                } else if st.state & kernel_core::power::battery::BST_STATE_DISCHARGING != 0 {
                    line.push_str(" (discharging)");
                }
                ui.label(&line);
            } else {
                ui.label("  No battery - mains power");
            }
            if st.thermal != ThermalWire::NoZones {
                let line = format!(
                    "  Thermal: {} ({}.{} C)",
                    st.thermal.as_str(),
                    st.temp_deci_c / 10,
                    (st.temp_deci_c % 10).unsigned_abs()
                );
                ui.label(&line);
            }
            let line = format!("  Sleep: {}", sleep_str(st.sleep_bits));
            ui.label(&line);
            if st.sleep_bits & (SLEEP_S3 | SLEEP_S0IX) != 0 && ui.button("  Suspend").clicked {
                state.suspend_clicked = true;
            }
        }
        None => {
            ui.label("  Power service unavailable");
        }
    }
    ui.end();
}
