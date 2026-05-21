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

use desktop_client::{DisplayConnection, SharedSurface, draw_text, fill, fill_rect, stroke_rect};
use kernel_core::display::protocol::{BufferId, ServerMessage};
use kernel_core::input::events::KeyEventKind;
use kernel_core::input::keymap::{KEY_BACKSPACE, KEY_ENTER, KEY_ESC};
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
const WIDTH_PX: u32 = 600;
const HEIGHT_PX: u32 = 400;
const MAX_VISIBLE: usize = 18;
const SERVICE_NAME: &str = "launcher";

const BG_COLOR: u32 = 0xFF_18_18_18;
const FG_COLOR: u32 = 0xFF_E8_E8_E8;
const HIGHLIGHT_COLOR: u32 = 0xFF_2E_8B_57;
const BORDER_COLOR: u32 = 0xFF_4A_4A_4A;
const PROMPT_BG: u32 = 0xFF_22_22_22;

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "launcher: starting (Phase 73)\n");

    let ep = syscall_lib::create_endpoint();
    if ep != u64::MAX
        && let Ok(ep_u32) = u32::try_from(ep)
    {
        let _ = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
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
    if !conn.set_toplevel_role() {
        return 3;
    }

    let surface = match SharedSurface::allocate(WIDTH_PX, HEIGHT_PX) {
        Some(s) => s,
        None => return 3,
    };
    let pixels = surface.pixels_mut();

    let mut query = String::new();
    let mut selected = 0usize;
    let mut filtered = filter(&candidates, &query);
    render(pixels, &query, &filtered, selected);
    if !conn.attach_damage_commit(BUFFER_ID, surface.shm_id, WIDTH_PX, HEIGHT_PX) {
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
                    render(pixels, &query, &filtered, selected);
                    let _ =
                        conn.attach_damage_commit(BUFFER_ID, surface.shm_id, WIDTH_PX, HEIGHT_PX);
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

fn render(pixels: &mut [u32], query: &str, filtered: &[&String], selected: usize) {
    fill(pixels, BG_COLOR);
    stroke_rect(
        pixels,
        WIDTH_PX,
        HEIGHT_PX,
        0,
        0,
        WIDTH_PX,
        HEIGHT_PX,
        BORDER_COLOR,
    );
    fill_rect(
        pixels,
        WIDTH_PX,
        HEIGHT_PX,
        4,
        4,
        WIDTH_PX - 8,
        24,
        PROMPT_BG,
    );
    let prompt = "▶ ";
    let _ = prompt;
    draw_text(
        pixels, WIDTH_PX, HEIGHT_PX, 12, 8, "> ", FG_COLOR, PROMPT_BG,
    );
    draw_text(
        pixels, WIDTH_PX, HEIGHT_PX, 30, 8, query, FG_COLOR, PROMPT_BG,
    );

    let row_h: i32 = 18;
    let list_top: i32 = 32;
    let visible = filtered.iter().take(MAX_VISIBLE);
    for (i, entry) in visible.enumerate() {
        let y = list_top + (i as i32) * row_h;
        let bg = if i == selected {
            HIGHLIGHT_COLOR
        } else {
            BG_COLOR
        };
        fill_rect(
            pixels,
            WIDTH_PX,
            HEIGHT_PX,
            4,
            y - 2,
            WIDTH_PX - 8,
            row_h as u32,
            bg,
        );
        draw_text(
            pixels,
            WIDTH_PX,
            HEIGHT_PX,
            12,
            y,
            file_name(entry),
            FG_COLOR,
            bg,
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
