//! edit — a minimal full-screen text editor for m3OS.
//!
//! A port of kibi/kilo concepts: line-based buffer, VT100 escape sequences,
//! raw terminal mode, incremental search, status bar.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::{AtomicBool, Ordering};
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    unsafe {
        if let Some(ref orig) = ORIG_TERMIOS {
            let _ = syscall_lib::tcsetattr(0, orig);
        }
    }
    syscall_lib::write(1, b"\r\nedit: out of memory\r\n");
    syscall_lib::exit(1)
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TAB_STOP: usize = 4;
const QUIT_TIMES: u8 = 3;
const VERSION: &str = "0.1.0";

// ---------------------------------------------------------------------------
// Global state for panic handler
// ---------------------------------------------------------------------------

static mut ORIG_TERMIOS: Option<syscall_lib::Termios> = None;

// ---------------------------------------------------------------------------
// SIGWINCH support
// ---------------------------------------------------------------------------

static WINCH_RECEIVED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Key enum
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Key {
    Char(u8),
    Ctrl(u8),
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    PageUp,
    PageDown,
    Home,
    End,
    Delete,
    Escape,
    None,
}

// ---------------------------------------------------------------------------
// Row — one line of text
// ---------------------------------------------------------------------------

struct Row {
    chars: Vec<u8>,
    render: Vec<u8>,
}

impl Row {
    fn new(chars: Vec<u8>) -> Self {
        let mut row = Row {
            chars,
            render: Vec::new(),
        };
        row.update_render();
        row
    }

    fn update_render(&mut self) {
        self.render.clear();
        for &c in &self.chars {
            if c == b'\t' {
                let spaces = TAB_STOP - (self.render.len() % TAB_STOP);
                for _ in 0..spaces {
                    self.render.push(b' ');
                }
            } else {
                self.render.push(c);
            }
        }
    }

    fn cx_to_rx(&self, cx: usize) -> usize {
        let mut rx = 0;
        for i in 0..cx.min(self.chars.len()) {
            if self.chars[i] == b'\t' {
                rx += TAB_STOP - (rx % TAB_STOP);
            } else {
                rx += 1;
            }
        }
        rx
    }

    fn rx_to_cx(&self, rx: usize) -> usize {
        let mut cur_rx = 0;
        for (i, &c) in self.chars.iter().enumerate() {
            if c == b'\t' {
                cur_rx += TAB_STOP - (cur_rx % TAB_STOP);
            } else {
                cur_rx += 1;
            }
            if cur_rx > rx {
                return i;
            }
        }
        self.chars.len()
    }

    fn insert_char(&mut self, at: usize, c: u8) {
        let at = at.min(self.chars.len());
        self.chars.insert(at, c);
        self.update_render();
    }

    fn delete_char(&mut self, at: usize) {
        if at < self.chars.len() {
            self.chars.remove(at);
            self.update_render();
        }
    }

    fn append_str(&mut self, s: &[u8]) {
        self.chars.extend_from_slice(s);
        self.update_render();
    }

    fn split_off(&mut self, at: usize) -> Vec<u8> {
        let rest = self.chars.split_off(at.min(self.chars.len()));
        self.update_render();
        rest
    }
}

// ---------------------------------------------------------------------------
// Append buffer — batch screen writes
// ---------------------------------------------------------------------------

struct ABuf {
    buf: Vec<u8>,
}

impl ABuf {
    fn new() -> Self {
        ABuf {
            buf: Vec::with_capacity(4096),
        }
    }

    fn push_str(&mut self, s: &[u8]) {
        self.buf.extend_from_slice(s);
    }

    fn push_byte(&mut self, b: u8) {
        self.buf.push(b);
    }

    fn flush(&self) {
        if self.buf.is_empty() {
            return;
        }
        let mut written = 0usize;
        while written < self.buf.len() {
            let n = syscall_lib::write(1, &self.buf[written..]);
            if n <= 0 {
                break;
            }
            written += n as usize;
        }
    }

    fn clear(&mut self) {
        self.buf.clear();
    }
}

// ---------------------------------------------------------------------------
// Editor state
// ---------------------------------------------------------------------------

struct Editor {
    cx: usize,
    cy: usize,
    rx: usize,
    row_offset: usize,
    col_offset: usize,
    screen_rows: usize,
    screen_cols: usize,
    rows: Vec<Row>,
    filename: Option<String>,
    status_msg: String,
    modified: bool,
    quit_times: u8,
    orig_termios: syscall_lib::Termios,
    search_last_match: Option<usize>,
    search_direction: i32,
    saved_cx: usize,
    saved_cy: usize,
}

impl Editor {
    fn new() -> Self {
        let orig = enable_raw_mode();
        let (rows, cols) = terminal_size();

        Editor {
            cx: 0,
            cy: 0,
            rx: 0,
            row_offset: 0,
            col_offset: 0,
            screen_rows: rows.saturating_sub(2).max(1),
            screen_cols: cols.max(1),
            rows: Vec::new(),
            filename: None,
            status_msg: String::new(),
            modified: false,
            quit_times: QUIT_TIMES,
            orig_termios: orig,
            search_last_match: None,
            search_direction: 1,
            saved_cx: 0,
            saved_cy: 0,
        }
    }

    // -----------------------------------------------------------------------
    // File I/O
    // -----------------------------------------------------------------------

    fn open_file(&mut self, filename: &str) {
        // Resolve relative paths to absolute using cwd.
        let abs_name = if filename.starts_with('/') {
            String::from(filename)
        } else {
            let mut cwd_buf = [0u8; 256];
            let cwd_len = syscall_lib::getcwd(&mut cwd_buf);
            if cwd_len > 0 {
                // getcwd returns length including null terminator; strip it.
                let str_len = (cwd_len as usize).saturating_sub(1);
                let cwd = core::str::from_utf8(&cwd_buf[..str_len]).unwrap_or("/");
                let mut path = String::from(cwd);
                if !path.ends_with('/') {
                    path.push('/');
                }
                path.push_str(filename);
                path
            } else {
                String::from(filename)
            }
        };
        self.filename = Some(abs_name.clone());
        self.rows.clear();

        // Build null-terminated path
        let mut path_buf: Vec<u8> = Vec::with_capacity(abs_name.len() + 1);
        path_buf.extend_from_slice(abs_name.as_bytes());
        path_buf.push(0);

        let fd = syscall_lib::open(&path_buf, syscall_lib::O_RDONLY, 0);
        if fd < 0 {
            // New file — start with empty buffer
            self.rows.push(Row::new(Vec::new()));
            self.modified = false;
            self.set_status_msg("(New file)");
            return;
        }

        let mut content = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = syscall_lib::read(fd as i32, &mut buf);
            if n < 0 {
                syscall_lib::close(fd as i32);
                self.rows.push(Row::new(Vec::new()));
                self.modified = false;
                self.set_status_msg("Error reading file");
                return;
            }
            if n == 0 {
                break;
            }
            content.extend_from_slice(&buf[..n as usize]);
        }
        syscall_lib::close(fd as i32);

        // Split into lines
        let mut start = 0;
        for i in 0..content.len() {
            if content[i] == b'\n' {
                let mut end = i;
                if end > start && content[end - 1] == b'\r' {
                    end -= 1;
                }
                self.rows.push(Row::new(content[start..end].to_vec()));
                start = i + 1;
            }
        }
        // Handle last line without trailing newline
        if start < content.len() {
            self.rows.push(Row::new(content[start..].to_vec()));
        }
        // If file was empty or ended with newline, ensure at least one row
        if self.rows.is_empty() {
            self.rows.push(Row::new(Vec::new()));
        }
        self.modified = false;
    }

    fn save_file(&mut self) {
        // Prefill with existing filename if we have one.
        let prefill: Vec<u8> = match &self.filename {
            Some(f) => f.as_bytes().to_vec(),
            None => Vec::new(),
        };

        let filename = match self.prompt_with_initial(b"Save as: ", &prefill, None) {
            Some(name) if !name.is_empty() => {
                self.filename = Some(name.clone());
                name
            }
            _ => {
                self.set_status_msg("Save aborted");
                return;
            }
        };

        // Serialize all rows
        let mut content = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            content.extend_from_slice(&row.chars);
            if i < self.rows.len() - 1 {
                content.push(b'\n');
            }
        }
        // Always end with newline
        if !content.is_empty() && content.last() != Some(&b'\n') {
            content.push(b'\n');
        }

        let mut path_buf: Vec<u8> = Vec::with_capacity(filename.len() + 1);
        path_buf.extend_from_slice(filename.as_bytes());
        path_buf.push(0);

        let fd = syscall_lib::open(
            &path_buf,
            syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC,
            0o644,
        );
        if fd < 0 {
            self.set_status_msg("Error: could not open file for writing");
            return;
        }

        let mut total_written: usize = 0;
        while total_written < content.len() {
            let n = syscall_lib::write(fd as i32, &content[total_written..]);
            if n <= 0 {
                syscall_lib::close(fd as i32);
                self.set_status_msg("Error: write failed");
                return;
            }
            total_written += n as usize;
        }
        syscall_lib::close(fd as i32);

        self.modified = false;
        let mut msg = String::from("Saved ");
        append_usize(&mut msg, content.len());
        msg.push_str(" bytes");
        self.set_status_msg(&msg);
    }

    // -----------------------------------------------------------------------
    // Key reading
    // -----------------------------------------------------------------------

    fn read_key(&self) -> Key {
        let mut buf = [0u8; 1];
        loop {
            let n = syscall_lib::read(0, &mut buf);
            if n == 1 {
                break;
            }
            if n < 0 {
                return Key::None;
            }
            // n == 0: retry
        }

        let c = buf[0];

        if c == 0x1b {
            // Escape sequence
            let mut seq = [0u8; 3];
            if syscall_lib::read(0, &mut seq[0..1]) != 1 {
                return Key::Escape;
            }
            if syscall_lib::read(0, &mut seq[1..2]) != 1 {
                return Key::Escape;
            }

            if seq[0] == b'[' {
                if seq[1] >= b'0' && seq[1] <= b'9' {
                    // Extended sequence like \x1b[5~ (PageUp)
                    if syscall_lib::read(0, &mut seq[2..3]) != 1 {
                        return Key::Escape;
                    }
                    if seq[2] == b'~' {
                        match seq[1] {
                            b'1' | b'7' => return Key::Home,
                            b'3' => return Key::Delete,
                            b'4' | b'8' => return Key::End,
                            b'5' => return Key::PageUp,
                            b'6' => return Key::PageDown,
                            _ => return Key::Escape,
                        }
                    }
                    return Key::Escape;
                }

                match seq[1] {
                    b'A' => return Key::ArrowUp,
                    b'B' => return Key::ArrowDown,
                    b'C' => return Key::ArrowRight,
                    b'D' => return Key::ArrowLeft,
                    b'H' => return Key::Home,
                    b'F' => return Key::End,
                    _ => return Key::Escape,
                }
            } else if seq[0] == b'O' {
                match seq[1] {
                    b'H' => return Key::Home,
                    b'F' => return Key::End,
                    _ => return Key::Escape,
                }
            }

            return Key::Escape;
        }

        // Ctrl keys
        if c <= 26 && c != b'\r' && c != b'\n' && c != b'\t' {
            return Key::Ctrl(c + b'a' - 1);
        }

        // Backspace (DEL)
        if c == 127 {
            return Key::Ctrl(b'h');
        }

        Key::Char(c)
    }

    // -----------------------------------------------------------------------
    // Cursor movement
    // -----------------------------------------------------------------------

    fn move_cursor(&mut self, key: Key) {
        match key {
            Key::ArrowLeft => {
                if self.cx > 0 {
                    self.cx -= 1;
                } else if self.cy > 0 {
                    self.cy -= 1;
                    self.cx = self.rows[self.cy].chars.len();
                }
            }
            Key::ArrowRight => {
                if self.cy < self.rows.len() {
                    let row_len = self.rows[self.cy].chars.len();
                    if self.cx < row_len {
                        self.cx += 1;
                    } else if self.cx == row_len && self.cy < self.rows.len() - 1 {
                        self.cy += 1;
                        self.cx = 0;
                    }
                }
            }
            Key::ArrowUp => {
                if self.cy > 0 {
                    self.cy -= 1;
                }
            }
            Key::ArrowDown => {
                if self.cy < self.rows.len().saturating_sub(1) {
                    self.cy += 1;
                }
            }
            _ => {}
        }

        // Snap cx to end of row
        let row_len = if self.cy < self.rows.len() {
            self.rows[self.cy].chars.len()
        } else {
            0
        };
        if self.cx > row_len {
            self.cx = row_len;
        }
    }

    // -----------------------------------------------------------------------
    // Text editing
    // -----------------------------------------------------------------------

    fn insert_char(&mut self, c: u8) {
        if self.cy == self.rows.len() {
            self.rows.push(Row::new(Vec::new()));
        }
        self.rows[self.cy].insert_char(self.cx, c);
        self.cx += 1;
        self.modified = true;
    }

    fn insert_newline(&mut self) {
        if self.cy >= self.rows.len() {
            self.rows.push(Row::new(Vec::new()));
            self.cy += 1;
            self.cx = 0;
            self.modified = true;
            return;
        }

        let rest = self.rows[self.cy].split_off(self.cx);
        self.cy += 1;
        self.rows.insert(self.cy, Row::new(rest));
        self.cx = 0;
        self.modified = true;
    }

    fn delete_char(&mut self) {
        if self.cy >= self.rows.len() {
            return;
        }
        if self.cx == 0 && self.cy == 0 {
            return;
        }

        if self.cx > 0 {
            self.rows[self.cy].delete_char(self.cx - 1);
            self.cx -= 1;
        } else {
            // Merge with previous line
            let current_chars = self.rows[self.cy].chars.clone();
            self.cx = self.rows[self.cy - 1].chars.len();
            self.rows[self.cy - 1].append_str(&current_chars);
            self.rows.remove(self.cy);
            self.cy -= 1;
        }
        self.modified = true;
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    fn find(&mut self) {
        self.saved_cx = self.cx;
        self.saved_cy = self.cy;
        self.search_last_match = None;
        self.search_direction = 1;

        let result = self.prompt(b"Search: ", Some(Editor::find_callback));
        if result.is_none() {
            // Cancelled — restore position
            self.cx = self.saved_cx;
            self.cy = self.saved_cy;
        }
        self.search_last_match = None;
    }

    fn find_callback(editor: &mut Editor, query: &[u8], key: Key) {
        match key {
            Key::ArrowRight | Key::ArrowDown => {
                editor.search_direction = 1;
            }
            Key::ArrowLeft | Key::ArrowUp => {
                editor.search_direction = -1;
            }
            Key::Escape | Key::Char(b'\r') => {
                editor.search_last_match = None;
                editor.search_direction = 1;
                return;
            }
            _ => {
                editor.search_last_match = None;
                editor.search_direction = 1;
            }
        }

        if query.is_empty() {
            return;
        }

        let start = match editor.search_last_match {
            Some(i) => {
                let next = i as i32 + editor.search_direction;
                if next < 0 {
                    editor.rows.len() - 1
                } else if next >= editor.rows.len() as i32 {
                    0
                } else {
                    next as usize
                }
            }
            None => 0,
        };

        for i in 0..editor.rows.len() {
            let idx = (start + i) % editor.rows.len();
            if let Some(pos) = find_substr(&editor.rows[idx].render, query) {
                editor.search_last_match = Some(idx);
                editor.cy = idx;
                editor.cx = editor.rows[idx].rx_to_cx(pos);
                editor.row_offset = editor.cy.saturating_sub(editor.screen_rows / 2);
                return;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Prompt — mini input line in message bar
    // -----------------------------------------------------------------------

    fn prompt(
        &mut self,
        prompt: &[u8],
        callback: Option<fn(&mut Editor, &[u8], Key)>,
    ) -> Option<String> {
        self.prompt_with_initial(prompt, &[], callback)
    }

    fn prompt_with_initial(
        &mut self,
        prompt: &[u8],
        initial: &[u8],
        callback: Option<fn(&mut Editor, &[u8], Key)>,
    ) -> Option<String> {
        let mut input = Vec::from(initial);

        loop {
            let mut msg = String::new();
            for &b in prompt {
                msg.push(b as char);
            }
            for &b in &input {
                msg.push(b as char);
            }
            self.status_msg = msg;
            self.refresh_screen();

            let key = self.read_key();

            match key {
                Key::Escape => {
                    self.status_msg.clear();
                    if let Some(cb) = callback {
                        cb(self, &input, key);
                    }
                    return None;
                }
                Key::Char(b'\r') | Key::Char(b'\n') => {
                    if !input.is_empty() {
                        self.status_msg.clear();
                        if let Some(cb) = callback {
                            cb(self, &input, key);
                        }
                        let s = String::from_utf8_lossy(&input).into_owned();
                        return Some(s);
                    }
                }
                Key::Ctrl(b'h') | Key::Delete => {
                    input.pop();
                    if let Some(cb) = callback {
                        cb(self, &input, key);
                    }
                }
                Key::Char(c) if c >= 32 && c < 127 => {
                    input.push(c);
                    if let Some(cb) = callback {
                        cb(self, &input, key);
                    }
                }
                _ => {
                    if let Some(cb) = callback {
                        cb(self, &input, key);
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Screen refresh
    // -----------------------------------------------------------------------

    fn scroll(&mut self) {
        self.rx = if self.cy < self.rows.len() {
            self.rows[self.cy].cx_to_rx(self.cx)
        } else {
            0
        };

        if self.cy < self.row_offset {
            self.row_offset = self.cy;
        }
        if self.cy >= self.row_offset + self.screen_rows {
            self.row_offset = self.cy - self.screen_rows + 1;
        }
        if self.rx < self.col_offset {
            self.col_offset = self.rx;
        }
        if self.rx >= self.col_offset + self.screen_cols {
            self.col_offset = self.rx - self.screen_cols + 1;
        }
    }

    fn refresh_screen(&mut self) {
        // Check for terminal resize
        if WINCH_RECEIVED.swap(false, Ordering::Relaxed) {
            let (rows, cols) = terminal_size();
            self.screen_rows = rows.saturating_sub(2).max(1);
            self.screen_cols = cols.max(1);
        }

        self.scroll();

        let mut ab = ABuf::new();

        ab.push_str(b"\x1b[?25l"); // hide cursor
        ab.push_str(b"\x1b[H"); // cursor home

        self.draw_rows(&mut ab);
        self.draw_status_bar(&mut ab);
        self.draw_message_bar(&mut ab);

        // Position cursor
        let cursor_row = self.cy - self.row_offset + 1;
        let cursor_col = self.rx - self.col_offset + 1;
        ab.push_str(b"\x1b[");
        push_usize(&mut ab, cursor_row);
        ab.push_byte(b';');
        push_usize(&mut ab, cursor_col);
        ab.push_byte(b'H');

        ab.push_str(b"\x1b[?25h"); // show cursor

        ab.flush();
        ab.clear();
    }

    fn draw_rows(&self, ab: &mut ABuf) {
        for y in 0..self.screen_rows {
            let file_row = y + self.row_offset;
            if file_row >= self.rows.len() {
                if self.rows.len() == 1
                    && self.rows[0].chars.is_empty()
                    && !self.modified
                    && y == self.screen_rows / 3
                {
                    // Welcome message
                    let mut welcome = String::from("edit -- version ");
                    welcome.push_str(VERSION);
                    let wlen = welcome.len().min(self.screen_cols);
                    let padding = (self.screen_cols.saturating_sub(wlen)) / 2;
                    if padding > 0 {
                        ab.push_byte(b'~');
                        for _ in 1..padding {
                            ab.push_byte(b' ');
                        }
                    } else {
                        ab.push_byte(b'~');
                    }
                    ab.push_str(welcome.as_bytes());
                } else {
                    ab.push_byte(b'~');
                }
            } else {
                let render = &self.rows[file_row].render;
                let start = self.col_offset.min(render.len());
                let end = (self.col_offset + self.screen_cols).min(render.len());
                if start < end {
                    ab.push_str(&render[start..end]);
                }
            }

            ab.push_str(b"\x1b[K"); // clear to end of line
            ab.push_str(b"\r\n");
        }
    }

    fn draw_status_bar(&self, ab: &mut ABuf) {
        ab.push_str(b"\x1b[7m"); // reverse video

        let fname = match &self.filename {
            Some(f) => f.as_str(),
            None => "[No Name]",
        };
        let modified_str = if self.modified { " (modified)" } else { "" };

        // Left side: filename + modified
        let mut left = String::new();
        // Truncate filename if needed
        let max_name = 20;
        if fname.len() > max_name {
            left.push_str(&fname[..max_name]);
        } else {
            left.push_str(fname);
        }
        left.push_str(modified_str);
        left.push_str(" - ");
        append_usize(&mut left, self.rows.len());
        left.push_str(" lines");

        // Right side: cursor position
        let mut right = String::new();
        append_usize(&mut right, self.cy + 1);
        right.push('/');
        append_usize(&mut right, self.rows.len());
        right.push_str(" col ");
        append_usize(&mut right, self.cx + 1);

        let left_len = left.len().min(self.screen_cols);
        ab.push_str(&left.as_bytes()[..left_len]);

        let remaining = self.screen_cols.saturating_sub(left_len);
        if right.len() < remaining {
            let pad = remaining - right.len();
            for _ in 0..pad {
                ab.push_byte(b' ');
            }
            ab.push_str(right.as_bytes());
        } else {
            for _ in 0..remaining {
                ab.push_byte(b' ');
            }
        }

        ab.push_str(b"\x1b[m"); // reset
        ab.push_str(b"\r\n");
    }

    fn draw_message_bar(&self, ab: &mut ABuf) {
        ab.push_str(b"\x1b[K"); // clear
        let msg_len = self.status_msg.len().min(self.screen_cols);
        if msg_len > 0 {
            ab.push_str(&self.status_msg.as_bytes()[..msg_len]);
        }
    }

    fn set_status_msg(&mut self, msg: &str) {
        self.status_msg = String::from(msg);
    }

    // -----------------------------------------------------------------------
    // Main input processing
    // -----------------------------------------------------------------------

    fn process_keypress(&mut self) -> bool {
        let key = self.read_key();

        match key {
            Key::Ctrl(b'q') => {
                if self.modified && self.quit_times > 0 {
                    self.quit_times -= 1;
                    let mut msg = String::from("WARNING: File has unsaved changes. Press Ctrl+Q ");
                    append_usize(&mut msg, self.quit_times as usize);
                    msg.push_str(" more time(s) to quit.");
                    self.set_status_msg(&msg);
                    return true;
                }
                return false;
            }
            Key::Ctrl(b's') => {
                self.save_file();
            }
            Key::Ctrl(b'f') => {
                self.find();
            }
            Key::Char(b'\r') | Key::Char(b'\n') => {
                self.insert_newline();
            }
            Key::Ctrl(b'h') => {
                self.delete_char();
            }
            Key::Delete => {
                // Move right then delete
                self.move_cursor(Key::ArrowRight);
                self.delete_char();
            }
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight => {
                self.move_cursor(key);
            }
            Key::PageUp => {
                self.cy = self.row_offset;
                for _ in 0..self.screen_rows {
                    self.move_cursor(Key::ArrowUp);
                }
            }
            Key::PageDown => {
                self.cy =
                    (self.row_offset + self.screen_rows - 1).min(self.rows.len().saturating_sub(1));
                for _ in 0..self.screen_rows {
                    self.move_cursor(Key::ArrowDown);
                }
            }
            Key::Home => {
                self.cx = 0;
            }
            Key::End => {
                if self.cy < self.rows.len() {
                    self.cx = self.rows[self.cy].chars.len();
                }
            }
            Key::Char(c) if c >= 32 && c < 127 => {
                self.insert_char(c);
            }
            Key::Char(b'\t') => {
                self.insert_char(b'\t');
            }
            Key::Escape | Key::None => {}
            _ => {}
        }

        // Reset quit counter on any key that isn't Ctrl+Q
        if key != Key::Ctrl(b'q') {
            self.quit_times = QUIT_TIMES;
        }

        true
    }

    // -----------------------------------------------------------------------
    // Run loop
    // -----------------------------------------------------------------------

    fn run(&mut self) {
        self.set_status_msg("HELP: Ctrl+S = save | Ctrl+Q = quit | Ctrl+F = find");

        loop {
            self.refresh_screen();
            if !self.process_keypress() {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal helpers
// ---------------------------------------------------------------------------

fn enable_raw_mode() -> syscall_lib::Termios {
    let orig = syscall_lib::tcgetattr(0).unwrap_or(syscall_lib::Termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: 0,
        c_lflag: 0,
        c_line: 0,
        c_cc: [0; syscall_lib::NCCS],
    });

    // Store for panic handler (single-threaded process, safe)
    unsafe {
        ORIG_TERMIOS = Some(orig);
    }

    let mut raw = orig;
    raw.c_iflag &= !(syscall_lib::ICRNL
        | syscall_lib::IXON
        | syscall_lib::BRKINT
        | syscall_lib::INPCK
        | syscall_lib::ISTRIP);
    raw.c_oflag &= !syscall_lib::OPOST;
    raw.c_cflag |= syscall_lib::CS8;
    raw.c_lflag &=
        !(syscall_lib::ICANON | syscall_lib::ECHO | syscall_lib::IEXTEN | syscall_lib::ISIG);

    let _ = syscall_lib::tcsetattr(0, &raw);
    orig
}

fn disable_raw_mode(orig: &syscall_lib::Termios) {
    let _ = syscall_lib::tcsetattr(0, orig);
}

fn terminal_size() -> (usize, usize) {
    match syscall_lib::get_window_size(0) {
        Ok((rows, cols)) => (rows as usize, cols as usize),
        Err(_) => (24, 80), // fallback
    }
}

// Signal restorer trampoline — must live in executable .text segment.
// Performs rt_sigreturn (syscall 15) so the kernel can restore context.
core::arch::global_asm!(
    ".global __sigrestorer",
    "__sigrestorer:",
    "mov rax, 15",
    "syscall",
);

unsafe extern "C" {
    fn __sigrestorer();
}

fn register_sigwinch_handler() {
    let sa = syscall_lib::SigAction {
        sa_handler: sigwinch_handler as *const () as u64,
        sa_flags: syscall_lib::SA_RESTORER,
        sa_restorer: __sigrestorer as *const () as u64,
        sa_mask: 0,
    };
    syscall_lib::rt_sigaction(
        syscall_lib::SIGWINCH as usize,
        &sa as *const syscall_lib::SigAction,
        core::ptr::null_mut(),
    );
}

extern "C" fn sigwinch_handler(_sig: i32) {
    WINCH_RECEIVED.store(true, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Utility helpers (no alloc needed for number formatting)
// ---------------------------------------------------------------------------

fn push_usize(ab: &mut ABuf, mut n: usize) {
    if n == 0 {
        ab.push_byte(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut pos = buf.len();
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    ab.push_str(&buf[pos..]);
}

fn append_usize(s: &mut String, mut n: usize) {
    if n == 0 {
        s.push('0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut pos = buf.len();
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    for &b in &buf[pos..] {
        s.push(b as char);
    }
}

fn find_substr(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    for i in 0..=(haystack.len() - needle.len()) {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Capture rsp FIRST, before any function calls corrupt the initial stack.
    // The SysV ABI places [argc, argv[0], argv[1], ..., null] at the entry rsp.
    let argc: usize;
    let argv_base: *const *const u8;
    unsafe {
        let rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
        let stack_ptr = rsp as *const u64;
        argc = *stack_ptr as usize;
        argv_base = stack_ptr.add(1) as *const *const u8;
    }

    register_sigwinch_handler();

    let mut editor = Editor::new();

    if argc >= 2 {
        // Read filename from argv[1]
        let filename_ptr = unsafe { *argv_base.add(1) };
        if !filename_ptr.is_null() {
            let mut len = 0;
            while unsafe { *filename_ptr.add(len) } != 0 {
                len += 1;
            }
            let filename_bytes = unsafe { core::slice::from_raw_parts(filename_ptr, len) };
            if let Ok(filename) = core::str::from_utf8(filename_bytes) {
                editor.open_file(filename);
            }
        }
    } else {
        // No filename — start with empty buffer
        editor.rows.push(Row::new(Vec::new()));
    }

    editor.run();

    // Restore terminal
    let orig = editor.orig_termios;
    disable_raw_mode(&orig);

    // Clear screen on exit
    syscall_lib::write(1, b"\x1b[2J\x1b[H");

    syscall_lib::exit(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Restore the actual original terminal settings saved at startup
    unsafe {
        if let Some(ref orig) = ORIG_TERMIOS {
            let _ = syscall_lib::tcsetattr(0, orig);
        }
    }
    syscall_lib::write(1, b"\r\npanic in edit\r\n");
    syscall_lib::exit(101)
}
