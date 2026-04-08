# Phase 50 — IPC Completion: Task List

**Status:** Complete
**Source Ref:** phase-50
**Depends on:** Phase 6 (IPC Core) ✅, Phase 7 (Core Servers) ✅, Phase 8 (Storage and VFS) ✅, Phase 39 (Unix Domain Sockets) ✅, Phase 46 (System Services) ✅, Phase 49 (Architectural Declaration) ✅
**Goal:** Finish the IPC transport model so ring-3 services can safely transfer capabilities, exchange bulk data, register without kernel-pointer assumptions, and follow a standardized server-loop lifecycle with explicit failure semantics.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Evaluation gate and shortcut audit | — | ✅ Done |
| B | Capability transfer completion | A | ✅ Done |
| C | Bulk-data transport | A, B | ✅ Done |
| D | Ring-3-safe registry and copy-from-user | A | ✅ Done |
| E | Syscall wiring and server-loop semantics | B, C, D | ✅ Done |
| F | Proof-of-concept service port | E | ✅ Done |
| G | Tests and validation | B, C, D, E, F | ✅ Done |
| H | Documentation, versioning, and roadmap integration | All | ✅ Done |

---

## Track A — Evaluation Gate and Shortcut Audit

### A.1 — Enumerate kernel-pointer shortcuts in IPC service syscalls

**Files:**
- `kernel/src/ipc/mod.rs`
- `kernel/src/main.rs`

**Symbol:** `ipc_register_service`, `ipc_lookup_service`
**Why it matters:** The safety comments at `ipc_register_service` (line 229) and `ipc_lookup_service` (line 254) explicitly state that name pointers are dereferenced as raw kernel addresses because all callers are kernel tasks — this assumption must be catalogued before it can be removed.

**Acceptance:**
- [x] A written list exists (in a commit message, PR description, or inline comment block) identifying every site in `kernel/src/ipc/mod.rs` and `kernel/src/main.rs` where IPC bypasses copy-from-user or assumes a shared kernel address space
- [x] Each identified site includes the file, line range, and the specific assumption being made
- [x] The list covers the five kernel-task server loops in `kernel/src/main.rs` (console_server, kbd_server, fat_server, vfs_server, and any others)

### A.2 — Verify Phase 49 ownership matrix covers all IPC-adjacent services

**Files:**
- `docs/appendix/architecture-and-syscalls.md`
- `docs/evaluation/microkernel-path.md`

**Symbol:** Keep/Move/Transition Matrix
**Why it matters:** The Phase 50 evaluation gate requires that Phase 49 explicitly defines which later services depend on the transport model — if any are missing the matrix must be updated before IPC work proceeds.

**Acceptance:**
- [x] Every in-kernel server loop in `kernel/src/main.rs` appears in the Keep/Move/Transition matrix with an explicit classification
- [x] `docs/evaluation/microkernel-path.md` Stage 1 concrete-work list is consistent with the audit from A.1
- [x] Any gaps discovered are resolved by adding missing rows or updating classifications before tracks B–E begin

### A.3 — Inventory bulk-data payload targets

**File:** `docs/roadmap/50-ipc-completion.md`
**Symbol:** Evaluation Gate — Bulk-data target set
**Why it matters:** The evaluation gate requires that the transport design covers strings, file blocks, packets, and framebuffer-sized payloads — the inventory confirms no subsystem-specific hole is left.

**Acceptance:**
- [x] A concrete list of payload types (service-name strings, FAT32 file blocks, network packet buffers, framebuffer spans, VFS path strings) is documented with their size ranges
- [x] Each payload type is mapped to a proposed transport mechanism (inline message, copy-from-user, page grant, or shared buffer)
- [x] The list is referenced by tracks C and E as the design target

---

## Track B — Capability Transfer Completion

### B.1 — Write failing tests for capability grant through IPC messages

**File:** `kernel-core/src/ipc/capability.rs`
**Symbol:** `CapabilityTable::grant`
**Why it matters:** TDD requires failing tests before implementation — the grant path must verify that a capability can be atomically moved from one table to another without duplication or loss.

**Acceptance:**
- [x] At least three host-side unit tests exist in `kernel-core` that call a `grant` or `transfer` method and assert correct behavior
- [x] Tests cover: successful grant (source slot cleared, destination slot populated), grant to a full table (returns `CapError::TableFull`), grant of an invalid handle (returns `CapError::InvalidHandle`)
- [x] All tests fail (red) before the implementation in B.2 lands

### B.2 — Implement atomic capability grant between capability tables

**File:** `kernel-core/src/ipc/capability.rs`
**Symbol:** `CapabilityTable::grant`
**Why it matters:** Without atomic grant semantics, capabilities can be duplicated (privilege escalation) or lost (service breakage) during transfer.

**Acceptance:**
- [x] `CapabilityTable` exposes a `grant(source_handle, dest_table) -> Result<CapHandle, CapError>` method (or equivalent two-table transfer API)
- [x] The grant atomically removes the capability from the source table and inserts it into the destination table
- [x] On destination-table-full, the source retains the capability and `CapError::TableFull` is returned
- [x] All tests from B.1 pass (green)

### B.3 — Extend Message to carry a capability slot

**File:** `kernel-core/src/ipc/message.rs`
**Symbol:** `Message`
**Why it matters:** The current `Message` struct has no field for capability transfer — services cannot exchange authorities through the existing message format.

**Acceptance:**
- [x] `Message` gains an optional capability field (e.g., `cap: Option<Capability>` or a reserved data word convention)
- [x] Existing message constructors (`new`, `with1`, `with2`) continue to work unchanged with no capability attached
- [x] A new constructor or setter allows attaching exactly one capability to a message
- [x] Existing unit tests in `kernel-core/src/ipc/message.rs` still pass

### B.4 — Wire capability transfer into endpoint send/recv

**File:** `kernel/src/ipc/endpoint.rs`
**Symbol:** `Endpoint::send`, `Endpoint::recv_msg`
**Why it matters:** The kernel must actually move the capability between process tables during message delivery — otherwise the Message field from B.3 is inert.

**Acceptance:**
- [x] When a message with an attached capability is delivered via `send` or `call`, the kernel calls the grant logic from B.2 to transfer the capability from sender to receiver
- [x] If the receiver's table is full, the send fails with an explicit error rather than silently dropping the capability
- [x] The sender's capability slot is cleared only after the receiver's slot is successfully populated
- [x] Trace events (`MessageDelivered`, `ReplyDeliver`) log when a capability is transferred

### B.5 — Add sys_cap_grant syscall for explicit out-of-band grants

**Files:**
- `kernel/src/ipc/mod.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs`

**Symbol:** `sys_cap_grant`
**Why it matters:** Not all capability transfers happen inside IPC messages — a direct grant syscall lets a parent pass authorities to a child or a supervisor delegate to a managed service.

**Acceptance:**
- [x] A new syscall (e.g., IPC dispatch number 6 or a new subsystem entry) accepts `(source_handle, target_task_id)` and grants the capability to the target
- [x] The syscall validates that the caller owns the source handle and that the target task exists
- [x] On success the source slot is cleared and the new handle in the target is returned
- [x] On failure an appropriate negative errno or `u64::MAX` is returned with no side effects

---

## Track C — Bulk-Data Transport

### C.1 — Design and document the bulk-data transport contract

**Files:**
- `docs/06-ipc.md`
- `kernel-core/src/ipc/mod.rs`

**Symbol:** (new section in docs/06-ipc.md)
**Why it matters:** Without a single documented contract, storage, networking, and graphics will each invent incompatible bulk-data hacks — the design must be settled before implementation.

**Acceptance:**
- [x] `docs/06-ipc.md` contains a new section describing the chosen bulk-data mechanism (copy-from-user validated buffers, page grants, or shared-buffer regions)
- [x] The section specifies ownership rules: who allocates, who frees, what happens on service crash
- [x] The section covers the payload types inventoried in A.3 with concrete size expectations
- [x] The mechanism is simple enough to implement in this phase and reusable enough that later phases do not need a replacement

### C.2 — Write failing tests for bulk-data copy-from-user path

**File:** `kernel-core/src/ipc/message.rs` or new `kernel-core/src/ipc/buffer.rs`
**Symbol:** `validate_user_buffer`, `copy_from_user`
**Why it matters:** TDD requires red tests before green implementation — the copy path must reject invalid addresses and correctly transfer data.

**Acceptance:**
- [x] Host-side tests exist that call the validation and copy functions with valid buffers, null pointers, out-of-range addresses, and zero-length buffers
- [x] Tests assert correct data transfer on success and explicit error returns on invalid input
- [x] All tests fail before C.3 implementation

### C.3 — Implement validated copy-from-user and copy-to-user primitives

**Files:**
- `kernel/src/mm/user_space.rs` (or appropriate mm module)
- `kernel-core/src/ipc/buffer.rs` (pure-logic validation)

**Symbol:** `copy_from_user`, `copy_to_user`
**Why it matters:** Every IPC path that reads or writes userspace memory must validate the address range against the process page tables — raw pointer dereference is the shortcut being eliminated.

**Acceptance:**
- [x] `copy_from_user(task_id, user_ptr, len) -> Result<Vec<u8>, MemError>` validates that the entire `[user_ptr, user_ptr+len)` range is mapped and readable in the task's address space
- [x] `copy_to_user(task_id, user_ptr, data) -> Result<(), MemError>` validates writability before copying
- [x] Invalid or unmapped ranges return an explicit error, never a panic or silent corruption
- [x] All tests from C.2 pass

### C.4 — Implement page-grant or shared-buffer mechanism for large transfers

**Files:**
- `kernel/src/mm/user_space.rs`
- `kernel/src/ipc/endpoint.rs`
- `kernel-core/src/ipc/capability.rs`

**Symbol:** `Capability::Grant` (new variant), `map_grant`
**Why it matters:** Copy-from-user works for small payloads but is too expensive for framebuffer-sized or streaming transfers — a zero-copy path is required by the evaluation gate.

**Acceptance:**
- [x] A new `Capability::Grant(PhysFrame, PageCount, Permissions)` variant (or equivalent) represents shared page ownership
- [x] The kernel can map a grant into a receiver's address space with specified permissions (read-only or read-write)
- [x] Revoking the grant unmaps the pages from the receiver without corrupting either address space
- [x] A host-side or QEMU test validates that a granted page is accessible in the receiver and inaccessible after revocation

---

## Track D — Ring-3-Safe Registry and Copy-from-User

### D.1 — Replace kernel-pointer dereference in ipc_register_service

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `ipc_register_service`
**Why it matters:** The current implementation at line 232 dereferences `name_ptr` as a raw kernel pointer — this is a memory-safety violation when called from ring-3 userspace.

**Acceptance:**
- [x] `ipc_register_service` uses `copy_from_user` (from C.3) to read the service name from the caller's address space
- [x] The `// Safety: Phase 7 only` comment block (lines 229–231) is removed and replaced with the validated path
- [x] Invalid or unmapped name pointers return an error to the caller instead of faulting the kernel
- [x] `cargo xtask check` passes with no new warnings

### D.2 — Replace kernel-pointer dereference in ipc_lookup_service

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `ipc_lookup_service`
**Why it matters:** The lookup syscall at line 257 has the same kernel-pointer assumption as the register path — both must be fixed for ring-3 safety.

**Acceptance:**
- [x] `ipc_lookup_service` uses `copy_from_user` to read the service name from the caller's address space
- [x] The `// Safety: Phase 7 only` comment block (lines 254–256) is removed
- [x] Invalid pointers produce an error return, not a kernel fault
- [x] `cargo xtask check` passes

### D.3 — Remove kernel-task-only assumptions from service registration

**Files:**
- `kernel/src/ipc/registry.rs`
- `kernel-core/src/ipc/registry.rs`

**Symbol:** `Registry::register`, `Registry::lookup`
**Why it matters:** The registry must work identically whether the caller is a kernel task or a ring-3 process — any remaining ring-0-only assumption prevents real service extraction.

**Acceptance:**
- [x] Registry registration accepts a `TaskId` parameter so the registry tracks which task owns each service
- [x] Registry lookup does not assume the caller is in kernel address space
- [x] Service re-registration (for restart) is supported: a new task can replace a dead service's entry
- [x] Existing registry unit tests in `kernel-core/src/ipc/registry.rs` are updated and pass

### D.4 — Increase registry capacity limits

**File:** `kernel-core/src/ipc/registry.rs`
**Symbol:** `MAX_SERVICES`, `MAX_NAME_LEN`
**Why it matters:** The current limits (8 services, 32-byte names) are too small for the service inventory identified in the Phase 49 ownership matrix — later phases will exceed them.

**Acceptance:**
- [x] `MAX_SERVICES` is increased to at least 16 (or dynamically sized)
- [x] `MAX_NAME_LEN` is reviewed and increased if any planned service name exceeds 32 bytes
- [x] Existing unit tests are updated for the new limits and pass
- [x] No regression in `cargo test -p kernel-core`

---

## Track E — Syscall Wiring and Server-Loop Semantics

### E.1 — Wire IPC dispatch into the main syscall handler

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `syscall_handler` dispatch chain (lines 966–983)
**Why it matters:** The IPC syscall dispatcher exists in `kernel/src/ipc/mod.rs` but is not called from the syscall entry point — IPC syscalls 1–5 and 7–10 currently return `ENOSYS` from userspace.

**Acceptance:**
- [x] The syscall dispatch chain in `kernel/src/arch/x86_64/syscall/mod.rs` includes a call to an IPC handler module (e.g., `ipc::handle_ipc_syscall()`)
- [x] IPC syscalls are routed to the existing `ipc::dispatch()` function
- [x] Non-IPC syscalls are unaffected
- [x] `cargo xtask check` passes with no new warnings

### E.2 — Create an IPC syscall subsystem module following the Phase 49 pattern

**File:** `kernel/src/arch/x86_64/syscall/ipc.rs` (new)
**Symbol:** `handle_ipc_syscall`
**Why it matters:** Phase 49 decomposed syscalls into per-subsystem modules (fs, mm, process, net, signal, io, time, misc) — IPC must follow the same pattern for consistency and maintainability.

**Acceptance:**
- [x] `kernel/src/arch/x86_64/syscall/ipc.rs` exists with a `handle_ipc_syscall` function matching the signature pattern of existing subsystem handlers
- [x] The module is declared in `kernel/src/arch/x86_64/syscall/mod.rs`
- [x] IPC syscall numbers are defined as named constants, not magic numbers
- [x] The dispatch delegates to `kernel::ipc::dispatch()` for the actual implementation

### E.3 — Document and implement server-loop failure semantics

**Files:**
- `docs/06-ipc.md`
- `kernel/src/ipc/endpoint.rs`

**Symbol:** `reply_recv_msg`
**Why it matters:** The evaluation gate requires documented failure semantics for the recv → handle → reply_recv loop — without them, extracted services cannot reliably detect peer death or restart.

**Acceptance:**
- [x] `docs/06-ipc.md` contains a new section documenting what happens when: a client dies before the server replies, a server dies while a client is blocked in `call`, a service restarts and re-registers
- [x] `reply_recv_msg` returns a distinguishable error or sentinel when the reply target no longer exists
- [x] A blocked `call` caller is unblocked with an error when the server endpoint is destroyed
- [x] The semantics are consistent with the Phase 46 service manager's restart behavior

### E.4 — Add endpoint and notification cleanup on task exit

**Files:**
- `kernel/src/ipc/endpoint.rs`
- `kernel/src/ipc/notification.rs`
- `kernel/src/process/mod.rs`

**Symbol:** `cleanup_task_ipc`
**Why it matters:** When a service crashes or exits, blocked callers must be unblocked and pending messages must not leak — otherwise service restart creates zombie IPC state.

**Acceptance:**
- [x] A `cleanup_task_ipc(task_id)` function is called during task exit
- [x] All callers blocked on the dying task's reply capability are woken with an error
- [x] The task's pending sends and receives are drained from all endpoint queues
- [x] Notification waiters for the dying task are cleared
- [x] No capability table entries leak after cleanup

---

## Track F — Proof-of-Concept Service Port

### F.1 — Port the console server loop to use validated IPC paths

**File:** `kernel/src/main.rs`
**Symbol:** `console_server` (lines 328–395)
**Why it matters:** The phase acceptance criteria require at least one representative service path using the new transport without kernel-pointer shortcuts — the console server is the simplest candidate.

**Acceptance:**
- [x] The console server's recv/reply_recv calls go through the syscall-visible IPC path (or the equivalent validated internal path) rather than directly calling `ipc::endpoint::recv_msg` with kernel pointers
- [x] Any string payloads (write data) are transferred via the bulk-data path from Track C, not raw pointer dereference
- [x] The console server can still be tested end-to-end via `cargo xtask run` with serial output working
- [x] The server handles client disconnection gracefully using the failure semantics from E.3

### F.2 — Validate the ported service under the Phase 46 service manager

**Files:**
- `kernel/src/main.rs`
- `userspace/init/src/main.rs`

**Symbol:** `console_server`, `ServiceConfig`
**Why it matters:** The evaluation gate requires that restart, disconnect, and reply/receive semantics work with the existing supervisor — a ported service that cannot be managed is not a real proof.

**Acceptance:**
- [x] The console server uses the same lifecycle as Phase 46 managed services (registration, health, restart)
- [x] If the console server is killed, blocked callers receive an error and the service can restart
- [x] Service death and restart are observable via syslog or trace ring events

---

## Track G — Tests and Validation

### G.1 — Host-side unit tests for capability grant logic

**File:** `kernel-core/src/ipc/capability.rs`
**Symbol:** `CapabilityTable::grant` tests
**Why it matters:** The grant path is the most security-sensitive new code — host-side tests catch logic errors without the overhead of QEMU boot.

**Acceptance:**
- [x] At least six test cases: successful grant, grant-to-full-table, grant-invalid-handle, double-grant (idempotency check), grant-then-revoke, grant across different capability types
- [x] `cargo test -p kernel-core` passes with all new tests

### G.2 — Host-side unit tests for registry ownership and re-registration

**File:** `kernel-core/src/ipc/registry.rs`
**Symbol:** `Registry` tests
**Why it matters:** Re-registration on restart is a new code path that must be validated to prevent stale-service bugs.

**Acceptance:**
- [x] Tests cover: register with owner, lookup returns correct owner, re-register after death replaces entry, re-register while alive returns error
- [x] `cargo test -p kernel-core` passes

### G.3 — Host-side unit tests for copy-from-user validation logic

**File:** `kernel-core/src/ipc/buffer.rs` (or equivalent)
**Symbol:** `validate_user_buffer` tests
**Why it matters:** Address validation is a security boundary — incorrect validation enables ring-3 code to read or write arbitrary kernel memory.

**Acceptance:**
- [x] Tests cover: valid range, zero-length, null pointer, range wrapping past address space end, partially-mapped range
- [x] `cargo test -p kernel-core` passes

### G.4 — Loom concurrency tests for capability grant under contention

**File:** `kernel-core/tests/ipc_loom.rs`
**Symbol:** (new test functions)
**Why it matters:** Capability grants touch two capability tables that may be accessed concurrently — loom catches ordering bugs that unit tests miss.

**Acceptance:**
- [x] At least one loom test verifies that concurrent grants to the same destination table never exceed capacity or lose capabilities
- [x] `RUSTFLAGS="--cfg loom" cargo test -p kernel-core --test ipc_loom` passes

### G.5 — QEMU integration test for IPC syscalls from userspace

**File:** `kernel/tests/ipc_syscall.rs` (new)
**Symbol:** (new test binary)
**Why it matters:** The syscall wiring from E.1 must be validated end-to-end from a real ring-3 process — host tests cannot cover the syscall gate.

**Acceptance:**
- [x] A QEMU test binary exercises `ipc_register_service`, `ipc_lookup_service`, `ipc_send`, `ipc_recv`, and `ipc_call` from userspace
- [x] The test verifies that invalid pointers return errors rather than faulting the kernel
- [x] `cargo xtask test --test ipc_syscall` passes

### G.6 — Run full quality gate

**Symbol:** `cargo xtask check`
**Why it matters:** The phase must not introduce clippy warnings, formatting regressions, or break existing host tests.

**Acceptance:**
- [x] `cargo xtask check` passes (clippy -D warnings + rustfmt + kernel-core host tests)
- [x] `cargo test -p kernel-core` passes including all new IPC tests
- [x] `cargo xtask test` passes all existing QEMU tests plus the new IPC test from G.5

---

## Track H — Documentation, Versioning, and Roadmap Integration

### H.1 — Create aligned learning doc for Phase 50

**File:** `docs/50-ipc-completion.md` (new)
**Symbol:** (aligned learning doc)
**Why it matters:** The learning-doc requirement in the phase design doc mandates an aligned doc following the template in `docs/appendix/doc-templates.md`.

**Acceptance:**
- [x] `docs/50-ipc-completion.md` exists and follows the **Template: aligned legacy learning doc** from `docs/appendix/doc-templates.md`
- [x] Covers capability grants, bulk-data paths, registry behavior, server-loop semantics, and the specific shortcuts removed
- [x] Scoped to Phase 50 only — does not cover deferred items like typed IDLs or advanced delegation
- [x] Includes a Related Roadmap Docs section linking the phase design doc and task doc

### H.2 — Update docs/06-ipc.md with finished transport model

**File:** `docs/06-ipc.md`
**Symbol:** (multiple sections)
**Why it matters:** The Phase 6 IPC doc is the canonical reference — it must reflect the completed transport model including capability grants, bulk-data paths, and failure semantics added in this phase.

**Acceptance:**
- [x] The doc describes capability grant semantics (not just message control)
- [x] The doc describes the bulk-data transport mechanism
- [x] The doc describes server-loop failure semantics (client death, server death, restart)
- [x] The "Deferred to Phase 7+" comment about capability grants is removed or updated
- [x] The doc is internally consistent — no references to the old kernel-pointer convention as current behavior

### H.3 — Update docs/07-core-servers.md and docs/08-storage-and-vfs.md

**Files:**
- `docs/07-core-servers.md`
- `docs/08-storage-and-vfs.md`

**Symbol:** (service model sections)
**Why it matters:** The phase design doc explicitly requires updating these docs to match the finished transport model.

**Acceptance:**
- [x] `docs/07-core-servers.md` reflects that services can be ring-3 processes, not just kernel tasks
- [x] `docs/08-storage-and-vfs.md` describes the bulk-data path for file blocks
- [x] No stale references to kernel-pointer conventions remain in either doc

### H.4 — Update docs/appendix/architecture-and-syscalls.md

**File:** `docs/appendix/architecture-and-syscalls.md`
**Symbol:** Keep/Move/Transition Matrix, Syscall Ownership Classification
**Why it matters:** The architecture doc must reflect any new syscalls added (sys_cap_grant) and updated IPC syscall classifications.

**Acceptance:**
- [x] Any new IPC syscalls are added to the syscall ownership table
- [x] The IPC section of the Keep/Move/Transition matrix reflects the completed transport model
- [x] The architecture review checklist still applies correctly to the updated IPC surface

### H.5 — Update evaluation docs

**Files:**
- `docs/evaluation/microkernel-path.md`
- `docs/evaluation/roadmap/R03-ipc-completion.md`

**Symbol:** Stage 1
**Why it matters:** The phase design doc requires these evaluation docs to point at the official implementation milestone when the phase lands.

**Acceptance:**
- [x] `docs/evaluation/microkernel-path.md` Stage 1 status is updated to reflect Phase 50 completion
- [x] `docs/evaluation/roadmap/R03-ipc-completion.md` references the actual implementation rather than the planned work
- [x] Version references in both docs point to `0.50.0`

### H.6 — Update docs/roadmap/README.md

**File:** `docs/roadmap/README.md`
**Symbol:** Phase 50 summary row (line 266)
**Why it matters:** The roadmap summary table must reflect the phase status and link the task doc.

**Acceptance:**
- [x] The Phase 50 row status is updated from "Planned" to "In Progress" when implementation begins, and "Complete" when it lands
- [x] The Tasks column links to `./tasks/50-ipc-completion-tasks.md` instead of "Deferred until implementation planning"
- [x] The Primary Outcome column accurately reflects the delivered capabilities

### H.7 — Update docs/README.md

**File:** `docs/README.md`
**Symbol:** Phase-aligned learning docs table
**Why it matters:** The main docs index must list the new Phase 50 learning doc so it is discoverable.

**Acceptance:**
- [x] A row for Phase 50 (IPC Completion) is added to the learning docs table in `docs/README.md`
- [x] The link points to `docs/50-ipc-completion.md`

### H.8 — Update docs/roadmap/tasks/README.md

**File:** `docs/roadmap/tasks/README.md`
**Symbol:** Task list index
**Why it matters:** The task list directory index must include Phase 50 for discoverability.

**Acceptance:**
- [x] Phase 50 is listed in the task doc index with a link to `50-ipc-completion-tasks.md`
- [x] The Mermaid dependency diagram includes Phase 50

### H.9 — Link task doc from phase design doc

**File:** `docs/roadmap/50-ipc-completion.md`
**Symbol:** Companion Task List section
**Why it matters:** The phase design doc currently says "defer until implementation planning begins" — this must be replaced with a real link.

**Acceptance:**
- [x] The Companion Task List section in `docs/roadmap/50-ipc-completion.md` links to `./tasks/50-ipc-completion-tasks.md`
- [x] The deferral text is removed

### H.10 — Bump kernel version to 0.50.0

**File:** `kernel/Cargo.toml`
**Symbol:** `version`
**Why it matters:** The project convention requires the kernel version to match the phase number when the phase lands.

**Acceptance:**
- [x] `kernel/Cargo.toml` version is `0.50.0`
- [x] `kernel-core/Cargo.toml` version is bumped if its public API changed (new capability, message, or buffer types)
- [x] No modified crate is left with a stale version after the phase lands

### H.11 — Update version references in docs and READMEs

**Files:**
- `docs/roadmap/README.md`
- `docs/evaluation/roadmap/R03-ipc-completion.md`
- `docs/README.md`
- `README.md`

**Symbol:** version strings
**Why it matters:** Stale version references create confusion about which capabilities shipped in which release.

**Acceptance:**
- [x] All docs and READMEs that reference the shipped kernel version reflect `0.50.0` where Phase 50 capabilities are described
- [x] No impacted README is left stale after the phase lands

### H.12 — Audit all affected READMEs

**Files:**
- `README.md`
- `docs/README.md`
- `docs/roadmap/README.md`
- `docs/roadmap/tasks/README.md`

**Symbol:** (multiple)
**Why it matters:** Phase 50 changes the IPC model, adds syscalls, and may add new source files — every affected README must reflect the new state.

**Acceptance:**
- [x] Root `README.md` architecture diagram and IPC description reflect the completed transport model
- [x] `docs/README.md` learning doc table includes Phase 50
- [x] `docs/roadmap/README.md` Phase 50 row is accurate and complete
- [x] `docs/roadmap/tasks/README.md` includes Phase 50 task doc
- [x] No impacted README contains stale descriptions of IPC behavior, capability model, or service registration

---

## Documentation Notes

- This phase finishes the IPC transport model that Phase 6 introduced as control-path-only.
- The bulk-data transport replaces the "IPC carries control messages only" convention from Phase 6 with a validated copy + page-grant model.
- Kernel-pointer shortcuts in `ipc_register_service` and `ipc_lookup_service` (documented since Phase 7) are eliminated.
- The server-loop failure semantics are new — Phase 6 documented the happy path but not client/server death handling.
- The `sys_cap_grant` syscall is new to Phase 50 and must be added to the syscall ABI table.
- All in-kernel servers in `kernel/src/main.rs` remain kernel tasks after this phase — full extraction to ring-3 processes is deferred to later serverization phases (51–54). This phase ensures the transport model is ready for that extraction.
