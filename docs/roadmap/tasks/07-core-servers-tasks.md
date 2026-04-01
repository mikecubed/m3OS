# Phase 7 — Core Servers: Task List

**Status:** Complete
**Source Ref:** phase-7
**Depends on:** Phase 6 ✅
**Goal:** Stand up the first userspace services — init, console output, and keyboard input — behind IPC contracts with a simple service registry for discovery.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | init and service registry | — | ✅ Done |
| B | Console and keyboard servers | A | ✅ Done |
| C | Validation and documentation | A, B | ✅ Done |

---

## Track A — Init and Service Registry

### A.1 — Implement init as the first userspace process

**File:** `kernel/src/main.rs`
**Symbol:** `init_task`
**Why it matters:** init (PID 1) anchors the entire userspace; every other service is spawned from it.

**Acceptance:**
- [x] init is the first userspace process started by the kernel
- [x] init is responsible for launching early services in a deterministic order

---

### A.2 — Add a simple service registry for discovering early services

**Files:** `kernel/src/ipc/registry.rs`, `kernel-core/src/ipc/registry.rs`
**Symbol:** `Registry`, `register`, `lookup`
**Why it matters:** Clients need a well-known mechanism to find services by name without hardcoded endpoint IDs.

**Acceptance:**
- [x] Named services can be registered and looked up by userspace clients
- [x] The registry model is simple enough for early bootstrap without complex dependency graphs

---

## Track B — Console and Keyboard Servers

### B.1 — Move console output behind console_server

**File:** `kernel/src/main.rs`
**Symbol:** `console_server_task`
**Why it matters:** Centralizing output through an IPC service decouples producers from the serial/framebuffer hardware path.

**Acceptance:**
- [x] Console output flows through `console_server` rather than direct hardware access
- [x] Userspace clients discover the console service via the registry

---

### B.2 — Route keyboard events through kbd_server

**File:** `kernel/src/main.rs`
**Symbol:** `kbd_server_task`
**Why it matters:** Routing keyboard input through a service allows multiple consumers and keeps interrupt handling minimal.

**Acceptance:**
- [x] Keyboard events are dispatched through `kbd_server` using IPC
- [x] The keyboard interrupt handler does minimal work (scancode + EOI) and notifies the server

---

### B.3 — Define IPC contracts for client-service communication

**Files:** `kernel/src/ipc/endpoint.rs`, `kernel/src/ipc/message.rs`
**Why it matters:** Well-defined contracts let clients find and talk to services without coupling to implementation details.

**Acceptance:**
- [x] IPC message format and endpoint protocol are documented and used consistently
- [x] Clients use the registry to discover endpoints before sending messages

---

### B.4 — Keep bootstrap ordering explicit and debuggable

**File:** `kernel/src/main.rs`
**Symbol:** `init_task`
**Why it matters:** A deterministic boot sequence makes failures reproducible and simplifies debugging from serial logs.

**Acceptance:**
- [x] Service startup order is explicit in `init_task`
- [x] Boot sequence is observable via serial log output

---

## Track C — Validation and Documentation

### C.1 — Verify init launches services in expected order

**Why it matters:** Confirms the bootstrap sequence is deterministic and correct.

**Acceptance:**
- [x] init spawns console and keyboard servers in the documented order
- [x] Serial log shows the expected startup sequence

---

### C.2 — Verify userspace clients can discover and use console_server

**Why it matters:** End-to-end validation that the registry and IPC path work for output.

**Acceptance:**
- [x] A userspace client discovers the console service and sends output through it

---

### C.3 — Verify keyboard events reach userspace through kbd_server

**Why it matters:** Confirms the keyboard path no longer relies on ad hoc kernel code.

**Acceptance:**
- [x] Keyboard events are received by userspace through `kbd_server` IPC

---

### C.4 — Document service startup sequence and ownership boundaries

**Why it matters:** Future phases need to understand what runs in kernel vs. userspace.

**Acceptance:**
- [x] Service startup sequence and kernel/server ownership boundaries are documented

---

### C.5 — Document the registry approach for early bootstrapping

**Why it matters:** The registry design informs how future services are discovered.

**Acceptance:**
- [x] Registry/nameserver approach is documented with rationale

---

### C.6 — Note on mature supervision and service discovery

**Why it matters:** Sets expectations for what a production OS would add beyond this toy implementation.

**Acceptance:**
- [x] Short note explains how real systems add supervision, restart policies, and richer discovery

---

## Documentation Notes

- Phase 7 introduced the first userspace services (console, keyboard) behind IPC, replacing direct hardware access patterns from Phase 6.
- The service registry provides named lookup, removing the need for hardcoded endpoint IDs.
