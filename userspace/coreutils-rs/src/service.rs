//! service -- manage system services (Phase 46).
//!
//! Subcommands:
//!   service list             -- show all services and their status
//!   service status <name>    -- detailed status for one service
//!   service start <name>     -- start a stopped service
//!   service stop <name>      -- stop a running service
//!   service restart <name>   -- restart a service
//!   service enable <name>    -- enable a disabled service
//!   service disable <name>   -- disable a service (prevent auto-start)
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

/// Check if the caller is root. Returns true if root.
fn require_root(action: &str) -> bool {
    if syscall_lib::getuid() != 0 {
        write_str(STDERR_FILENO, "service: ");
        write_str(STDERR_FILENO, action);
        write_str(STDERR_FILENO, " requires root privileges\n");
        false
    } else {
        true
    }
}

/// Pad a string to at least `width` characters with trailing spaces.
fn write_padded(fd: i32, s: &str, width: usize) {
    write_str(fd, s);
    let len = s.len();
    if len < width {
        let mut rem = width - len;
        while rem > 0 {
            write_str(fd, " ");
            rem -= 1;
        }
    }
}

/// Parse a key=value pair from a status line field.
/// Returns the value part after '=' if the field starts with the given key.
fn parse_field<'a>(field: &'a str, key: &str) -> Option<&'a str> {
    if field.starts_with(key) && field.len() > key.len() && field.as_bytes()[key.len()] == b'=' {
        Some(&field[key.len() + 1..])
    } else {
        None
    }
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
    // Header.
    write_padded(STDOUT_FILENO, "NAME", 20);
    write_padded(STDOUT_FILENO, "STATUS", 22);
    write_padded(STDOUT_FILENO, "PID", 8);
    write_str(STDOUT_FILENO, "RESTARTS\n");

    write_padded(STDOUT_FILENO, "----", 20);
    write_padded(STDOUT_FILENO, "------", 22);
    write_padded(STDOUT_FILENO, "---", 8);
    write_str(STDOUT_FILENO, "--------\n");

    // Status file format: <name> <status> pid=<N> restarts=<N> changed=<epoch>
    let data = &buf[..n as usize];
    let text = match core::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => {
            write_str(STDERR_FILENO, "service: invalid UTF-8 in status file\n");
            return 1;
        }
    };
    for line in text.split('\n') {
        if line.is_empty() {
            continue;
        }
        // Parse fields: name status pid=N restarts=N changed=N
        let mut fields = line.split(' ');
        let name = match fields.next() {
            Some(n) => n,
            None => continue,
        };
        let status = match fields.next() {
            Some(s) => s,
            None => continue,
        };
        let mut pid = "0";
        let mut restarts = "0";
        for field in fields {
            if let Some(v) = parse_field(field, "pid") {
                pid = v;
            } else if let Some(v) = parse_field(field, "restarts") {
                restarts = v;
            }
        }

        write_padded(STDOUT_FILENO, name, 20);
        write_padded(STDOUT_FILENO, status, 22);
        write_padded(STDOUT_FILENO, pid, 8);
        write_str(STDOUT_FILENO, restarts);
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
    let text = match core::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => {
            write_str(STDERR_FILENO, "service: invalid UTF-8 in status file\n");
            return 1;
        }
    };
    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix(name)
            && (rest.starts_with(' ') || rest.starts_with('\t'))
        {
            // Parse: <name> <status> pid=<N> restarts=<N> changed=<epoch>
            let mut fields = line.split(' ');
            let _svc_name = fields.next(); // skip name, we already have it
            let status = fields.next().unwrap_or("unknown");
            let mut pid = "0";
            let mut restarts = "0";
            let mut changed = "0";
            for field in fields {
                if let Some(v) = parse_field(field, "pid") {
                    pid = v;
                } else if let Some(v) = parse_field(field, "restarts") {
                    restarts = v;
                } else if let Some(v) = parse_field(field, "changed") {
                    changed = v;
                }
            }

            // Determine exit code from status (e.g. "stopped:42")
            let (state_str, exit_code) = if let Some(code_str) = status.strip_prefix("stopped:") {
                ("stopped", code_str)
            } else {
                (status, "-")
            };

            write_str(STDOUT_FILENO, "Name:           ");
            write_str(STDOUT_FILENO, name);
            write_str(STDOUT_FILENO, "\n");

            write_str(STDOUT_FILENO, "State:          ");
            write_str(STDOUT_FILENO, state_str);
            write_str(STDOUT_FILENO, "\n");

            write_str(STDOUT_FILENO, "PID:            ");
            write_str(STDOUT_FILENO, pid);
            write_str(STDOUT_FILENO, "\n");

            write_str(STDOUT_FILENO, "Restarts:       ");
            write_str(STDOUT_FILENO, restarts);
            write_str(STDOUT_FILENO, "\n");

            write_str(STDOUT_FILENO, "Last exit:      ");
            write_str(STDOUT_FILENO, exit_code);
            write_str(STDOUT_FILENO, "\n");

            write_str(STDOUT_FILENO, "Last changed:   ");
            write_str(STDOUT_FILENO, changed);
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
    // Init polls /var/run/init.cmd in its main loop. Note: this is a
    // last-writer-wins mechanism -- rapid successive commands may clobber
    // each other. For single-operator use this is acceptable.
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
            "usage: service {list|status|start|stop|restart|enable|disable} [name]\n",
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
            if !require_root("start") {
                return 1;
            }
            send_command("start", args[2])
        }
        "stop" => {
            if args.len() < 3 {
                write_str(STDERR_FILENO, "usage: service stop <name>\n");
                return 1;
            }
            if !require_root("stop") {
                return 1;
            }
            send_command("stop", args[2])
        }
        "restart" => {
            if args.len() < 3 {
                write_str(STDERR_FILENO, "usage: service restart <name>\n");
                return 1;
            }
            if !require_root("restart") {
                return 1;
            }
            send_command("restart", args[2])
        }
        "enable" => {
            if args.len() < 3 {
                write_str(STDERR_FILENO, "usage: service enable <name>\n");
                return 1;
            }
            if !require_root("enable") {
                return 1;
            }
            send_command("enable", args[2])
        }
        "disable" => {
            if args.len() < 3 {
                write_str(STDERR_FILENO, "usage: service disable <name>\n");
                return 1;
            }
            if !require_root("disable") {
                return 1;
            }
            send_command("disable", args[2])
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
