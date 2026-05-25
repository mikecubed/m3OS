//! Phase 73 Track F — lockscreen with password authentication.
//!
//! Maps a full-output Layer surface with
//! `KeyboardInteractivity::Exclusive` so every keystroke is routed to
//! the lockscreen (compositor honours the claim via
//! `SurfaceRegistry::active_exclusive_layer`). The lockscreen reads
//! the authenticated user from `/run/m3os-current-session`, prompts
//! for that user's password, validates it against `/etc/shadow`, and
//! only exits on success. Typed characters are masked as `*`.
//!
//! Behaviour:
//!   * Boot path: shows the active username plus a masked password
//!     field with a blinking caret-ish underline.
//!   * Enter validates against `/etc/shadow` using
//!     `syscall_lib::sha256::verify_password` (same primitive as
//!     greeter). Correct password exits the process; the compositor
//!     releases the exclusive grant and the previously-focused
//!     Toplevel resumes receiving input.
//!   * Backspace deletes one character.
//!   * After three consecutive failures the screen shows
//!     "Too many attempts" and the prompt rejects input for a few
//!     seconds before re-enabling.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;

use desktop_client::{DisplayConnection, SharedSurface, anchor, draw_text_scaled, fill, fill_rect};
use kernel_core::display::protocol::{BufferId, KeyboardInteractivity, Layer, ServerMessage};
use kernel_core::input::events::{KeyEvent, KeyEventKind};
use kernel_core::input::keymap::{KEY_BACKSPACE, KEY_ENTER};
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

const BUFFER_ID: BufferId = BufferId(1);
const SERVICE_NAME: &str = "lockscreen";

const BG_COLOR: u32 = 0xFF_00_00_00;
const FG_COLOR: u32 = 0xFF_E0_E0_E0;
const ACCENT_COLOR: u32 = 0xFF_60_C0_FF;
const PANEL_BG: u32 = 0xFF_10_10_18;
const ERROR_COLOR: u32 = 0xFF_E8_5A_5A;

const FAILURE_THRESHOLD: u32 = 3;
const BACKOFF_SECS: u64 = 5;
const MAX_PASSWORD_LEN: usize = 128;
const SESSION_STATE_PATH: &[u8] = b"/run/m3os-current-session\0";
/// 2× scaled bitmap font keeps the prompt legible at 1080p+.
const FONT_SCALE: u32 = 2;
const GLYPH_W: i32 = 8 * FONT_SCALE as i32;
const GLYPH_H: i32 = 16 * FONT_SCALE as i32;

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "lockscreen: starting (Phase 73)\n");

    // Singleton guard: only one lockscreen should ever own the
    // `"lockscreen"` IPC service name. `m3ctl lock` forks an
    // unprivileged child unconditionally; if a lockscreen is already
    // running, that child reaches this point, fails to register, and
    // exits without claiming an exclusive-keyboard surface. Without
    // this guard, repeated `m3ctl lock` invocations would stack new
    // overlays on top of each other.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "lockscreen: create_endpoint failed\n");
        return 4;
    }
    let Ok(ep_u32) = u32::try_from(ep) else {
        syscall_lib::write_str(STDOUT_FILENO, "lockscreen: endpoint id out of u32 range\n");
        return 4;
    };
    if syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "lockscreen: another instance is already running — exiting\n",
        );
        return 0;
    }

    let username = read_session_username();
    if username.is_empty() {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "lockscreen: no /run/m3os-current-session; defaulting to 'root'\n",
        );
    }

    let conn = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "lockscreen: display_server unavailable\n");
            return 2;
        }
    };
    // Anchor to a single edge (TOP) only: `derive_exclusive_rect`
    // rejects multi-edge anchor masks as "not a full-edge tiling",
    // which would drop our exclusive-keyboard claim along with it.
    // `compute_layer_geometry`'s single-axis stretch rule still
    // gives us full-width geometry; the surface buffer is sized to
    // the running framebuffer so 1080p (or larger) boots get a
    // full-output lock screen instead of a centred 1280×800 island.
    if !conn.set_layer_role(
        Layer::Overlay,
        anchor::ANCHOR_TOP,
        0,
        KeyboardInteractivity::Exclusive,
    ) {
        syscall_lib::write_str(STDOUT_FILENO, "lockscreen: SetSurfaceRole failed\n");
        return 3;
    }

    let (width, height) = desktop_client::output_size();
    let surface = match SharedSurface::allocate(width, height) {
        Some(s) => s,
        None => return 3,
    };
    let pixels = surface.pixels_mut();

    let mut state = UiState {
        username: if username.is_empty() {
            String::from("root")
        } else {
            username
        },
        password: String::new(),
        failures: 0,
        status: Status::Idle,
        locked_until_secs: 0,
    };

    render(pixels, &state, width, height);
    let _ = conn.attach_damage_commit(BUFFER_ID, surface.shm_id, width, height);

    loop {
        // Backoff handling: while we're in the "too many attempts"
        // window, ignore key input but keep polling so the surface
        // doesn't appear hung.
        let now_secs = clock_secs();
        if let Status::Backoff = state.status
            && now_secs >= state.locked_until_secs
        {
            state.status = Status::Idle;
            state.password.clear();
            state.failures = 0;
            render(pixels, &state, width, height);
            let _ = conn.attach_damage_commit(BUFFER_ID, surface.shm_id, width, height);
        }

        match conn.pull_event() {
            Some(ServerMessage::Key(ev)) => {
                if ev.kind == KeyEventKind::Up {
                    continue;
                }
                let dirty = handle_key(&ev, &mut state);
                if matches!(state.status, Status::Unlocked) {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "lockscreen: auth ok; releasing exclusive grant\n",
                    );
                    break;
                }
                if dirty {
                    render(pixels, &state, width, height);
                    let _ = conn.attach_damage_commit(BUFFER_ID, surface.shm_id, width, height);
                }
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

#[derive(PartialEq, Eq)]
enum Status {
    Idle,
    BadPassword,
    Backoff,
    Unlocked,
}

struct UiState {
    username: String,
    password: String,
    failures: u32,
    status: Status,
    locked_until_secs: u64,
}

/// Process one key event and update `state`. Returns `true` when the
/// caller should re-render the surface (input changed, status flipped,
/// or backoff started).
fn handle_key(ev: &KeyEvent, state: &mut UiState) -> bool {
    // While in backoff, swallow every keystroke.
    if matches!(state.status, Status::Backoff) {
        return false;
    }

    if ev.keycode == KEY_ENTER.0 {
        return submit(state);
    }
    if ev.keycode == KEY_BACKSPACE.0 {
        if state.password.pop().is_some() {
            return true;
        }
        return false;
    }
    // Append printable ASCII. The kbd_server resolves keymap + shift,
    // so `symbol` is the final char value.
    if ev.symbol >= 0x20 && ev.symbol < 0x7F && state.password.len() < MAX_PASSWORD_LEN {
        if let Some(ch) = char::from_u32(ev.symbol) {
            state.password.push(ch);
            return true;
        }
    }
    false
}

/// Validate the entered password against `/etc/shadow`. On success
/// flips `state.status` to `Unlocked`; on failure increments the
/// counter and either re-prompts or enters backoff.
fn submit(state: &mut UiState) -> bool {
    if state.password.is_empty() {
        return false;
    }
    let ok = verify_password(state.username.as_bytes(), state.password.as_bytes());
    state.password.clear();
    if ok {
        state.status = Status::Unlocked;
        return true;
    }
    state.failures = state.failures.saturating_add(1);
    if state.failures >= FAILURE_THRESHOLD {
        state.status = Status::Backoff;
        state.locked_until_secs = clock_secs().saturating_add(BACKOFF_SECS);
    } else {
        state.status = Status::BadPassword;
    }
    true
}

/// Read `/etc/shadow`, find the row matching `username`, verify
/// `password` via `syscall_lib::sha256::verify_password`. Pure
/// authentication path — identical contract to greeter's
/// `verify_shadow_password`.
fn verify_password(username: &[u8], password: &[u8]) -> bool {
    let mut shadow_buf = [0u8; 4096];
    let n = read_file(b"/etc/shadow\0", &mut shadow_buf);
    if n == 0 {
        return false;
    }
    let shadow = &shadow_buf[..n];
    for line in shadow.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let colon = match line.iter().position(|&b| b == b':') {
            Some(i) => i,
            None => continue,
        };
        let name = &line[..colon];
        if name != username {
            continue;
        }
        let rest = &line[colon + 1..];
        let hash_end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
        let hash_field = &rest[..hash_end];
        // Locked accounts (`!` / `*`) never authenticate.
        if hash_field == b"!" || hash_field == b"*" || hash_field.is_empty() {
            return false;
        }
        return syscall_lib::sha256::verify_password(password, hash_field);
    }
    false
}

/// Read `username` from the session marker file written by greeter.
/// Returns an empty string when the marker is absent (skip-login
/// boots) — the caller falls back to a default name.
fn read_session_username() -> String {
    let mut buf = [0u8; 512];
    let n = read_file(SESSION_STATE_PATH, &mut buf);
    if n == 0 {
        return String::new();
    }
    let text = match core::str::from_utf8(&buf[..n]) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("user=") {
            return String::from(rest.trim());
        }
    }
    String::new()
}

fn read_file(path: &[u8], buf: &mut [u8]) -> usize {
    let fd = syscall_lib::open(path, syscall_lib::O_RDONLY, 0);
    if fd < 0 {
        return 0;
    }
    let mut total = 0usize;
    while total < buf.len() {
        let n = syscall_lib::read(fd as i32, &mut buf[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    let _ = syscall_lib::close(fd as i32);
    total
}

fn clock_secs() -> u64 {
    let (sec, _ns) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    if sec < 0 { 0 } else { sec as u64 }
}

fn render(pixels: &mut [u32], state: &UiState, width: u32, height: u32) {
    fill(pixels, BG_COLOR);

    // Centered prompt panel sized for the 2× font.
    let panel_w: u32 = 800;
    let panel_h: u32 = 320;
    let cx: i32 = (width as i32 - panel_w as i32) / 2;
    let cy: i32 = (height as i32 - panel_h as i32) / 2;
    fill_rect(pixels, width, height, cx, cy, panel_w, panel_h, PANEL_BG);
    fill_rect(
        pixels,
        width,
        height,
        cx + 32,
        cy + 32,
        panel_w - 64,
        3,
        ACCENT_COLOR,
    );

    let title = "Screen locked";
    let title_w = (title.len() as i32) * GLYPH_W;
    draw_text_scaled(
        pixels,
        width,
        height,
        cx + (panel_w as i32 - title_w) / 2,
        cy + 56,
        title,
        FG_COLOR,
        PANEL_BG,
        FONT_SCALE,
    );

    // User line.
    let mut user_line = String::from("user: ");
    user_line.push_str(&state.username);
    draw_text_scaled(
        pixels,
        width,
        height,
        cx + 40,
        cy + 120,
        &user_line,
        FG_COLOR,
        PANEL_BG,
        FONT_SCALE,
    );

    // Password line with `*` masking.
    let prompt_label = "password: ";
    let mut password_line = String::from(prompt_label);
    let mut mask: Vec<u8> = Vec::with_capacity(state.password.len());
    mask.resize(state.password.len().min(32), b'*');
    password_line.push_str(core::str::from_utf8(&mask).unwrap_or(""));
    draw_text_scaled(
        pixels,
        width,
        height,
        cx + 40,
        cy + 176,
        &password_line,
        FG_COLOR,
        PANEL_BG,
        FONT_SCALE,
    );

    // Underline beneath the input area to make the field discoverable.
    let underline_x = cx + 40 + (prompt_label.len() as i32) * GLYPH_W;
    let underline_y = cy + 176 + GLYPH_H + 4;
    let underline_w: u32 =
        (panel_w - 80).saturating_sub(prompt_label.len() as u32 * GLYPH_W as u32);
    fill_rect(
        pixels,
        width,
        height,
        underline_x,
        underline_y,
        underline_w,
        2,
        ACCENT_COLOR,
    );

    let (status_text, status_color) = match state.status {
        Status::Idle => ("Enter password to unlock", FG_COLOR),
        Status::BadPassword => ("Incorrect password", ERROR_COLOR),
        Status::Backoff => ("Too many attempts; try again shortly", ERROR_COLOR),
        Status::Unlocked => ("Unlocking...", ACCENT_COLOR),
    };
    let status_w = (status_text.len() as i32) * GLYPH_W;
    draw_text_scaled(
        pixels,
        width,
        height,
        cx + (panel_w as i32 - status_w) / 2,
        cy + (panel_h as i32) - 64,
        status_text,
        status_color,
        PANEL_BG,
        FONT_SCALE,
    );
}
