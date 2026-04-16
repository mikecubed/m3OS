#![no_std]
#![no_main]

use core::ptr;

use syscall_lib::{
    O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDERR_FILENO, STDOUT_FILENO, Stat, close, dup2, execve,
    exit, fork, geteuid, getpid, open, pipe, read, stat, unlink, waitpid, write, write_str,
};

const TCC_PATH: &[u8] = b"/usr/bin/tcc\0";
const HELLO_SOURCE_PATH: &[u8] = b"/usr/src/hello.c\0";
const HELLO_BIN_PATH: &[u8] = b"/usr/src/h\0";
const UDP_SMOKE_PATH: &[u8] = b"/root/udp-smoke\0";
const SERVICE_STATUS_PATH: &[u8] = b"/var/run/services.status\0";
const SMOKE_FILE_PATH: &[u8] = b"/root/smoke_test_file\0";
const LOGGER_PATH: &[u8] = b"/bin/logger\0";
const SYSTEM_LOG_PATH: &[u8] = b"/var/log/messages\0";

const TCC_ARGV0: &[u8] = b"tcc\0";
const TCC_VERSION_ARG: &[u8] = b"--version\0";
const TCC_STATIC_ARG: &[u8] = b"-static\0";
const TCC_OUTPUT_ARG: &[u8] = b"-o\0";
const LOGGER_ARGV0: &[u8] = b"logger\0";

const SERVICE_STATUS_NEEDLE: &[u8] = b"syslogd running";
const TCC_VERSION_NEEDLE: &[u8] = b"tcc version";
const HELLO_NEEDLE: &[u8] = b"hello, world";
const UDP_PASS_NEEDLE: &[u8] = b"udp-smoke: PASS";

const READ_BUF_LEN: usize = 4096;
const FILE_SCAN_BUF_LEN: usize = 1152;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "SMOKE:BEGIN\n");

    begin("auth");
    if geteuid() != 0 {
        return fail("auth", "expected root shell session", 1);
    }
    pass("auth");

    let mut command_output = [0u8; READ_BUF_LEN];

    begin("tcc-version");
    let tcc_version_argv = [TCC_ARGV0.as_ptr(), TCC_VERSION_ARG.as_ptr(), ptr::null()];
    if let Err(code) = run_command_expect_output(
        "tcc-version",
        TCC_PATH,
        &tcc_version_argv,
        TCC_VERSION_NEEDLE,
        &mut command_output,
    ) {
        return code;
    }
    pass("tcc-version");

    begin("tcc-compile");
    let tcc_compile_argv = [
        TCC_ARGV0.as_ptr(),
        TCC_STATIC_ARG.as_ptr(),
        HELLO_SOURCE_PATH.as_ptr(),
        TCC_OUTPUT_ARG.as_ptr(),
        HELLO_BIN_PATH.as_ptr(),
        ptr::null(),
    ];
    if let Err(code) = run_command_expect_success(
        "tcc-compile",
        TCC_PATH,
        &tcc_compile_argv,
        &mut command_output,
    ) {
        return code;
    }
    pass("tcc-compile");

    begin("hello");
    let hello_argv = [HELLO_BIN_PATH.as_ptr(), ptr::null()];
    if let Err(code) = run_command_expect_output(
        "hello",
        HELLO_BIN_PATH,
        &hello_argv,
        HELLO_NEEDLE,
        &mut command_output,
    ) {
        return code;
    }
    let _ = unlink(HELLO_BIN_PATH);
    pass("hello");

    begin("service");
    if !wait_for_file_contains(SERVICE_STATUS_PATH, SERVICE_STATUS_NEEDLE, 30) {
        return fail(
            "service",
            "syslogd not marked running in /var/run/services.status",
            4,
        );
    }
    pass("service");

    begin("storage");
    if let Err(code) = create_and_verify_smoke_file() {
        return code;
    }
    pass("storage");

    begin("net");
    let udp_smoke_argv = [UDP_SMOKE_PATH.as_ptr(), ptr::null()];
    if let Err(code) = run_command_expect_output(
        "net",
        UDP_SMOKE_PATH,
        &udp_smoke_argv,
        UDP_PASS_NEEDLE,
        &mut command_output,
    ) {
        return code;
    }
    pass("net");

    begin("log");
    if let Err(code) = inject_and_verify_log_marker(&mut command_output) {
        return code;
    }
    pass("log");

    write_str(STDOUT_FILENO, "SMOKE:PASS\n");
    0
}

fn create_and_verify_smoke_file() -> Result<(), i32> {
    let fd = open(SMOKE_FILE_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0o644);
    if fd < 0 {
        return Err(fail("storage", "open(/root/smoke_test_file) failed", 5));
    }
    let _ = close(fd as i32);

    let mut meta = Stat::zeroed();
    if stat(SMOKE_FILE_PATH, &mut meta) < 0 {
        return Err(fail("storage", "stat(/root/smoke_test_file) failed", 6));
    }

    if unlink(SMOKE_FILE_PATH) < 0 {
        return Err(fail("storage", "unlink(/root/smoke_test_file) failed", 7));
    }

    Ok(())
}

fn inject_and_verify_log_marker(command_output: &mut [u8]) -> Result<(), i32> {
    let mut marker_buf = [0u8; 64];
    let marker_len = build_log_marker(&mut marker_buf);
    if marker_len == 0 {
        return Err(fail("log", "failed to build log marker", 8));
    }
    let marker = &marker_buf[..marker_len];
    let marker_cstr = &marker_buf[..marker_len + 1];

    let logger_argv = [LOGGER_ARGV0.as_ptr(), marker_cstr.as_ptr(), ptr::null()];
    run_command_expect_success("log", LOGGER_PATH, &logger_argv, command_output)?;

    if !wait_for_file_contains(SYSTEM_LOG_PATH, marker, 15) {
        return Err(fail("log", "marker missing from /var/log/messages", 9));
    }

    Ok(())
}

fn build_log_marker(buf: &mut [u8]) -> usize {
    let prefix = b"SMOKE_LOG_MARKER_";
    if buf.len() <= prefix.len() + 1 {
        return 0;
    }
    buf[..prefix.len()].copy_from_slice(prefix);
    let mut len = prefix.len();
    let pid = getpid();
    let pid = if pid < 0 { 0 } else { pid as u64 };
    let digits_end = buf.len() - 1;
    len += write_decimal_into(&mut buf[len..digits_end], pid);
    buf[len] = 0;
    len
}

fn write_decimal_into(buf: &mut [u8], mut n: u64) -> usize {
    if buf.is_empty() {
        return 0;
    }
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }

    let mut tmp = [0u8; 20];
    let mut pos = tmp.len();
    while n > 0 {
        pos -= 1;
        tmp[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let digits = tmp.len() - pos;
    if digits > buf.len() {
        return 0;
    }
    buf[..digits].copy_from_slice(&tmp[pos..]);
    digits
}

fn wait_for_file_contains(path: &[u8], needle: &[u8], attempts: usize) -> bool {
    for _ in 0..attempts {
        if let Ok(found) = file_contains(path, needle)
            && found
        {
            return true;
        }
        let _ = syscall_lib::nanosleep(1);
    }
    false
}

fn file_contains(path: &[u8], needle: &[u8]) -> Result<bool, ()> {
    if needle.is_empty() {
        return Ok(true);
    }

    let fd = open(path, O_RDONLY, 0);
    if fd < 0 {
        return Err(());
    }

    let fd = fd as i32;
    let mut scan_buf = [0u8; FILE_SCAN_BUF_LEN];
    let mut carry_len = 0usize;
    loop {
        let n = read(fd, &mut scan_buf[carry_len..]);
        if n < 0 {
            let _ = close(fd);
            return Err(());
        }
        if n == 0 {
            break;
        }
        let total = carry_len + n as usize;
        if contains_bytes(&scan_buf[..total], needle) {
            let _ = close(fd);
            return Ok(true);
        }

        let keep = core::cmp::min(needle.len().saturating_sub(1), total);
        scan_buf.copy_within(total - keep..total, 0);
        carry_len = keep;
    }
    let _ = close(fd);
    Ok(false)
}

fn run_command_expect_success(
    stage: &str,
    path: &[u8],
    argv: &[*const u8],
    output: &mut [u8],
) -> Result<(), i32> {
    let (status, len) = match run_command_capture(path, argv, output) {
        Ok(result) => result,
        Err(msg) => return Err(fail(stage, msg, 10)),
    };

    if exit_code(status) != Some(0) {
        return Err(fail_with_output(
            stage,
            "command exited non-zero",
            11,
            &output[..len],
        ));
    }

    Ok(())
}

fn run_command_expect_output(
    stage: &str,
    path: &[u8],
    argv: &[*const u8],
    needle: &[u8],
    output: &mut [u8],
) -> Result<(), i32> {
    let (status, len) = match run_command_capture(path, argv, output) {
        Ok(result) => result,
        Err(msg) => return Err(fail(stage, msg, 12)),
    };

    if exit_code(status) != Some(0) {
        return Err(fail_with_output(
            stage,
            "command exited non-zero",
            13,
            &output[..len],
        ));
    }

    if !contains_bytes(&output[..len], needle) {
        return Err(fail_with_output(
            stage,
            "expected output marker missing",
            14,
            &output[..len],
        ));
    }

    Ok(())
}

fn run_command_capture(
    path: &[u8],
    argv: &[*const u8],
    buf: &mut [u8],
) -> Result<(i32, usize), &'static str> {
    let mut fds = [0i32; 2];
    if pipe(&mut fds) < 0 {
        return Err("pipe() failed");
    }

    let pid = fork();
    if pid < 0 {
        let _ = close(fds[0]);
        let _ = close(fds[1]);
        return Err("fork() failed");
    }

    if pid == 0 {
        let _ = close(fds[0]);
        if dup2(fds[1], STDOUT_FILENO) < 0 || dup2(fds[1], STDERR_FILENO) < 0 {
            exit(126);
        }
        let _ = close(fds[1]);
        let envp = [ptr::null()];
        let _ = execve(path, argv, &envp);
        write_str(STDOUT_FILENO, "execve() failed\n");
        exit(127);
    }

    let _ = close(fds[1]);

    let mut total = 0usize;
    let mut discard = [0u8; 256];
    loop {
        let read_buf = if total < buf.len() {
            &mut buf[total..]
        } else {
            &mut discard[..]
        };

        let n = read(fds[0], read_buf);
        if n < 0 {
            let _ = close(fds[0]);
            let mut status = 0i32;
            let _ = waitpid(pid as i32, &mut status, 0);
            return Err("read() failed");
        }
        if n == 0 {
            break;
        }
        if total < buf.len() {
            total += n as usize;
        }
    }

    let _ = close(fds[0]);

    let mut status = 0i32;
    if waitpid(pid as i32, &mut status, 0) != pid {
        return Err("waitpid() failed");
    }

    Ok((status, total.min(buf.len())))
}

fn exit_code(status: i32) -> Option<i32> {
    if (status & 0x7f) == 0 {
        Some((status >> 8) & 0xff)
    } else {
        None
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn pass(stage: &str) {
    write_str(STDOUT_FILENO, "SMOKE:");
    write_str(STDOUT_FILENO, stage);
    write_str(STDOUT_FILENO, ":PASS\n");
}

fn begin(stage: &str) {
    write_str(STDOUT_FILENO, "SMOKE:");
    write_str(STDOUT_FILENO, stage);
    write_str(STDOUT_FILENO, ":BEGIN\n");
}

fn fail(stage: &str, msg: &str, code: i32) -> i32 {
    write_str(STDOUT_FILENO, "SMOKE:");
    write_str(STDOUT_FILENO, stage);
    write_str(STDOUT_FILENO, ":FAIL ");
    write_str(STDOUT_FILENO, msg);
    write_str(STDOUT_FILENO, "\n");
    code
}

fn fail_with_output(stage: &str, msg: &str, code: i32, output: &[u8]) -> i32 {
    let code = fail(stage, msg, code);
    if !output.is_empty() {
        write_str(STDOUT_FILENO, "SMOKE:output:");
        let _ = write(STDOUT_FILENO, output);
        if !output.ends_with(b"\n") {
            write_str(STDOUT_FILENO, "\n");
        }
    }
    code
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "SMOKE:panic:FAIL\n");
    exit(101)
}
