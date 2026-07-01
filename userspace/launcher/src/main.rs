//! Phase 73 Track D — SUPER+SPACE fuzzy-filter launcher.
//!
//! A floating Toplevel that scans `/usr/bin` and `/usr/local/bin` on
//! startup, presents the filtered list, and `execve`s the selected
//! binary on Return. Escape closes without launching.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;

use desktop_client::{
    DisplayConnection, SharedSurface, anchor, draw_text_scaled, fill, fill_rect, stroke_rect,
};
use kernel_core::display::protocol::{BufferId, KeyboardInteractivity, Layer, ServerMessage};
use kernel_core::input::events::KeyEventKind;
use kernel_core::input::keymap::{KEY_BACKSPACE, KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_UP};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "launcher: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "launcher: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const BUFFER_ID: BufferId = BufferId(1);
/// Base launcher surface size at 1× scale. The surface is scaled up on HiDPI
/// panels via [`ui_scale`] so the launcher stays legible at 1080p+ — it
/// previously rendered as a fixed 600×400 box of tiny 8×16 text regardless of
/// panel size.
const BASE_WIDTH_PX: u32 = 600;
const BASE_HEIGHT_PX: u32 = 400;
/// Base list-row height and list top offset (1×); scaled with the surface.
const BASE_ROW_H: i32 = 18;
const BASE_LIST_TOP: i32 = 32;
/// Upper bound on visible rows regardless of panel size.
const MAX_VISIBLE_CAP: usize = 64;
const SERVICE_NAME: &str = "launcher";

/// Integer UI scale chosen from the panel height: 1× below ~1000 px, 2× at
/// 1080p/1200p, 3× on ≥2000 px (4K) panels. Matches the bar's 2×-at-1080p
/// choice so the launcher's 8×16 font renders at a comparable density.
fn ui_scale(out_h: u32) -> u32 {
    if out_h >= 2000 {
        3
    } else if out_h >= 1000 {
        2
    } else {
        1
    }
}

const BG_COLOR: u32 = 0xFF_18_18_18;
const FG_COLOR: u32 = 0xFF_E8_E8_E8;
const HIGHLIGHT_COLOR: u32 = 0xFF_2E_8B_57;
const BORDER_COLOR: u32 = 0xFF_4A_4A_4A;
const PROMPT_BG: u32 = 0xFF_22_22_22;

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "launcher: starting (Phase 73)\n");

    // Singleton guard: only one launcher should ever own the
    // `"launcher"` IPC service name. A second SUPER+SPACE press while a
    // launcher is already up forks here, fails to register, and exits
    // immediately. Cheap (one extra fork) and keeps the chord symmetric
    // with `SpawnTerm`.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "launcher: create_endpoint failed\n");
        return 4;
    }
    let Ok(ep_u32) = u32::try_from(ep) else {
        syscall_lib::write_str(STDOUT_FILENO, "launcher: endpoint id out of u32 range\n");
        return 4;
    };
    if syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "launcher: another instance is already running — exiting\n",
        );
        return 0;
    }

    let candidates = scan_executables();
    syscall_lib::write_str(STDOUT_FILENO, "launcher: scanned ");
    let mut digits = [0u8; 16];
    let n = u_to_dec(candidates.len() as u32, &mut digits);
    let _ = syscall_lib::write(STDOUT_FILENO, &digits[..n]);
    syscall_lib::write_str(STDOUT_FILENO, " entries\n");

    let conn = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "launcher: display_server unavailable\n");
            return 2;
        }
    };
    // Float the launcher centered as an Overlay Layer rather than a Toplevel.
    // A Toplevel is inserted into the dwindle tile set, so the launcher would
    // shrink into the next tile slot; a centered Overlay layer is composited
    // above all tiles (and the bar) and positioned at its intrinsic buffer
    // size in the middle of the screen. `KeyboardInteractivity::Exclusive` is
    // required so the input dispatcher routes keystrokes to the launcher
    // (Layer surfaces do not receive Toplevel focus) — the same path the
    // lockscreen uses. `exclusive_zone = 0` so it reserves no edge space.
    if !conn.set_layer_role(
        Layer::Overlay,
        anchor::ANCHOR_CENTER,
        0,
        KeyboardInteractivity::Exclusive,
    ) {
        return 3;
    }

    // Size the launcher to the panel: base size × UI scale, clamped so the
    // surface never exceeds the framebuffer. On a 1080p panel this is a 2×
    // 1200×800 surface with a 2×-scaled font, instead of a fixed 600×400 box
    // of tiny text.
    let (out_w, out_h) = desktop_client::output_size();
    let scale = ui_scale(out_h);
    let width_px = (BASE_WIDTH_PX * scale).min(out_w.max(BASE_WIDTH_PX));
    let height_px = (BASE_HEIGHT_PX * scale).min(out_h.max(BASE_HEIGHT_PX));
    // Rows that fit below the prompt at this scale (kept in lock-step with the
    // row_h/list_top render() uses, so the KEY_DOWN clamp matches what's drawn).
    let row_h = (BASE_ROW_H * scale as i32).max(1);
    let list_top = BASE_LIST_TOP * scale as i32;
    let max_visible =
        (((height_px as i32 - list_top) / row_h).max(1) as usize).min(MAX_VISIBLE_CAP);

    let surface = match SharedSurface::allocate(width_px, height_px) {
        Some(s) => s,
        None => return 3,
    };
    let pixels = surface.pixels_mut();

    let mut query = String::new();
    let mut selected = 0usize;
    let mut filtered = filter(&candidates, &query);
    render(
        pixels,
        width_px,
        height_px,
        scale,
        max_visible,
        &query,
        &filtered,
        selected,
    );
    if !conn.attach_damage_commit(BUFFER_ID, surface.shm_id, width_px, height_px) {
        surface.release();
        return 5;
    }

    let mut dirty = false;
    loop {
        match conn.pull_event() {
            Some(ServerMessage::Key(ev)) => {
                if ev.kind == KeyEventKind::Up {
                    continue;
                }
                let kc = ev.keycode;
                if kc == KEY_ESC.0 {
                    syscall_lib::write_str(STDOUT_FILENO, "launcher: ESC — exiting\n");
                    break;
                }
                if kc == KEY_ENTER.0 {
                    if let Some(entry) = filtered.get(selected) {
                        syscall_lib::write_str(STDOUT_FILENO, "launcher: launching '");
                        let _ = syscall_lib::write(STDOUT_FILENO, entry.as_bytes());
                        syscall_lib::write_str(STDOUT_FILENO, "'\n");
                        launch(entry);
                    }
                    break;
                }
                if kc == KEY_BACKSPACE.0 {
                    let _ = query.pop();
                    filtered = filter(&candidates, &query);
                    selected = 0;
                    dirty = true;
                    continue;
                }
                if kc == KEY_UP.0 {
                    if !filtered.is_empty() && selected > 0 {
                        selected -= 1;
                        dirty = true;
                    }
                    continue;
                }
                if kc == KEY_DOWN.0 {
                    let max_idx = filtered.len().min(max_visible).saturating_sub(1);
                    if selected < max_idx {
                        selected += 1;
                        dirty = true;
                    }
                    continue;
                }
                if let Some(ch) = char::from_u32(ev.symbol)
                    && ev.symbol >= 0x20
                    && ev.symbol < 0x7F
                    && query.len() < 64
                {
                    query.push(ch);
                    filtered = filter(&candidates, &query);
                    selected = 0;
                    dirty = true;
                }
            }
            Some(ServerMessage::CloseRequest { .. }) => break,
            Some(ServerMessage::Disconnect { .. }) => break,
            Some(_) => {}
            None => {
                if dirty {
                    render(
                        pixels,
                        width_px,
                        height_px,
                        scale,
                        max_visible,
                        &query,
                        &filtered,
                        selected,
                    );
                    let _ =
                        conn.attach_damage_commit(BUFFER_ID, surface.shm_id, width_px, height_px);
                    dirty = false;
                }
                let _ = syscall_lib::nanosleep_for(0, 10_000_000);
            }
        }
    }

    conn.goodbye();
    surface.release();
    0
}

fn u_to_dec(mut n: u32, out: &mut [u8]) -> usize {
    if n == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 16];
    let mut len = 0;
    while n > 0 && len < tmp.len() {
        tmp[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    for i in 0..len {
        out[i] = tmp[len - 1 - i];
    }
    len
}

fn scan_executables() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for dir in &[
        b"/usr/bin\0".as_ref(),
        b"/usr/local/bin\0".as_ref(),
        b"/bin\0".as_ref(),
    ] {
        read_dir_into(dir, dir, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn read_dir_into(dir_path: &[u8], prefix: &[u8], out: &mut Vec<String>) {
    let fd = syscall_lib::open(dir_path, syscall_lib::O_RDONLY, 0);
    if fd < 0 {
        return;
    }
    let prefix_str: &str = match core::str::from_utf8(&prefix[..prefix.len() - 1]) {
        Ok(s) => s,
        Err(_) => {
            let _ = syscall_lib::close(fd as i32);
            return;
        }
    };
    let mut buf = [0u8; 4096];
    loop {
        let n = syscall_lib::getdents64(fd as i32, &mut buf);
        if n <= 0 {
            break;
        }
        let mut off = 0usize;
        let total = n as usize;
        while off + 19 <= total {
            // struct linux_dirent64: u64 d_ino, i64 d_off, u16 d_reclen,
            // u8 d_type, char d_name[]
            let reclen =
                u16::from_le_bytes(buf[off + 16..off + 18].try_into().unwrap_or([0u8; 2])) as usize;
            if reclen == 0 || off + reclen > total {
                break;
            }
            let name_start = off + 19;
            let name_end = (off + reclen).min(buf.len());
            // Name is NUL-padded.
            let mut nz = name_start;
            while nz < name_end && buf[nz] != 0 {
                nz += 1;
            }
            if let Ok(name) = core::str::from_utf8(&buf[name_start..nz]) {
                if name != "." && name != ".." && !name.is_empty() {
                    let mut full = String::with_capacity(prefix_str.len() + 1 + name.len());
                    full.push_str(prefix_str);
                    full.push('/');
                    full.push_str(name);
                    out.push(full);
                }
            }
            off += reclen;
        }
    }
    let _ = syscall_lib::close(fd as i32);
}

fn filter<'a>(all: &'a [String], query: &str) -> Vec<&'a String> {
    if query.is_empty() {
        return all.iter().take(64).collect();
    }
    let q = query.to_ascii_lowercase();
    let mut scored: Vec<(i32, &'a String)> = all
        .iter()
        .filter_map(|entry| {
            let name = file_name(entry).to_ascii_lowercase();
            score(&name, &q).map(|s| (s, entry))
        })
        .collect();
    // Higher score first.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(64).map(|(_, e)| e).collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path)
}

fn score(name: &str, query: &str) -> Option<i32> {
    if name.starts_with(query) {
        return Some(1000 - name.len() as i32);
    }
    if name.contains(query) {
        return Some(500 - name.len() as i32);
    }
    // Subsequence match.
    let mut q_iter = query.chars();
    let mut q_next = q_iter.next()?;
    for c in name.chars() {
        if c == q_next {
            match q_iter.next() {
                Some(c2) => q_next = c2,
                None => return Some(100 - name.len() as i32),
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn render(
    pixels: &mut [u32],
    width_px: u32,
    height_px: u32,
    scale: u32,
    max_visible: usize,
    query: &str,
    filtered: &[&String],
    selected: usize,
) {
    let s = scale.max(1);
    let si = s as i32;
    fill(pixels, BG_COLOR);
    stroke_rect(
        pixels,
        width_px,
        height_px,
        0,
        0,
        width_px,
        height_px,
        BORDER_COLOR,
    );
    // Prompt row.
    fill_rect(
        pixels,
        width_px,
        height_px,
        4 * si,
        4 * si,
        width_px - 8 * s,
        24 * s,
        PROMPT_BG,
    );
    let prompt_y = 8 * si;
    let prompt_w = draw_text_scaled(
        pixels,
        width_px,
        height_px,
        12 * si,
        prompt_y,
        "> ",
        FG_COLOR,
        PROMPT_BG,
        s,
    );
    draw_text_scaled(
        pixels,
        width_px,
        height_px,
        12 * si + prompt_w,
        prompt_y,
        query,
        FG_COLOR,
        PROMPT_BG,
        s,
    );

    let row_h: i32 = BASE_ROW_H * si;
    let list_top: i32 = BASE_LIST_TOP * si;
    let visible = filtered.iter().take(max_visible);
    for (i, entry) in visible.enumerate() {
        let y = list_top + (i as i32) * row_h;
        let bg = if i == selected {
            HIGHLIGHT_COLOR
        } else {
            BG_COLOR
        };
        fill_rect(
            pixels,
            width_px,
            height_px,
            4 * si,
            y - 2 * si,
            width_px - 8 * s,
            row_h as u32,
            bg,
        );
        draw_text_scaled(
            pixels,
            width_px,
            height_px,
            12 * si,
            y,
            file_name(entry),
            FG_COLOR,
            bg,
            s,
        );
    }
}

fn launch(path: &str) {
    // Fork and exec; parent exits regardless.
    let pid = syscall_lib::fork();
    if pid == 0 {
        let mut path_buf = [0u8; 256];
        if path.len() + 1 > path_buf.len() {
            syscall_lib::exit(1);
        }
        path_buf[..path.len()].copy_from_slice(path.as_bytes());
        path_buf[path.len()] = 0;
        let argv: [*const u8; 2] = [path_buf.as_ptr(), core::ptr::null()];
        let envp: [*const u8; 1] = [core::ptr::null()];
        let _ = syscall_lib::execve(&path_buf[..path.len() + 1], &argv, &envp);
        syscall_lib::write_str(STDOUT_FILENO, "launcher: execve failed\n");
        syscall_lib::exit(127);
    }
    // Parent: best-effort. Don't wait — the child should run
    // independently.
    let _ = pid;
}
