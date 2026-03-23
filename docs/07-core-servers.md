# Core Servers — Phase 7

## Overview

Phase 7 introduces the first layer of server infrastructure on top of the IPC engine built in
Phase 6. The additions are:

- **Service registry** — a static name-to-endpoint table that lets tasks find each other by name
- **init orchestration** — a kernel task that creates endpoints, spawns servers, and registers them
- **console_server** — receives formatted string messages over IPC and writes them to serial
- **kbd_server** — waits on a keyboard IRQ notification and forwards key events to clients

### Why structure output and input as servers?

A common question is: "why not just call the serial driver and keyboard driver directly?"

The microkernel answer is **policy/mechanism separation**:

- The kernel owns the *mechanism* — port I/O, interrupt routing, IPC delivery.
- A server owns the *policy* — which clients may write, how output is formatted, which
  process receives key events first.

Moving these decisions out of the kernel has concrete benefits:

1. **Auditability** — every console write and every key event passes through a named endpoint;
   you can log, filter, or redirect it by changing one server, not the kernel.
2. **Replaceability** — swap `console_server` for a framebuffer terminal server without
   touching the kernel.
3. **Least privilege** — a process that holds only a console endpoint capability cannot perform
   arbitrary I/O; it can only write strings to the console service.

---

## Service Registry

### Data model

The registry is a flat array of at most **8 entries**, each holding a name and an `EndpointId`:

```
ServiceEntry {
    name:     [u8; 32],  // raw bytes; not NUL-terminated
    name_len: usize,     // actual byte count, 0–32
    ep_id:    EndpointId,
}

Registry {
    entries: [Option<Entry>; 8],
}
```

There is no heap allocation. The array lives in a static global protected by a spinlock.
Slots that have not been populated are `None`; there is no separate validity flag.
Names are not NUL-terminated; the registry uses an explicit `name_len` field.
The 8-entry limit is intentional: Phase 7 has four services at most (init, console, kbd, and
one spare), and a larger table would invite scope creep.

### API

```
register(name: &str, ep_id: EndpointId) -> Result<(), RegistryError>
lookup(name: &str) -> Option<EndpointId>
```

`register` fails if the name is already present or the table is full.
`lookup` performs a linear scan; with at most 8 entries this is always fast enough.

### Ring-3 access (syscalls 9 and 10)

When real userspace processes exist (Phase 8+), they cannot call the registry directly because
it lives in kernel address space. Two syscalls provide the bridge:

| Syscall | Number | Arguments | Returns |
|---|---|---|---|
| `sys_registry_register` | 9 | endpoint cap handle, name ptr, name len | `u64` — `0` on success; `u64::MAX` on error |
| `sys_registry_lookup` | 10 | name ptr, name len | `u64` — new endpoint `CapHandle` on success; `u64::MAX` on error |

In Phase 7, these syscalls are wired up but only used internally: all servers are kernel tasks
and can call the registry functions directly.

### Comparison with real systems

| System | Service discovery mechanism |
|---|---|
| seL4 (CapDL) | Endpoints are pre-allocated by a static capability description; no runtime nameserver needed at boot |
| Mach / macOS | Bootstrap server holds a port (Mach port = endpoint); clients look up names via `bootstrap_look_up()` |
| L4/Fiasco | No built-in nameserver; convention is that the root task distributes capabilities directly |
| Plan 9 | Every service is a filesystem; discovery is `open("/srv/console")` |

Our registry is closest to Mach's bootstrap server, simplified to a static array because we
have no dynamic allocation requirement in Phase 7.

---

## Bootstrap Sequence

### Who starts what

The kernel's `kernel_main` function calls `init_task` as its final initialization step, after
memory, interrupts, and the scheduler are running. `init_task` is the only task the kernel
creates directly; everything else is started by `init_task`.

```mermaid
flowchart TD
    KM["kernel_main"] --> IT["init_task\n(kernel thread)"]
    IT --> CE["create console endpoint"]
    IT --> KE["create kbd endpoint"]
    IT --> RC["register console ep"]
    IT --> RK["register kbd ep"]
    IT --> SC["spawn console_server"]
    IT --> SK["spawn kbd_server"]
    SC --> CS["console_server loop"]
    SK --> KS["kbd_server loop"]
```

### Step-by-step ordering

Each step emits a `log::info!` line so the boot log shows the exact sequence:

```
[init] creating console endpoint
[init] creating kbd endpoint
[init] registering console (ep=1)
[init] registering kbd (ep=2)
[init] spawning console_server
[init] spawning kbd_server
[init] bootstrap complete
```

The ordering guarantee is intentional: endpoints are registered *before* the server tasks are
spawned. This means any task that calls `lookup("console")` after init completes will always
find a valid endpoint — even if `console_server` has not yet executed a single instruction.

The server tasks are in `Blocked(Receiving)` state immediately on creation; they will only
consume CPU time when a client sends them a message.

### Comparison with real init daemons

| System | How services are ordered |
|---|---|
| systemd | Dependency graph (`Requires=`, `After=`); parallel startup with socket activation |
| s6 | Supervision tree; `s6-rc` computes ordering from service definitions |
| launchd (macOS) | Mach port activation — service is launched on first port message |
| OpenRC | Shell scripts with `need` / `use` dependency keywords |

Phase 7 uses **fixed ordering** because there are only four services and their dependency
graph is trivial. A production system needs dynamic ordering because the graph has hundreds
of nodes and contains cycles that require activation-on-demand to break.

---

## console_server

### What it does

`console_server` exposes a single operation: write a string to the serial output.

Message format (using the Phase 6 `Message` type):

```
label:     CONSOLE_WRITE = 0
data[0]:   pointer to string (kernel virtual address)
data[1]:   string length in bytes
data[2..]: unused
```

The server loop:

```
recv(console_ep) -> msg
while running:
    match msg.label:
        CONSOLE_WRITE =>
            ptr  = msg.data[0] as *const u8
            len  = msg.data[1] as usize
            // write len bytes from ptr to serial
            reply(msg.client, Message { label: 0, .. })  // acknowledge
        _ =>
            reply(msg.client, Message { label: ERR_UNKNOWN_OP, .. })
    msg = reply_recv(console_ep)
```

The reply label `0` means success. Clients block on `call(console_ep, msg)` and unblock when
the reply arrives — the write is synchronous from the client's perspective.

### Why route through IPC instead of calling the serial driver directly?

In Phase 7, with all tasks in kernel address space, the server adds latency rather than
removing it. The value is architectural:

- A future `console_server` can buffer writes, rate-limit noisy tasks, or redirect output
  to a framebuffer terminal — all without changing any client.
- Access control becomes possible: only tasks that hold a console endpoint capability can
  write output.

### Comparison with production consoles

| Feature | Phase 7 console_server | Production (e.g., Linux tty subsystem) |
|---|---|---|
| Output path | IPC message -> serial write | write() syscall -> line discipline -> UART driver |
| Virtual terminals | None | Multiple VTs per physical console |
| Per-client state | None | Each open fd has its own line discipline state |
| Framebuffer mixing | None | fbcon or DRM/KMS composites text layer |
| ANSI escape handling | None | Full termios processing |

---

## kbd_server

### What it does

`kbd_server` bridges hardware keyboard events to IPC clients. It has two sides:

1. **IRQ side** — waits on a keyboard notification object; woken by each IRQ1
2. **Client side** — forwards key events to registered clients via IPC

### IRQ side

Phase 6 introduced `Notification` objects: a machine-word bitfield the kernel can set
atomically from an interrupt handler. `kbd_server` allocates a notification object at startup
and binds it to IRQ1:

```
let kbd_notif = Notification::new();
bind_irq(IRQ_KEYBOARD, &kbd_notif, bit: 0);

loop:
    kbd_notif.wait()              // sleep until IRQ1 fires
    scancode = port_read(0x60)    // read PS/2 data port
    pic_eoi(IRQ_KEYBOARD)         // send End Of Interrupt to PIC
    key_event = translate(scancode)
    forward(key_event)
```

The interrupt handler itself does nothing except set the notification bit and return. All
real work happens in the `kbd_server` task context after the notification wakes it. This is
why the CLAUDE.md rule says "no allocation, no blocking, no IPC from within an interrupt
handler" — those operations happen here, in the server, not in the handler.

### Client side

In Phase 7, `kbd_server` forwards events to whichever client registered first (typically
`init` or a future shell task). The forwarding is a simple `send`:

```
send(client_ep, Message { label: KEY_EVENT, data: [scancode, 0, 0, 0], .. })
```

The server does not wait for a reply before accepting the next IRQ; key events are fire-and-
forget from `kbd_server`'s perspective. If the client is not ready, the send blocks — which
is acceptable because the keyboard is slow compared to any task.

### Comparison with production input stacks

| Feature | Phase 7 kbd_server | Production (Linux evdev / Wayland) |
|---|---|---|
| Input source | PS/2 port 0x60 | HID subsystem (USB HID, PS/2, I2C) |
| Event format | Raw scancode | `struct input_event` (type/code/value) |
| Focus routing | Single static client | Wayland compositor tracks focused surface |
| Buffering | None; blocks on client | Kernel ring buffer per `/dev/input/eventN` fd |
| Key repeat | Not implemented | Handled by evdev at configurable rate |
| Multi-device | Not implemented | Merged by libinput or compositor |

---

## Limitations and Deferred Work

### Servers are kernel tasks, not ring-3 processes

The most important limitation: in Phase 7, `console_server` and `kbd_server` run as kernel
threads in ring 0, sharing the kernel address space. They are not isolated processes.

This is a deliberate deferral. Building true ring-3 servers requires an ELF loader (to parse
and map server binaries) and a process manager (to allocate page tables, set up the initial
stack, and transfer capabilities). Those belong to Phase 8.

The architecture is otherwise identical to what ring-3 servers would look like: the IPC paths,
the endpoint capability model, and the service registry all work the same way. Moving servers
to ring 3 will be a contained change to how tasks are *created*, not to how they *communicate*.

### No process isolation

Because servers share the kernel address space, a bug in `console_server` can corrupt kernel
data structures. In a real microkernel, this is impossible by construction: each server runs in
its own page table and can only reach kernel memory through validated syscalls.

### String pointers are kernel addresses

The `CONSOLE_WRITE` message passes a pointer directly in the IPC payload:

```
data[0]: pointer to string (kernel virtual address)
```

In Phase 8+, when clients are ring-3 processes, this cannot work: the pointer would be a
user virtual address, which the server cannot dereference directly. The correct mechanism is a
**page capability grant**: the client maps a page into the server's address space, writes the
string there, then sends only the page capability and offset. Phase 7 defers this because there
are no ring-3 processes yet.

### Registry is static

The service registry supports `register` and `lookup` but not:

- **Deregistration** — once registered, an entry cannot be removed
- **Restart policy** — if a server crashes, nothing restarts it
- **Health monitoring** — there is no heartbeat or watchdog mechanism

These are standard features of production service managers (systemd, s6, SMF). They are
deferred because Phase 7's goal is to prove that the IPC plumbing works, not to build a
production service supervisor.

---

## See Also

- `docs/06-ipc.md` — IPC model, message format, capabilities, and notification objects
- `docs/roadmap/07-core-servers.md` — phase milestone plan and acceptance criteria
- `docs/08-roadmap.md` — overall project design questions and phase scope
