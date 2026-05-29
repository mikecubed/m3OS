//! ktrace — deep per-task scheduler trace tool.
//!
//! Controls and dumps the kernel "focus" trace ring (a deep, pid-filtered
//! scheduler/IPC event log) via `sys_ktrace` (syscall 0x1002). Built to pin
//! intermittent dispatch-starvation / lost-wake bugs from a *second* shell
//! while a first session is wedged.
//!
//! Usage:
//!   ktrace arm <pid> [pid...]   arm the focus ring on these pids (max 8)
//!   ktrace dump                 print the task table + focus timeline
//!   ktrace tasks                print the live task table (idx/pid/state/...)
//!   ktrace len                  print the focus ring entry count
//!   ktrace off                  disarm the focus ring
#![no_std]
#![no_main]

use syscall_lib::{
    STDOUT_FILENO, exit, ktrace_arm, ktrace_disarm, ktrace_dump_cores, ktrace_dump_serial,
    ktrace_dump_tasks_serial, ktrace_focus_len, ktrace_read_focus, ktrace_tasks, write, write_str,
    write_u64,
};

syscall_lib::entry_point!(main);

const BUF: usize = 2048;

fn main(args: &[&str]) -> i32 {
    match args.get(1).copied().unwrap_or("") {
        "arm" => cmd_arm(args.get(2..).unwrap_or(&[])),
        "off" | "disarm" => {
            ktrace_disarm();
            write_str(STDOUT_FILENO, "ktrace: disarmed\n");
            0
        }
        "len" => {
            write_str(STDOUT_FILENO, "focus entries: ");
            write_u64(STDOUT_FILENO, ktrace_focus_len());
            write_str(STDOUT_FILENO, "\n");
            0
        }
        "tasks" => cmd_tasks(),
        "dump" => cmd_dump(),
        // Dump the focus ring to the serial console — the robust path that
        // works even while the userspace I/O path is wedged by a hang.
        "serial" => {
            let n = ktrace_dump_serial();
            write_str(STDOUT_FILENO, "ktrace: dumped to serial, entries = ");
            write_u64(STDOUT_FILENO, n);
            write_str(STDOUT_FILENO, "\n");
            0
        }
        // Dump per-core dispatch state to serial — names what holds each core
        // (and what is Ready-but-waiting) while a session is wedged.
        "cores" => {
            ktrace_dump_cores();
            write_str(
                STDOUT_FILENO,
                "ktrace: dumped per-core dispatch state to serial\n",
            );
            0
        }
        // Dump every task's state (incl. Blocked) to serial — lightweight,
        // distinguishes lost-wake (Blk*) from dispatch-starvation (Ready).
        "states" => {
            let n = ktrace_dump_tasks_serial();
            write_str(
                STDOUT_FILENO,
                "ktrace: dumped task states to serial, tasks = ",
            );
            write_u64(STDOUT_FILENO, n);
            write_str(STDOUT_FILENO, "\n");
            0
        }
        _ => {
            usage();
            1
        }
    }
}

fn usage() {
    write_str(
        STDOUT_FILENO,
        "usage: ktrace <arm PID... | dump | serial | cores | tasks | len | off>\n",
    );
}

fn cmd_arm(pid_args: &[&str]) -> i32 {
    let mut pids = [0u32; 8];
    let mut n = 0;
    for s in pid_args {
        if n >= pids.len() {
            break;
        }
        if let Some(p) = parse_u32(s.as_bytes()) {
            pids[n] = p;
            n += 1;
        }
    }
    if n == 0 {
        write_str(
            STDOUT_FILENO,
            "ktrace: arm needs at least one numeric pid\n",
        );
        return 1;
    }
    let resolved = ktrace_arm(&pids[..n]);
    if resolved == u64::MAX {
        write_str(STDOUT_FILENO, "ktrace: arm failed (trace feature off?)\n");
        return 1;
    }
    write_str(STDOUT_FILENO, "ktrace: armed; resolved task idxs = ");
    write_u64(STDOUT_FILENO, resolved);
    write_str(STDOUT_FILENO, "\n");
    0
}

fn cmd_tasks() -> i32 {
    let mut buf = [0u8; BUF];
    let rows = ktrace_tasks(&mut buf);
    if rows == u64::MAX {
        write_str(STDOUT_FILENO, "ktrace: tasks failed (trace feature off?)\n");
        return 1;
    }
    write_nul_terminated(&buf);
    0
}

fn cmd_dump() -> i32 {
    // Lead with the task table so the focus timeline's idx/pid are decodable.
    cmd_tasks();
    write_str(STDOUT_FILENO, "---- focus trace (oldest first) ----\n");
    let mut buf = [0u8; BUF];
    let mut offset = 0u64;
    let mut total = 0u64;
    loop {
        let n = ktrace_read_focus(offset, &mut buf);
        if n == u64::MAX {
            write_str(STDOUT_FILENO, "ktrace: read_focus failed\n");
            return 1;
        }
        if n == 0 {
            break;
        }
        write_nul_terminated(&buf);
        offset += n;
        total += n;
        // Safety bound: stop if the ring is larger than we expect.
        if total > 1_000_000 {
            break;
        }
    }
    write_str(STDOUT_FILENO, "---- end (");
    write_u64(STDOUT_FILENO, total);
    write_str(STDOUT_FILENO, " entries) ----\n");
    0
}

fn write_nul_terminated(buf: &[u8]) {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let _ = write(STDOUT_FILENO, &buf[..end]);
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    let mut n: u32 = 0;
    let mut saw = false;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        saw = true;
        n = n.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    if saw { Some(n) } else { None }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "ktrace: PANIC\n");
    exit(101)
}
