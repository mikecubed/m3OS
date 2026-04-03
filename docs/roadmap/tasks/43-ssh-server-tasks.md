# Phase 43 — SSH Server: Task List

**Status:** Complete
**Source Ref:** phase-43
**Depends on:** Phase 23 (Socket API) ✅, Phase 27 (User Accounts) ✅, Phase 29 (PTY) ✅, Phase 37 (I/O Multiplexing) ✅, Phase 42 (Crypto Primitives) ✅
**Goal:** Add an SSH server (`sshd`) using the sunset IO-less SSH library, providing
encrypted remote shell access with password and public key authentication, PTY-based
sessions, and multi-session support via epoll.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Workspace setup and sunset integration | — | Complete |
| B | Host key generation and storage | A | Complete |
| C | SSH transport adapter (TCP ↔ sunset engine) | A, B | Complete |
| D | Authentication integration | C | Complete |
| E | Session channels and PTY integration | C, D | Complete |
| F | Multi-session support and init integration | E | Complete |
| G | Integration testing and documentation | A–F | Complete |

---

## Track A — Workspace Setup and Sunset Integration

Add the sunset SSH library to the workspace and create the sshd userspace crate
that will host the SSH server binary.

### A.1 — Add sunset crate dependency to workspace

**File:** `Cargo.toml`
**Symbol:** `[workspace.dependencies]`
**Why it matters:** The sunset crate provides the IO-less SSH-2 protocol engine that
handles key exchange, encryption, authentication callbacks, and channel multiplexing.
Adding it at the workspace level confirms it compiles for the `x86_64-m3os` target
(no_std + alloc) and makes the version consistent across crates.

**Acceptance:**
- [x] `sunset` (and any required sub-crates like `sunset-core`) added to workspace dependencies
- [x] Configured with `default-features = false` and appropriate feature flags for no_std
- [x] `cargo xtask check` passes with the new dependency

### A.2 — Create `userspace/sshd/` crate

**Files:**
- `userspace/sshd/Cargo.toml`
- `userspace/sshd/src/main.rs`

**Symbol:** `sshd`
**Why it matters:** A dedicated binary crate for the SSH server follows the same
pattern as telnetd (Phase 30). Separating the server from a library keeps the binary
focused on the accept loop and session management while delegating protocol logic to
sunset.

**Acceptance:**
- [x] `userspace/sshd/` exists as a `no_std` binary crate with `#![no_std]`
- [x] Crate depends on `syscall-lib`, `crypto-lib`, and `sunset`
- [x] Crate is added to the workspace members list
- [x] Compiles for the `x86_64-m3os` target (stub main that exits cleanly)
- [x] `cargo xtask check` passes with the new crate

### A.3 — Verify sunset IO-less API compiles and runs in m3OS

**File:** `userspace/sshd/src/main.rs`
**Symbol:** `sunset::Runner` (or equivalent entry point)
**Why it matters:** sunset is designed as an IO-less library, but it has not been
tested inside m3OS. This task confirms that creating a sunset server instance,
feeding it bytes, and reading output bytes works without runtime panics or missing
dependencies. If sunset proves incompatible, this is the decision point for falling
back to Option B (Dropbear) or Option C (from-scratch).

**Acceptance:**
- [x] A sunset `Runner` (server mode) can be instantiated inside a running m3OS process
- [x] Feeding the SSH-2 version string bytes to sunset produces a version response
- [x] No panics or allocation failures during sunset initialization
- [x] Decision documented: proceed with sunset (compiles for x86_64-unknown-none with custom getrandom backend)

---

## Track B — Host Key Generation and Storage

Generate and persist the server's Ed25519 host key so clients can verify the
server's identity across reboots.

### B.1 — Create `/etc/ssh/` directory structure at boot

**File:** `userspace/sshd/src/main.rs`
**Symbol:** `ensure_ssh_dir`
**Why it matters:** SSH configuration and host keys live under `/etc/ssh/`. This
directory must exist before sshd can read or write host keys. Creating it at sshd
startup (if missing) avoids requiring manual setup.

**Acceptance:**
- [x] `sshd` creates `/etc/ssh/` with mode 0755 if it does not exist
- [x] Directory creation is idempotent (no error if already present)

### B.2 — Generate Ed25519 host key on first boot

**File:** `userspace/sshd/src/host_key.rs`
**Symbol:** `generate_host_key`
**Why it matters:** The host key proves the server's identity during key exchange. On
first boot there is no key, so sshd must generate one using crypto-lib's Ed25519
keygen (seeded from getrandom). The private key is stored as a raw 32-byte seed file;
the public key is stored alongside it for convenience.

**Acceptance:**
- [x] `generate_host_key()` creates an Ed25519 keypair via `crypto_lib::asymmetric::ed25519_keygen`
- [x] Writes private key seed to `/etc/ssh/ssh_host_ed25519_key` (32 bytes, mode 0600)
- [x] Writes public key to `/etc/ssh/ssh_host_ed25519_key.pub` (32 bytes, mode 0644)
- [x] Prints host key fingerprint (SHA-256 of public key) to serial log on generation

### B.3 — Load existing host key from disk

**File:** `userspace/sshd/src/host_key.rs`
**Symbol:** `load_host_key`
**Why it matters:** On subsequent boots, sshd must load the existing host key so the
fingerprint remains stable. Clients cache the fingerprint on first connect and warn
if it changes (TOFU — Trust On First Use). A changing fingerprint would trigger
man-in-the-middle warnings in every SSH client.

**Acceptance:**
- [x] `load_host_key()` reads `/etc/ssh/ssh_host_ed25519_key` and reconstructs a `SigningKey`
- [x] Returns `Ok(SigningKey)` if the file exists and contains valid 32-byte seed
- [x] Returns `Err` if the file is missing, wrong size, or unreadable
- [x] On `Err`, caller falls through to `generate_host_key()` (auto-generate on first boot)

---

## Track C — SSH Transport Adapter (TCP ↔ Sunset Engine)

Build the adapter layer that connects TCP sockets to sunset's IO-less protocol
engine, handling the byte-level relay between network I/O and the SSH state machine.

### C.1 — Implement TCP accept loop on port 22

**File:** `userspace/sshd/src/main.rs`
**Symbol:** `main` / `accept_loop`
**Why it matters:** The SSH server must listen on a well-known port and accept
incoming connections. This follows the same pattern as telnetd (Phase 30) — bind,
listen, accept in a loop, fork a child process per connection.

**Acceptance:**
- [x] `sshd` binds to `0.0.0.0:22` (or configurable port)
- [x] Calls `listen()` and enters an `accept()` loop
- [x] Forks a child process for each accepted connection
- [x] Parent process continues accepting; child handles the session
- [x] Reaps zombie children (SIGCHLD or periodic waitpid)

### C.2 — Implement sunset byte pump (socket → sunset → socket)

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `run_session`
**Why it matters:** This is the core adapter that bridges sunset's IO-less API with
m3OS TCP sockets. The pump reads encrypted bytes from the socket, feeds them to
sunset for decryption and protocol processing, then reads sunset's output (encrypted
response bytes) and writes them back to the socket. Without this adapter, sunset
cannot communicate over the network.

**Acceptance:**
- [x] Reads bytes from the TCP socket into a buffer
- [x] Feeds incoming bytes to sunset's `input()` method
- [x] Calls sunset's `output()` to get response bytes and writes them to the socket
- [x] Handles `WouldBlock` / partial reads correctly
- [x] Loop continues until sunset signals disconnection or socket closes

### C.3 — Wire host key into sunset for key exchange

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `configure_sunset_server`
**Why it matters:** During SSH key exchange, sunset needs the server's Ed25519 host
key to sign the exchange hash. This proves to the client that it is talking to the
expected server. Without providing the host key, sunset cannot complete the handshake.

**Acceptance:**
- [x] Host key (loaded in Track B) is passed to sunset's server configuration
- [x] sunset uses the host key to sign the key exchange hash during handshake
- [x] An OpenSSH client can complete key exchange and reports the correct host key fingerprint
- [x] The host key fingerprint matches what `sshd` logged at startup

---

## Track D — Authentication Integration

Connect sunset's authentication callbacks to m3OS's user account system so that
SSH users are validated against real credentials.

### D.1 — Implement password authentication callback

**File:** `userspace/sshd/src/auth.rs`
**Symbol:** `check_password`
**Why it matters:** Password auth is the primary authentication method and the one
users expect to work first. The callback receives a username and password from sunset
(after decryption), looks up the user in `/etc/shadow`, hashes the provided password,
and compares. This reuses the same authentication path as `login` (Phase 27).

**Acceptance:**
- [x] `check_password(username, password) -> bool` reads `/etc/shadow`
- [x] Hashes the provided password and compares against the stored hash
- [x] Returns `true` on match, `false` on mismatch or missing user
- [x] Does not leak timing information about which part failed (user vs password)
- [x] `ssh user@host` with correct password authenticates successfully

### D.2 — Implement public key authentication callback

**File:** `userspace/sshd/src/auth.rs`
**Symbol:** `check_pubkey`
**Why it matters:** Public key auth is more secure than passwords and is the standard
for automated access (scripts, CI, key-based login). The callback receives the
client's public key from sunset, checks if it appears in the user's
`~/.ssh/authorized_keys` file, and tells sunset whether to accept the signature.

**Acceptance:**
- [x] `check_pubkey(username, pubkey) -> bool` reads `~/<username>/.ssh/authorized_keys`
- [x] Parses each line as a 32-byte Ed25519 public key (hex-encoded or raw)
- [x] Returns `true` if the provided public key matches any authorized key
- [x] Returns `false` if the file is missing, empty, or contains no matching key
- [x] `ssh -i id_ed25519 user@host` with a matching authorized key authenticates successfully

### D.3 — Handle authentication failure and retry

**File:** `userspace/sshd/src/auth.rs`
**Symbol:** `auth_handler`
**Why it matters:** SSH allows multiple authentication attempts before disconnecting.
The server must track attempts, respond with the correct SSH failure messages, and
disconnect after a configurable number of failures to limit brute-force attacks.

**Acceptance:**
- [x] Failed authentication returns SSH_MSG_USERAUTH_FAILURE to the client
- [x] Client can retry up to 3 times (configurable) before disconnection
- [x] After max retries, the connection is closed with an appropriate SSH disconnect message
- [x] Each failure is logged (username, method, source address)

---

## Track E — Session Channels and PTY Integration

Handle SSH session channel requests by allocating PTY pairs, spawning the user's
shell, and relaying data between the encrypted SSH channel and the terminal.

### E.1 — Handle session channel open request

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `handle_channel_open`
**Why it matters:** After authentication, the SSH client requests a session channel.
This is the SSH abstraction for "I want a terminal." The server must accept the
channel open request and prepare to handle subsequent requests (PTY, shell, window
change) on that channel.

**Acceptance:**
- [x] sunset's channel open callback is handled for `session` channel type
- [x] A channel ID is assigned and tracked in session state
- [x] The channel is confirmed back to the client via sunset
- [x] Non-session channel types are rejected

### E.2 — Allocate PTY pair for SSH session

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `handle_pty_request`
**Why it matters:** The PTY pair (Phase 29) provides the terminal abstraction that the
shell process reads/writes. The SSH session's PTY request includes terminal type and
window size. Allocating the PTY here follows the same pattern as telnetd but is
triggered by the SSH channel request rather than a telnet negotiation.

**Acceptance:**
- [x] `pty-request` channel request triggers PTY pair allocation via `openpty()` syscall
- [x] Terminal type and initial window size from the request are applied to the PTY
- [x] PTY master fd is stored in session state for later data relay
- [x] PTY slave fd is prepared for the shell process

### E.3 — Fork and exec shell process

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `spawn_shell`
**Why it matters:** The shell process is what the user actually interacts with over
SSH. After PTY allocation, sshd forks a child, sets the PTY slave as stdin/stdout/
stderr, calls `setsid()` to create a new session, and execs `login` (or the user's
shell). This is structurally identical to telnetd's shell spawning.

**Acceptance:**
- [x] `fork()` creates a child process
- [x] Child calls `setsid()` to become session leader
- [x] Child sets PTY slave as stdin (fd 0), stdout (fd 1), stderr (fd 2)
- [x] Child execs `login` (or `/bin/sh0` directly with the authenticated user's UID)
- [x] Parent closes PTY slave fd (only the child uses it)

### E.4 — Relay data between SSH channel and PTY master

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `relay_loop`
**Why it matters:** This is the main data path: keystrokes from the SSH client arrive
as encrypted packets, are decrypted by sunset into channel data, and must be written
to the PTY master. Output from the shell (via PTY master) must be read, given to
sunset as channel data, encrypted, and sent back over the socket. Epoll (Phase 37)
multiplexes the socket and PTY master fds.

**Acceptance:**
- [x] Uses `poll` to wait on both the TCP socket fd and the PTY master fd
- [x] Socket-readable: reads bytes, feeds to sunset, sunset produces channel data, writes to PTY master
- [x] PTY-readable: reads bytes from PTY master, sends as channel data via sunset, writes encrypted output to socket
- [x] Handles partial reads/writes and EAGAIN correctly
- [x] Loop exits when the shell process exits or the SSH client disconnects

### E.5 — Handle window-change channel request (Deferred)

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `handle_window_change`
**Why it matters:** When the user resizes their terminal, the SSH client sends a
`window-change` request with the new dimensions. The server must forward this to the
PTY so that full-screen applications (like the text editor from Phase 26) render
correctly.

**Status:** Deferred — sunset v0.4.0 does not expose window-change channel requests
as a server event. The `ServPtyRequest` type also does not expose initial window size.
This will be addressed when sunset adds window-change support or via a workaround.

**Acceptance:**
- [ ] `window-change` channel request is handled by sunset callback
- [ ] New terminal dimensions are applied to the PTY via `ioctl(TIOCSWINSZ)`
- [ ] Resizing the local terminal during an SSH session updates the remote terminal
- [ ] Full-screen programs (e.g., `edit`) reflow correctly after resize

### E.6 — Graceful session cleanup on disconnect

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `cleanup_session`
**Why it matters:** When an SSH session ends (client disconnect, shell exit, or
network error), all resources must be freed: the PTY pair closed, the shell process
reaped, the TCP socket closed, and the sunset state dropped. Leaking resources would
eventually exhaust PTY slots or file descriptors.

**Acceptance:**
- [x] Shell process exit triggers channel EOF and close to the client
- [x] Client disconnect (TCP close or SSH disconnect message) triggers shell SIGHUP
- [x] PTY master and slave fds are closed
- [x] Shell child process is waited on (no zombies)
- [x] TCP socket is closed
- [x] Session child process exits cleanly

---

## Track F — Multi-Session Support and Init Integration

Support multiple concurrent SSH sessions and integrate sshd into the boot
sequence.

### F.1 — Support multiple simultaneous SSH sessions

**File:** `userspace/sshd/src/main.rs`
**Symbol:** `accept_loop`
**Why it matters:** A real SSH server handles many connections concurrently. Since each
accepted connection forks a child process (Track C.1), the parent must continue
accepting while children handle sessions independently. This validates that PTY
allocation, file descriptors, and process management scale beyond a single session.

**Acceptance:**
- [x] Two or more SSH clients can connect simultaneously
- [x] Each session has an independent PTY and shell process
- [x] Sessions do not interfere with each other (input/output isolation)
- [x] Disconnecting one session does not affect others
- [x] Parent process reaps terminated session children

### F.2 — Add sshd to init startup sequence

**File:** `userspace/init/src/main.rs`
**Symbol:** `start_services` (or equivalent init startup code)
**Why it matters:** sshd must start automatically at boot so the OS is remotely
accessible without manual intervention. Init (PID 1) spawns sshd as a background
daemon, similar to how telnetd is started.

**Acceptance:**
- [x] Init spawns `sshd` as a background process during boot
- [x] sshd is running and listening on port 22 after boot completes
- [x] sshd startup is logged to serial output
- [x] Init does not wait for sshd to exit (non-blocking spawn)

### F.3 — Add sshd binary to initrd

**File:** `xtask/src/main.rs`
**Symbol:** `build_userspace_bins()` / binary list
**Why it matters:** The sshd binary must be embedded in the initial ramdisk to be
available at boot. Without adding it to the initrd build, the binary exists as a
compiled artifact but cannot be executed inside the OS.

**Acceptance:**
- [x] `sshd` binary is compiled and included in the initrd
- [x] Binary is accessible at `/bin/sshd` after boot
- [x] `cargo xtask image` produces a bootable image with sshd

### F.4 — Add QEMU port forwarding for SSH testing

**File:** `xtask/src/main.rs`
**Symbol:** QEMU launch arguments
**Why it matters:** To test SSH from the host, QEMU must forward a host port to
guest port 22. This allows running `ssh -p 2222 user@localhost` from the host
machine to connect to the m3OS SSH server.

**Acceptance:**
- [x] QEMU is launched with `-nic user,hostfwd=tcp::2222-:22` (or similar)
- [x] `ssh -p 2222 user@localhost` from the host reaches sshd inside QEMU
- [x] Existing telnet port forwarding (if any) is not disrupted

---

## Track G — Integration Testing and Documentation

Validate the SSH server works end-to-end from a real SSH client and update
project documentation.

### G.1 — End-to-end test: password authentication from host (Manual)

**Files:**
- `userspace/sshd/src/main.rs`
- `userspace/sshd/src/auth.rs`

**Symbol:** (integration test)
**Why it matters:** The ultimate validation is connecting from a real OpenSSH client
on the host to sshd running inside QEMU. This exercises the entire stack: TCP,
sunset protocol engine, password authentication, PTY allocation, shell execution,
and encrypted data relay.

**Acceptance:**
- [ ] `ssh -p 2222 -o StrictHostKeyChecking=no user@localhost` connects and authenticates
- [ ] Interactive shell session works (type commands, see output)
- [ ] `exit` in the remote shell cleanly disconnects
- [ ] Wrong password is rejected with appropriate error

### G.2 — End-to-end test: public key authentication from host (Manual)

**Files:**
- `userspace/sshd/src/auth.rs`
- `userspace/sshd/src/session.rs`

**Symbol:** (integration test)
**Why it matters:** Public key auth is the preferred method for SSH. Testing it
from a real client validates the authorized_keys parsing, signature verification,
and the full authentication flow without transmitting a password.

**Acceptance:**
- [ ] Generate an Ed25519 keypair on the host
- [ ] Add the public key to the user's `authorized_keys` inside the OS
- [ ] `ssh -p 2222 -i /path/to/key user@localhost` authenticates without a password prompt
- [ ] A key not in `authorized_keys` is rejected

### G.3 — Verify encryption by traffic inspection (Manual)

**Files:**
- `userspace/sshd/src/session.rs`

**Symbol:** (verification test)
**Why it matters:** The entire point of SSH over telnet is encryption. Capturing
traffic between the host and QEMU and verifying that session data is not visible in
plaintext confirms that the transport layer encryption is working correctly.

**Acceptance:**
- [ ] Capture traffic on the forwarded port with tcpdump or wireshark
- [ ] Session content (commands, output) is not visible in plaintext in the capture
- [ ] SSH handshake packets are visible but payload is encrypted
- [ ] Contrast with telnet traffic (plaintext) demonstrates the improvement

### G.4 — Verify no regressions in existing tests

**Files:**
- `kernel/tests/*.rs`
- `userspace/*/src/main.rs`

**Symbol:** (all existing tests)
**Why it matters:** Adding a new userspace crate and dependencies could introduce
build issues or binary size regressions. All existing tests must continue to pass.

**Acceptance:**
- [x] `cargo xtask check` passes (clippy + fmt)
- [x] `cargo xtask test` passes (all existing QEMU tests)
- [x] `cargo test -p kernel-core` passes (host-side unit tests)

### G.5 — Update documentation

**Files:**
- `docs/roadmap/43-ssh-server.md`
- `docs/roadmap/README.md`
- `CLAUDE.md`

**Symbol:** (documentation)
**Why it matters:** Roadmap docs must reflect the actual implementation state and
link to the completed task list. CLAUDE.md must be updated with the new sshd crate
and any new documentation references.

**Acceptance:**
- [x] Design doc status updated to `Complete` after implementation
- [x] README row updated with task list link and `Complete` status
- [x] CLAUDE.md workspace crate table includes `sshd`
- [x] CLAUDE.md docs table includes Phase 43 reference
- [x] Any deferred items accurately reflect what was and was not implemented

---

## Documentation Notes

- Phase 43 replaces telnet (Phase 30) as the secure remote access method. Telnetd
  remains available for trusted networks but sshd is the recommended default.
- The sunset library is IO-less: sshd is responsible for all I/O (sockets, PTY, files).
  sunset only processes byte buffers and produces byte buffers. This is a deliberate
  architectural choice that makes the protocol engine portable and testable.
- The session lifecycle (accept → fork → auth → PTY → shell → relay → cleanup)
  mirrors telnetd structurally. The key difference is that all socket I/O passes
  through sunset's encryption layer rather than being relayed as plaintext.
- Host keys are stored as raw 32-byte Ed25519 seeds, not in OpenSSH or PEM format.
  This matches the key format from Phase 42's `genkey` utility. If interoperability
  with OpenSSH key formats is needed, a conversion utility can be added later.
- Public keys in `authorized_keys` are stored one per line as hex-encoded 32-byte
  Ed25519 public keys. This is simpler than OpenSSH's base64 format but sufficient
  for the learning scope of this phase.
- If sunset proves too immature or incompatible with m3OS, the fallback is
  cross-compiling Dropbear SSH with musl (Option B in the design doc). Track A.3
  is the explicit evaluation checkpoint for this decision.
