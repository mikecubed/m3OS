//! Userspace UDP network service for m3OS (Phase 54 Track C).
//!
//! Owns the migrated UDP **policy** for the Phase 54 network slice.
//! The kernel retains packet I/O mechanism (virtio-net send/recv,
//! per-port datagram queues, wait-queue wakeup, user-buffer copy).
//! This service answers socket create/bind/connect/sendto-validate/
//! recvfrom-validate/close requests via IPC.
//!
//! # Architecture
//!
//! ```text
//! app → sendto(fd, buf, dst) → kernel syscall handler
//!       → detects UDP + "net_udp" registered
//!       → IPC call_msg(net_udp_ep, NET_UDP_SENDTO, handle, dst)
//!       → this server: validate policy, reply with src_port
//!       → kernel: transmit packet via ipv4::send (mechanism)
//! ```
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;
use kernel_core::net::udp_protocol::{
    NET_UDP_BIND, NET_UDP_CLOSE, NET_UDP_CONNECT, NET_UDP_CREATE, NET_UDP_RECVFROM, NET_UDP_SENDTO,
    unpack_ip_port,
};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "net_server: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "net_server: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const REPLY_CAP_HANDLE: u32 = 1;
const MAX_HANDLES: usize = 64;
const MAX_BINDINGS: usize = 16;

/// Negative errno values (matches kernel convention).
const NEG_EINVAL: u64 = (-22_i64) as u64;
const NEG_EADDRINUSE: u64 = (-98_i64) as u64;
const NEG_ENOTCONN: u64 = (-107_i64) as u64;
const NEG_ENOMEM: u64 = (-12_i64) as u64;
const NEG_EPIPE: u64 = (-32_i64) as u64;

// ---------------------------------------------------------------------------
// UDP handle state — the policy this service owns
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum HandleState {
    Free,
    Unbound,
    Bound,
    Connected,
    Closed,
}

#[derive(Clone, Copy)]
struct UdpHandle {
    state: HandleState,
    local_port: u16,
    local_ip: [u8; 4],
    remote_ip: [u8; 4],
    remote_port: u16,
    bound: bool,
    shut_wr: bool,
}

impl UdpHandle {
    const fn free() -> Self {
        Self {
            state: HandleState::Free,
            local_port: 0,
            local_ip: [0; 4],
            remote_ip: [0; 4],
            remote_port: 0,
            bound: false,
            shut_wr: false,
        }
    }
}

struct HandleTable {
    handles: [UdpHandle; MAX_HANDLES],
    /// Simple port binding tracker: which ports are bound.
    bound_ports: [u16; MAX_BINDINGS],
    bound_count: usize,
    /// Monotonic counter for ephemeral port allocation.
    ephemeral_counter: u16,
}

impl HandleTable {
    const fn new() -> Self {
        Self {
            handles: [UdpHandle::free(); MAX_HANDLES],
            bound_ports: [0u16; MAX_BINDINGS],
            bound_count: 0,
            ephemeral_counter: 0xC000,
        }
    }

    fn create(&mut self, idx: usize) -> bool {
        if idx >= MAX_HANDLES {
            return false;
        }
        self.handles[idx] = UdpHandle {
            state: HandleState::Unbound,
            local_port: 0,
            local_ip: [0; 4],
            remote_ip: [0; 4],
            remote_port: 0,
            bound: false,
            shut_wr: false,
        };
        true
    }

    fn get(&self, idx: usize) -> Option<&UdpHandle> {
        if idx >= MAX_HANDLES {
            return None;
        }
        let h = &self.handles[idx];
        if h.state == HandleState::Free {
            return None;
        }
        Some(h)
    }

    fn get_mut(&mut self, idx: usize) -> Option<&mut UdpHandle> {
        if idx >= MAX_HANDLES {
            return None;
        }
        let h = &mut self.handles[idx];
        if h.state == HandleState::Free {
            return None;
        }
        Some(h)
    }

    fn is_port_bound(&self, port: u16) -> bool {
        self.bound_ports[..self.bound_count]
            .iter()
            .any(|&p| p == port)
    }

    fn bind_port(&mut self, port: u16) -> bool {
        if self.is_port_bound(port) {
            return false;
        }
        if self.bound_count >= MAX_BINDINGS {
            return false;
        }
        self.bound_ports[self.bound_count] = port;
        self.bound_count += 1;
        true
    }

    fn unbind_port(&mut self, port: u16) {
        for i in 0..self.bound_count {
            if self.bound_ports[i] == port {
                self.bound_ports[i] = self.bound_ports[self.bound_count - 1];
                self.bound_count -= 1;
                return;
            }
        }
    }

    fn alloc_ephemeral(&mut self) -> Option<u16> {
        for _ in 0..1024 {
            let port = self.ephemeral_counter;
            self.ephemeral_counter = self.ephemeral_counter.wrapping_add(1) | 0xC000;
            if !self.is_port_bound(port) {
                return Some(port);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Request handlers
// ---------------------------------------------------------------------------

fn handle_create(table: &mut HandleTable, idx: u64) -> (u64, u64) {
    let idx = idx as usize;
    if !table.create(idx) {
        return (NEG_ENOMEM, 0);
    }
    (0, idx as u64)
}

fn handle_bind(table: &mut HandleTable, handle: u64, port: u64, ip_u32: u64) -> (u64, u64) {
    let idx = handle as usize;
    let port = port as u16;
    let ip = [
        (ip_u32 >> 24) as u8,
        (ip_u32 >> 16) as u8,
        (ip_u32 >> 8) as u8,
        ip_u32 as u8,
    ];

    let h = match table.get_mut(idx) {
        Some(h) => h,
        None => return (NEG_EINVAL, 0),
    };

    if h.bound {
        return (NEG_EINVAL, 0); // already bound
    }

    if !table.bind_port(port) {
        return (NEG_EADDRINUSE, 0);
    }

    // Re-borrow after bind_port
    let h = table.get_mut(idx).unwrap();
    h.local_port = port;
    h.local_ip = ip;
    h.bound = true;
    h.state = HandleState::Bound;
    (0, 0)
}

fn handle_connect(table: &mut HandleTable, handle: u64, packed_ip_port: u64) -> (u64, u64) {
    let idx = handle as usize;
    let (ip, port) = unpack_ip_port(packed_ip_port);

    let needs_bind = match table.get(idx) {
        Some(h) => !h.bound,
        None => return (NEG_EINVAL, 0),
    };

    let mut ephemeral_port: u16 = 0;
    if needs_bind {
        let ep = match table.alloc_ephemeral() {
            Some(p) => p,
            None => return (NEG_EADDRINUSE, 0),
        };
        if !table.bind_port(ep) {
            return (NEG_EADDRINUSE, 0);
        }
        ephemeral_port = ep;
        let h = table.get_mut(idx).unwrap();
        h.local_port = ep;
        h.bound = true;
    }

    let h = table.get_mut(idx).unwrap();
    h.remote_ip = ip;
    h.remote_port = port;
    h.state = HandleState::Connected;
    (0, ephemeral_port as u64)
}

fn handle_sendto(table: &HandleTable, handle: u64, packed_dst: u64, _len: u64) -> (u64, u64) {
    let idx = handle as usize;
    let h = match table.get(idx) {
        Some(h) => h,
        None => return (NEG_EINVAL, 0),
    };

    if h.shut_wr {
        return (NEG_EPIPE, 0);
    }

    let (dst_ip, dst_port) = unpack_ip_port(packed_dst);
    // If no explicit destination, must be connected
    if dst_ip == [0, 0, 0, 0] && dst_port == 0 {
        if h.remote_port == 0 {
            return (NEG_ENOTCONN, 0);
        }
    }

    // Must be bound to have a source port
    if !h.bound || h.local_port == 0 {
        return (NEG_EINVAL, 0);
    }

    (0, h.local_port as u64)
}

fn handle_recvfrom(table: &HandleTable, handle: u64) -> (u64, u64) {
    let idx = handle as usize;
    let h = match table.get(idx) {
        Some(h) => h,
        None => return (NEG_EINVAL, 0),
    };

    if !h.bound || h.local_port == 0 {
        return (NEG_EINVAL, 0);
    }

    (0, h.local_port as u64)
}

fn handle_close(table: &mut HandleTable, handle: u64) -> (u64, u64) {
    let idx = handle as usize;
    let port = match table.get(idx) {
        Some(h) => {
            if h.bound {
                h.local_port
            } else {
                0
            }
        }
        None => return (NEG_EINVAL, 0),
    };

    if port != 0 {
        table.unbind_port(port);
    }
    if idx < MAX_HANDLES {
        table.handles[idx] = UdpHandle::free();
    }
    (0, port as u64)
}

// ---------------------------------------------------------------------------
// Server loop
// ---------------------------------------------------------------------------

fn server_loop(ep_handle: u32) -> ! {
    let mut table = HandleTable::new();
    let mut msg = syscall_lib::IpcMessage::new(0);
    let mut recv_buf = [0u8; 64];

    syscall_lib::ipc_recv_msg(ep_handle, &mut msg, &mut recv_buf);

    loop {
        let (reply_label, reply_data0) = dispatch(&mut table, &msg);

        syscall_lib::ipc_reply(REPLY_CAP_HANDLE, reply_label, reply_data0);

        msg = syscall_lib::IpcMessage::new(0);
        syscall_lib::ipc_recv_msg(ep_handle, &mut msg, &mut recv_buf);
    }
}

fn dispatch(table: &mut HandleTable, msg: &syscall_lib::IpcMessage) -> (u64, u64) {
    match msg.label {
        NET_UDP_CREATE => handle_create(table, msg.data[0]),
        NET_UDP_BIND => handle_bind(table, msg.data[0], msg.data[1], msg.data[2]),
        NET_UDP_CONNECT => handle_connect(table, msg.data[0], msg.data[1]),
        NET_UDP_SENDTO => handle_sendto(table, msg.data[0], msg.data[1], msg.data[2]),
        NET_UDP_RECVFROM => handle_recvfrom(table, msg.data[0]),
        NET_UDP_CLOSE => handle_close(table, msg.data[0]),
        _ => (NEG_EINVAL, 0),
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "net_server: starting (UDP service)\n");

    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "net_server: create_endpoint failed\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    let ret = syscall_lib::ipc_register_service(ep_handle, "net_udp");
    if ret == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "net_server: register_service failed\n");
        return 1;
    }

    syscall_lib::write_str(STDOUT_FILENO, "net_server: registered as net_udp\n");

    server_loop(ep_handle);
}
