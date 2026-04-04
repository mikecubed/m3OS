//! SSH session handler (Tracks C, D, E).
//!
//! Manages the lifecycle of a single SSH connection: key exchange, authentication,
//! channel management, PTY allocation, shell spawning, and data relay.
//!
//! Uses the async-rt executor to drive I/O readiness and sunset event processing.

extern crate alloc;

use alloc::string::String;
use async_rt::executor;
use async_rt::reactor::Reactor;
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

/// C.2/E.4: Run a complete SSH session on the given client socket.
pub fn run_session(sock_fd: i32, host_key: &HostKey) -> i32 {
    let mut reactor = Reactor::new();
    executor::block_on(&mut reactor, async_session(sock_fd, host_key))
}

/// Async session handler — driven by the executor's reactor.
async fn async_session(sock_fd: i32, host_key: &HostKey) -> i32 {
    let mut inbuf = alloc::vec![0u8; BUF_SIZE];
    let mut outbuf = alloc::vec![0u8; BUF_SIZE];
    let mut runner = Runner::new_server(&mut inbuf, &mut outbuf);

    // Socket stays blocking — the reactor tells us when data is available,
    // and blocking writes ensure flush_output always completes. Only the PTY
    // master is set to non-blocking (for select-like behavior).

    // Session state.
    let mut authenticated = false;
    let mut user_info: Option<auth::UserInfo> = None;
    let mut chan_handle: Option<ChanHandle> = None;
    let mut pty_master: Option<i32> = None;
    let mut pty_slave: Option<i32> = None;
    let mut shell_pid: Option<isize> = None;
    let mut auth_attempts: u32 = 0;
    let mut shell_spawned = false;

    let mut sock_buf = [0u8; 4096];
    let mut pty_buf = [0u8; 4096];
    let mut chan_buf = [0u8; 4096];

    // Pending data buffers: carry unconsumed bytes across main loop iterations
    // when sunset's input buffer is full (backpressure).
    let mut sock_pending_buf = [0u8; 4096];
    let mut sock_pending_len: usize = 0;
    let mut pty_pending_buf = [0u8; 4096];
    let mut pty_pending_len: usize = 0;

    loop {
        // Flush pending output.
        flush_output(&mut runner, sock_fd);

        // Check if shell has exited.
        if let Some(pid) = shell_pid {
            let mut status: i32 = 0;
            let ret = waitpid(pid as i32, &mut status, WNOHANG);
            if ret > 0 {
                shell_pid = None;
                drain_pty(&mut runner, sock_fd, pty_master, &chan_handle, &mut pty_buf);
                flush_output(&mut runner, sock_fd);
                break;
            }
        }

        // Wait for socket or PTY readiness via the reactor.
        WaitIo::new(sock_fd, pty_master).await;

        // Feed any pending bytes to sunset first.
        while sock_pending_len > 0 {
            match runner.input(&sock_pending_buf[..sock_pending_len]) {
                Ok(0) => {
                    flush_output(&mut runner, sock_fd);
                    let _ = runner.progress();
                    flush_output(&mut runner, sock_fd);
                    break;
                }
                Ok(c) => {
                    sock_pending_buf.copy_within(c..sock_pending_len, 0);
                    sock_pending_len -= c;
                }
                Err(_) => {
                    cleanup(shell_pid, pty_master, pty_slave);
                    return 1;
                }
            }
        }

        // Socket readable: feed bytes to sunset.
        if sock_pending_len == 0 {
            let n = syscall_lib::read(sock_fd, &mut sock_buf);
            if n > 0 {
                let mut consumed = 0;
                while consumed < n as usize {
                    match runner.input(&sock_buf[consumed..n as usize]) {
                        Ok(0) => {
                            flush_output(&mut runner, sock_fd);
                            let _ = runner.progress();
                            flush_output(&mut runner, sock_fd);
                            break;
                        }
                        Ok(c) => consumed += c,
                        Err(_) => {
                            cleanup(shell_pid, pty_master, pty_slave);
                            return 1;
                        }
                    }
                }
                // Stash unconsumed bytes for next iteration.
                if consumed < n as usize {
                    let remaining = n as usize - consumed;
                    let stash = remaining.min(sock_pending_buf.len());
                    sock_pending_buf[..stash]
                        .copy_from_slice(&sock_buf[consumed..consumed + stash]);
                    sock_pending_len = stash;
                }
            } else if n == 0 {
                // EOF on socket — remote closed.
                break;
            }
            // n < 0: no data yet (shouldn't happen with blocking socket + reactor,
            // but harmless — just continue to the event loop).
        }

        // Drain any pending PTY data into the channel first.
        if let Some(ref ch) = chan_handle {
            while pty_pending_len > 0 {
                match runner.write_channel(
                    ch,
                    ChanData::Normal,
                    &pty_pending_buf[..pty_pending_len],
                ) {
                    Ok(0) => {
                        flush_output(&mut runner, sock_fd);
                        break;
                    }
                    Ok(w) => {
                        pty_pending_buf.copy_within(w..pty_pending_len, 0);
                        pty_pending_len -= w;
                        flush_output(&mut runner, sock_fd);
                    }
                    Err(_) => break,
                }
            }
        }

        // PTY readable: read from PTY, send as channel data.
        if pty_pending_len == 0 {
            if let Some(pty_fd) = pty_master {
                let n = syscall_lib::read(pty_fd, &mut pty_buf);
                if n > 0 {
                    if let Some(ref ch) = chan_handle {
                        let data = &pty_buf[..n as usize];
                        let mut sent = 0;
                        while sent < data.len() {
                            match runner.write_channel(ch, ChanData::Normal, &data[sent..]) {
                                Ok(0) => {
                                    flush_output(&mut runner, sock_fd);
                                    break;
                                }
                                Ok(w) => sent += w,
                                Err(_) => break,
                            }
                            flush_output(&mut runner, sock_fd);
                        }
                        // Stash unsent bytes for next iteration.
                        if sent < data.len() {
                            let remaining = data.len() - sent;
                            let stash = remaining.min(pty_pending_buf.len());
                            pty_pending_buf[..stash].copy_from_slice(&data[sent..sent + stash]);
                            pty_pending_len = stash;
                        }
                    }
                }
            }
        }

        // Drive sunset event loop.
        loop {
            flush_output(&mut runner, sock_fd);
            match runner.progress() {
                Ok(Event::Serv(ServEvent::Hostkeys(hostkeys))) => {
                    if hostkeys.hostkeys(&[&host_key.key]).is_err() {
                        cleanup(shell_pid, pty_master, pty_slave);
                        return 1;
                    }
                    break;
                }
                Ok(Event::Serv(ServEvent::FirstAuth(first_auth))) => {
                    let _ = first_auth.reject();
                    break;
                }
                Ok(Event::Serv(ServEvent::PasswordAuth(pw_auth))) => {
                    auth_attempts += 1;
                    let ok = match (pw_auth.username(), pw_auth.password()) {
                        (Ok(u), Ok(p)) => {
                            let u = String::from(u);
                            let p = String::from(p);
                            match auth::check_password(&u, &p) {
                                Some(info) => {
                                    authenticated = true;
                                    user_info = Some(info);
                                    let _ = pw_auth.allow();
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
                    if !ok && auth_attempts >= MAX_AUTH_ATTEMPTS {
                        cleanup(shell_pid, pty_master, pty_slave);
                        return 1;
                    }
                    break;
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
                                    authenticated = true;
                                    user_info = Some(info);
                                }
                                let _ = pk_auth.allow();
                            }
                            None => {
                                if is_real {
                                    auth_attempts += 1;
                                }
                                let _ = pk_auth.reject();
                                rejected = true;
                            }
                        },
                        _ => {
                            if is_real {
                                auth_attempts += 1;
                            }
                            let _ = pk_auth.reject();
                            rejected = true;
                        }
                    }
                    if rejected && auth_attempts >= MAX_AUTH_ATTEMPTS {
                        cleanup(shell_pid, pty_master, pty_slave);
                        return 1;
                    }
                    break;
                }
                Ok(Event::Serv(ServEvent::OpenSession(open_session))) => {
                    if !authenticated {
                        let _ = open_session
                            .reject(sunset::ChanFail::SSH_OPEN_ADMINISTRATIVELY_PROHIBITED);
                    } else {
                        match open_session.accept() {
                            Ok(handle) => {
                                chan_handle = Some(handle);
                            }
                            Err(_) => {}
                        }
                    }
                    break;
                }
                Ok(Event::Serv(ServEvent::SessionPty(pty_req))) => {
                    match syscall_lib::openpty() {
                        Ok((master, slave)) => {
                            set_nonblocking(master);
                            pty_master = Some(master);
                            pty_slave = Some(slave);
                            let _ = pty_req.succeed();
                        }
                        Err(_) => {
                            let _ = pty_req.fail();
                        }
                    }
                    break;
                }
                Ok(Event::Serv(ServEvent::SessionShell(shell_req))) => {
                    // Allocate PTY if not already done.
                    if pty_master.is_none() {
                        if let Ok((m, s)) = syscall_lib::openpty() {
                            set_nonblocking(m);
                            pty_master = Some(m);
                            pty_slave = Some(s);
                        }
                    }
                    if shell_spawned {
                        let _ = shell_req.fail();
                    } else if let (Some(master), Some(slave), Some(info)) =
                        (pty_master, pty_slave, &user_info)
                    {
                        let pid = fork();
                        if pid < 0 {
                            write_str(STDOUT_FILENO, "sshd: shell fork failed\n");
                            let _ = shell_req.fail();
                        } else if pid == 0 {
                            close(master);
                            close(sock_fd);
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
                            let ue_len =
                                build_env(b"USER=", info.username.as_bytes(), &mut user_env);
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
                            let argv: [*const u8; 2] =
                                [shell_path[..slen + 1].as_ptr(), core::ptr::null()];
                            let ret = syscall_lib::execve(&shell_path[..slen + 1], &argv, &envp);
                            if ret < 0 {
                                let sh0: &[u8] = b"/bin/sh0\0";
                                let argv2: [*const u8; 2] = [sh0.as_ptr(), core::ptr::null()];
                                syscall_lib::execve(sh0, &argv2, &envp);
                            }
                            exit(1);
                        } else {
                            close(slave);
                            pty_slave = None;
                            shell_pid = Some(pid);
                            shell_spawned = true;
                            let _ = shell_req.succeed();
                        }
                    } else {
                        let _ = shell_req.fail();
                    }
                    break;
                }
                Ok(Event::Serv(ServEvent::SessionExec(exec_req))) => {
                    let _ = exec_req.fail();
                    break;
                }
                Ok(Event::Serv(ServEvent::SessionSubsystem(sub_req))) => {
                    let _ = sub_req.fail();
                    break;
                }
                Ok(Event::Serv(ServEvent::SessionEnv(env_req))) => {
                    let _ = env_req.fail();
                    continue;
                }
                Ok(Event::Serv(ServEvent::Defunct)) => {
                    cleanup(shell_pid, pty_master, pty_slave);
                    return 0;
                }
                Ok(Event::Serv(ServEvent::PollAgain) | Event::Progressed) => continue,
                Ok(Event::None) => break,
                Ok(_) => break,
                Err(_) => {
                    break;
                }
            }
        } // end inner event loop

        // Flush any responses generated by the event loop.
        flush_output(&mut runner, sock_fd);

        // Read channel data from sunset and write to PTY.
        if let (Some(ch), Some(pty_fd)) = (&chan_handle, pty_master) {
            loop {
                match runner.read_channel(ch, ChanData::Normal, &mut chan_buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        write_all(pty_fd, &chan_buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        }
    }

    cleanup(shell_pid, pty_master, pty_slave);
    0
}

/// A future that registers socket (and optional PTY) FDs with the reactor,
/// yields once to let the executor call poll_once(), then completes.
struct WaitIo {
    sock_fd: i32,
    pty_fd: Option<i32>,
    registered: bool,
}

impl WaitIo {
    fn new(sock_fd: i32, pty_fd: Option<i32>) -> Self {
        Self {
            sock_fd,
            pty_fd,
            registered: false,
        }
    }
}

impl core::future::Future for WaitIo {
    type Output = ();

    fn poll(
        mut self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<()> {
        if self.registered {
            core::task::Poll::Ready(())
        } else {
            self.registered = true;
            let reactor = executor::reactor();
            reactor.register_read(self.sock_fd, cx.waker().clone());
            if let Some(pty_fd) = self.pty_fd {
                reactor.register_read(pty_fd, cx.waker().clone());
            }
            core::task::Poll::Pending
        }
    }
}

/// Drain remaining PTY output through sunset and onto the socket.
fn drain_pty(
    runner: &mut Runner<'_, Server>,
    sock_fd: i32,
    pty_master: Option<i32>,
    chan_handle: &Option<ChanHandle>,
    buf: &mut [u8],
) {
    let pty_fd = match pty_master {
        Some(fd) => fd,
        None => return,
    };
    let ch = match chan_handle {
        Some(h) => h,
        None => return,
    };
    loop {
        let n = syscall_lib::read(pty_fd, buf);
        if n <= 0 {
            break;
        }
        let data = &buf[..n as usize];
        let mut sent = 0;
        while sent < data.len() {
            match runner.write_channel(ch, ChanData::Normal, &data[sent..]) {
                Ok(0) => {
                    flush_output(runner, sock_fd);
                    match runner.write_channel(ch, ChanData::Normal, &data[sent..]) {
                        Ok(0) | Err(_) => break,
                        Ok(w) => sent += w,
                    }
                }
                Ok(w) => sent += w,
                Err(_) => break,
            }
            flush_output(runner, sock_fd);
        }
    }
}

/// E.6: Clean up all session resources.
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

/// Send all pending output bytes to the socket.
fn flush_output(runner: &mut Runner<'_, Server>, sock_fd: i32) {
    loop {
        let out = runner.output_buf();
        if out.is_empty() {
            break;
        }
        let len = out.len();
        let mut tmp = [0u8; 4096];
        let chunk = len.min(tmp.len());
        tmp[..chunk].copy_from_slice(&out[..chunk]);
        let written = write_all_count(sock_fd, &tmp[..chunk]);
        if written == 0 {
            break;
        }
        runner.consume_output(written);
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
