//! service — manage system services (Phase 46).
//!
//! Subcommands:
//!   service list             — show all services and their status
//!   service status <name>    — detailed status for one service
//!   service start <name>     — start a stopped service
//!   service stop <name>      — stop a running service
//!   service restart <name>   — restart a service
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, write_str};

syscall_lib::entry_point!(main);

const STATUS_PATH: &[u8] = b"/var/run/services.status\0";
const CMD_PATH: &[u8] = b"/var/run/init.cmd\0";

fn read_file(path: &[u8], buf: &mut [u8]) -> isize {
    let fd = syscall_lib::open(path, 0, 0);
    if fd < 0 {
        return fd;
    }
    let n = syscall_lib::read(fd as i32, buf);
    syscall_lib::close(fd as i32);
    n
}

fn write_file(path: &[u8], data: &[u8]) -> isize {
    let fd = syscall_lib::open(
        path,
        syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC,
        0o644,
    );
    if fd < 0 {
        return fd;
    }
    let n = syscall_lib::write(fd as i32, data);
    syscall_lib::close(fd as i32);
    n
}

fn cmd_list() -> i32 {
    let mut buf = [0u8; 4096];
    let n = read_file(STATUS_PATH, &mut buf);
    if n <= 0 {
        write_str(
            STDERR_FILENO,
            "service: no status available (is init running?)\n",
        );
        return 1;
    }
    write_str(
        STDOUT_FILENO,
        "NAME            STATUS          PID     RESTARTS\n",
    );
    write_str(
        STDOUT_FILENO,
        "----            ------          ---     --------\n",
    );
    // Status file format: one line per service: name status pid restart_count
    let data = &buf[..n as usize];
    let text = unsafe { core::str::from_utf8_unchecked(data) };
    for line in text.split('\n') {
        if line.is_empty() {
            continue;
        }
        write_str(STDOUT_FILENO, line);
        write_str(STDOUT_FILENO, "\n");
    }
    0
}

fn cmd_status(name: &str) -> i32 {
    let mut buf = [0u8; 4096];
    let n = read_file(STATUS_PATH, &mut buf);
    if n <= 0 {
        write_str(STDERR_FILENO, "service: no status available\n");
        return 1;
    }
    let data = &buf[..n as usize];
    let text = unsafe { core::str::from_utf8_unchecked(data) };
    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix(name)
            && (rest.starts_with(' ') || rest.starts_with('\t'))
        {
            write_str(STDOUT_FILENO, "Service: ");
            write_str(STDOUT_FILENO, name);
            write_str(STDOUT_FILENO, "\n");
            write_str(STDOUT_FILENO, line);
            write_str(STDOUT_FILENO, "\n");
            return 0;
        }
    }
    write_str(STDERR_FILENO, "service: '");
    write_str(STDERR_FILENO, name);
    write_str(STDERR_FILENO, "' not found\n");
    1
}

fn send_command(cmd: &str, name: &str) -> i32 {
    let mut buf = [0u8; 128];
    let cmd_bytes = cmd.as_bytes();
    let name_bytes = name.as_bytes();
    let total = cmd_bytes.len() + 1 + name_bytes.len() + 1; // "cmd name\n"
    if total > buf.len() {
        write_str(STDERR_FILENO, "service: name too long\n");
        return 1;
    }
    buf[..cmd_bytes.len()].copy_from_slice(cmd_bytes);
    buf[cmd_bytes.len()] = b' ';
    buf[cmd_bytes.len() + 1..cmd_bytes.len() + 1 + name_bytes.len()].copy_from_slice(name_bytes);
    buf[total - 1] = b'\n';

    let ret = write_file(CMD_PATH, &buf[..total]);
    if ret < 0 {
        write_str(STDERR_FILENO, "service: failed to send command to init\n");
        return 1;
    }
    // Signal init (PID 1) with SIGUSR1 to process the command.
    syscall_lib::kill(1, syscall_lib::SIGUSR1);
    write_str(STDOUT_FILENO, "service: ");
    write_str(STDOUT_FILENO, cmd);
    write_str(STDOUT_FILENO, " ");
    write_str(STDOUT_FILENO, name);
    write_str(STDOUT_FILENO, " requested\n");
    0
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_str(
            STDERR_FILENO,
            "usage: service {list|status|start|stop|restart} [name]\n",
        );
        return 1;
    }

    match args[1] {
        "list" => cmd_list(),
        "status" => {
            if args.len() < 3 {
                write_str(STDERR_FILENO, "usage: service status <name>\n");
                return 1;
            }
            cmd_status(args[2])
        }
        "start" => {
            if args.len() < 3 {
                write_str(STDERR_FILENO, "usage: service start <name>\n");
                return 1;
            }
            send_command("start", args[2])
        }
        "stop" => {
            if args.len() < 3 {
                write_str(STDERR_FILENO, "usage: service stop <name>\n");
                return 1;
            }
            send_command("stop", args[2])
        }
        "restart" => {
            if args.len() < 3 {
                write_str(STDERR_FILENO, "usage: service restart <name>\n");
                return 1;
            }
            send_command("restart", args[2])
        }
        _ => {
            write_str(STDERR_FILENO, "service: unknown subcommand '");
            write_str(STDERR_FILENO, args[1]);
            write_str(STDERR_FILENO, "'\n");
            1
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
