//! SSH session handler (Tracks C, D, E).
//!
//! Manages the lifecycle of a single SSH connection: key exchange, authentication,
//! channel management, PTY allocation, shell spawning, and data relay.

extern crate alloc;

use alloc::string::String;
use sunset::{ChanData, ChanHandle, Event, Runner, ServEvent, Server};
use syscall_lib::{STDOUT_FILENO, WNOHANG, close, dup2, exit, fork, setsid, waitpid, write_str};

use crate::auth;
use crate::host_key::HostKey;

/// Poll syscall (syscall 7).
const SYS_POLL: u64 = 7;
const POLLIN: i16 = 0x001;
const POLLHUP: i16 = 0x010;
const POLLERR: i16 = 0x008;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

fn poll(fds: &mut [PollFd], timeout_ms: i32) -> isize {
    unsafe {
        syscall_lib::syscall3(
            SYS_POLL,
            fds.as_mut_ptr() as u64,
            fds.len() as u64,
            timeout_ms as u64,
        ) as isize
    }
}

/// Ioctl constants for PTY/terminal control.
const TIOCSCTTY: usize = 0x540E;

/// SSH buffer sizes — must fit the largest SSH packet.
const BUF_SIZE: usize = 36000;

/// Maximum authentication attempts before disconnecting.
const MAX_AUTH_ATTEMPTS: u32 = 6;

/// C.2/E.4: Run a complete SSH session on the given client socket.
pub fn run_session(sock_fd: i32, host_key: &HostKey) -> i32 {
    let mut inbuf = alloc::vec![0u8; BUF_SIZE];
    let mut outbuf = alloc::vec![0u8; BUF_SIZE];
    let mut runner = Runner::new_server(&mut inbuf, &mut outbuf);

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
    // when sunset or the channel cannot accept more data (backpressure).
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
                // Drain remaining PTY output before exiting.
                drain_pty(&mut runner, sock_fd, pty_master, &chan_handle, &mut pty_buf);
                flush_output(&mut runner, sock_fd);
                break;
            }
        }

        // Build poll set.
        let mut pfds_arr = [
            PollFd {
                fd: sock_fd,
                events: POLLIN,
                revents: 0,
            },
            PollFd {
                fd: -1,
                events: 0,
                revents: 0,
            },
        ];
        let nfds = if let Some(pty_fd) = pty_master {
            pfds_arr[1] = PollFd {
                fd: pty_fd,
                events: POLLIN,
                revents: 0,
            };
            2
        } else {
            1
        };

        let ret = poll(&mut pfds_arr[..nfds], 200);
        if ret < 0 {
            break;
        }

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
        if sock_pending_len == 0 && (pfds_arr[0].revents & POLLIN) != 0 {
            let n = syscall_lib::read(sock_fd, &mut sock_buf);
            if n <= 0 {
                break;
            }
            let mut consumed = 0;
            while consumed < n as usize {
                match runner.input(&sock_buf[consumed..n as usize]) {
                    Ok(0) => {
                        flush_output(&mut runner, sock_fd);
                        let _ = runner.progress();
                        flush_output(&mut runner, sock_fd);
                        break; // Stash remainder below.
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
                sock_pending_buf[..stash].copy_from_slice(&sock_buf[consumed..consumed + stash]);
                sock_pending_len = stash;
            }
        }

        if (pfds_arr[0].revents & (POLLHUP | POLLERR)) != 0 && (pfds_arr[0].revents & POLLIN) == 0 {
            break;
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
                        break; // Still blocked — try again next iteration.
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
        if pty_pending_len == 0 && nfds > 1 && (pfds_arr[1].revents & POLLIN) != 0 {
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
                                    break; // Stash remainder below.
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

        if nfds > 1 && (pfds_arr[1].revents & POLLHUP) != 0 {
            // PTY closed — drain any remaining output before exiting.
            drain_pty(&mut runner, sock_fd, pty_master, &chan_handle, &mut pty_buf);
            flush_output(&mut runner, sock_fd);
            break;
        }

        // Drive sunset event loop.
        loop {
            match runner.progress() {
                Ok(Event::Serv(ServEvent::Hostkeys(hostkeys))) => {
                    if hostkeys.hostkeys(&[&host_key.key]).is_err() {
                        cleanup(shell_pid, pty_master, pty_slave);
                        return 1;
                    }
                }
                Ok(Event::Serv(ServEvent::FirstAuth(first_auth))) => {
                    let _ = first_auth.reject();
                }
                Ok(Event::Serv(ServEvent::PasswordAuth(pw_auth))) => {
                    auth_attempts += 1;
                    let (u, p) = match (pw_auth.username(), pw_auth.password()) {
                        (Ok(u), Ok(p)) => (String::from(u), String::from(p)),
                        _ => {
                            let _ = pw_auth.reject();
                            if auth_attempts >= MAX_AUTH_ATTEMPTS {
                                cleanup(shell_pid, pty_master, pty_slave);
                                return 1;
                            }
                            continue;
                        }
                    };
                    match auth::check_password(&u, &p) {
                        Some(info) => {
                            authenticated = true;
                            user_info = Some(info);
                            let _ = pw_auth.allow();
                        }
                        None => {
                            write_str(STDOUT_FILENO, "sshd: auth failed\n");
                            let _ = pw_auth.reject();
                            if auth_attempts >= MAX_AUTH_ATTEMPTS {
                                cleanup(shell_pid, pty_master, pty_slave);
                                return 1;
                            }
                        }
                    }
                }
                Ok(Event::Serv(ServEvent::PubkeyAuth(pk_auth))) => {
                    let is_real = pk_auth.real();
                    let username = match pk_auth.username() {
                        Ok(u) => String::from(u),
                        Err(_) => {
                            if is_real {
                                auth_attempts += 1;
                            }
                            let _ = pk_auth.reject();
                            if auth_attempts >= MAX_AUTH_ATTEMPTS {
                                cleanup(shell_pid, pty_master, pty_slave);
                                return 1;
                            }
                            continue;
                        }
                    };
                    let pubkey = match pk_auth.pubkey() {
                        Ok(pk) => pk,
                        Err(_) => {
                            if is_real {
                                auth_attempts += 1;
                            }
                            let _ = pk_auth.reject();
                            if auth_attempts >= MAX_AUTH_ATTEMPTS {
                                cleanup(shell_pid, pty_master, pty_slave);
                                return 1;
                            }
                            continue;
                        }
                    };
                    let pk_bytes: &[u8] = match &pubkey {
                        sunset::PubKey::Ed25519(ed_pk) => &ed_pk.key.0,
                        _ => {
                            if is_real {
                                auth_attempts += 1;
                            }
                            let _ = pk_auth.reject();
                            if auth_attempts >= MAX_AUTH_ATTEMPTS {
                                cleanup(shell_pid, pty_master, pty_slave);
                                return 1;
                            }
                            continue;
                        }
                    };
                    match auth::check_pubkey(&username, pk_bytes) {
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
                            if auth_attempts >= MAX_AUTH_ATTEMPTS {
                                cleanup(shell_pid, pty_master, pty_slave);
                                return 1;
                            }
                        }
                    }
                }
                Ok(Event::Serv(ServEvent::OpenSession(open_session))) => {
                    if !authenticated {
                        let _ = open_session
                            .reject(sunset::ChanFail::SSH_OPEN_ADMINISTRATIVELY_PROHIBITED);
                        continue;
                    }
                    match open_session.accept() {
                        Ok(handle) => {
                            chan_handle = Some(handle);
                        }
                        Err(_) => {}
                    }
                }
                Ok(Event::Serv(ServEvent::SessionPty(pty_req))) => match syscall_lib::openpty() {
                    Ok((master, slave)) => {
                        pty_master = Some(master);
                        pty_slave = Some(slave);
                        let _ = pty_req.succeed();
                    }
                    Err(_) => {
                        let _ = pty_req.fail();
                    }
                },
                Ok(Event::Serv(ServEvent::SessionShell(shell_req))) => {
                    // Only acknowledge success after the shell is actually spawned.
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
                            // Child: set up PTY and exec shell.
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

                            // Drop privileges — abort if either fails.
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
                            // Parent: shell spawned successfully.
                            close(slave);
                            pty_slave = None;
                            shell_pid = Some(pid);
                            shell_spawned = true;
                            let _ = shell_req.succeed();
                        }
                    } else {
                        let _ = shell_req.fail();
                    }
                }
                Ok(Event::Serv(ServEvent::SessionExec(exec_req))) => {
                    let _ = exec_req.fail();
                }
                Ok(Event::Serv(ServEvent::SessionSubsystem(sub_req))) => {
                    let _ = sub_req.fail();
                }
                Ok(Event::Serv(ServEvent::SessionEnv(env_req))) => {
                    let _ = env_req.fail();
                }
                Ok(Event::Serv(ServEvent::Defunct)) => {
                    cleanup(shell_pid, pty_master, pty_slave);
                    return 0;
                }
                Ok(Event::Serv(ServEvent::PollAgain)) => continue,
                Ok(Event::Progressed) => continue,
                Ok(Event::None) => break,
                Ok(_) => break,
                Err(_) => {
                    cleanup(shell_pid, pty_master, pty_slave);
                    return 1;
                }
            }
        }

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

/// Drain remaining PTY output through sunset and onto the socket.
/// Called when the shell has exited or the PTY reports POLLHUP so that
/// the last bytes of output are not truncated on the client.
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
    // Read until EOF (read returns 0 or error).
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
                    // One more attempt after flush.
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
        // Write to socket first, then consume only what was sent.
        let written = write_all_count(sock_fd, &tmp[..chunk]);
        if written == 0 {
            break; // Socket write failed — stop flushing.
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
    let max = buf.len() - 1; // Reserve 1 byte for NUL.

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

    buf[pos] = 0; // Always NUL-terminate.
    pos + 1
}
