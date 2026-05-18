//! Unix domain sockets — Phase 39.
//!
//! Provides `AF_UNIX` stream and datagram sockets for local IPC.
//! Uses a separate table from `SOCKET_TABLE` since IPv4-centric fields
//! (IP addresses, ports, TCP slots) do not apply to path-based semantics.

extern crate alloc;

use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
    vec::Vec,
};

use crate::process::FdBackend;
use crate::task::scheduler::IrqSafeMutex;
use crate::task::wait_queue::WaitQueue;

// ===========================================================================
// A.1 — UnixSocketType and UnixSocketState enums
// ===========================================================================

/// Unix socket type: stream (connection-oriented) or datagram (connectionless).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketType {
    Stream,
    Datagram,
}

/// Unix socket lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketState {
    Unbound,
    Bound,
    Listening,
    Connecting,
    Connected,
    #[allow(dead_code)]
    Closed,
}

// ===========================================================================
// A.2 — UnixSocket and UnixDatagram structs
// ===========================================================================

/// A single datagram message with sender information.
pub struct UnixDatagram {
    pub data: Vec<u8>,
    pub sender_path: Option<String>,
}

/// Maximum size of the stream receive buffer per socket.
pub const UNIX_STREAM_BUF_SIZE: usize = 8192;

/// Maximum number of queued datagrams per socket.
pub const UNIX_DGRAM_QUEUE_MAX: usize = 32;

/// One in-flight ancillary fd queued for delivery to a `recvmsg(2)` caller.
///
/// Phase 69d follow-up: tmux's client/server protocol relies on
/// `SCM_RIGHTS`-style fd-passing.  When `sendmsg` carries fds, the kernel
/// clones the underlying `FdBackend`, increments any associated refcounts
/// (pipe, PTY, socket, epoll), and parks the clone on the **receiver**'s
/// `anc_queue` tagged with the byte position of the **first** byte that
/// rides with it.  `recvmsg` drains the queue front whenever its byte
/// cursor crosses `deliver_at_stream_pos`.
pub struct InflightFd {
    pub backend: FdBackend,
    /// Sender's stored cloexec bit, captured at `sendmsg` time.  Note
    /// that the **receiver-visible** cloexec is decided at `recvmsg`
    /// time per Linux's contract: cleared by default, set when the
    /// caller passed `MSG_CMSG_CLOEXEC`.  This field is retained for
    /// diagnostics only and is not consulted on install.
    pub cloexec: bool,
    /// Sender's file offset at `sendmsg` time.  Preserved so a passed
    /// regular-file fd lands in the receiver at the position the
    /// sender had it; matches `dup(2)` semantics.
    pub offset: usize,
    /// Sender's `O_NONBLOCK` bit.
    pub nonblock: bool,
    /// Sender's read-permission bit.  An fd opened `O_WRONLY` stays
    /// write-only on the receiver.
    pub readable: bool,
    /// Sender's write-permission bit.
    pub writable: bool,
    /// Position in the receiver's incoming byte stream where this fd
    /// "rides".  Compared against the receiver's running consumed-bytes
    /// counter on every `recvmsg`.
    pub deliver_at_stream_pos: u64,
}

/// Per-socket kernel object for Unix domain sockets.
pub struct UnixSocket {
    pub socket_type: UnixSocketType,
    pub state: UnixSocketState,
    /// Filesystem path this socket is bound to (if any).
    pub path: Option<String>,
    /// Handle index of the peer socket (for connected stream or connected datagram).
    pub peer: Option<usize>,
    /// Stream receive buffer (byte-oriented ring buffer).
    pub recv_buf: VecDeque<u8>,
    /// Datagram receive queue (message-oriented).
    pub dgram_queue: VecDeque<UnixDatagram>,
    /// Pending connection backlog (handle indices of connecting sockets).
    pub backlog: VecDeque<usize>,
    /// Maximum backlog size for listening sockets.
    pub backlog_limit: usize,
    /// True if shutdown(SHUT_RD) was called.
    pub shut_rd: bool,
    /// True if shutdown(SHUT_WR) was called.
    pub shut_wr: bool,
    /// Reference count — number of FDs pointing to this socket.
    pub refcount: u32,
    /// Phase 69d follow-up: ancillary-data queue for `SCM_RIGHTS`.
    pub anc_queue: VecDeque<InflightFd>,
    /// Total bytes ever appended to `recv_buf` (monotonic; never wraps).
    /// Used as the timestamp for `InflightFd::deliver_at_stream_pos`.
    pub stream_pos_appended: u64,
    /// Total bytes ever consumed from `recv_buf` (monotonic).  Used to
    /// decide which ancillary entries are now eligible for delivery.
    pub stream_pos_consumed: u64,
}

impl UnixSocket {
    /// Create a new Unix socket of the given type with default state.
    pub fn new(socket_type: UnixSocketType) -> Self {
        Self {
            socket_type,
            state: UnixSocketState::Unbound,
            path: None,
            peer: None,
            recv_buf: VecDeque::new(),
            dgram_queue: VecDeque::new(),
            backlog: VecDeque::new(),
            backlog_limit: 0,
            shut_rd: false,
            shut_wr: false,
            refcount: 1,
            anc_queue: VecDeque::new(),
            stream_pos_appended: 0,
            stream_pos_consumed: 0,
        }
    }
}

// ===========================================================================
// A.3 — UNIX_SOCKET_TABLE global table
// ===========================================================================

/// Maximum number of Unix domain sockets system-wide.
pub const MAX_UNIX_SOCKETS: usize = 32;

struct UnixSocketTable {
    entries: [Option<UnixSocket>; MAX_UNIX_SOCKETS],
}

impl UnixSocketTable {
    const fn new() -> Self {
        const NONE: Option<UnixSocket> = None;
        Self {
            entries: [NONE; MAX_UNIX_SOCKETS],
        }
    }
}

// Phase 57b G.2.a — IrqSafeMutex inherits Track F.1's preempt-discipline.
// AF_UNIX sockets are touched only from socket syscalls (task context);
// no ISR holds this lock.  Pure type change.
static UNIX_SOCKET_TABLE: IrqSafeMutex<UnixSocketTable> = IrqSafeMutex::new(UnixSocketTable::new());

/// Allocate a new Unix socket entry. Returns the handle (index) or None if full.
pub fn alloc_unix_socket(socket_type: UnixSocketType) -> Option<usize> {
    let mut table = UNIX_SOCKET_TABLE.lock();
    for (i, slot) in table.entries.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(UnixSocket::new(socket_type));
            return Some(i);
        }
    }
    None
}

/// Decrement refcount; free the entry only when it reaches zero.
pub fn free_unix_socket(handle: usize) {
    let mut cleanup_path: Option<String> = None;
    let mut peer_handle: Option<usize> = None;
    let mut orphan_anc: alloc::vec::Vec<InflightFd> = alloc::vec::Vec::new();
    {
        let mut table = UNIX_SOCKET_TABLE.lock();
        let should_free =
            if let Some(entry) = table.entries.get_mut(handle).and_then(|s| s.as_mut()) {
                entry.refcount = entry.refcount.saturating_sub(1);
                entry.refcount == 0
            } else {
                return;
            };
        if should_free {
            if let Some(entry) = table.entries.get_mut(handle).and_then(|s| s.as_mut()) {
                cleanup_path = entry.path.clone();
                peer_handle = entry.peer;
                // Drain any pending backlog connections.
                entry.backlog.clear();
                // Phase 69d follow-up: collect undelivered SCM_RIGHTS
                // ancillary fds so refcounts on the underlying kernel
                // objects (pipes, PTYs, sockets, epoll) can be released
                // below — outside the table lock.
                while let Some(inflight) = entry.anc_queue.pop_front() {
                    orphan_anc.push(inflight);
                }
            }
            // Phase 69d follow-up (PR #177 review fix): purge cross-fd
            // flock state for this handle BEFORE the slot is cleared so
            // a concurrent allocator cannot recycle the handle and
            // surface a previous holder's lock.  Safe to call under the
            // table lock because the flock module only takes its own
            // mutex.
            crate::flock::unix_socket_purge(handle);
            // Unbind path before freeing the slot to prevent stale resolution.
            if let Some(ref path) = cleanup_path {
                unbind_path(path);
            }
            // Clear the peer's reference to this handle to prevent stale pointers.
            if let Some(ph) = peer_handle
                && let Some(peer_entry) = table.entries.get_mut(ph).and_then(|s| s.as_mut())
                && peer_entry.peer == Some(handle)
            {
                peer_entry.peer = None;
            }
            if let Some(slot) = table.entries.get_mut(handle) {
                *slot = None;
            }
        }
    }
    // Phase 69d follow-up: release refcounts on undelivered ancillary
    // fds so a `sendmsg(SCM_RIGHTS)` whose `recvmsg` never arrives does
    // not leak kernel objects (pipes, PTYs, sockets, epoll).
    for inflight in orphan_anc {
        release_inflight_anc_backend(&inflight);
    }
    // Wake peer so they see EOF/POLLHUP.
    if let Some(peer) = peer_handle {
        wake_unix_socket(peer);
    }
    // Wake any pollers on this socket.
    wake_unix_socket(handle);
}

/// Mirror of `crate::process::add_fd_refs` for a single backend — drop
/// the refcount the matching `acquire_inflight_anc_backend` bumped.
fn release_inflight_anc_backend(inflight: &InflightFd) {
    match &inflight.backend {
        FdBackend::PipeRead { pipe_id } => crate::pipe::pipe_close_reader(*pipe_id),
        FdBackend::PipeWrite { pipe_id } => crate::pipe::pipe_close_writer(*pipe_id),
        FdBackend::Socket { handle } => crate::net::release_socket_pub(*handle),
        FdBackend::UnixSocket { handle } => free_unix_socket(*handle),
        FdBackend::PtyMaster { pty_id } => crate::pty::close_master(*pty_id),
        FdBackend::PtySlave { pty_id } => crate::pty::close_slave(*pty_id),
        FdBackend::Epoll { instance_id } => crate::epoll::epoll_free_pub(*instance_id),
        _ => {
            // Path-backed fds (Tmpfs, Ext2, Fat32, Ramdisk, Dir, Proc, Dev*)
            // and virtual ones (Stdin/Stdout) have no refcount to drop.
        }
    }
}

/// Increment refcount (called when FD table is cloned on fork/dup).
pub fn add_unix_socket_ref(handle: usize) {
    let mut table = UNIX_SOCKET_TABLE.lock();
    if let Some(entry) = table.entries.get_mut(handle).and_then(|s| s.as_mut()) {
        entry.refcount += 1;
    }
}

/// Access a Unix socket entry immutably under the lock.
pub fn with_unix_socket<F, R>(handle: usize, f: F) -> Option<R>
where
    F: FnOnce(&UnixSocket) -> R,
{
    let table = UNIX_SOCKET_TABLE.lock();
    table.entries.get(handle)?.as_ref().map(f)
}

/// Access a Unix socket entry mutably under the lock.
pub fn with_unix_socket_mut<F, R>(handle: usize, f: F) -> Option<R>
where
    F: FnOnce(&mut UnixSocket) -> R,
{
    let mut table = UNIX_SOCKET_TABLE.lock();
    table.entries.get_mut(handle)?.as_mut().map(f)
}

/// Access two Unix socket entries mutably under the lock (for peer operations).
/// Returns None if either handle is invalid or they are the same.
#[allow(dead_code)]
pub fn with_unix_socket_pair<F, R>(h1: usize, h2: usize, f: F) -> Option<R>
where
    F: FnOnce(&mut UnixSocket, &mut UnixSocket) -> R,
{
    if h1 == h2 || h1 >= MAX_UNIX_SOCKETS || h2 >= MAX_UNIX_SOCKETS {
        return None;
    }
    let mut table = UNIX_SOCKET_TABLE.lock();
    // Split the entries slice to get mutable references to both.
    let (lo, hi) = if h1 < h2 { (h1, h2) } else { (h2, h1) };
    let (left, right) = table.entries.split_at_mut(hi);
    let lo_entry = left[lo].as_mut()?;
    let hi_entry = right[0].as_mut()?;
    if h1 < h2 {
        Some(f(lo_entry, hi_entry))
    } else {
        Some(f(hi_entry, lo_entry))
    }
}

// ===========================================================================
// A.4 — Unix socket WaitQueues
// ===========================================================================

/// Per-socket wait queues for blocking I/O and poll/epoll registration.
#[allow(clippy::declare_interior_mutable_const)]
pub static UNIX_SOCKET_WAITQUEUES: [WaitQueue; MAX_UNIX_SOCKETS] = {
    const WQ: WaitQueue = WaitQueue::new();
    [WQ; MAX_UNIX_SOCKETS]
};

/// Wake all tasks waiting on the given Unix socket.
pub fn wake_unix_socket(handle: usize) {
    if handle < MAX_UNIX_SOCKETS {
        UNIX_SOCKET_WAITQUEUES[handle].wake_all();
    }
}

// ===========================================================================
// D.4 — Path-to-handle map for named sockets
// ===========================================================================

// Phase 57b G.2.a — IrqSafeMutex inherits Track F.1's preempt-discipline.
// Path-map lookups happen only from task-context bind/connect syscalls.
static UNIX_PATH_MAP: IrqSafeMutex<BTreeMap<String, usize>> = IrqSafeMutex::new(BTreeMap::new());

/// Register a binding from a filesystem path to a Unix socket handle.
/// Returns `Err(())` if the path is already bound.
pub fn bind_path(path: &str, handle: usize) -> Result<(), ()> {
    let mut map = UNIX_PATH_MAP.lock();
    if map.contains_key(path) {
        return Err(());
    }
    map.insert(String::from(path), handle);
    Ok(())
}

/// Look up which Unix socket handle is bound to a given path.
pub fn lookup_path(path: &str) -> Option<usize> {
    let map = UNIX_PATH_MAP.lock();
    map.get(path).copied()
}

/// Remove the binding for a path (called on socket close or explicit unbind).
pub fn unbind_path(path: &str) {
    let mut map = UNIX_PATH_MAP.lock();
    map.remove(path);
}

// ===========================================================================
// E.3 — Stream read/write data path
// ===========================================================================

/// Write data to a connected stream socket's peer recv_buf.
/// Returns the number of bytes written, or a negative error.
pub fn unix_stream_write(handle: usize, data: &[u8]) -> Result<usize, i64> {
    let peer_handle = with_unix_socket(handle, |s| {
        if s.shut_wr {
            return Err(-32_i64); // EPIPE
        }
        match s.peer {
            Some(p) => Ok(p),
            None => Err(-107_i64), // ENOTCONN
        }
    })
    .ok_or(-9_i64)??; // EBADF

    // Check if peer is still alive and has space.
    let written = with_unix_socket_mut(peer_handle, |peer| {
        let space = UNIX_STREAM_BUF_SIZE.saturating_sub(peer.recv_buf.len());
        if space == 0 {
            return Err(-11_i64); // EAGAIN — buffer full
        }
        let n = data.len().min(space);
        peer.recv_buf.extend(&data[..n]);
        peer.stream_pos_appended = peer.stream_pos_appended.saturating_add(n as u64);
        Ok(n)
    })
    .ok_or(-32_i64)??; // EPIPE — peer socket freed

    wake_unix_socket(peer_handle);
    Ok(written)
}

/// Read data from a stream socket's own recv_buf.
/// Returns the number of bytes read (0 = EOF).
pub fn unix_stream_read(handle: usize, buf: &mut [u8]) -> Result<usize, i64> {
    let (n, peer, state, shut_rd) = with_unix_socket_mut(handle, |s| {
        let n = buf.len().min(s.recv_buf.len());
        for (i, byte) in s.recv_buf.drain(..n).enumerate() {
            buf[i] = byte;
        }
        s.stream_pos_consumed = s.stream_pos_consumed.saturating_add(n as u64);
        (n, s.peer, s.state, s.shut_rd)
    })
    .ok_or(-9_i64)?; // EBADF

    // Reject reads on unconnected sockets.
    if !matches!(state, UnixSocketState::Connected) && peer.is_none() && n == 0 {
        return Err(-107_i64); // ENOTCONN
    }

    // If we read data, wake the peer (space freed in recv_buf).
    if n > 0 {
        if let Some(p) = peer {
            wake_unix_socket(p);
        }
        return Ok(n);
    }

    // Buffer empty: check for EOF conditions.
    if shut_rd {
        return Ok(0); // shut_rd was set, return EOF
    }

    // Check if peer closed or shut_wr.
    let peer_alive = if let Some(p) = peer {
        with_unix_socket(p, |ps| !ps.shut_wr).unwrap_or(false)
    } else {
        false
    };
    if !peer_alive {
        return Ok(0); // EOF — peer gone or shut_wr
    }

    Err(-11_i64) // EAGAIN — no data yet, peer still alive
}

// ===========================================================================
// Phase 69d follow-up — ancillary fd passing (SCM_RIGHTS)
// ===========================================================================

/// Attach an `InflightFd` to the **peer**'s ancillary queue so that the
/// receiver's next `recvmsg` past the current write cursor materializes
/// the fd.  Caller is responsible for having cloned the underlying
/// `FdBackend` and bumped any associated refcount before parking it
/// here.  Returns `Err(-32)` (EPIPE) if the peer socket has been freed.
pub fn unix_stream_attach_anc(handle: usize, fds: alloc::vec::Vec<InflightFd>) -> Result<(), i64> {
    let peer_handle = with_unix_socket(handle, |s| s.peer).ok_or(-9_i64)?;
    let Some(peer) = peer_handle else {
        return Err(-107_i64); // ENOTCONN
    };
    with_unix_socket_mut(peer, |peer_sock| {
        for fd in fds {
            peer_sock.anc_queue.push_back(fd);
        }
    })
    .ok_or(-32_i64)?;
    // Wake the peer so a recvmsg blocked solely on ancillary data (zero
    // payload) unblocks immediately.
    wake_unix_socket(peer);
    Ok(())
}

/// Atomic ancillary-then-data delivery to a stream peer.  Mirrors the
/// Linux invariant that `sendmsg` delivers data and the cmsg as one
/// unit — both arrive on the receiver before any side-thread can wake
/// and start reading.  Returns the number of bytes written, or a
/// negative errno.
///
/// The inflight cmsgs are stamped with `deliver_at_stream_pos = current
/// peer.stream_pos_appended` under the same lock as the `recv_buf`
/// extend, so by the time the wake fires, the receiver sees both the
/// new bytes and the matching ancillary entries.
/// Inflight vector is taken by `&mut` so the caller still owns any
/// elements that were not consumed on the error path — letting it
/// release refcounts on the underlying kernel objects.  On success
/// the vector is drained empty.
pub fn unix_stream_write_with_anc(
    handle: usize,
    data: &[u8],
    inflight: &mut alloc::vec::Vec<InflightFd>,
) -> Result<usize, i64> {
    let peer_handle = with_unix_socket(handle, |s| {
        if s.shut_wr {
            return Err(-32_i64); // EPIPE
        }
        match s.peer {
            Some(p) => Ok(p),
            None => Err(-107_i64),
        }
    })
    .ok_or(-9_i64)??;

    let written = with_unix_socket_mut(peer_handle, |peer| {
        let space = UNIX_STREAM_BUF_SIZE.saturating_sub(peer.recv_buf.len());
        if space == 0 && !data.is_empty() {
            return Err(-11_i64); // EAGAIN
        }
        // Stamp inflight cmsgs with the **current** stream offset so
        // they ride with the first byte of this sendmsg.
        let deliver_at = peer.stream_pos_appended;
        for mut fd in inflight.drain(..) {
            fd.deliver_at_stream_pos = deliver_at;
            peer.anc_queue.push_back(fd);
        }
        let n = data.len().min(space);
        if n > 0 {
            peer.recv_buf.extend(&data[..n]);
            peer.stream_pos_appended = peer.stream_pos_appended.saturating_add(n as u64);
        }
        Ok(n)
    })
    .ok_or(-32_i64)??;

    wake_unix_socket(peer_handle);
    Ok(written)
}

/// Drain ancillary fds whose `deliver_at_stream_pos <= stream_pos_consumed`
/// from `handle`'s queue, returning them to the caller.  The caller is
/// responsible for materializing each one as a new fd in the receiver's
/// fd table.  Refcounts on the underlying kernel objects were already
/// incremented at `unix_stream_attach_anc` time, so the caller just
/// installs the entry — no further refcount manipulation.
pub fn unix_stream_drain_ready_anc(handle: usize, max_count: usize) -> alloc::vec::Vec<InflightFd> {
    let mut out = alloc::vec::Vec::new();
    with_unix_socket_mut(handle, |s| {
        let cursor = s.stream_pos_consumed;
        while out.len() < max_count {
            let Some(front) = s.anc_queue.front() else {
                break;
            };
            if front.deliver_at_stream_pos > cursor {
                break;
            }
            // SAFETY: front() succeeded so pop_front() must succeed.
            out.push(s.anc_queue.pop_front().unwrap());
        }
    });
    out
}

/// Look up the current `stream_pos_appended` on a peer socket.  Was
/// originally used by the two-phase `sys_sendmsg` (offset query then
/// write) before delivery became atomic in
/// `unix_stream_write_with_anc`; kept as a public helper for
/// diagnostics and any future caller that needs the peer's high-water
/// mark without taking a write lock.
pub fn peer_stream_pos_appended(handle: usize) -> Option<u64> {
    let peer = with_unix_socket(handle, |s| s.peer)?;
    let p = peer?;
    with_unix_socket(p, |s| s.stream_pos_appended)
}

// ===========================================================================
// F.1/F.2 — Datagram send/receive
// ===========================================================================

/// Send a datagram to a target Unix socket.
/// `target_handle` is the destination socket's handle.
pub fn unix_dgram_send(
    sender_path: Option<String>,
    target_handle: usize,
    data: &[u8],
) -> Result<usize, i64> {
    let n = data.len();
    with_unix_socket_mut(target_handle, |target| {
        if target.dgram_queue.len() >= UNIX_DGRAM_QUEUE_MAX {
            return Err(-11_i64); // EAGAIN
        }
        target.dgram_queue.push_back(UnixDatagram {
            data: Vec::from(data),
            sender_path,
        });
        Ok(n)
    })
    .ok_or(-111_i64)? // ECONNREFUSED — target gone
}

/// Receive a datagram from a datagram socket's own queue.
/// Returns (bytes_copied, sender_path).
pub fn unix_dgram_recv(handle: usize, buf: &mut [u8]) -> Result<(usize, Option<String>), i64> {
    let result = with_unix_socket_mut(handle, |s| match s.dgram_queue.pop_front() {
        Some(dgram) => {
            let n = buf.len().min(dgram.data.len());
            buf[..n].copy_from_slice(&dgram.data[..n]);
            Ok((n, dgram.sender_path))
        }
        None => Err(-11_i64), // EAGAIN
    })
    .ok_or(-9_i64)?; // EBADF

    // Wake senders/pollers that may be waiting for queue space.
    if result.is_ok() {
        wake_unix_socket(handle);
    }
    result
}
