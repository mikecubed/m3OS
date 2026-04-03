//! m3OS interactive shell — userspace (Phase 20).
//!
//! Minimal `no_std` shell: reads lines from stdin, tokenizes, handles
//! builtins (`cd`, `exit`), fork-exec-wait for external commands,
//! two-stage pipes, I/O redirection, PATH resolution.
#![no_std]
#![no_main]

use syscall_lib::{
    O_APPEND, O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO,
    chdir, close, dup2, execve, exit, fork, open, pipe, read, waitpid, write, write_str, write_u64,
};

const MAX_LINE: usize = 256;
const MAX_TOKENS: usize = 32;
const MAX_PATH: usize = 128;

/// PATH directories to search for commands.
const PATH_DIRS: [&[u8]; 3] = [b"/bin", b"/sbin", b"/usr/bin"];

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    main_loop();
}

fn main_loop() -> ! {
    loop {
        write_str(STDOUT_FILENO, "$ ");

        let mut line_buf = [0u8; MAX_LINE];
        let len = read_line(&mut line_buf);
        if len == 0 {
            continue;
        }

        // Parse and execute the command line.
        execute_line(&line_buf[..len]);
    }
}

/// Read a line from stdin, handling backspace and character echo.
/// Returns the number of valid bytes (excluding null terminator).
fn read_line(buf: &mut [u8; MAX_LINE]) -> usize {
    let mut pos = 0usize;
    loop {
        let mut byte = [0u8; 1];
        let n = read(STDIN_FILENO, &mut byte);
        if n <= 0 {
            // EOF or error — return what we have.
            if pos > 0 {
                return pos;
            }
            // Yield CPU briefly (~10ms) and retry.
            syscall_lib::nanosleep(1);
            continue;
        }

        let ch = byte[0];

        if ch == b'\n' || ch == b'\r' {
            // Echo newline (stdin_feeder sends \n but doesn't echo it).
            let _ = write(STDOUT_FILENO, b"\n");
            return pos;
        }

        if ch == 0x7f || ch == 0x08 {
            // Backspace — erase last character.
            if pos > 0 {
                pos -= 1;
                // Move cursor back, overwrite with space, move back again.
                let _ = write(STDOUT_FILENO, b"\x08 \x08");
            }
            continue;
        }

        // Ctrl-C: discard line, print new prompt.
        if ch == 0x03 {
            let _ = write(STDOUT_FILENO, b"^C\n");
            return 0;
        }

        if pos < MAX_LINE - 1 && ch >= 0x20 {
            buf[pos] = ch;
            pos += 1;
            // Echo the character.
            let _ = write(STDOUT_FILENO, &byte);
        }
    }
}

/// Maximum pipeline stages supported.
const MAX_PIPELINE_STAGES: usize = 8;

/// Parse a command line and execute it.
fn execute_line(line: &[u8]) {
    // Skip leading whitespace.
    let line = trim(line);
    if line.is_empty() {
        return;
    }

    // Check for pipe — handle N-stage pipelines directly.
    if find_byte(line, b'|').is_some() {
        execute_pipeline_n(line);
        return;
    }

    // Tokenize.
    let mut token_storage = [[0u8; MAX_PATH]; MAX_TOKENS];
    let mut token_lens = [0usize; MAX_TOKENS];
    let argc = tokenize(line, &mut token_storage, &mut token_lens);
    if argc == 0 {
        return;
    }

    // Check for I/O redirection.
    let mut redir_out: Option<(&[u8], u64)> = None;
    let mut redir_in: Option<&[u8]> = None;
    let mut cmd_argc = argc;

    let mut i = 0;
    while i < argc {
        let tok = &token_storage[i][..token_lens[i]];
        if tok == b">" && i + 1 < argc {
            redir_out = Some((
                &token_storage[i + 1][..token_lens[i + 1]],
                O_WRONLY | O_CREAT | O_TRUNC,
            ));
            cmd_argc = i;
            break;
        } else if tok == b">>" && i + 1 < argc {
            redir_out = Some((
                &token_storage[i + 1][..token_lens[i + 1]],
                O_WRONLY | O_CREAT | O_APPEND,
            ));
            cmd_argc = i;
            break;
        } else if tok == b"<" && i + 1 < argc {
            redir_in = Some(&token_storage[i + 1][..token_lens[i + 1]]);
            cmd_argc = i;
            break;
        }
        i += 1;
    }

    if cmd_argc == 0 {
        return;
    }

    let cmd = &token_storage[0][..token_lens[0]];

    // Builtins.
    if cmd == b"cd" {
        let path = if cmd_argc > 1 {
            &token_storage[1][..token_lens[1]]
        } else {
            b"/\0" as &[u8]
        };
        // Ensure null-terminated.
        let mut path_buf = [0u8; MAX_PATH];
        let plen = copy_min(path, &mut path_buf);
        path_buf[plen] = 0;
        let ret = chdir(&path_buf[..plen + 1]);
        if ret < 0 {
            write_str(STDOUT_FILENO, "cd: no such directory\n");
        }
        return;
    }

    if cmd == b"exit" {
        exit(0);
    }

    // External command: fork + exec.
    execute_external(&token_storage, &token_lens, cmd_argc, redir_out, redir_in);
}

/// Execute an N-stage pipeline: cmd1 | cmd2 | ... | cmdN.
/// Uses the classic prev-read approach: creates each pipe just before forking
/// the next stage, so fd numbering is simple and correct.
fn execute_pipeline_n(line: &[u8]) {
    // Collect command segments by splitting on '|'.
    let mut segments: [&[u8]; MAX_PIPELINE_STAGES] = [b""; MAX_PIPELINE_STAGES];
    let mut n_segs = 0usize;
    let mut start = 0usize;
    let mut raw_count = 0usize;
    let mut i = 0usize;
    while i <= line.len() {
        let at_end = i == line.len();
        let at_pipe = !at_end && line[i] == b'|';
        if at_end || at_pipe {
            let seg = trim(&line[start..i]);
            raw_count += 1;
            if seg.is_empty() {
                write_str(STDERR_FILENO, "sh: syntax error: empty pipeline segment\n");
                return;
            }
            if raw_count > MAX_PIPELINE_STAGES {
                write_str(STDERR_FILENO, "sh: too many pipeline stages\n");
                return;
            }
            segments[n_segs] = seg;
            n_segs += 1;
            start = i + 1;
        }
        i += 1;
    }

    if n_segs < 2 {
        // Degenerate — just run the single segment.
        if n_segs == 1 {
            exec_simple_command(segments[0]);
        }
        return;
    }

    // Classic "prev_read" pipeline:
    //   - For stage m, create a new pipe[m] if m < n_segs-1.
    //   - Fork child: wire prev_read → stdin, cur_write → stdout.
    //   - Parent closes write end and old read end; keeps new read end.
    let mut pids: [i32; MAX_PIPELINE_STAGES] = [-1i32; MAX_PIPELINE_STAGES];
    let mut prev_read: i32 = -1; // read end of the pipe that feeds this stage

    let mut m = 0usize;
    while m < n_segs {
        // Create a new pipe for this stage's output (unless this is the last stage).
        let cur_read: i32;
        let cur_write: i32;
        if m < n_segs - 1 {
            let mut fds = [0i32; 2];
            if pipe(&mut fds) < 0 {
                write_str(STDERR_FILENO, "sh: pipe failed\n");
                // Reap any children already started.
                let mut k = 0usize;
                while k < m {
                    if pids[k] > 0 {
                        let mut st = 0i32;
                        waitpid(pids[k], &mut st, 0);
                    }
                    k += 1;
                }
                if prev_read >= 0 {
                    close(prev_read);
                }
                return;
            }
            cur_read = fds[0];
            cur_write = fds[1];
        } else {
            cur_read = -1;
            cur_write = -1;
        }

        let pid = fork();
        if pid == 0 {
            // Child: set up stdin from previous pipe.
            if prev_read >= 0 {
                dup2(prev_read, STDIN_FILENO);
                close(prev_read);
            }
            // Set up stdout to next pipe.
            if cur_write >= 0 {
                dup2(cur_write, STDOUT_FILENO);
                close(cur_write);
            }
            // Close the read end of the new pipe (child doesn't need it).
            if cur_read >= 0 {
                close(cur_read);
            }
            exec_simple_command(segments[m]);
            exit(127);
        }

        // Parent: close write end of new pipe and the old read end.
        if cur_write >= 0 {
            close(cur_write);
        }
        if prev_read >= 0 {
            close(prev_read);
        }

        if pid < 0 {
            write_str(STDERR_FILENO, "sh: fork failed\n");
            if cur_read >= 0 {
                close(cur_read);
            }
            // Reap all children already forked.
            let mut j = 0usize;
            while j < m {
                if pids[j] > 0 {
                    let mut st = 0i32;
                    waitpid(pids[j], &mut st, 0);
                }
                j += 1;
            }
            return;
        }

        pids[m] = pid as i32;
        prev_read = cur_read;
        m += 1;
    }

    // Close the last read end (should be -1 for the last stage).
    if prev_read >= 0 {
        close(prev_read);
    }

    // Wait for all children.
    let mut r = 0usize;
    while r < n_segs {
        if pids[r] > 0 {
            let mut status: i32 = 0;
            waitpid(pids[r], &mut status, 0);
        }
        r += 1;
    }
}

/// Tokenize and exec a simple command (used inside pipeline children).
fn exec_simple_command(line: &[u8]) {
    let mut token_storage = [[0u8; MAX_PATH]; MAX_TOKENS];
    let mut token_lens = [0usize; MAX_TOKENS];
    let argc = tokenize(line, &mut token_storage, &mut token_lens);
    if argc == 0 {
        return;
    }

    let cmd = &token_storage[0][..token_lens[0]];
    exec_with_path_resolution(cmd, &token_storage, &token_lens, argc);
}

/// Execute an external command with optional I/O redirection.
fn execute_external(
    token_storage: &[[u8; MAX_PATH]; MAX_TOKENS],
    token_lens: &[usize; MAX_TOKENS],
    argc: usize,
    redir_out: Option<(&[u8], u64)>,
    redir_in: Option<&[u8]>,
) {
    let pid = fork();
    if pid as u64 == u64::MAX {
        write_str(STDERR_FILENO, "sh: fork failed\n");
        return;
    }

    if pid == 0 {
        // Child process.

        // Handle output redirection.
        if let Some((path, flags)) = redir_out {
            let mut path_buf = [0u8; MAX_PATH];
            let plen = copy_min(path, &mut path_buf);
            path_buf[plen] = 0;
            let fd = open(&path_buf[..plen + 1], flags, 0o644);
            if fd < 0 {
                write_str(STDERR_FILENO, "sh: cannot open output file\n");
                exit(1);
            }
            dup2(fd as i32, STDOUT_FILENO);
            close(fd as i32);
        }

        // Handle input redirection.
        if let Some(path) = redir_in {
            let mut path_buf = [0u8; MAX_PATH];
            let plen = copy_min(path, &mut path_buf);
            path_buf[plen] = 0;
            let fd = open(&path_buf[..plen + 1], O_RDONLY, 0);
            if fd < 0 {
                write_str(STDERR_FILENO, "sh: cannot open input file\n");
                exit(1);
            }
            dup2(fd as i32, STDIN_FILENO);
            close(fd as i32);
        }

        let cmd = &token_storage[0][..token_lens[0]];
        exec_with_path_resolution(cmd, token_storage, token_lens, argc);
        // exec_with_path_resolution calls exit(127) on failure.
        exit(127);
    }

    // Parent: wait for the child.
    let mut status: i32 = 0;
    waitpid(pid as i32, &mut status, 0);

    // Print non-zero exit status.
    let exit_code = (status >> 8) & 0xff;
    if exit_code != 0 && status & 0x7f == 0 {
        // Only print for normal exits (not signals) with non-zero code.
        write_str(STDOUT_FILENO, "exit ");
        write_u64(STDOUT_FILENO, exit_code as u64);
        write_str(STDOUT_FILENO, "\n");
    }
}

/// Try to exec `cmd` by resolving it against PATH directories.
/// If cmd starts with `/`, try it directly. Otherwise try each PATH dir.
/// Also tries appending `.elf` for ramdisk backward compatibility.
fn exec_with_path_resolution(
    cmd: &[u8],
    token_storage: &[[u8; MAX_PATH]; MAX_TOKENS],
    token_lens: &[usize; MAX_TOKENS],
    argc: usize,
) {
    // Build argv array: each entry is a pointer to a null-terminated string.
    let mut argv_bufs = [[0u8; MAX_PATH]; MAX_TOKENS];
    let mut argv_ptrs = [core::ptr::null::<u8>(); MAX_TOKENS + 1];
    for i in 0..argc {
        let len = copy_min(&token_storage[i][..token_lens[i]], &mut argv_bufs[i]);
        argv_bufs[i][len] = 0;
        argv_ptrs[i] = argv_bufs[i].as_ptr();
    }
    argv_ptrs[argc] = core::ptr::null();

    // envp: inherit from process (pass minimal env).
    let env_path: &[u8] = b"PATH=/bin:/sbin:/usr/bin\0";
    let env_home: &[u8] = b"HOME=/\0";
    let env_term: &[u8] = b"TERM=m3os\0";
    let envp: [*const u8; 4] = [
        env_path.as_ptr(),
        env_home.as_ptr(),
        env_term.as_ptr(),
        core::ptr::null(),
    ];

    let cmd_len = cmd_byte_len(cmd);

    if cmd_len > 0 && cmd[0] == b'/' {
        // Absolute path — try directly.
        let mut path_buf = [0u8; MAX_PATH];
        let plen = copy_min(&cmd[..cmd_len], &mut path_buf);
        path_buf[plen] = 0;
        execve(&path_buf[..plen + 1], &argv_ptrs[..argc + 1], &envp);
        // Also try with .elf suffix.
        if plen + 4 < MAX_PATH {
            path_buf[plen] = b'.';
            path_buf[plen + 1] = b'e';
            path_buf[plen + 2] = b'l';
            path_buf[plen + 3] = b'f';
            path_buf[plen + 4] = 0;
            execve(&path_buf[..plen + 5], &argv_ptrs[..argc + 1], &envp);
        }
    } else {
        // Relative: search PATH.
        for dir in &PATH_DIRS {
            let dir_len = dir.len();
            let mut path_buf = [0u8; MAX_PATH];

            // dir/cmd
            if dir_len + 1 + cmd_len < MAX_PATH {
                copy_bytes(dir, &mut path_buf, dir_len);
                path_buf[dir_len] = b'/';
                copy_bytes(&cmd[..cmd_len], &mut path_buf[dir_len + 1..], cmd_len);
                let total = dir_len + 1 + cmd_len;
                path_buf[total] = 0;
                execve(&path_buf[..total + 1], &argv_ptrs[..argc + 1], &envp);
            }

            // dir/cmd.elf
            if dir_len + 1 + cmd_len + 4 < MAX_PATH {
                copy_bytes(dir, &mut path_buf, dir_len);
                path_buf[dir_len] = b'/';
                copy_bytes(&cmd[..cmd_len], &mut path_buf[dir_len + 1..], cmd_len);
                let base = dir_len + 1 + cmd_len;
                path_buf[base] = b'.';
                path_buf[base + 1] = b'e';
                path_buf[base + 2] = b'l';
                path_buf[base + 3] = b'f';
                path_buf[base + 4] = 0;
                execve(&path_buf[..base + 5], &argv_ptrs[..argc + 1], &envp);
            }
        }
    }

    // All attempts failed.
    write_str(STDERR_FILENO, "command not found: ");
    let _ = write(STDERR_FILENO, &cmd[..cmd_len]);
    write_str(STDERR_FILENO, "\n");
    exit(127);
}

// ===========================================================================
// Tokenizer
// ===========================================================================

/// Split `line` into whitespace-separated tokens.
/// Respects single-quoted strings. Returns the number of tokens.
fn tokenize(
    line: &[u8],
    storage: &mut [[u8; MAX_PATH]; MAX_TOKENS],
    lens: &mut [usize; MAX_TOKENS],
) -> usize {
    let mut count = 0usize;
    let mut i = 0usize;
    let n = cmd_byte_len(line);

    while i < n && count < MAX_TOKENS {
        // Skip whitespace.
        while i < n && (line[i] == b' ' || line[i] == b'\t') {
            i += 1;
        }
        if i >= n {
            break;
        }

        let mut pos = 0usize;

        if line[i] == b'\'' {
            // Quoted string — copy until closing quote.
            i += 1;
            while i < n && line[i] != b'\'' && pos < MAX_PATH - 1 {
                storage[count][pos] = line[i];
                pos += 1;
                i += 1;
            }
            if i < n && line[i] == b'\'' {
                i += 1;
            }
        } else {
            // Unquoted token — check for special single-char tokens.
            let ch = line[i];
            if ch == b'|' || ch == b'<' {
                storage[count][0] = ch;
                pos = 1;
                i += 1;
            } else if ch == b'>' {
                if i + 1 < n && line[i + 1] == b'>' {
                    storage[count][0] = b'>';
                    storage[count][1] = b'>';
                    pos = 2;
                    i += 2;
                } else {
                    storage[count][0] = b'>';
                    pos = 1;
                    i += 1;
                }
            } else {
                // Regular word.
                while i < n
                    && line[i] != b' '
                    && line[i] != b'\t'
                    && line[i] != b'|'
                    && line[i] != b'>'
                    && line[i] != b'<'
                    && pos < MAX_PATH - 1
                {
                    storage[count][pos] = line[i];
                    pos += 1;
                    i += 1;
                }
            }
        }

        if pos > 0 {
            lens[count] = pos;
            count += 1;
        }
    }

    count
}

// ===========================================================================
// Utility functions (no alloc)
// ===========================================================================

fn trim(s: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && (s[start] == b' ' || s[start] == b'\t') {
        start += 1;
    }
    while end > start && (s[end - 1] == b' ' || s[end - 1] == b'\t' || s[end - 1] == 0) {
        end -= 1;
    }
    &s[start..end]
}

fn find_byte(s: &[u8], b: u8) -> Option<usize> {
    let mut i = 0;
    while i < s.len() {
        if s[i] == b {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn cmd_byte_len(s: &[u8]) -> usize {
    let mut i = 0;
    while i < s.len() && s[i] != 0 {
        i += 1;
    }
    i
}

fn copy_min(src: &[u8], dst: &mut [u8]) -> usize {
    let len = if src.len() < dst.len() {
        src.len()
    } else {
        dst.len()
    };
    let mut i = 0;
    while i < len {
        dst[i] = src[i];
        i += 1;
    }
    len
}

fn copy_bytes(src: &[u8], dst: &mut [u8], count: usize) {
    let mut i = 0;
    while i < count && i < src.len() && i < dst.len() {
        dst[i] = src[i];
        i += 1;
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDERR_FILENO, "sh: PANIC\n");
    exit(101)
}
