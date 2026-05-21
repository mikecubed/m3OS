//! Phase 73 Track E.3 — `notify-send` CLI.
//!
//! Connects to `/run/notifyd.sock`, sends one framed JSON message,
//! exits.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use core::alloc::Layout;

use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "notify-send: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "notify-send: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const SOCKET_PATH: &str = "/run/notifyd.sock";

fn program_main(args: &[&str]) -> i32 {
    let mut timeout: u32 = 5000;
    let mut positional: heapless::Vec<&str, 4> = heapless::Vec::new();
    let mut idx = 1;
    while idx < args.len() {
        let arg = args[idx];
        if arg == "--timeout" {
            if idx + 1 >= args.len() {
                syscall_lib::write_str(STDOUT_FILENO, "notify-send: --timeout requires value\n");
                return 1;
            }
            timeout = args[idx + 1].parse::<u32>().unwrap_or(5000);
            idx += 2;
            continue;
        }
        let _ = positional.push(arg);
        idx += 1;
    }
    if positional.len() < 2 {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "Usage: notify-send [--timeout MS] TITLE BODY\n",
        );
        return 1;
    }
    let title = json_escape(positional[0]);
    let body = json_escape(positional[1]);
    let payload = format!("{{\"title\":\"{title}\",\"body\":\"{body}\",\"timeout_ms\":{timeout}}}");

    let fd = syscall_lib::socket(
        syscall_lib::AF_UNIX as i32,
        syscall_lib::SOCK_STREAM as i32,
        0,
    );
    if fd < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "notify-send: socket() failed\n");
        return 1;
    }
    let addr = syscall_lib::SockaddrUn::new(SOCKET_PATH);
    if syscall_lib::connect_unix(fd as i32, &addr) < 0 {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "notify-send: connect() failed (notifyd not running?)\n",
        );
        let _ = syscall_lib::close(fd as i32);
        return 1;
    }

    let len = payload.len() as u32;
    let len_bytes = len.to_le_bytes();
    let _ = syscall_lib::write(fd as i32, &len_bytes);
    let _ = syscall_lib::write(fd as i32, payload.as_bytes());
    let _ = syscall_lib::close(fd as i32);
    0
}

fn json_escape(s: &str) -> alloc::string::String {
    let mut out = alloc::string::String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Tiny in-place static vec to avoid a heap dep just for argument
/// parsing. Three-line shim; the `heapless` crate is overkill for
/// this binary so we inline the only API we need.
mod heapless {
    pub struct Vec<T, const N: usize> {
        items: [Option<T>; N],
        len: usize,
    }

    impl<T: Copy, const N: usize> Default for Vec<T, N> {
        fn default() -> Self {
            Self {
                items: [(); N].map(|_| None),
                len: 0,
            }
        }
    }

    impl<T: Copy, const N: usize> Vec<T, N> {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn push(&mut self, item: T) -> Result<(), T> {
            if self.len >= N {
                return Err(item);
            }
            self.items[self.len] = Some(item);
            self.len += 1;
            Ok(())
        }

        pub fn len(&self) -> usize {
            self.len
        }
    }

    impl<T: Copy, const N: usize> core::ops::Index<usize> for Vec<T, N> {
        type Output = T;

        fn index(&self, idx: usize) -> &T {
            self.items[idx].as_ref().expect("index in range")
        }
    }
}
