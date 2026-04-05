//! SSH session handler — multi-task architecture (Phases 4–5).
//!
//! Runs three cooperating async tasks within the async-rt executor:
//!   1. **I/O task** — relays bytes between the network socket and sunset's
//!      input/output buffers.
//!   2. **Progress task** — drives `runner.progress()` and handles all SSH
//!      events (auth, channel open, PTY, shell spawn).
//!   3. **Channel relay task** — relays data between the PTY master and the
//!      sunset channel once a shell has been spawned.
//!
//! The `Runner` is shared via `Rc<Mutex<Runner>>` (our async Mutex). Every
//! event returned by `progress()` is handled — and its resume method called —
//! within the same Mutex lock scope, preventing the BadUsage recovery path.

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use async_rt::executor;
use async_rt::io::AsyncFd;
use async_rt::reactor::Reactor;
use async_rt::sync::{Mutex, Notify};
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use sunset::{ChanData, ChanHandle, Event, Runner, ServEvent, Server};
use syscall_lib::{
    STDOUT_FILENO, WNOHANG, close, dup2, exit, fork, set_nonblocking, setsid, waitpid, write_str,
};

use crate::auth;
use crate::host_key::HostKey;

/// Ioctl constants for PTY/terminal control.
const TIOCSCTTY: usize = 0x540E;

/// SSH buffer sizes — must fit the largest SSH packet.
const BUF_SIZE: usize = 36000;

/// Maximum authentication attempts before disconnecting.
const MAX_AUTH_ATTEMPTS: u32 = 6;
const NEG_EAGAIN: isize = -11;
const NEG_EINTR: isize = -4;

// ---------------------------------------------------------------------------
// Shared session state
// ---------------------------------------------------------------------------

/// Session metadata shared between tasks via `Rc<RefCell<..>>`.
///
/// The `ChanHandle` is stored separately in an `Rc<RefCell<Option<ChanHandle>>>`
/// because it is not `Clone` and must be borrowed by reference while the mutex
/// guard is also held.
struct SessionState {
    authenticated: bool,
    user_info: Option<auth::UserInfo>,
    pty_master: Option<i32>,
    pty_slave: Option<i32>,
    shell_pid: Option<isize>,
    shell_spawned: bool,
    session_done: bool,
    auth_attempts: u32,
    exit_code: i32,
}

impl SessionState {
    fn new() -> Self {
        Self {
            authenticated: false,
            user_info: None,
            pty_master: None,
            pty_slave: None,
            shell_pid: None,
            shell_spawned: false,
            session_done: false,
            auth_attempts: 0,
            exit_code: 0,
        }
    }
}

/// Type aliases for shared state.
type SharedRunner = Rc<Mutex<Runner<'static, Server>>>;
type SharedState = Rc<RefCell<SessionState>>;
type SharedChan = Rc<RefCell<Option<ChanHandle>>>;
type SharedNotify = Rc<Notify>;
type SharedOutputLock = Rc<Mutex<()>>;

// ---------------------------------------------------------------------------
// WaitWake helper
// ---------------------------------------------------------------------------

/// Park the current task until some registered waker fires.
///
/// Callers pair this with non-blocking I/O and retry their own read/write work
/// after wakeup, so the wake may come from the reactor or from runner/channel
/// wakers that share the same task waker.
struct WaitWake {
    fd: i32,
    registered: bool,
}

impl Future for WaitWake {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if fd_is_readable(self.fd) || self.registered {
            Poll::Ready(())
        } else {
            self.registered = true;
            executor::reactor().register_read(self.fd, cx.waker().clone());
            if fd_is_readable(self.fd) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run a complete SSH session on the given client socket.
pub fn run_session(sock_fd: i32, host_key: &HostKey) -> i32 {
    if set_nonblocking(sock_fd) < 0 {
        write_str(
            STDOUT_FILENO,
            "sshd: failed to set client socket nonblocking\n",
        );
        return 1;
    }
    let mut reactor = Reactor::new();
    executor::block_on(&mut reactor, async_session(sock_fd, host_key))
}

// ---------------------------------------------------------------------------
// Main async session
// ---------------------------------------------------------------------------

/// Async session handler — orchestrates three cooperating tasks.
///
/// The runner buffers are leaked to obtain `'static` references, which allows
/// spawning tasks that share the `Rc<Mutex<Runner>>`. This is acceptable
/// because each session runs in a forked child process that exits when done.
async fn async_session(sock_fd: i32, host_key: &HostKey) -> i32 {
    // Allocate runner buffers and leak them for 'static lifetime.
    // The forked child process exits after this function, so the leak is benign.
    let inbuf: &'static mut [u8] = Box::leak(alloc::vec![0u8; BUF_SIZE].into_boxed_slice());
    let outbuf: &'static mut [u8] = Box::leak(alloc::vec![0u8; BUF_SIZE].into_boxed_slice());
    let runner = Runner::new_server(inbuf, outbuf);

    let runner: SharedRunner = Rc::new(Mutex::new(runner));
    let state: SharedState = Rc::new(RefCell::new(SessionState::new()));
    let chan: SharedChan = Rc::new(RefCell::new(None));
    let progress_notify: SharedNotify = Rc::new(Notify::new());
    let session_notify: SharedNotify = Rc::new(Notify::new());
    let output_lock: SharedOutputLock = Rc::new(Mutex::new(()));

    // Spawn the I/O task.
    let _io = executor::spawn(io_task(
        sock_fd,
        runner.clone(),
        state.clone(),
        progress_notify.clone(),
        session_notify.clone(),
        output_lock.clone(),
    ));

    // Spawn the progress task. Clone the host signing key so the spawned
    // task owns it ('static bound required by spawn).
    let host_sign_key = host_key.key.clone();
    let _prog = executor::spawn(progress_task(
        sock_fd,
        runner.clone(),
        state.clone(),
        chan.clone(),
        host_sign_key,
        progress_notify.clone(),
        session_notify.clone(),
        output_lock.clone(),
    ));

    // Main loop: wait for session completion, check shell exit status.
    loop {
        if state.borrow().session_done {
            break;
        }

        // Check if shell has exited.
        let shell_pid = state.borrow().shell_pid;
        if let Some(pid) = shell_pid {
            let mut status: i32 = 0;
            let ret = waitpid(pid as i32, &mut status, WNOHANG);
            if ret > 0 {
                // Shell exited — drain remaining PTY output, then stop.
                let pty_master = state.borrow().pty_master;
                let has_chan = chan.borrow().is_some();
                if let (Some(pty_fd), true) = (pty_master, has_chan) {
                    let mut buf = [0u8; 4096];
                    drain_pty_locked(&runner, sock_fd, pty_fd, &chan, &mut buf, &output_lock).await;
                }
                if !flush_output_locked(&runner, sock_fd, &output_lock).await {
                    state.borrow_mut().exit_code = 1;
                }
                state.borrow_mut().shell_pid = None;
                state.borrow_mut().session_done = true;
                break;
            }
        }

        session_notify.wait().await;
    }

    let exit_code = state.borrow().exit_code;
    let shell_pid = state.borrow().shell_pid;
    let pty_master = state.borrow().pty_master;
    let pty_slave = state.borrow().pty_slave;
    cleanup(shell_pid, pty_master, pty_slave);
    close(sock_fd);
    exit_code
}

// ---------------------------------------------------------------------------
// Task 1: I/O Task — socket ↔ runner input/output
// ---------------------------------------------------------------------------

async fn io_task(
    sock_fd: i32,
    runner: SharedRunner,
    state: SharedState,
    progress_notify: SharedNotify,
    session_notify: SharedNotify,
    output_lock: SharedOutputLock,
) {
    let mut sock_buf = [0u8; 4096];
    let mut pending = [0u8; 4096];
    let mut pending_len: usize = 0;

    // The I/O task must wake on EITHER:
    //   - Socket readable (client sent data) — via reactor
    //   - Runner has output to send (channel data packetized) — via output_waker
    //
    // We register both wakers before yielding, so either event wakes this task.

    loop {
        if state.borrow().session_done {
            return;
        }

        // --- Output direction: flush sunset output to socket ---
        if !flush_output_locked(&runner, sock_fd, &output_lock).await {
            state.borrow_mut().session_done = true;
            state.borrow_mut().exit_code = 1;
            session_notify.signal();
            return;
        }

        // --- Input direction: feed pending bytes to runner ---
        while pending_len > 0 {
            let mut guard = runner.lock().await;
            match guard.input(&pending[..pending_len]) {
                Ok(0) => {
                    drop(guard);
                    progress_notify.signal();
                    session_notify.signal();
                    break;
                }
                Ok(c) => {
                    pending.copy_within(c..pending_len, 0);
                    pending_len -= c;
                    drop(guard);
                    progress_notify.signal();
                    session_notify.signal();
                }
                Err(_) => {
                    state.borrow_mut().session_done = true;
                    state.borrow_mut().exit_code = 1;
                    session_notify.signal();
                    return;
                }
            }
        }

        // Arm runner wakers before sleeping. If we already have buffered socket
        // data, wake when sunset is ready to accept more input again; otherwise
        // we can strand pending handshake bytes waiting on socket readability.
        let should_wait = {
            let waker = get_current_waker().await;
            let mut guard = runner.lock().await;
            let mut input_ready = false;
            if pending_len > 0 {
                if guard.is_input_ready() {
                    input_ready = true;
                } else {
                    guard.set_input_waker(&waker);
                }
            }
            guard.set_output_waker(&waker);
            !input_ready
        };

        if !should_wait {
            continue;
        }

        // Wait for socket readable (incoming data) or output waker (outgoing data).
        WaitWake {
            fd: sock_fd,
            registered: false,
        }
        .await;

        if state.borrow().session_done {
            return;
        }

        // Flush output first (handles the output_waker wakeup case).
        if !flush_output_locked(&runner, sock_fd, &output_lock).await {
            state.borrow_mut().session_done = true;
            state.borrow_mut().exit_code = 1;
            session_notify.signal();
            return;
        }

        // Read from the client socket directly — it is non-blocking, so wakes
        // from runner output or spurious polls are harmless retries.
        if pending_len == 0 {
            let n = syscall_lib::read(sock_fd, &mut sock_buf);
            if n > 0 {
                let mut consumed = 0;
                while consumed < n as usize {
                    let mut guard = runner.lock().await;
                    match guard.input(&sock_buf[consumed..n as usize]) {
                        Ok(0) => {
                            drop(guard);
                            progress_notify.signal();
                            session_notify.signal();
                            break;
                        }
                        Ok(c) => {
                            consumed += c;
                            drop(guard);
                            progress_notify.signal();
                            session_notify.signal();
                        }
                        Err(_) => {
                            state.borrow_mut().session_done = true;
                            state.borrow_mut().exit_code = 1;
                            session_notify.signal();
                            return;
                        }
                    }
                }
                if consumed < n as usize {
                    let remaining = n as usize - consumed;
                    let stash = remaining.min(pending.len());
                    pending[..stash].copy_from_slice(&sock_buf[consumed..consumed + stash]);
                    pending_len = stash;
                }
            } else if n == 0 {
                state.borrow_mut().session_done = true;
                session_notify.signal();
                return;
            } else if n != NEG_EAGAIN && n != NEG_EINTR {
                state.borrow_mut().session_done = true;
                state.borrow_mut().exit_code = 1;
                session_notify.signal();
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Task 2: Progress Task — drives sunset event loop
// ---------------------------------------------------------------------------

/// Post-event action — tells the progress loop what to do after releasing
/// the runner lock.
enum ProgressAction {
    /// Normal — flush output, continue to next event.
    Continue,
    /// `continue` the loop immediately (PollAgain / Progressed / SessionEnv).
    LoopContinue,
    /// Yield to let I/O task feed more data (Event::None).
    Yield,
    /// Spawn the channel relay task for the given PTY master fd.
    SpawnRelay(i32),
    /// Fatal — set session_done and return from the task.
    Fatal,
    /// Defunct — set session_done (graceful) and return.
    Defunct,
}

async fn progress_task(
    sock_fd: i32,
    runner: SharedRunner,
    state: SharedState,
    chan: SharedChan,
    host_sign_key: sunset::SignKey,
    progress_notify: SharedNotify,
    session_notify: SharedNotify,
    output_lock: SharedOutputLock,
) {
    loop {
        if state.borrow().session_done {
            return;
        }

        if !flush_output_locked(&runner, sock_fd, &output_lock).await {
            state.borrow_mut().session_done = true;
            state.borrow_mut().exit_code = 1;
            session_notify.signal();
            return;
        }

        // Acquire the runner lock, call progress(), handle the event
        // (including calling the resume method), then release the lock.
        // The action enum tells us what to do after the lock is released.
        let action = {
            let mut guard = runner.lock().await;
            match guard.progress() {
                Ok(Event::Serv(ServEvent::Hostkeys(hostkeys))) => {
                    if hostkeys.hostkeys(&[&host_sign_key]).is_err() {
                        ProgressAction::Fatal
                    } else {
                        ProgressAction::Continue
                    }
                }
                Ok(Event::Serv(ServEvent::FirstAuth(first_auth))) => {
                    let _ = first_auth.reject();
                    ProgressAction::Continue
                }
                Ok(Event::Serv(ServEvent::PasswordAuth(pw_auth))) => {
                    let mut st = state.borrow_mut();
                    st.auth_attempts += 1;
                    let attempts = st.auth_attempts;
                    drop(st);

                    let ok = match (pw_auth.username(), pw_auth.password()) {
                        (Ok(u), Ok(p)) => {
                            let u = String::from(u);
                            let p = String::from(p);
                            match auth::check_password(&u, &p) {
                                Some(info) => {
                                    let _ = pw_auth.allow();
                                    let mut st = state.borrow_mut();
                                    st.authenticated = true;
                                    st.user_info = Some(info);
                                    true
                                }
                                None => {
                                    let _ = pw_auth.reject();
                                    false
                                }
                            }
                        }
                        _ => {
                            let _ = pw_auth.reject();
                            false
                        }
                    };
                    if !ok && attempts >= MAX_AUTH_ATTEMPTS {
                        ProgressAction::Fatal
                    } else {
                        ProgressAction::Continue
                    }
                }
                Ok(Event::Serv(ServEvent::PubkeyAuth(pk_auth))) => {
                    let is_real = pk_auth.real();
                    let mut rejected = false;
                    let username = pk_auth.username().ok().map(String::from);
                    let pubkey = pk_auth.pubkey().ok();
                    let pk_bytes: Option<&[u8]> = pubkey.as_ref().and_then(|pk| match pk {
                        sunset::PubKey::Ed25519(ed_pk) => Some(ed_pk.key.0.as_slice()),
                        _ => None,
                    });
                    match (username.as_deref(), pk_bytes) {
                        (Some(u), Some(kb)) => match auth::check_pubkey(u, kb) {
                            Some(info) => {
                                if is_real {
                                    let mut st = state.borrow_mut();
                                    st.authenticated = true;
                                    st.user_info = Some(info);
                                }
                                let _ = pk_auth.allow();
                            }
                            None => {
                                if is_real {
                                    state.borrow_mut().auth_attempts += 1;
                                }
                                let _ = pk_auth.reject();
                                rejected = true;
                            }
                        },
                        _ => {
                            if is_real {
                                state.borrow_mut().auth_attempts += 1;
                            }
                            let _ = pk_auth.reject();
                            rejected = true;
                        }
                    }
                    let attempts = state.borrow().auth_attempts;
                    if rejected && attempts >= MAX_AUTH_ATTEMPTS {
                        ProgressAction::Fatal
                    } else {
                        ProgressAction::Continue
                    }
                }
                Ok(Event::Serv(ServEvent::OpenSession(open_session))) => {
                    let authenticated = state.borrow().authenticated;
                    if !authenticated {
                        let _ = open_session
                            .reject(sunset::ChanFail::SSH_OPEN_ADMINISTRATIVELY_PROHIBITED);
                    } else if let Ok(handle) = open_session.accept() {
                        *chan.borrow_mut() = Some(handle);
                    }
                    ProgressAction::Continue
                }
                Ok(Event::Serv(ServEvent::SessionPty(pty_req))) => {
                    match syscall_lib::openpty() {
                        Ok((master, slave)) => {
                            set_nonblocking(master);
                            let mut st = state.borrow_mut();
                            st.pty_master = Some(master);
                            st.pty_slave = Some(slave);
                            let _ = pty_req.succeed();
                        }
                        Err(_) => {
                            let _ = pty_req.fail();
                        }
                    }
                    ProgressAction::Continue
                }
                Ok(Event::Serv(ServEvent::SessionShell(shell_req))) => {
                    // Allocate PTY if not already done.
                    if state.borrow().pty_master.is_none() {
                        if let Ok((m, s)) = syscall_lib::openpty() {
                            set_nonblocking(m);
                            let mut st = state.borrow_mut();
                            st.pty_master = Some(m);
                            st.pty_slave = Some(s);
                        }
                    }
                    if state.borrow().shell_spawned {
                        let _ = shell_req.fail();
                        ProgressAction::Continue
                    } else {
                        let pty_master = state.borrow().pty_master;
                        let pty_slave = state.borrow().pty_slave;
                        let user_info = state.borrow().user_info.clone();
                        if let (Some(master), Some(slave), Some(info)) =
                            (pty_master, pty_slave, &user_info)
                        {
                            let pid = fork();
                            if pid < 0 {
                                write_str(STDOUT_FILENO, "sshd: shell fork failed\n");
                                let _ = shell_req.fail();
                                ProgressAction::Continue
                            } else if pid == 0 {
                                // Child process — close ALL inherited fds
                                // except the PTY slave. The child inherits the
                                // reactor self-pipe, socket, PTY master, etc.
                                for fd in 3..64 {
                                    if fd != slave {
                                        close(fd);
                                    }
                                }
                                spawn_shell(slave, &info);
                            } else {
                                // Parent.
                                close(slave);
                                {
                                    let mut st = state.borrow_mut();
                                    st.pty_slave = None;
                                    st.shell_pid = Some(pid);
                                    st.shell_spawned = true;
                                }
                                let _ = shell_req.succeed();
                                ProgressAction::SpawnRelay(master)
                            }
                        } else {
                            let _ = shell_req.fail();
                            ProgressAction::Continue
                        }
                    }
                }
                Ok(Event::Serv(ServEvent::SessionExec(exec_req))) => {
                    let _ = exec_req.fail();
                    ProgressAction::Continue
                }
                Ok(Event::Serv(ServEvent::SessionSubsystem(sub_req))) => {
                    let _ = sub_req.fail();
                    ProgressAction::Continue
                }
                Ok(Event::Serv(ServEvent::SessionEnv(env_req))) => {
                    let _ = env_req.fail();
                    ProgressAction::LoopContinue
                }
                Ok(Event::Serv(ServEvent::Defunct)) => ProgressAction::Defunct,
                Ok(Event::Serv(ServEvent::PollAgain) | Event::Progressed) => {
                    ProgressAction::LoopContinue
                }
                Ok(Event::None) => ProgressAction::Yield,
                Ok(_) => ProgressAction::Continue,
                Err(_) => ProgressAction::Continue,
            }
            // guard is dropped here — Event temporaries are gone
        };

        match action {
            ProgressAction::Continue => {}
            ProgressAction::LoopContinue => continue,
            ProgressAction::Yield => {
                progress_notify.wait().await;
                continue;
            }
            ProgressAction::SpawnRelay(pty_master) => {
                executor::spawn(channel_relay_task(
                    sock_fd,
                    pty_master,
                    runner.clone(),
                    state.clone(),
                    chan.clone(),
                    progress_notify.clone(),
                    session_notify.clone(),
                    output_lock.clone(),
                ));
            }
            ProgressAction::Fatal => {
                state.borrow_mut().session_done = true;
                state.borrow_mut().exit_code = 1;
                session_notify.signal();
                return;
            }
            ProgressAction::Defunct => {
                state.borrow_mut().session_done = true;
                session_notify.signal();
                return;
            }
        }

        if !flush_output_locked(&runner, sock_fd, &output_lock).await {
            state.borrow_mut().session_done = true;
            state.borrow_mut().exit_code = 1;
            session_notify.signal();
            return;
        }
        session_notify.signal();
    }
}

// ---------------------------------------------------------------------------
// Task 3: Channel Relay Task — PTY ↔ runner channel
// ---------------------------------------------------------------------------

async fn channel_relay_task(
    sock_fd: i32,
    pty_fd: i32,
    runner: SharedRunner,
    state: SharedState,
    chan: SharedChan,
    progress_notify: SharedNotify,
    session_notify: SharedNotify,
    output_lock: SharedOutputLock,
) {
    let mut chan_buf = [0u8; 4096];
    let mut pty_buf = [0u8; 4096];
    let mut pty_pending = [0u8; 4096];
    let mut pty_pending_len: usize = 0;
    // Pending data from channel -> PTY that couldn't be written (EAGAIN).
    let mut pty_write_pending = [0u8; 4096];
    let mut pty_write_pending_len: usize = 0;
    // The relay task wakes on:
    //   - PTY readable (shell produced output) — via reactor
    //   - Channel data available (client typed something) — via channel_read_waker
    //   - Channel write capacity returned (backpressure cleared) — via channel_write_waker

    loop {
        if state.borrow().session_done {
            return;
        }

        // --- Direction 1: runner channel -> PTY (client keystrokes -> shell) ---

        // First, drain any pending PTY write data from a previous EAGAIN.
        while pty_write_pending_len > 0 {
            let n = syscall_lib::write(pty_fd, &pty_write_pending[..pty_write_pending_len]);
            if n > 0 {
                let w = n as usize;
                pty_write_pending.copy_within(w..pty_write_pending_len, 0);
                pty_write_pending_len -= w;
            } else {
                break; // EAGAIN or error — retry next iteration
            }
        }

        if pty_write_pending_len == 0 {
            let ch_ref = chan.borrow();
            if let Some(ref ch) = *ch_ref {
                let mut guard = runner.lock().await;
                loop {
                    match guard.read_channel(ch, ChanData::Normal, &mut chan_buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut written = 0;
                            while written < n {
                                let w = syscall_lib::write(pty_fd, &chan_buf[written..n]);
                                if w > 0 {
                                    written += w as usize;
                                } else {
                                    break; // EAGAIN
                                }
                            }
                            // Stash unwritten data for retry.
                            if written < n {
                                let remaining = n - written;
                                let stash = remaining.min(pty_write_pending.len());
                                pty_write_pending[..stash]
                                    .copy_from_slice(&chan_buf[written..written + stash]);
                                pty_write_pending_len = stash;
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }

        // --- Direction 2: PTY -> runner channel (shell output -> client) ---

        // Drain pending PTY data first.
        while pty_pending_len > 0 {
            let ch_ref = chan.borrow();
            if let Some(ref ch) = *ch_ref {
                let mut guard = runner.lock().await;
                match guard.write_channel(ch, ChanData::Normal, &pty_pending[..pty_pending_len]) {
                    Ok(0) => {
                        drop(guard);
                        drop(ch_ref);
                        if !flush_output_locked(&runner, sock_fd, &output_lock).await {
                            state.borrow_mut().session_done = true;
                            state.borrow_mut().exit_code = 1;
                            session_notify.signal();
                            return;
                        }
                        break;
                    }
                    Ok(w) => {
                        pty_pending.copy_within(w..pty_pending_len, 0);
                        pty_pending_len -= w;
                        drop(guard);
                        drop(ch_ref);
                        if !flush_output_locked(&runner, sock_fd, &output_lock).await {
                            state.borrow_mut().session_done = true;
                            state.borrow_mut().exit_code = 1;
                            session_notify.signal();
                            return;
                        }
                        progress_notify.signal();
                        session_notify.signal();
                    }
                    Err(_) => break,
                }
            } else {
                break;
            }
        }

        // Read PTY (non-blocking). May return EAGAIN if no data.
        if pty_pending_len == 0 {
            let n = syscall_lib::read(pty_fd, &mut pty_buf);
            if n > 0 {
                let data = &pty_buf[..n as usize];
                let mut sent = 0;
                while sent < data.len() {
                    let ch_ref = chan.borrow();
                    if let Some(ref ch) = *ch_ref {
                        let mut guard = runner.lock().await;
                        match guard.write_channel(ch, ChanData::Normal, &data[sent..]) {
                            Ok(0) => {
                                drop(guard);
                                drop(ch_ref);
                                if !flush_output_locked(&runner, sock_fd, &output_lock).await {
                                    state.borrow_mut().session_done = true;
                                    state.borrow_mut().exit_code = 1;
                                    session_notify.signal();
                                    return;
                                }
                                break;
                            }
                            Ok(w) => {
                                sent += w;
                                drop(guard);
                                drop(ch_ref);
                                if !flush_output_locked(&runner, sock_fd, &output_lock).await {
                                    state.borrow_mut().session_done = true;
                                    state.borrow_mut().exit_code = 1;
                                    session_notify.signal();
                                    return;
                                }
                                progress_notify.signal();
                                session_notify.signal();
                            }
                            Err(_) => break,
                        }
                    } else {
                        break;
                    }
                }
                if sent < data.len() {
                    let remaining = data.len() - sent;
                    let stash = remaining.min(pty_pending.len());
                    pty_pending[..stash].copy_from_slice(&data[sent..sent + stash]);
                    pty_pending_len = stash;
                }
            }
        }

        if !flush_output_locked(&runner, sock_fd, &output_lock).await {
            state.borrow_mut().session_done = true;
            state.borrow_mut().exit_code = 1;
            session_notify.signal();
            return;
        }

        if state.borrow().session_done {
            return;
        }

        // Register wakers before sleeping. The relay must wake on:
        //   - PTY readability (reactor)
        //   - Channel read readiness (client sent data)
        //   - Channel write readiness (backpressure cleared, if we have pending data)
        let should_wait = {
            let waker = get_current_waker().await;
            let mut guard = runner.lock().await;
            let ch_ref = chan.borrow();
            let mut channel_read_ready = false;
            let mut channel_write_ready = false;
            if let Some(ref ch) = *ch_ref {
                channel_read_ready = matches!(
                    guard.read_channel_ready(),
                    Some((num, ChanData::Normal, len)) if num == ch.num() && len > 0
                );
                if !channel_read_ready {
                    guard.set_channel_read_waker(ch, ChanData::Normal, &waker);
                }
                if pty_pending_len > 0 {
                    match guard.write_channel_ready(ch, ChanData::Normal) {
                        Ok(Some(len)) if len > 0 => channel_write_ready = true,
                        Ok(Some(_)) => guard.set_channel_write_waker(ch, ChanData::Normal, &waker),
                        Ok(None) | Err(_) => channel_write_ready = true,
                    }
                }
            }
            drop(ch_ref);
            drop(guard);
            !(channel_read_ready || channel_write_ready || fd_is_readable(pty_fd))
        };
        if !should_wait {
            continue;
        }
        WaitWake {
            fd: pty_fd,
            registered: false,
        }
        .await;
    }
}

/// Capture the current task's waker. Returns immediately on first poll.
struct GetWaker;

impl Future for GetWaker {
    type Output = core::task::Waker;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<core::task::Waker> {
        Poll::Ready(cx.waker().clone())
    }
}

async fn get_current_waker() -> core::task::Waker {
    GetWaker.await
}

fn fd_is_readable(fd: i32) -> bool {
    let mut pfd = syscall_lib::PollFd {
        fd,
        events: syscall_lib::POLLIN,
        revents: 0,
    };
    let ready = syscall_lib::poll(core::slice::from_mut(&mut pfd), 0);
    ready > 0
        && (pfd.revents & (syscall_lib::POLLIN | syscall_lib::POLLHUP | syscall_lib::POLLERR)) != 0
}

// ---------------------------------------------------------------------------
// Shell spawning (child process)
// ---------------------------------------------------------------------------

/// Set up the PTY slave and exec the user's shell. Does not return on success.
fn spawn_shell(slave: i32, info: &auth::UserInfo) -> ! {
    if setsid() < 0 {
        write_str(STDOUT_FILENO, "sshd: setsid failed\n");
        exit(1);
    }
    if syscall_lib::ioctl(slave, TIOCSCTTY, 0) < 0 {
        write_str(STDOUT_FILENO, "sshd: TIOCSCTTY failed\n");
        exit(1);
    }
    if dup2(slave, 0) < 0 || dup2(slave, 1) < 0 || dup2(slave, 2) < 0 {
        write_str(STDOUT_FILENO, "sshd: dup2 failed\n");
        exit(1);
    }
    if slave > 2 {
        close(slave);
    }
    if syscall_lib::setgid(info.gid) < 0 {
        write_str(STDOUT_FILENO, "sshd: setgid failed\n");
        exit(1);
    }
    if syscall_lib::setuid(info.uid) < 0 {
        write_str(STDOUT_FILENO, "sshd: setuid failed\n");
        exit(1);
    }
    let home_bytes = info.home.as_bytes();
    let mut home_env = [0u8; 128];
    let he_len = build_env(b"HOME=", home_bytes, &mut home_env);
    let mut user_env = [0u8; 128];
    let ue_len = build_env(b"USER=", info.username.as_bytes(), &mut user_env);
    let env_path: &[u8] = b"PATH=/bin:/sbin:/usr/bin\0";
    let env_term: &[u8] = b"TERM=xterm\0";
    let env_editor: &[u8] = b"EDITOR=/bin/edit\0";
    let envp: [*const u8; 6] = [
        env_path.as_ptr(),
        home_env[..he_len].as_ptr(),
        env_term.as_ptr(),
        env_editor.as_ptr(),
        user_env[..ue_len].as_ptr(),
        core::ptr::null(),
    ];
    let mut shell_path = [0u8; 128];
    let shell_bytes = info.shell.as_bytes();
    let slen = shell_bytes.len().min(shell_path.len() - 1);
    shell_path[..slen].copy_from_slice(&shell_bytes[..slen]);
    shell_path[slen] = 0;
    let argv: [*const u8; 2] = [shell_path[..slen + 1].as_ptr(), core::ptr::null()];
    let ret = syscall_lib::execve(&shell_path[..slen + 1], &argv, &envp);
    if ret < 0 {
        let sh0: &[u8] = b"/bin/sh0\0";
        let argv2: [*const u8; 2] = [sh0.as_ptr(), core::ptr::null()];
        syscall_lib::execve(sh0, &argv2, &envp);
    }
    exit(1);
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Flush sunset's output buffer to the socket, acquiring the mutex.
async fn flush_output_locked(
    runner: &SharedRunner,
    sock_fd: i32,
    output_lock: &SharedOutputLock,
) -> bool {
    let _flush_guard = output_lock.lock().await;
    loop {
        let mut guard = runner.lock().await;
        let out = guard.output_buf();
        if out.is_empty() {
            return true;
        }
        let mut tmp = [0u8; 4096];
        let chunk = out.len().min(tmp.len());
        tmp[..chunk].copy_from_slice(&out[..chunk]);
        drop(guard);

        if write_all_nonblocking(sock_fd, &tmp[..chunk]).await.is_err() {
            return false;
        }

        let mut guard = runner.lock().await;
        guard.consume_output(chunk);
    }
}

async fn write_all_nonblocking(fd: i32, data: &[u8]) -> Result<(), ()> {
    let async_fd = AsyncFd::new(fd);
    let mut written = 0usize;
    while written < data.len() {
        let n = syscall_lib::write(fd, &data[written..]);
        if n > 0 {
            written += n as usize;
        } else if n == NEG_EINTR {
            continue;
        } else if n == NEG_EAGAIN {
            async_fd.writable().await;
        } else {
            return Err(());
        }
    }
    Ok(())
}

/// Drain remaining PTY output through sunset and onto the socket, with locking.
async fn drain_pty_locked(
    runner: &SharedRunner,
    sock_fd: i32,
    pty_fd: i32,
    chan: &SharedChan,
    buf: &mut [u8],
    output_lock: &SharedOutputLock,
) {
    loop {
        let n = syscall_lib::read(pty_fd, buf);
        if n <= 0 {
            break;
        }
        let data_len = n as usize;
        let mut sent = 0;
        while sent < data_len {
            let ch_ref = chan.borrow();
            if let Some(ref ch) = *ch_ref {
                let mut guard = runner.lock().await;
                match guard.write_channel(ch, ChanData::Normal, &buf[sent..data_len]) {
                    Ok(0) => {
                        drop(guard);
                        drop(ch_ref);
                        if !flush_output_locked(runner, sock_fd, output_lock).await {
                            return;
                        }
                        // Retry once.
                        let ch_ref2 = chan.borrow();
                        if let Some(ref ch2) = *ch_ref2 {
                            let mut g2 = runner.lock().await;
                            match g2.write_channel(ch2, ChanData::Normal, &buf[sent..data_len]) {
                                Ok(0) | Err(_) => break,
                                Ok(w) => sent += w,
                            }
                        } else {
                            break;
                        }
                    }
                    Ok(w) => {
                        sent += w;
                        drop(guard);
                        drop(ch_ref);
                    }
                    Err(_) => break,
                }
            } else {
                break;
            }
            if !flush_output_locked(runner, sock_fd, output_lock).await {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Clean up all session resources.
fn cleanup(shell_pid: Option<isize>, pty_master: Option<i32>, pty_slave: Option<i32>) {
    if let Some(pid) = shell_pid {
        syscall_lib::kill(pid as i32, 1); // SIGHUP
        let mut status: i32 = 0;
        waitpid(pid as i32, &mut status, 0);
    }
    if let Some(fd) = pty_master {
        close(fd);
    }
    if let Some(fd) = pty_slave {
        close(fd);
    }
}

/// Build an environment string like "KEY=value\0".
/// Always NUL-terminates, reserving one byte for the terminator.
fn build_env(prefix: &[u8], value: &[u8], buf: &mut [u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }

    let mut pos = 0;
    let max = buf.len() - 1;

    for &b in prefix {
        if pos < max {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in value {
        if pos < max {
            buf[pos] = b;
            pos += 1;
        }
    }

    buf[pos] = 0;
    pos + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_rt::executor::block_on;
    use std::thread;
    use std::time::Duration;
    use std::vec::Vec;

    fn make_pipe() -> (i32, i32) {
        let mut fds = [0i32; 2];
        assert_eq!(syscall_lib::pipe(&mut fds), 0);
        (fds[0], fds[1])
    }

    #[test]
    fn write_all_nonblocking_writes_immediately_when_fd_is_writable() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();

        block_on(&mut reactor, async {
            write_all_nonblocking(write_fd, b"hello async ssh")
                .await
                .unwrap();
        });

        let mut buf = [0u8; 32];
        let n = syscall_lib::read(read_fd, &mut buf);
        assert_eq!(n, 15);
        assert_eq!(&buf[..n as usize], b"hello async ssh");

        close(read_fd);
        close(write_fd);
    }

    #[test]
    fn write_all_nonblocking_waits_for_pipe_space() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();
        assert_eq!(set_nonblocking(write_fd), 0);

        let fill = [0xAAu8; 4096];
        loop {
            let n = syscall_lib::write(write_fd, &fill);
            if n < 0 {
                break;
            }
        }

        let payload = b"ssh flush survives eagain";
        let drainer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            let mut drain = [0u8; 8192];
            let _ = syscall_lib::read(read_fd, &mut drain);
            read_fd
        });

        block_on(&mut reactor, async {
            write_all_nonblocking(write_fd, payload).await.unwrap();
        });

        close(write_fd);
        let read_fd = drainer.join().unwrap();
        let mut out = Vec::new();
        loop {
            let mut chunk = [0u8; 4096];
            let n = syscall_lib::read(read_fd, &mut chunk);
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n as usize]);
        }
        assert!(out.ends_with(payload));
        close(read_fd);
    }
}
