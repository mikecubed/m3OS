//! patch — apply a unified diff patch to files.
#![no_std]
#![no_main]

#[path = "common.rs"]
mod common;

use common::write_all;
use syscall_lib::{
    O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, rename,
    write, write_str,
};

syscall_lib::entry_point!(main);

const MAX_PATCH: usize = 65536;
const MAX_FILE: usize = 65536;
const MAX_OUT: usize = 65536;
const MAX_LINES: usize = 2048;
const MAX_HUNK_LINES: usize = 256;
const MAX_HUNKS: usize = 32;

// ---- Data structures ----

#[derive(Clone, Copy)]
struct HunkLine {
    kind: u8,   // b' ', b'+', b'-'
    start: u32, // byte offset in patch_buf
    len: u32,
}

impl HunkLine {
    const fn zero() -> Self {
        HunkLine {
            kind: 0,
            start: 0,
            len: 0,
        }
    }
}

struct Hunk {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    #[allow(dead_code)]
    new_count: u32,
    lines: [HunkLine; MAX_HUNK_LINES],
    line_count: u32,
}

impl Hunk {
    const fn zero() -> Self {
        Hunk {
            old_start: 0,
            old_count: 0,
            new_start: 0,
            new_count: 0,
            lines: [HunkLine::zero(); MAX_HUNK_LINES],
            line_count: 0,
        }
    }
}

struct FilePatch {
    old_path: [u8; 256],
    new_path: [u8; 256],
    hunks: [Hunk; MAX_HUNKS],
    hunk_count: u32,
}

impl FilePatch {
    const fn zero() -> Self {
        FilePatch {
            old_path: [0u8; 256],
            new_path: [0u8; 256],
            hunks: [const { Hunk::zero() }; MAX_HUNKS],
            hunk_count: 0,
        }
    }
}

// ---- Static buffers ----
// Using statics so we don't blow the stack with large arrays.

static mut PATCH_BUF: [u8; MAX_PATCH] = [0u8; MAX_PATCH];
static mut FILE_BUF: [u8; MAX_FILE] = [0u8; MAX_FILE];
static mut OUT_BUF: [u8; MAX_OUT] = [0u8; MAX_OUT];
static mut PATCH_LINE_STARTS: [u32; MAX_LINES] = [0u32; MAX_LINES];
static mut FILE_LINE_STARTS: [u32; MAX_LINES] = [0u32; MAX_LINES];

// ---- Helpers ----

fn parse_u32_bytes(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut v = 0u32;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.wrapping_mul(10).wrapping_add((b - b'0') as u32);
    }
    Some(v)
}

/// Read stdin into buf. Returns bytes read or None on error/overflow.
fn read_all_stdin(buf: &mut [u8]) -> Option<usize> {
    let mut fill = 0usize;
    loop {
        let space = buf.len() - fill;
        if space == 0 {
            return None;
        }
        let n = read(0, &mut buf[fill..]);
        if n == 0 {
            break;
        }
        if n < 0 {
            return None;
        }
        fill += n as usize;
    }
    Some(fill)
}

/// Read a file into buf. Returns bytes read or None on error/overflow.
fn read_file(path: &[u8], buf: &mut [u8]) -> Option<usize> {
    let fd = open(path, O_RDONLY, 0);
    if fd < 0 {
        return None;
    }
    let fd = fd as i32;
    let mut fill = 0usize;
    loop {
        let space = buf.len() - fill;
        if space == 0 {
            close(fd);
            return None;
        }
        let n = read(fd, &mut buf[fill..]);
        if n == 0 {
            break;
        }
        if n < 0 {
            close(fd);
            return None;
        }
        fill += n as usize;
    }
    close(fd);
    Some(fill)
}

/// Collect line start offsets. Returns number of lines.
fn collect_lines(buf: &[u8], fill: usize, starts: &mut [u32; MAX_LINES]) -> usize {
    if fill == 0 {
        return 0;
    }
    let mut count = 0usize;
    if count < MAX_LINES {
        starts[count] = 0;
        count += 1;
    }
    for (i, &byte) in buf[..fill].iter().enumerate() {
        if byte == b'\n' {
            let next = i + 1;
            if next < fill && count < MAX_LINES {
                starts[count] = next as u32;
                count += 1;
            }
        }
    }
    count
}

/// Get the byte slice for line `i` from the buffer (including newline if present).
fn get_line<'a>(
    buf: &'a [u8],
    fill: usize,
    starts: &[u32; MAX_LINES],
    count: usize,
    i: usize,
) -> &'a [u8] {
    if i >= count {
        return b"";
    }
    let start = starts[i] as usize;
    let end = if i + 1 < count {
        starts[i + 1] as usize
    } else {
        fill
    };
    &buf[start..end]
}

/// Strip leading `n` path components from `path`.
fn strip_components(path: &[u8], n: u32) -> &[u8] {
    let mut p = path;
    // Strip leading slashes
    while p.first() == Some(&b'/') {
        p = &p[1..];
    }
    let mut count = n;
    while count > 0 {
        if let Some(pos) = p.iter().position(|&b| b == b'/') {
            p = &p[pos + 1..];
            while p.first() == Some(&b'/') {
                p = &p[1..];
            }
        } else {
            return b"";
        }
        count -= 1;
    }
    p
}

/// Copy path bytes (up to first whitespace/tab/newline) into a [u8; 256].
/// Returns the number of bytes copied (not including NUL).
fn copy_path(src: &[u8], dst: &mut [u8; 256]) -> usize {
    let mut len = 0usize;
    for &b in src {
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
            break;
        }
        if len < 255 {
            dst[len] = b;
            len += 1;
        }
    }
    dst[len] = 0;
    len
}

/// Write `data` to file at NUL-terminated path. Returns true on success.
fn write_file(path: &[u8], data: &[u8]) -> bool {
    let fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644);
    if fd < 0 {
        return false;
    }
    let fd = fd as i32;
    let mut off = 0usize;
    while off < data.len() {
        let w = write(fd, &data[off..]);
        if w <= 0 {
            close(fd);
            return false;
        }
        off += w as usize;
    }
    close(fd);
    true
}

/// Build a NUL-terminated path in a fixed buffer. Returns length of path (not NUL).
fn make_nul_path(src: &[u8], buf: &mut [u8; 256]) -> Option<usize> {
    // src may or may not be NUL-terminated; stop at NUL or end
    let len = src.iter().position(|&b| b == 0).unwrap_or(src.len());
    if len == 0 || len >= 255 {
        return None;
    }
    buf[..len].copy_from_slice(&src[..len]);
    buf[len] = 0;
    Some(len)
}

// ---- Patch parsing ----

/// Parse the patch buffer into a FilePatch structure.
/// Returns true on success.
fn parse_patch(
    buf: &[u8],
    fill: usize,
    starts: &[u32; MAX_LINES],
    line_count: usize,
    fp: &mut FilePatch,
    strip: u32,
) -> bool {
    let mut li = 0usize;

    // Find `--- ` header
    while li < line_count {
        let line = get_line(buf, fill, starts, line_count, li);
        if line.starts_with(b"--- ") {
            break;
        }
        li += 1;
    }
    if li >= line_count {
        return false;
    }

    // Parse old path
    {
        let line = get_line(buf, fill, starts, line_count, li);
        let rest = &line[4..]; // skip "--- "
        // Skip "a/" or "b/" prefix if present (common in git diffs)
        let rest = if rest.starts_with(b"a/") || rest.starts_with(b"b/") {
            &rest[2..]
        } else {
            rest
        };
        let rest = strip_components(rest, strip);
        copy_path(rest, &mut fp.old_path);
    }
    li += 1;

    // Expect `+++ ` line
    if li >= line_count {
        return false;
    }
    {
        let line = get_line(buf, fill, starts, line_count, li);
        if !line.starts_with(b"+++ ") {
            return false;
        }
        let rest = &line[4..];
        let rest = if rest.starts_with(b"a/") || rest.starts_with(b"b/") {
            &rest[2..]
        } else {
            rest
        };
        let rest = strip_components(rest, strip);
        copy_path(rest, &mut fp.new_path);
    }
    li += 1;

    // Parse hunks
    while li < line_count && (fp.hunk_count as usize) < MAX_HUNKS {
        let line = get_line(buf, fill, starts, line_count, li);
        if !line.starts_with(b"@@ ") {
            li += 1;
            continue;
        }

        // Parse `@@ -old_start[,old_count] +new_start[,new_count] @@`
        let inner = &line[3..];
        // Find `-`
        let Some(minus_pos) = inner.iter().position(|&b| b == b'-') else {
            li += 1;
            continue;
        };
        let after_minus = &inner[minus_pos + 1..];
        // Parse old_start
        let comma_or_space = after_minus
            .iter()
            .position(|&b| b == b',' || b == b' ')
            .unwrap_or(after_minus.len());
        let old_start = parse_u32_bytes(&after_minus[..comma_or_space]).unwrap_or(0);
        let old_count = if after_minus[comma_or_space..].starts_with(b",") {
            let after_comma = &after_minus[comma_or_space + 1..];
            let end = after_comma
                .iter()
                .position(|&b| b == b' ')
                .unwrap_or(after_comma.len());
            parse_u32_bytes(&after_comma[..end]).unwrap_or(1)
        } else {
            1
        };

        // Find `+`
        let Some(plus_pos) = inner.iter().position(|&b| b == b'+') else {
            li += 1;
            continue;
        };
        let after_plus = &inner[plus_pos + 1..];
        let comma_or_space2 = after_plus
            .iter()
            .position(|&b| b == b',' || b == b' ')
            .unwrap_or(after_plus.len());
        let new_start = parse_u32_bytes(&after_plus[..comma_or_space2]).unwrap_or(0);
        let new_count = if after_plus[comma_or_space2..].starts_with(b",") {
            let after_comma = &after_plus[comma_or_space2 + 1..];
            let end = after_comma
                .iter()
                .position(|&b| b == b' ')
                .unwrap_or(after_comma.len());
            parse_u32_bytes(&after_comma[..end]).unwrap_or(1)
        } else {
            1
        };

        let hidx = fp.hunk_count as usize;
        fp.hunks[hidx].old_start = old_start;
        fp.hunks[hidx].old_count = old_count;
        fp.hunks[hidx].new_start = new_start;
        fp.hunks[hidx].new_count = new_count;
        fp.hunks[hidx].line_count = 0;

        li += 1;

        // Read hunk body
        while li < line_count {
            let hline = get_line(buf, fill, starts, line_count, li);
            if hline.is_empty() {
                li += 1;
                break;
            }
            let first = hline[0];
            if first == b'@' || first == b'-' && hline.starts_with(b"--- ") {
                break;
            }
            if first == b'\\' {
                // "\ No newline at end of file" — skip
                li += 1;
                continue;
            }
            if first == b' ' || first == b'+' || first == b'-' {
                let lc = fp.hunks[hidx].line_count as usize;
                if lc < MAX_HUNK_LINES {
                    let line_start = starts[li];
                    // line content without leading kind byte and without trailing newline
                    let content_start = line_start + 1;
                    let content_len = if hline.len() > 1 {
                        let raw_len = hline.len() - 1; // skip kind char
                        if hline[hline.len() - 1] == b'\n' {
                            raw_len - 1
                        } else {
                            raw_len
                        }
                    } else {
                        0
                    };
                    fp.hunks[hidx].lines[lc] = HunkLine {
                        kind: first,
                        start: content_start,
                        len: content_len as u32,
                    };
                    fp.hunks[hidx].line_count += 1;
                }
            }
            li += 1;
        }

        fp.hunk_count += 1;
    }

    fp.hunk_count > 0 || !fp.old_path.starts_with(b"\0")
}

// ---- Patch application ----

fn apply_patch(
    fp: &FilePatch,
    file_buf: &[u8],
    file_fill: usize,
    file_starts: &[u32; MAX_LINES],
    file_line_count: usize,
    patch_buf: &[u8],
    out_buf: &mut [u8; MAX_OUT],
) -> Option<usize> {
    let mut out_pos = 0usize;
    let mut src_line: usize = 0; // 0-based current position in source file

    for hi in 0..(fp.hunk_count as usize) {
        let hunk = &fp.hunks[hi];
        // old_start is 1-based; convert to 0-based
        let hunk_start = if hunk.old_start > 0 {
            (hunk.old_start - 1) as usize
        } else {
            0
        };

        // Copy context lines from source up to hunk start
        while src_line < hunk_start && src_line < file_line_count {
            let line = get_line(file_buf, file_fill, file_starts, file_line_count, src_line);
            if out_pos + line.len() > out_buf.len() {
                return None;
            }
            out_buf[out_pos..out_pos + line.len()].copy_from_slice(line);
            out_pos += line.len();
            src_line += 1;
        }

        // Process hunk lines
        for li in 0..(hunk.line_count as usize) {
            let hl = &hunk.lines[li];
            match hl.kind {
                b' ' => {
                    // Context line: copy from source, advance src_line
                    let line =
                        get_line(file_buf, file_fill, file_starts, file_line_count, src_line);
                    if out_pos + line.len() > out_buf.len() {
                        return None;
                    }
                    out_buf[out_pos..out_pos + line.len()].copy_from_slice(line);
                    out_pos += line.len();
                    src_line += 1;
                }
                b'-' => {
                    // Removed line: skip in source
                    src_line += 1;
                }
                b'+' => {
                    // Added line: emit from patch
                    let start = hl.start as usize;
                    let len = hl.len as usize;
                    if start + len > patch_buf.len() {
                        return None;
                    }
                    let content = &patch_buf[start..start + len];
                    if out_pos + content.len() + 1 > out_buf.len() {
                        return None;
                    }
                    out_buf[out_pos..out_pos + content.len()].copy_from_slice(content);
                    out_pos += content.len();
                    // Add newline after added line
                    out_buf[out_pos] = b'\n';
                    out_pos += 1;
                }
                _ => {}
            }
        }
    }

    // Copy remaining source lines after last hunk
    while src_line < file_line_count {
        let line = get_line(file_buf, file_fill, file_starts, file_line_count, src_line);
        if out_pos + line.len() > out_buf.len() {
            return None;
        }
        out_buf[out_pos..out_pos + line.len()].copy_from_slice(line);
        out_pos += line.len();
        src_line += 1;
    }

    Some(out_pos)
}

fn nul_len(buf: &[u8]) -> usize {
    buf.iter().position(|&b| b == 0).unwrap_or(buf.len())
}

#[allow(clippy::deref_addrof)]
fn main(args: &[&str]) -> i32 {
    let mut strip: u32 = 1;
    let mut argi = 1usize;

    // Parse -pN argument
    while argi < args.len() {
        let arg = args[argi].as_bytes();
        if arg.len() >= 2 && arg[0] == b'-' && arg[1] == b'p' {
            if arg.len() > 2 {
                strip = common::parse_u32_bytes(&arg[2..]).unwrap_or(1);
            } else {
                argi += 1;
                if argi < args.len() {
                    strip = common::parse_u32_bytes(args[argi].as_bytes()).unwrap_or(1);
                }
            }
        }
        argi += 1;
    }

    // Read patch from stdin
    let patch_fill = unsafe {
        match read_all_stdin(&mut *(&raw mut PATCH_BUF)) {
            Some(n) => n,
            None => {
                write_str(STDERR_FILENO, "patch: failed to read patch from stdin\n");
                return 1;
            }
        }
    };

    // Collect patch lines
    let patch_line_count = unsafe {
        collect_lines(
            &*(&raw const PATCH_BUF),
            patch_fill,
            &mut *(&raw mut PATCH_LINE_STARTS),
        )
    };

    // Parse patch
    let mut fp = FilePatch::zero();
    let ok = unsafe {
        parse_patch(
            &*(&raw const PATCH_BUF),
            patch_fill,
            &*(&raw const PATCH_LINE_STARTS),
            patch_line_count,
            &mut fp,
            strip,
        )
    };
    if !ok {
        write_str(STDERR_FILENO, "patch: could not parse patch\n");
        return 1;
    }

    // Determine target path (old_path, unless it's /dev/null)
    let old_path_len = nul_len(&fp.old_path);
    if old_path_len == 0 {
        write_str(STDERR_FILENO, "patch: no target file\n");
        return 1;
    }

    let is_dev_null = &fp.old_path[..old_path_len] == b"/dev/null";

    // Build NUL-terminated target path
    let mut target_path = [0u8; 256];
    if is_dev_null {
        // Creating a new file — use new_path
        let np_len = nul_len(&fp.new_path);
        if np_len == 0 {
            write_str(STDERR_FILENO, "patch: no new path for /dev/null patch\n");
            return 1;
        }
        target_path[..np_len].copy_from_slice(&fp.new_path[..np_len]);
        target_path[np_len] = 0;
    } else {
        let Some(len) = make_nul_path(&fp.old_path[..old_path_len], &mut target_path) else {
            write_str(STDERR_FILENO, "patch: path too long\n");
            return 1;
        };
        target_path[len] = 0;
    }

    // Read target file (if not /dev/null)
    let file_fill = if is_dev_null {
        0
    } else {
        unsafe {
            match read_file(&target_path, &mut *(&raw mut FILE_BUF)) {
                Some(n) => n,
                None => {
                    write_str(STDERR_FILENO, "patch: cannot read target file\n");
                    return 1;
                }
            }
        }
    };

    // Collect file lines
    let file_line_count = unsafe {
        collect_lines(
            &*(&raw const FILE_BUF),
            file_fill,
            &mut *(&raw mut FILE_LINE_STARTS),
        )
    };

    // Apply patch
    let out_len = unsafe {
        match apply_patch(
            &fp,
            &*(&raw const FILE_BUF),
            file_fill,
            &*(&raw const FILE_LINE_STARTS),
            file_line_count,
            &*(&raw const PATCH_BUF),
            &mut *(&raw mut OUT_BUF),
        ) {
            Some(n) => n,
            None => {
                write_str(STDERR_FILENO, "patch: output too large or apply failed\n");
                return 1;
            }
        }
    };

    // Write output to a temp path then rename
    let mut tmp_path = [0u8; 256];
    // Build temp path: target + ".patch_tmp\0"
    let tplen = nul_len(&target_path);
    const SUFFIX: &[u8] = b".patch_tmp";
    if tplen + SUFFIX.len() + 1 > 255 {
        // Path too long for temp: write directly
        // SAFETY: OUT_BUF is a static array; out_len <= MAX_OUT
        let out_data =
            unsafe { core::slice::from_raw_parts(&raw const OUT_BUF as *const u8, out_len) };
        if !write_file(&target_path, out_data) {
            write_str(STDERR_FILENO, "patch: cannot write output file\n");
            return 1;
        }
    } else {
        tmp_path[..tplen].copy_from_slice(&target_path[..tplen]);
        tmp_path[tplen..tplen + SUFFIX.len()].copy_from_slice(SUFFIX);
        tmp_path[tplen + SUFFIX.len()] = 0;

        // SAFETY: OUT_BUF is a static array; out_len <= MAX_OUT
        let out_data =
            unsafe { core::slice::from_raw_parts(&raw const OUT_BUF as *const u8, out_len) };
        if !write_file(&tmp_path[..=tplen + SUFFIX.len()], out_data) {
            write_str(STDERR_FILENO, "patch: cannot write temp file\n");
            return 1;
        }

        // Rename temp -> target
        if rename(&tmp_path[..=tplen + SUFFIX.len()], &target_path[..=tplen]) < 0 {
            // Fallback: write directly
            if !write_file(&target_path, out_data) {
                write_str(STDERR_FILENO, "patch: cannot rename output file\n");
                return 1;
            }
        }
    }

    write_all(STDOUT_FILENO, b"patching file ");
    write_all(STDOUT_FILENO, &target_path[..tplen]);
    write_all(STDOUT_FILENO, b"\n");

    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
