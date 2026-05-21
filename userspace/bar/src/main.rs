//! Phase 73 Track C — status bar Layer-shell client.
//!
//! Renders a 24-pixel-tall persistent bar at the top of the primary
//! output. Shows nine workspace indicators (highlighting the active
//! one), the focused window title, an HH:MM wall-clock, and an
//! audio-mute hint.
//!
//! ## Gating: graphical-only sessions wait for login
//!
//! In graphical-only boots (`/etc/m3os-graphical-only` present), the
//! greeter owns the screen until the user authenticates. Showing the
//! bar over the login form is wrong — it leaks workspace / clock
//! state and steals the 24 px Layer-Top strip from the greeter's
//! framebuffer-spanning Toplevel. We poll for the session marker
//! greeter writes after a successful auth and only then connect to
//! `display_server`. In every other boot mode (skip-login, smoke),
//! no marker is involved and the bar starts immediately.
//!
//! ## Workspace indicator: real state, not a placeholder
//!
//! The active workspace is read from `display_server`'s control
//! socket via `ControlCommand::QueryWorkspaces` — polled at the bar's
//! native ~5 Hz cadence. The previous Phase 73 sketch cycled the
//! highlighted cell every 5 s as a "keep it visually alive" stand-in;
//! that hid the real `SUPER+1..9` chord output and is now gone.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use core::alloc::Layout;

use desktop_client::{DisplayConnection, SharedSurface, anchor, draw_text, fill, fill_rect};
use kernel_core::display::control::{ControlCommand, ControlEvent, decode_event, encode_command};
use kernel_core::display::protocol::{BufferId, KeyboardInteractivity, Layer, ServerMessage};
use kernel_core::input::events::PointerButton;
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

// Workspace-cell geometry shared between `render` and the
// pointer-click handler.
const WS_BOX_W: u32 = 22;
const WS_GAP: u32 = 2;
const WS_LEFT_PAD: i32 = 4;
const WS_TOP_PAD: i32 = 2;
const WS_BOX_H: u32 = 20;

const DISPLAY_CONTROL_SERVICE_NAME: &str = "display-control";
const LABEL_DISPLAY_CTL_CMD: u64 = 1;
const GRAPHICAL_ONLY_MARKER_PATH: &[u8] = b"/etc/m3os-graphical-only\0";
const SESSION_STATE_PATH: &[u8] = b"/run/m3os-current-session\0";

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "bar: starting (Phase 73)\n");

    // In graphical-only mode the greeter owns the framebuffer until a
    // user authenticates. Wait for the session marker before we
    // declare our Layer surface, so the login screen is not pierced
    // by a 24 px bar at the top.
    if file_exists(GRAPHICAL_ONLY_MARKER_PATH) {
        syscall_lib::write_str(STDOUT_FILENO, "bar: waiting for login session\n");
        wait_for_session_marker();
        syscall_lib::write_str(STDOUT_FILENO, "bar: session active; connecting\n");
    }

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
    // Anchor only to TOP. `compute_layer_geometry`'s
    // "single horizontal axis anchor" rule (TOP-only, no LEFT/RIGHT)
    // already stretches the surface to the output width — adding
    // LEFT|RIGHT would not change geometry but *would* push the
    // anchor mask to three edge bits, at which point
    // `derive_exclusive_rect` rejects the surface as not a full-edge
    // tiling and silently drops the 24 px reservation. That was the
    // visible "bar overlaps toplevel content" symptom in the first
    // Phase 73 attempt at this fix.
    if !conn.set_layer_role(
        Layer::Top,
        anchor::ANCHOR_TOP,
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

    // Open the control socket lazily — bar can run without it (the
    // workspace indicator will just stay on workspace 1). Lookup is
    // retried on every workspace-poll tick until it succeeds.
    let mut control_handle: Option<u32> = lookup_display_control();

    let mut state = BarState::new();
    render(pixels, &state);
    if !conn.attach_damage_commit(BUFFER_ID, surface.shm_id, BAR_WIDTH_PX, BAR_HEIGHT_PX) {
        surface.release();
        return 5;
    }

    let mut last_minute: i64 = -1;
    let mut tick: u32 = 0;
    let mut last_workspace: u8 = 0;
    loop {
        // Refresh clock at most twice per second; rerender only when
        // the minute string changes.
        let (sec, _ns) = syscall_lib::clock_gettime(syscall_lib::CLOCK_REALTIME);
        let mut needs_render = false;
        if sec > 0 {
            let minute = sec / 60;
            if minute != last_minute {
                state.set_clock_from_epoch(sec);
                last_minute = minute;
                needs_render = true;
            }
        }

        // Poll the compositor's authoritative workspace state once
        // per ~second. Cheap (one IPC round-trip) and keeps the
        // highlighted cell in sync with `SUPER+1..9` chord output.
        if tick.is_multiple_of(5) {
            if control_handle.is_none() {
                control_handle = lookup_display_control();
            }
            if let Some(h) = control_handle
                && let Some(active) = query_active_workspace(h)
            {
                if active != last_workspace && active >= 1 && active <= 9 {
                    state.active_workspace = active;
                    last_workspace = active;
                    needs_render = true;
                }
            }
        }

        // Drain client events the compositor enqueues on our surface.
        // A button-down inside a workspace cell flips the active
        // workspace via `ControlCommand::SwitchWorkspace`; everything
        // else (motion, key events) is discarded.
        for _ in 0..16 {
            match conn.pull_event() {
                Some(ServerMessage::Pointer(ev)) => {
                    if let PointerButton::Down(_) = ev.button
                        && let Some(target) = workspace_cell_at(ev.abs_position)
                        && let Some(h) = control_handle
                    {
                        let _ = send_switch_workspace(h, target);
                    }
                }
                Some(_) => {}
                None => break,
            }
        }

        if needs_render {
            render(pixels, &state);
            let _ =
                conn.attach_damage_commit(BUFFER_ID, surface.shm_id, BAR_WIDTH_PX, BAR_HEIGHT_PX);
        }

        tick = tick.wrapping_add(1);
        let _ = syscall_lib::nanosleep_for(0, 200_000_000);
    }
}

/// Test whether a NUL-terminated path resolves. Open+close is the
/// cheapest probe available pre-libc; the `O_RDONLY` flag matches
/// `init`'s `graphical_only_enabled` test.
fn file_exists(path: &[u8]) -> bool {
    let fd = syscall_lib::open(path, syscall_lib::O_RDONLY, 0);
    if fd < 0 {
        return false;
    }
    let _ = syscall_lib::close(fd as i32);
    true
}

/// Poll `/run/m3os-current-session` until greeter writes it. Polls at
/// 5 Hz so the bar follows the login transition with no perceptible
/// delay but does not pin the CPU.
fn wait_for_session_marker() {
    loop {
        if file_exists(SESSION_STATE_PATH) {
            return;
        }
        let _ = syscall_lib::nanosleep_for(0, 200_000_000);
    }
}

/// Best-effort lookup of `display-control`. Returns `None` so the
/// caller can retry on the next workspace poll without aborting.
fn lookup_display_control() -> Option<u32> {
    let raw = syscall_lib::ipc_lookup_service(DISPLAY_CONTROL_SERVICE_NAME);
    if raw == u64::MAX {
        None
    } else {
        Some(raw as u32)
    }
}

/// Map an `abs_position` from a `PointerEvent` to the 1-based
/// workspace number whose cell contains the point. Returns `None`
/// when the click is outside any workspace cell — clicks on the
/// title / clock / mute regions do nothing.
fn workspace_cell_at(abs: Option<(i32, i32)>) -> Option<u8> {
    let (x, y) = abs?;
    if y < WS_TOP_PAD || y >= WS_TOP_PAD + WS_BOX_H as i32 {
        return None;
    }
    let cell_pitch = (WS_BOX_W + WS_GAP) as i32;
    let local = x - WS_LEFT_PAD;
    if local < 0 {
        return None;
    }
    let cell_idx = local / cell_pitch;
    if !(0..9).contains(&cell_idx) {
        return None;
    }
    let cell_local = local - cell_idx * cell_pitch;
    if cell_local >= WS_BOX_W as i32 {
        return None;
    }
    Some((cell_idx + 1) as u8)
}

/// Issue `ControlCommand::SwitchWorkspace { n }`. Best-effort: the
/// reply Ack / Error is decoded but the bar does not surface either
/// — the next `QueryWorkspaces` tick will reflect the actual state
/// the compositor settled on.
fn send_switch_workspace(handle: u32, n: u8) -> Option<()> {
    let mut req_buf = [0u8; 32];
    let req_len = encode_command(&ControlCommand::SwitchWorkspace { n }, &mut req_buf).ok()?;
    let label = syscall_lib::ipc_call_buf(handle, LABEL_DISPLAY_CTL_CMD, 0, &req_buf[..req_len]);
    if label == u64::MAX {
        return None;
    }
    let mut reply_buf = [0u8; 64];
    let _ = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    Some(())
}

/// Issue `ControlCommand::QueryWorkspaces` and return the 1-based
/// number of the active workspace. `None` on any IPC, encode, or
/// decode failure — the bar then keeps the previously-known value.
fn query_active_workspace(handle: u32) -> Option<u8> {
    let mut req_buf = [0u8; 32];
    let req_len = encode_command(&ControlCommand::QueryWorkspaces, &mut req_buf).ok()?;
    let label = syscall_lib::ipc_call_buf(handle, LABEL_DISPLAY_CTL_CMD, 0, &req_buf[..req_len]);
    if label == u64::MAX {
        return None;
    }
    let mut reply_buf = [0u8; 512];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == 0 || n == u64::MAX {
        return None;
    }
    let used = (n as usize).min(reply_buf.len());
    let (event, _) = decode_event(&reply_buf[..used]).ok()?;
    match event {
        ControlEvent::WorkspaceListReply { entries } => {
            entries.iter().find(|e| e.active).map(|e| e.workspace)
        }
        _ => None,
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

    // Workspace indicators: 9 boxes, each 22 px wide. Geometry lives
    // in module-level constants so the pointer-click handler in
    // `workspace_cell_at` agrees with the painted layout.
    for i in 1u32..=9 {
        let x = ((i - 1) * (WS_BOX_W + WS_GAP)) as i32 + WS_LEFT_PAD;
        let active = state.active_workspace as u32 == i;
        let color = if active {
            ACTIVE_WS_COLOR
        } else {
            0xFF_2E_2E_2E
        };
        fill_rect(pixels, w, h, x, WS_TOP_PAD, WS_BOX_W, WS_BOX_H, color);
        let label_x = x + 7;
        let label_y = WS_TOP_PAD + 2;
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
