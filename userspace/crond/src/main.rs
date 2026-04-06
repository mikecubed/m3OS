//! crond — cron scheduler daemon for m3OS (Phase 46).
//!
//! Reads `/etc/crontab` and per-user crontabs from `/var/spool/cron/<user>`,
//! then loops every 60 seconds executing jobs whose schedule matches the
//! current wall-clock minute. Logs execution to `/dev/log` via Unix domain
//! socket. Reloads crontab files on SIGHUP.
#![no_std]
#![no_main]

use syscall_lib::{
    AF_UNIX, CLOCK_REALTIME, SOCK_DGRAM, STDERR_FILENO, STDOUT_FILENO, SigAction, SockaddrUn,
    WNOHANG, clock_gettime, close, execve, exit, fork, gmtime, nanosleep, open, read, rt_sigaction,
    sendto_unix, socket, waitpid, write, write_str,
};

syscall_lib::entry_point!(main);

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_ENTRIES: usize = 32;
const MAX_LINE_LEN: usize = 512;
const MAX_CMD_LEN: usize = 256;
const MAX_FILE_SIZE: usize = 4096;
const MAX_USERNAME_LEN: usize = 32;
const PASSWD_PATH: &[u8] = b"/etc/passwd\0";

// ---------------------------------------------------------------------------
// Global reload flag (set by SIGHUP handler, read/cleared by main loop)
// ---------------------------------------------------------------------------

use core::sync::atomic::{AtomicBool, Ordering};

static RELOAD_FLAG: AtomicBool = AtomicBool::new(false);

extern "C" fn sighup_handler(_sig: i32) {
    RELOAD_FLAG.store(true, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Crontab entry
// ---------------------------------------------------------------------------

/// Describes a single time field: wildcard, exact value, range, or step.
#[derive(Clone, Copy)]
enum TimeField {
    /// Matches any value.
    Any,
    /// Matches a single exact value.
    Exact(u32),
    /// Matches values in [lo, hi] inclusive.
    Range(u32, u32),
    /// Matches when `value % step == 0` (for `*/N`).
    Step(u32),
}

impl TimeField {
    fn matches(self, value: u32) -> bool {
        match self {
            TimeField::Any => true,
            TimeField::Exact(v) => value == v,
            TimeField::Range(lo, hi) => value >= lo && value <= hi,
            TimeField::Step(s) => {
                if s == 0 {
                    false
                } else {
                    value.is_multiple_of(s)
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct CronEntry {
    minute: TimeField,
    hour: TimeField,
    day: TimeField,
    month: TimeField,
    weekday: TimeField,
    /// Null-terminated command string.
    cmd: [u8; MAX_CMD_LEN],
    cmd_len: usize,
    /// Whether this is an `@reboot` job (run once at startup).
    reboot: bool,
}

impl CronEntry {
    const fn empty() -> Self {
        CronEntry {
            minute: TimeField::Any,
            hour: TimeField::Any,
            day: TimeField::Any,
            month: TimeField::Any,
            weekday: TimeField::Any,
            cmd: [0u8; MAX_CMD_LEN],
            cmd_len: 0,
            reboot: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Number parsing helpers
// ---------------------------------------------------------------------------

/// Parse a decimal integer from a byte slice, returning (value, bytes_consumed).
/// Uses checked arithmetic to reject overflow rather than wrapping silently.
fn parse_u32(s: &[u8]) -> (u32, usize) {
    let mut val: u32 = 0;
    let mut i = 0;
    while i < s.len() && s[i] >= b'0' && s[i] <= b'9' {
        val = match val
            .checked_mul(10)
            .and_then(|v| v.checked_add((s[i] - b'0') as u32))
        {
            Some(v) => v,
            None => return (0, 0), // overflow → treat as parse failure
        };
        i += 1;
    }
    (val, i)
}

/// Parse a single time field token.
/// Returns `TimeField::Any` for tokens that cannot be parsed (invalid input
/// is silently treated as a wildcard rather than crashing).
fn parse_time_field(token: &[u8]) -> TimeField {
    if token.is_empty() {
        return TimeField::Any;
    }
    if token[0] == b'*' {
        if token.len() >= 3 && token[1] == b'/' {
            let (step, consumed) = parse_u32(&token[2..]);
            if consumed == 0 || consumed != token.len() - 2 {
                return TimeField::Any; // malformed → wildcard
            }
            return TimeField::Step(step);
        }
        return TimeField::Any;
    }
    // Check for range: M-N
    let mut dash_pos = 0;
    let mut has_dash = false;
    let mut i = 0;
    while i < token.len() {
        if token[i] == b'-' {
            dash_pos = i;
            has_dash = true;
            break;
        }
        i += 1;
    }
    if has_dash && dash_pos > 0 && dash_pos + 1 < token.len() {
        let (lo, lo_consumed) = parse_u32(&token[..dash_pos]);
        let (hi, hi_consumed) = parse_u32(&token[dash_pos + 1..]);
        if lo_consumed == dash_pos && hi_consumed == token.len() - dash_pos - 1 && lo_consumed > 0 {
            return TimeField::Range(lo, hi);
        }
        return TimeField::Any; // malformed range → wildcard
    }
    // Plain number — must consume the entire token.
    let (val, consumed) = parse_u32(token);
    if consumed == token.len() && consumed > 0 {
        TimeField::Exact(val)
    } else {
        TimeField::Any // non-numeric → wildcard
    }
}

// ---------------------------------------------------------------------------
// Line / token extraction helpers
// ---------------------------------------------------------------------------

/// Skip leading whitespace, return index of first non-space byte.
fn skip_ws(line: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < line.len() && (line[i] == b' ' || line[i] == b'\t') {
        i += 1;
    }
    i
}

/// Extract a whitespace-delimited token starting at `start`.
/// Returns (token_start, token_end).
fn next_token(line: &[u8], start: usize) -> (usize, usize) {
    let s = skip_ws(line, start);
    let mut e = s;
    while e < line.len() && line[e] != b' ' && line[e] != b'\t' && line[e] != b'\n' {
        e += 1;
    }
    (s, e)
}

/// Check if two byte slices are equal.
fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

// ---------------------------------------------------------------------------
// Crontab file parsing
// ---------------------------------------------------------------------------

/// Parse a single line into a CronEntry. Returns Some(entry) on success.
fn parse_line(line: &[u8], len: usize) -> Option<CronEntry> {
    if len == 0 {
        return None;
    }
    let first = skip_ws(line, 0);
    if first >= len || line[first] == b'#' || line[first] == b'\n' {
        return None;
    }

    let mut entry = CronEntry::empty();

    // Check for special strings
    let (ts, te) = next_token(line, first);
    if te > ts && line[ts] == b'@' {
        let keyword = &line[ts..te];
        if bytes_eq(keyword, b"@reboot") {
            entry.reboot = true;
        } else if bytes_eq(keyword, b"@hourly") {
            entry.minute = TimeField::Exact(0);
            // hour, day, month, weekday stay Any
        } else if bytes_eq(keyword, b"@daily") || bytes_eq(keyword, b"@midnight") {
            entry.minute = TimeField::Exact(0);
            entry.hour = TimeField::Exact(0);
        } else {
            // Unknown special string, skip
            return None;
        }
        // Rest of the line is the command
        let cmd_start = skip_ws(line, te);
        if cmd_start >= len {
            return None;
        }
        // Find end of line (trim trailing newline)
        let mut cmd_end = len;
        while cmd_end > cmd_start && (line[cmd_end - 1] == b'\n' || line[cmd_end - 1] == b'\r') {
            cmd_end -= 1;
        }
        let cmd_len = cmd_end - cmd_start;
        if cmd_len == 0 || cmd_len >= MAX_CMD_LEN {
            return None;
        }
        entry.cmd[..cmd_len].copy_from_slice(&line[cmd_start..cmd_end]);
        entry.cmd_len = cmd_len;
        return Some(entry);
    }

    // Standard 5-field format: minute hour day month weekday command
    let (t1s, t1e) = (ts, te);
    entry.minute = parse_time_field(&line[t1s..t1e]);

    let (t2s, t2e) = next_token(line, t1e);
    if t2s >= len {
        return None;
    }
    entry.hour = parse_time_field(&line[t2s..t2e]);

    let (t3s, t3e) = next_token(line, t2e);
    if t3s >= len {
        return None;
    }
    entry.day = parse_time_field(&line[t3s..t3e]);

    let (t4s, t4e) = next_token(line, t3e);
    if t4s >= len {
        return None;
    }
    entry.month = parse_time_field(&line[t4s..t4e]);

    let (t5s, t5e) = next_token(line, t4e);
    if t5s >= len {
        return None;
    }
    entry.weekday = parse_time_field(&line[t5s..t5e]);

    // Everything after the 5 fields is the command
    let cmd_start = skip_ws(line, t5e);
    if cmd_start >= len {
        return None;
    }
    let mut cmd_end = len;
    while cmd_end > cmd_start && (line[cmd_end - 1] == b'\n' || line[cmd_end - 1] == b'\r') {
        cmd_end -= 1;
    }
    let cmd_len = cmd_end - cmd_start;
    if cmd_len == 0 || cmd_len >= MAX_CMD_LEN {
        return None;
    }
    entry.cmd[..cmd_len].copy_from_slice(&line[cmd_start..cmd_end]);
    entry.cmd_len = cmd_len;
    Some(entry)
}

/// Read a file into a buffer. Returns number of bytes read, or 0 on failure.
fn read_file(path: &[u8], buf: &mut [u8]) -> usize {
    let fd = open(path, syscall_lib::O_RDONLY, 0);
    if fd < 0 {
        return 0;
    }
    let mut total = 0usize;
    loop {
        let n = read(fd as i32, &mut buf[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
        if total >= buf.len() {
            break;
        }
    }
    close(fd as i32);
    total
}

/// Parse an entire crontab file into the entries array starting at `count`.
/// Returns the new count.
fn parse_crontab(
    data: &[u8],
    data_len: usize,
    entries: &mut [CronEntry; MAX_ENTRIES],
    mut count: usize,
) -> usize {
    let mut line_start = 0;
    let mut i = 0;
    while i <= data_len && count < MAX_ENTRIES {
        let at_end = i == data_len;
        let at_newline = !at_end && data[i] == b'\n';
        if at_end || at_newline {
            let line_end = if at_newline { i + 1 } else { i };
            let line_len = line_end - line_start;
            if line_len > 0
                && line_len <= MAX_LINE_LEN
                && let Some(entry) = parse_line(&data[line_start..line_end], line_len)
            {
                entries[count] = entry;
                count += 1;
            }
            line_start = i + 1;
        }
        i += 1;
    }
    count
}

fn load_user_crontabs_from_passwd(
    passwd: &[u8],
    entries: &mut [CronEntry; MAX_ENTRIES],
    mut count: usize,
    file_buf: &mut [u8; MAX_FILE_SIZE],
) -> usize {
    const PREFIX: &[u8] = b"/var/spool/cron/";

    for line in passwd.split(|&b| b == b'\n') {
        if line.is_empty() || count >= MAX_ENTRIES {
            break;
        }

        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let username = &line[..colon];
        if username.is_empty() || username.len() > MAX_USERNAME_LEN {
            continue;
        }

        let total = PREFIX.len() + username.len() + 1;
        let mut path = [0u8; 128];
        if total > path.len() {
            continue;
        }

        path[..PREFIX.len()].copy_from_slice(PREFIX);
        path[PREFIX.len()..PREFIX.len() + username.len()].copy_from_slice(username);
        path[total - 1] = 0;

        let n = read_file(&path[..total], file_buf);
        if n > 0 {
            count = parse_crontab(file_buf, n, entries, count);
        }
    }

    count
}

/// Load all crontab files (system + per-user).
fn load_all_crontabs(
    entries: &mut [CronEntry; MAX_ENTRIES],
    file_buf: &mut [u8; MAX_FILE_SIZE],
) -> usize {
    let mut count = 0;

    // System crontab
    let n = read_file(b"/etc/crontab\0", file_buf);
    if n > 0 {
        count = parse_crontab(file_buf, n, entries, count);
    }

    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file(PASSWD_PATH, &mut passwd_buf);
    if passwd_len > 0 {
        count = load_user_crontabs_from_passwd(&passwd_buf[..passwd_len], entries, count, file_buf);
    }

    count
}

// ---------------------------------------------------------------------------
// Syslog via /dev/log Unix domain socket
// ---------------------------------------------------------------------------

/// Open a DGRAM socket connected to /dev/log for syslog messages.
fn open_syslog() -> i32 {
    let fd = socket(AF_UNIX as i32, SOCK_DGRAM as i32, 0);
    if fd < 0 {
        return -1;
    }
    fd as i32
}

/// Send a syslog message. Format: "<priority>crond: <msg>".
/// Priority 14 = LOG_USER | LOG_INFO.
fn syslog_msg(log_fd: i32, msg: &[u8]) {
    if log_fd < 0 {
        return;
    }
    // Build message: "<14>crond: " + msg + "\n"
    let prefix = b"<14>crond: ";
    let mut buf = [0u8; 512];
    let mut pos = 0;
    for &b in prefix {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in msg {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    if pos < buf.len() {
        buf[pos] = b'\n';
        pos += 1;
    }

    let addr = SockaddrUn::new("/dev/log");
    let _ = sendto_unix(log_fd, &buf[..pos], 0, &addr);
}

/// Format and log job execution.
fn log_job(log_fd: i32, cmd: &[u8], cmd_len: usize) {
    let mut msg = [0u8; 300];
    let prefix = b"executing: ";
    let mut pos = 0;
    for &b in prefix {
        if pos < msg.len() {
            msg[pos] = b;
            pos += 1;
        }
    }
    let mut i = 0;
    while i < cmd_len && pos < msg.len() {
        msg[pos] = cmd[i];
        pos += 1;
        i += 1;
    }
    syslog_msg(log_fd, &msg[..pos]);
}

// ---------------------------------------------------------------------------
// Job execution
// ---------------------------------------------------------------------------

/// Execute a command by fork+exec through /bin/sh -c "<cmd>".
fn exec_job(entry: &CronEntry, log_fd: i32) {
    log_job(log_fd, &entry.cmd, entry.cmd_len);

    let pid = fork();
    if pid < 0 {
        syslog_msg(log_fd, b"fork failed");
        return;
    }
    if pid == 0 {
        // Child: exec /bin/sh -c "command"
        // Build null-terminated command string
        let mut cmd_nt = [0u8; MAX_CMD_LEN + 1];
        cmd_nt[..entry.cmd_len].copy_from_slice(&entry.cmd[..entry.cmd_len]);
        cmd_nt[entry.cmd_len] = 0;

        let sh_path = b"/bin/sh\0";
        let dash_c = b"-c\0";

        let argv: [*const u8; 4] = [
            sh_path.as_ptr(),
            dash_c.as_ptr(),
            cmd_nt.as_ptr(),
            core::ptr::null(),
        ];
        let envp: [*const u8; 1] = [core::ptr::null()];
        let _ = execve(sh_path, &argv, &envp);
        // If execve fails, exit child
        exit(127);
    }
    // Parent does not wait — we reap children in the main loop.
}

// ---------------------------------------------------------------------------
// Reap zombie children
// ---------------------------------------------------------------------------

fn reap_children() {
    let mut status: i32 = 0;
    loop {
        let ret = waitpid(-1, &mut status, WNOHANG);
        if ret <= 0 {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Check if a cron entry matches the current time
// ---------------------------------------------------------------------------

fn entry_matches(
    entry: &CronEntry,
    minute: u32,
    hour: u32,
    day: u32,
    month: u32,
    weekday: u32,
) -> bool {
    if entry.reboot {
        return false; // @reboot jobs are handled separately at startup
    }
    entry.minute.matches(minute)
        && entry.hour.matches(hour)
        && entry.day.matches(day)
        && entry.month.matches(month)
        && entry.weekday.matches(weekday)
}

// ---------------------------------------------------------------------------
// Main daemon
// ---------------------------------------------------------------------------

fn main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "crond: starting\n");

    // Install SIGHUP handler for crontab reload
    let sa = SigAction {
        sa_handler: sighup_handler as *const () as u64,
        sa_flags: 0,
        sa_restorer: 0,
        sa_mask: 0,
    };
    let _ = rt_sigaction(
        syscall_lib::SIGHUP as usize,
        &sa as *const SigAction,
        core::ptr::null_mut(),
    );

    // Open syslog socket
    let log_fd = open_syslog();

    syslog_msg(log_fd, b"daemon started");

    // Load crontab entries
    let mut entries = [CronEntry::empty(); MAX_ENTRIES];
    let mut file_buf = [0u8; MAX_FILE_SIZE];
    let mut count = load_all_crontabs(&mut entries, &mut file_buf);

    write_str(STDOUT_FILENO, "crond: loaded ");
    write_u32(STDOUT_FILENO, count as u32);
    write_str(STDOUT_FILENO, " crontab entries\n");

    // Run @reboot jobs once
    {
        let mut i = 0;
        while i < count {
            if entries[i].reboot {
                exec_job(&entries[i], log_fd);
            }
            i += 1;
        }
    }

    // Track last-run minute to avoid re-triggering within the same minute
    let mut last_minute: i64 = -1;

    // Main loop: check every 60 seconds
    loop {
        // Check for SIGHUP reload
        if RELOAD_FLAG.swap(false, Ordering::AcqRel) {
            entries = [CronEntry::empty(); MAX_ENTRIES];
            count = load_all_crontabs(&mut entries, &mut file_buf);
            syslog_msg(log_fd, b"reloaded crontab files");
            write_str(STDOUT_FILENO, "crond: reloaded ");
            write_u32(STDOUT_FILENO, count as u32);
            write_str(STDOUT_FILENO, " entries\n");
        }

        // Get current time
        let (sec, _nsec) = clock_gettime(CLOCK_REALTIME);
        if sec < 0 {
            write_str(STDERR_FILENO, "crond: clock_gettime failed\n");
            nanosleep(60);
            continue;
        }

        let dt = gmtime(sec as u64);
        let current_epoch_minute = sec / 60;

        // Only fire jobs once per minute
        if current_epoch_minute != last_minute {
            last_minute = current_epoch_minute;

            let mut i = 0;
            while i < count {
                if entry_matches(
                    &entries[i],
                    dt.minute,
                    dt.hour,
                    dt.day,
                    dt.month,
                    dt.weekday,
                ) {
                    exec_job(&entries[i], log_fd);
                }
                i += 1;
            }
        }

        // Reap any finished children
        reap_children();

        // Sleep until roughly the next minute boundary.
        // Compute seconds remaining in the current minute.
        let secs_into_minute = (sec % 60) as u64;
        let sleep_secs = if secs_into_minute < 59 {
            60 - secs_into_minute
        } else {
            60
        };
        nanosleep(sleep_secs);
    }
}

// ---------------------------------------------------------------------------
// Utility: write a u32 as decimal to a file descriptor
// ---------------------------------------------------------------------------

fn write_u32(fd: i32, mut n: u32) {
    if n == 0 {
        let _ = write(fd, b"0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut pos = 10;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let _ = write(fd, &buf[pos..10]);
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "crond: PANIC\n");
    exit(101)
}
