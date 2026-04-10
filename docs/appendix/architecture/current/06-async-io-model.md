# Current Architecture: Async I/O Model

**Subsystem:** Kernel poll/select/epoll, userspace async executor, sunset SSH integration, sshd multi-task model
**Key source files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` — sys_poll, sys_select, sys_epoll_*, fd_poll_events, fd_register_waiter
- `kernel/src/task/wait_queue.rs` — WaitQueue (used by poll registration)
- `userspace/sshd/src/session.rs` — SSH session multi-task architecture
- `userspace/async-rt/src/executor.rs` — Cooperative async executor
- `userspace/async-rt/src/reactor.rs` — poll-based I/O reactor
- `sunset-local/src/runner.rs` — SSH state machine (vendored)
- `sunset-local/src/channel.rs` — SSH channel wakeup paths

## 1. Overview

m3OS provides kernel-level I/O multiplexing through `poll`, `select`, and `epoll` syscalls, all driven by wait queues attached to FD backends (pipes, sockets, PTYs). The kernel implementation is wait-queue-driven — no busy-waiting or periodic polling.

The sshd daemon uses a userspace cooperative async executor (`async-rt`) built on top of the kernel `poll` syscall. Each SSH session runs three cooperating async tasks that share the sunset SSH state machine via `Rc<Mutex<Runner>>`.

The SSHD hang analysis identified a critical write-side wakeup bug in this model: PTY output stalls when the SSH channel encounters backpressure, because the relay task sleeps without registering for channel write readiness.

## 2. Data Structures

### 2.1 Kernel Poll Infrastructure

```rust
// Syscall layer (mod.rs)
// pollfd struct from userspace:
struct pollfd {
    fd: i32,
    events: i16,   // Requested events (POLLIN, POLLOUT, etc.)
    revents: i16,  // Returned events
}

// Event constants:
const POLLIN:  i16 = 0x0001;
const POLLOUT: i16 = 0x0004;
const POLLERR: i16 = 0x0008;
const POLLHUP: i16 = 0x0010;
const POLLNVAL: i16 = 0x0020;
```

### 2.2 Epoll Instance

```rust
struct EpollInstance {
    interests: Vec<EpollInterest>,
}

struct EpollInterest {
    fd: i32,
    events: u32,  // EPOLLIN | EPOLLOUT | ...
}
```

### 2.3 Userspace Async Executor

```rust
// userspace/async-rt/src/executor.rs
pub struct Executor {
    tasks: Slab<TaskSlot>,           // Fixed-index task store
    run_queue: VecDeque<usize>,      // Ready task indices
    root_woken: AtomicBool,          // Root future needs polling
}

struct TaskSlot {
    future: Pin<Box<dyn Future<Output = ()>>>,
    header: TaskHeader,
}

struct TaskHeader {
    woken: AtomicBool,
    wake_pipe_fd: i32,  // Write here to interrupt reactor.poll_once()
}
```

### 2.4 I/O Reactor

```rust
// userspace/async-rt/src/reactor.rs
pub struct Reactor {
    pub(crate) wake_read_fd: i32,    // Self-pipe read end
    pub(crate) wake_write_fd: i32,   // Self-pipe write end
    pub(crate) interests: Vec<Interest>,
}

pub struct Interest {
    pub fd: i32,
    pub read_waker: Option<Waker>,
    pub write_waker: Option<Waker>,
}
```

### 2.5 SSH Session Shared State

```rust
// userspace/sshd/src/session.rs
type SharedRunner = Rc<Mutex<Runner>>;
type SharedState = Rc<RefCell<SessionState>>;
type SharedChan = Rc<RefCell<Option<ChanHandle>>>;
type SharedOutputLock = Rc<Mutex<()>>;
```

## 3. Algorithms

### 3.1 Kernel `sys_poll` Algorithm

```mermaid
flowchart TD
    A["sys_poll(fds, nfds, timeout)"] --> B["Save syscall_user_rsp"]
    B --> C["Copy pollfd structs from userspace"]

    C --> D["Scan all FDs: fd_poll_events(entry)"]
    D --> E{Any FD ready?}
    E -->|Yes| F["Write revents back, return count"]
    E -->|"No, timeout=0"| F

    E -->|"No, timeout>0 or -1"| G{Pending signal?}
    G -->|Yes| H["Return EINTR"]

    G -->|No| I["Create shared Arc<AtomicBool> woken"]
    I --> J["For each FD: fd_register_waiter(fd, task_id, &woken)"]
    J --> K["Re-check readiness (TOCTOU close)"]
    K --> L{Any ready now?}
    L -->|Yes| M["Deregister, goto scan"]

    L -->|No, positive timeout| N["yield_now()"]
    N --> O["restore_caller_context(pid, saved_rsp)"]

    L -->|"No, timeout=-1"| P["block_current_unless_woken(&woken)"]
    P --> Q["restore_caller_context(pid, saved_rsp)"]

    O --> R["Deregister from all wait queues"]
    Q --> R
    R --> D
```

**Key design detail:** The shared `woken` flag is registered on ALL FD wait queues simultaneously. Any single FD event sets the flag and wakes the task. After waking, the task deregisters from all queues and re-scans.

### 3.2 `fd_poll_events` — Per-FD Readiness Check

```mermaid
flowchart TD
    A["fd_poll_events(fd_entry)"] --> B{FD backend type?}

    B -->|Pipe read| C["POLLIN if pipe has data<br/>POLLHUP if write end closed"]
    B -->|Pipe write| D["POLLOUT if pipe has space<br/>POLLERR if read end closed"]
    B -->|TCP socket| E["POLLIN if recv buffer non-empty<br/>POLLOUT if connected + send space<br/>POLLHUP if closed/reset"]
    B -->|PtyMaster| F["POLLIN if s2m has data<br/>POLLOUT if m2s has space"]
    B -->|PtySlave| G["POLLIN if line ready (canon) or m2s has data (raw)<br/>POLLOUT if s2m has space"]
    B -->|DeviceTTY| H["POLLIN if stdin has data<br/>POLLOUT always"]
    B -->|Unix socket| I["POLLIN if recv buffer non-empty<br/>POLLOUT if send space"]
    B -->|Ramdisk/File| J["POLLIN | POLLOUT always"]
```

### 3.3 Userspace Async Executor Loop

```mermaid
flowchart TD
    A["block_on(&mut reactor, root_future)"] --> B["Poll root future if woken"]
    B --> C["poll_spawned_tasks()<br/>Drain run_queue, poll each woken task"]
    C --> D["reactor.poll_once(0)<br/>Non-blocking FD check"]
    D --> E["requeue_woken()<br/>Scan slab for newly woken tasks"]
    E --> F{Any task runnable?}
    F -->|Yes| B
    F -->|No| G["reactor.poll_once(100)<br/>100ms blocking poll"]
    G --> H["Fire wakers for ready FDs"]
    H --> B
```

### 3.4 SSH Session Task Architecture

```mermaid
graph TB
    subgraph "Session Process (forked child)"
        ROOT["async_session (root future)<br/>- Session lifecycle<br/>- Shell waitpid<br/>- session_notify consumer"]

        IO["io_task<br/>- Socket read → runner.input()<br/>- runner.output_buf() → socket write<br/>- output_waker registration"]

        PROG["progress_task<br/>- runner.progress() event loop<br/>- Auth handling<br/>- Channel open<br/>- Shell fork + PTY setup<br/>- Spawns RELAY on SessionShell"]

        RELAY["channel_relay_task<br/>- PTY master read → write_channel<br/>- read_channel → PTY master write<br/>- Pending buffer management"]
    end

    subgraph "Shared State"
        RUNNER["Rc<Mutex<Runner>><br/>sunset SSH state machine"]
        STATE["Rc<RefCell<SessionState>><br/>auth state, PIDs, FDs"]
        CHAN["Rc<RefCell<Option<ChanHandle>>><br/>SSH channel handle"]
        OUTLOCK["Rc<Mutex<()>><br/>Output serialization lock"]
    end

    ROOT --> RUNNER
    IO --> RUNNER
    PROG --> RUNNER
    RELAY --> RUNNER

    IO --> OUTLOCK
    RELAY --> OUTLOCK

    PROG --> STATE
    PROG --> CHAN
    RELAY --> CHAN
```

### 3.5 The SSHD Hang: Write-Side Wakeup Bug

```mermaid
flowchart TD
    A["Shell writes prompt to PTY slave"] --> B["PTY master becomes readable"]
    B --> C["channel_relay_task reads from PTY master"]
    C --> D{"write_channel\nresult?"}

    D -->|"bytes written"| E["flush_output_locked\nSSH packet queued and flushed"]
    E --> F["I/O task sends to socket"]

    D -->|"zero: backpressure"| G["Stash data in pty_pending"]
    G --> H["flush_output_locked\nno new packet to flush"]
    H --> I["Register wakers before sleeping"]

    I --> J["set_channel_read_waker"]
    J --> K{"pty_pending\nnon-empty?"}
    K -->|Yes| L["set_channel_write_waker\nregistered correctly"]
    K -->|No| M["Skip write waker"]

    L --> N["Sleep on WaitWake for pty_fd POLLIN"]
    M --> N

    N --> O{"What wakes\nrelay?"}
    O -->|"PTY has new output"| C
    O -->|"Client keystroke"| C
    O -->|"Channel write space freed"| P{"wake_write\ncorrect?"}

    P -->|"Yes"| Q["Retry pty_pending"]
    P -->|"No: sunset bug"| R["Wakes read_waker instead\nWrong task woken"]

    R --> S["Session appears hung\nuntil client types a key"]

    style R fill:#ff6666,color:#000
    style S fill:#ff6666,color:#000
```

### 3.6 The Sunset `Channel::wake_write()` Bug

```mermaid
flowchart LR
    subgraph "Current (buggy)"
        A1["consume_output() or<br/>ChannelWindowAdjust"] --> B1["channel_wake_write()"]
        B1 --> C1["Channel::wake_write()"]
        C1 --> D1["self.read_waker.take() ← WRONG"]
        D1 --> E1["Read-side task wakes"]
    end

    subgraph "Correct (expected)"
        A2["consume_output() or<br/>ChannelWindowAdjust"] --> B2["channel_wake_write()"]
        B2 --> C2["Channel::wake_write()"]
        C2 --> D2["self.write_waker.take() ← CORRECT"]
        D2 --> E2["Write-side relay task wakes"]
    end

    style D1 fill:#ff6666,color:#000
    style D2 fill:#00cc66,color:#000
```

**Evidence:** `sunset-local/src/channel.rs:840-845` — `Channel::wake_write()` uses `self.read_waker.take()` for normal data instead of `self.write_waker.take()`. This means even if `sshd` correctly calls `set_channel_write_waker()`, the backpressure-cleared event wakes the wrong waker.

## 4. Reactor and Waker Mechanism

### 4.1 How Wakers Work

```mermaid
sequenceDiagram
    participant Task as Async Task
    participant Exec as Executor
    participant React as Reactor
    participant Kernel as Kernel (poll syscall)

    Task->>Task: Future::poll() returns Pending
    Task->>React: Register interest: fd=5, read_waker=my_waker

    Exec->>React: poll_once(100) — blocking
    React->>React: Build pollfd array from interests + wake_pipe
    React->>Kernel: sys_poll(pfds, nfds, 100)
    Note over Kernel: Task blocks in poll

    Note over Kernel: Data arrives on fd=5
    Kernel-->>React: fd=5 has POLLIN

    React->>React: interests[5].read_waker.take().wake()
    Note over React: Waker writes 1 byte to wake_pipe<br/>(interrupts any concurrent poll_once)
    React->>Exec: Task's woken flag set to true

    Exec->>Task: Future::poll() → can now read fd=5
```

### 4.2 Self-Pipe for Waker Interruption

The reactor uses a self-pipe (`wake_read_fd` / `wake_write_fd`) to interrupt blocking `poll_once()` calls:
1. Self-pipe read end is always in the `pollfd` array
2. When a `Waker` fires, it writes 1 byte to `wake_write_fd`
3. The kernel `poll` returns with the self-pipe readable
4. Reactor drains the self-pipe and processes woken tasks

This ensures that waker events from any source (channel readiness, output buffer drain, etc.) can immediately interrupt a blocking poll.

## 5. Non-Blocking I/O

For `O_NONBLOCK` FDs (sockets, PTYs, pipes):
- `read()` returns `-EAGAIN` if no data available
- `write()` returns `-EAGAIN` if buffer full
- The async executor wraps these in `poll`-then-retry loops via the reactor

```mermaid
flowchart TD
    A["AsyncRead::poll_read(fd)"] --> B["syscall_lib::read(fd, buf)"]
    B --> C{Result?}
    C -->|"Ok(n > 0)"| D["Return Ready(n)"]
    C -->|"Err(EAGAIN)"| E["reactor.register_read(fd, waker)"]
    E --> F["Return Pending"]
    C -->|"Err(other)"| G["Return Ready(Err)"]
    C -->|"Ok(0)"| H["Return Ready(EOF)"]
```

## 6. I/O Task Detail

### 6.1 Output Direction (runner → socket)

```mermaid
flowchart TD
    A["flush_output_locked()"] --> B["Lock output mutex"]
    B --> C["runner.output_buf() → pending bytes"]
    C --> D{Bytes available?}
    D -->|No| E["Return"]
    D -->|Yes| F["syscall_lib::write(sock_fd, bytes)"]
    F --> G{Result?}
    G -->|"Ok(n)"| H["runner.consume_output(n)"]
    H --> I{All consumed?}
    I -->|Yes| E
    I -->|No| F
    G -->|"EAGAIN"| J["Stash remaining in pending_out"]
```

### 6.2 Input Direction (socket → runner)

```mermaid
flowchart TD
    A["Read from socket"] --> B["syscall_lib::read(sock_fd, sock_buf)"]
    B --> C{Result?}
    C -->|"Ok(n)"| D["runner.input(&sock_buf[..n])"]
    D --> E{All consumed?}
    E -->|Yes| F["signal progress_notify"]
    E -->|No| G["Stash remainder in pending_in"]
    G --> F
    C -->|"EAGAIN"| H["Register for POLLIN, yield"]
    C -->|"Ok(0)"| I["Connection closed"]
```

## 7. Known Issues

### 7.1 SSHD Write-Side Wakeup Bug (Two Compounding Defects)

**Defect 1 (sshd):** When `write_channel()` returns `Ok(0)`, the relay stashes data in `pty_pending` and correctly registers `set_channel_write_waker` when `pty_pending_len > 0` (this was previously missing and has been partially fixed).

**Defect 2 (sunset library):** `Channel::wake_write()` in `sunset-local/src/channel.rs:840-845` calls `self.read_waker.take()` instead of `self.write_waker.take()`. Even with correct registration, the wrong waker fires.

**Combined effect:** PTY output stalls under channel backpressure until a client keystroke "nudges" output through by triggering `channel_read_waker`.

### 7.2 Cooperative Executor Cannot Preempt Long-Running Futures

**Evidence:** `userspace/async-rt/src/executor.rs` — single-threaded cooperative polling. A future that does CPU-intensive work without yielding blocks all other tasks.

**Impact:** If `runner.progress()` or any other future takes too long, I/O and relay tasks are starved.

### 7.3 100ms Reactor Poll Timeout

**Evidence:** `reactor.poll_once(100)` — the reactor blocks for up to 100ms when no task is runnable.

**Impact:** Adds up to 100ms latency for events that arrive while the reactor is blocking. Lower timeout = more CPU usage. Higher timeout = more latency.

### 7.4 No Backpressure From Socket Writes

**Evidence:** `flush_output_locked()` writes in a loop until EAGAIN. If the socket send buffer is large, this can block the entire executor for the duration of a large write.

**Impact:** Other async tasks are starved during large socket writes.

### 7.5 Shared `Rc<Mutex<Runner>>` Contention

**Evidence:** All three tasks share the same `Rc<Mutex<Runner>>`. Lock contention between I/O, relay, and progress tasks is serialized.

**Impact:** Under heavy traffic, tasks may spin-wait on the runner lock. The cooperative executor means only one task runs at a time, so actual deadlock is impossible, but priority inversion is possible.

## 8. Comparison Points for External Kernels

| Aspect | m3OS Current | What to Compare |
|---|---|---|
| I/O multiplexing | poll/select/epoll (wait-queue-driven) | All kernels: similar poll/epoll; Zircon: ports |
| Async executor | Userspace cooperative (single-threaded) | Redox: scheme-based async; Zircon: port-based event loop |
| SSH integration | Vendored sunset + 3-task cooperative model | N/A (OS-specific) |
| Waker mechanism | Self-pipe + AtomicBool woken flag | Zircon: port signals; Linux: eventfd |
| Non-blocking I/O | O_NONBLOCK + EAGAIN | Universal |
