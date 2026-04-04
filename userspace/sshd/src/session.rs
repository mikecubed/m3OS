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
use async_rt::reactor::Reactor;
use async_rt::sync::Mutex;
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

// ---------------------------------------------------------------------------
// Yield-once helper
// ---------------------------------------------------------------------------

struct YieldOnce {
    done: bool,
}

impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.done {
            Poll::Ready(())
        } else {
            self.done = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

fn yield_once() -> YieldOnce {
    YieldOnce { done: false }
}

// ---------------------------------------------------------------------------
// WaitReadable helper
// ---------------------------------------------------------------------------

struct WaitReadable {
    fd: i32,
    registered: bool,
}

impl Future for WaitReadable {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.registered {
            Poll::Ready(())
        } else {
            self.registered = true;
            executor::reactor().register_read(self.fd, cx.waker().clone());
            Poll::Pending
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run a complete SSH session on the given client socket.
pub fn run_session(sock_fd: i32, host_key: &HostKey) -> i32 {
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

    // Spawn the I/O task.
    let _io = executor::spawn(io_task(sock_fd, runner.clone(), state.clone()));

    // Spawn the progress task. Clone the host signing key so the spawned
    // task owns it ('static bound required by spawn).
    let host_sign_key = host_key.key.clone();
    let _prog = executor::spawn(progress_task(
        sock_fd,
        runner.clone(),
        state.clone(),
        chan.clone(),
        host_sign_key,
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
                    drain_pty_locked(&runner, sock_fd, pty_fd, &chan, &mut buf).await;
                }
                flush_output_locked(&runner, sock_fd).await;
                state.borrow_mut().shell_pid = None;
                state.borrow_mut().session_done = true;
                break;
            }
        }

        yield_once().await;
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

async fn io_task(sock_fd: i32, runner: SharedRunner, state: SharedState) {
    let mut sock_buf = [0u8; 4096];
    let mut pending = [0u8; 4096];
    let mut pending_len: usize = 0;

    loop {
        if state.borrow().session_done {
            return;
        }

        // --- Output direction: flush sunset output to socket ---
        flush_output_locked(&runner, sock_fd).await;

        // --- Input direction: feed pending bytes to runner ---
        while pending_len > 0 {
            let mut guard = runner.lock().await;
            match guard.input(&pending[..pending_len]) {
                Ok(0) => {
                    // Runner's input buffer is full — yield to let the
                    // progress task drain it, then retry next iteration.
                    drop(guard);
                    yield_once().await;
                    break;
                }
                Ok(c) => {
                    pending.copy_within(c..pending_len, 0);
                    pending_len -= c;
                    drop(guard);
                }
                Err(_) => {
                    state.borrow_mut().session_done = true;
                    state.borrow_mut().exit_code = 1;
                    return;
                }
            }
        }

        // Wait for socket to become readable.
        WaitReadable {
            fd: sock_fd,
            registered: false,
        }
        .await;

        if state.borrow().session_done {
            return;
        }

        // Read from socket.
        if pending_len == 0 {
            let n = syscall_lib::read(sock_fd, &mut sock_buf);
            if n > 0 {
                let mut consumed = 0;
                while consumed < n as usize {
                    let mut guard = runner.lock().await;
                    match guard.input(&sock_buf[consumed..n as usize]) {
                        Ok(0) => {
                            // Runner's input buffer is full — yield to let
                            // the progress task drain it. Stash unconsumed
                            // bytes below.
                            drop(guard);
                            yield_once().await;
                            break;
                        }
                        Ok(c) => {
                            consumed += c;
                            drop(guard);
                        }
                        Err(_) => {
                            state.borrow_mut().session_done = true;
                            state.borrow_mut().exit_code = 1;
                            return;
                        }
                    }
                }
                // Stash unconsumed bytes.
                if consumed < n as usize {
                    let remaining = n as usize - consumed;
                    let stash = remaining.min(pending.len());
                    pending[..stash].copy_from_slice(&sock_buf[consumed..consumed + stash]);
                    pending_len = stash;
                }
            } else if n == 0 {
                state.borrow_mut().session_done = true;
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
) {
    loop {
        if state.borrow().session_done {
            return;
        }

        flush_output_locked(&runner, sock_fd).await;

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
                                // Child process.
                                close(master);
                                close(sock_fd);
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
                yield_once().await;
                continue;
            }
            ProgressAction::SpawnRelay(pty_master) => {
                executor::spawn(channel_relay_task(
                    sock_fd,
                    pty_master,
                    runner.clone(),
                    state.clone(),
                    chan.clone(),
                ));
            }
            ProgressAction::Fatal => {
                state.borrow_mut().session_done = true;
                state.borrow_mut().exit_code = 1;
                return;
            }
            ProgressAction::Defunct => {
                state.borrow_mut().session_done = true;
                return;
            }
        }

        flush_output_locked(&runner, sock_fd).await;
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
) {
    let mut chan_buf = [0u8; 4096];
    let mut pty_buf = [0u8; 4096];
    let mut pty_pending = [0u8; 4096];
    let mut pty_pending_len: usize = 0;

    // PTY master is non-blocking. The relay task must wake on EITHER:
    //   - PTY readable (shell produced output) — via reactor
    //   - Channel data available (client typed something) — via sunset waker
    //
    // We capture the task's waker and register it with both the reactor
    // (for PTY readiness) and sunset (for channel read readiness). When
    // either source has data, the task wakes and handles both directions.

    loop {
        if state.borrow().session_done {
            return;
        }

        // --- Direction 1: runner channel -> PTY (client keystrokes -> shell) ---
        {
            let ch_ref = chan.borrow();
            if let Some(ref ch) = *ch_ref {
                let mut guard = runner.lock().await;
                loop {
                    match guard.read_channel(ch, ChanData::Normal, &mut chan_buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            write_all(pty_fd, &chan_buf[..n]);
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
                        flush_output_locked(&runner, sock_fd).await;
                        break;
                    }
                    Ok(w) => {
                        pty_pending.copy_within(w..pty_pending_len, 0);
                        pty_pending_len -= w;
                        drop(guard);
                        drop(ch_ref);
                        flush_output_locked(&runner, sock_fd).await;
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
                                flush_output_locked(&runner, sock_fd).await;
                                break;
                            }
                            Ok(w) => {
                                sent += w;
                                drop(guard);
                                drop(ch_ref);
                                flush_output_locked(&runner, sock_fd).await;
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

        flush_output_locked(&runner, sock_fd).await;

        if state.borrow().session_done {
            return;
        }

        // Register for BOTH wakeup sources before yielding:
        //   1. PTY readable — reactor register_read
        //   2. Channel data — sunset set_channel_read_waker
        // Both use the same task waker, so either source wakes the task.
        WaitPtyOrChannel {
            pty_fd,
            runner: &runner,
            chan: &chan,
            registered: false,
        }
        .await;
    }
}

/// Future that registers for both PTY readiness (reactor) and channel data
/// readiness (sunset waker), then yields once. Wakes when either has data.
struct WaitPtyOrChannel<'a> {
    pty_fd: i32,
    runner: &'a SharedRunner,
    chan: &'a SharedChan,
    registered: bool,
}

impl<'a> Future for WaitPtyOrChannel<'a> {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.registered {
            return Poll::Ready(());
        }
        self.registered = true;

        // Register PTY fd for read readiness with the reactor.
        executor::reactor().register_read(self.pty_fd, cx.waker().clone());

        // Register the same waker as sunset's channel read waker.
        // If the runner mutex is not contended (likely — I/O and progress tasks
        // yield frequently), we can set the waker. If contended, we skip it
        // and rely on the reactor + poll_once(0) to catch channel data on the
        // next iteration.
        let ch_ref = self.chan.borrow();
        if let Some(ref ch) = *ch_ref {
            // Try to set the channel read waker. We use the Mutex's try_lock
            // (fast path: if not locked, set waker; if locked, skip).
            if let Some(mut guard) = try_lock_mutex(self.runner) {
                guard.set_channel_read_waker(ch, ChanData::Normal, cx.waker());
            }
        }

        Poll::Pending
    }
}

/// Try to lock the async mutex synchronously (non-blocking).
/// Returns Some(guard) if the lock is not currently held, None otherwise.
fn try_lock_mutex<'a, T>(
    mutex: &'a async_rt::sync::Mutex<T>,
) -> Option<async_rt::sync::MutexGuard<'a, T>> {
    mutex.try_lock()
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
async fn flush_output_locked(runner: &SharedRunner, sock_fd: i32) {
    loop {
        let mut guard = runner.lock().await;
        let out = guard.output_buf();
        if out.is_empty() {
            break;
        }
        let mut tmp = [0u8; 4096];
        let chunk = out.len().min(tmp.len());
        tmp[..chunk].copy_from_slice(&out[..chunk]);
        drop(guard);

        let written = write_all_count(sock_fd, &tmp[..chunk]);
        if written == 0 {
            break;
        }

        let mut guard = runner.lock().await;
        guard.consume_output(written);
    }
}

/// Drain remaining PTY output through sunset and onto the socket, with locking.
async fn drain_pty_locked(
    runner: &SharedRunner,
    sock_fd: i32,
    pty_fd: i32,
    chan: &SharedChan,
    buf: &mut [u8],
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
                        flush_output_locked(runner, sock_fd).await;
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
            flush_output_locked(runner, sock_fd).await;
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

/// Write all bytes to a file descriptor, handling partial writes.
fn write_all(fd: i32, data: &[u8]) {
    let mut written = 0;
    while written < data.len() {
        let n = syscall_lib::write(fd, &data[written..]);
        if n <= 0 {
            break;
        }
        written += n as usize;
    }
}

/// Write all bytes to a file descriptor. Returns number of bytes written.
fn write_all_count(fd: i32, data: &[u8]) -> usize {
    let mut written = 0;
    while written < data.len() {
        let n = syscall_lib::write(fd, &data[written..]);
        if n <= 0 {
            break;
        }
        written += n as usize;
    }
    written
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
