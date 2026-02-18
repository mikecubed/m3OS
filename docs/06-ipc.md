# IPC — Interprocess Communication

## Overview

IPC is the **core primitive of a microkernel**. Because drivers, filesystems, and most
OS services run in userspace, they must communicate with each other and with the kernel
through a well-defined, controlled mechanism.

The kernel enforces:
- **Isolation** — processes cannot access each other's memory directly
- **Security** — IPC endpoints are capabilities; you can only communicate with a server
  if you hold a valid handle to its endpoint
- **Scheduling integration** — blocked threads sleep until a message arrives; woken
  atomically on delivery

---

## Design Decision: IPC Model

> ⚠️ **This is an open design question.** The choice made here affects the entire
> architecture. Both options are described below.

### Option A: Synchronous Rendezvous (L4-style) — Recommended for Phase 6

Both sender and receiver must be ready simultaneously. The kernel transfers the message
directly from one thread's registers/stack to the other's, with no intermediate buffer.

**Pros:**
- Very fast (zero-copy, no allocation)
- Simple to implement correctly
- Natural backpressure — sender blocks until receiver is ready
- Used by L4, seL4, Fiasco, OKL4

**Cons:**
- Can cause priority inversion
- Harder to do "fire and forget"
- Not natural for async/event-driven servers

### Option B: Async Channels (ring buffers)

Each endpoint has a bounded ring buffer. Sender writes; receiver reads at its own pace.

**Pros:**
- Decoupled; sender doesn't block
- Natural for driver/event patterns (keyboard events, network packets)

**Cons:**
- Requires kernel-managed buffers (allocation)
- More complex to implement and reason about

### Recommendation

Start with **synchronous rendezvous** for correctness and simplicity. Add **async
notification objects** (seL4-style) for event delivery (hardware IRQs, etc.) in a
later phase. This is the approach used by most research microkernels.

---

## Synchronous Message Passing

```mermaid
sequenceDiagram
    participant Client
    participant Kernel
    participant Server

    Client->>Kernel: syscall send(endpoint, msg) — blocks
    Note over Kernel: Client is Blocked(Sending)
    Server->>Kernel: syscall recv(endpoint) — blocks
    Note over Kernel: Server was Blocked(Receiving)
    Kernel->>Kernel: Match sender + receiver
    Kernel->>Kernel: Copy message (registers or small buffer)
    Kernel-->>Client: unblock (send complete)
    Kernel-->>Server: unblock with message
    Server->>Kernel: syscall reply(client, response)
    Kernel-->>Client: reply delivered, client unblocks
```

### Call Semantics

| Operation | Description |
|---|---|
| `send(ep, msg)` | Send message to endpoint; block until received |
| `recv(ep)` | Wait for a message on endpoint; block until one arrives |
| `call(ep, msg)` | `send` + immediately wait for a reply (RPC pattern) |
| `reply(cap, msg)` | Send reply to the thread that called us |
| `reply_recv(cap, msg)` | Reply and immediately start waiting for next message |

The `call` + `reply_recv` pair enables a tight **server loop** without extra syscalls:

```
server loop:
    msg = recv(my_endpoint)        # wait for first client
    loop:
        response = handle(msg)
        msg = reply_recv(client, response)  # reply + wait for next
```

---

## Capabilities & Handles

A **capability** is an unforgeable token that grants the holder specific rights to a
kernel object (endpoint, memory page, interrupt, etc.). In our implementation:

- Capabilities are **integer handles** stored in a per-process **capability table**
- The kernel validates the handle on every syscall
- Capabilities can be **granted** (sent via IPC) or **revoked** by the parent

```mermaid
graph TD
    P1["Process A\ncap_table"] -->|"handle 3 →"| EP1["Endpoint\n(console_server)"]
    P2["Process B\ncap_table"] -->|"handle 1 →"| EP1
    P3["init\ncap_table"] -->|"handle 0 →"| EP1
    EP1 -->|"owned by"| CONSOLE["console_server thread"]

    style EP1 fill:#8e44ad,color:#fff
```

### Capability Types (initial set)

| Type | Description |
|---|---|
| `EndpointCap` | Right to send/receive on an IPC endpoint |
| `ThreadCap` | Right to manage (start/stop/kill) a thread |
| `PageCap` | Right to map a specific physical page |
| `IrqCap` | Right to receive delivery of a specific hardware IRQ |

---

## Message Format

Messages are small by design (synchronous zero-copy transfer through CPU registers):

```
Message {
    label:  u64,         // identifies the operation (like a method ID)
    data:   [u64; 4],    // up to 4 words of inline data (fits in registers)
    caps:   [CapSlot; 2] // up to 2 capability grants (optional)
}
```

For larger data transfers (e.g., reading a file block), the kernel supports
**shared memory grants**: temporarily mapping a physical page into the receiver's
address space, then revoking it after the call. This is the "zero-copy" path.

---

## IPC and IRQ Delivery

Hardware interrupts need to reach userspace drivers (e.g., `kbd_server`). The pattern:

1. At init time, `kbd_server` registers for IRQ1 via a syscall, receiving an `IrqCap`
2. The kernel's IRQ handler, instead of doing work, sends a notification to the `IrqCap`
3. `kbd_server` blocks on `recv(irq_cap)` — woken up on each keypress
4. `kbd_server` reads the scancode from port `0x60` (permitted via an `IoCap`)

```mermaid
sequenceDiagram
    participant HW as Hardware
    participant Kernel
    participant KbdServer

    KbdServer->>Kernel: recv(irq_cap) — wait for IRQ
    HW->>Kernel: IRQ1 fires
    Kernel->>Kernel: IDT handler: deliver to IrqCap holder
    Kernel-->>KbdServer: unblock, notify IRQ arrived
    KbdServer->>KbdServer: read scancode from port 0x60
    KbdServer->>KbdServer: translate → key event
    KbdServer->>Kernel: send(console_ep, key_event)
```

---

## Server Registration Pattern

Servers register themselves with `init` at startup, which acts as a nameserver:

```mermaid
sequenceDiagram
    participant init
    participant ConsoleServer
    participant Shell

    ConsoleServer->>init: register("console", my_endpoint_cap)
    Shell->>init: lookup("console") → console_ep_cap
    Shell->>ConsoleServer: call(console_ep, Write("hello\n"))
    ConsoleServer-->>Shell: reply(OK)
```

---

## Open Questions

1. **Synchronous vs. async** — Start sync (Phase 6); add async notifications later?
2. **Capability table size** — Fixed-size per process (simple) or growable?
3. **Capability delegation depth** — Can a server re-grant capabilities it received?
4. **Large data transfer** — Page grants immediately, or copy-on-write semantics?
5. **Timeout on IPC calls** — Should `call()` support a timeout to prevent deadlock?
