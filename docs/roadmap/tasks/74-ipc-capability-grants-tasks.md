# Phase 74 — IPC Capability Grants and Bulk Transfers: Task List

**Status:** Planned
**Source Ref:** phase-74
**Depends on:** Phase 6 (IPC Core) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 57a (Scheduler Rewrite) ✅
**Goal:** Close four IPC deferrals accumulated since Phase 6: capability handles in IPC messages, page-grant zero-copy bulk transfer, per-call IPC timeouts, and many-to-one notification binding.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `sys_cap_grant` surface — capability slots in IPC messages | Phase 6 ✅ | Planned |
| B | Page-grant bulk transfer and frame-allocator epoch tracking | Phase 55a ✅, A | Planned |
| C | IPC timeouts (`ipc_call_timeout`, `ipc_recv_timeout`) | Phase 57a ✅ | Planned |
| D | Many-to-one notification binding (`sys_notif_bind`) | Phase 55c ✅ | Planned |
| E | Documentation updates and deferral comment removal | A–D | Planned |
| F | Optional bulk-path migration for existing servers | B | Planned |

---

## Track A — `sys_cap_grant` via IPC Messages

### A.1 — Extend `IpcMessage` with capability slots

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `IpcMessage`
**Why it matters:** The capability-slot extension is the ABI change that every subsequent track depends on; it must land first.

**Acceptance:**
- [ ] `IpcMessage` gains `cap_slots: [CapHandle; 2]` and `n_caps: u8` fields
- [ ] Syscall ABI documentation in `docs/appendix/architecture-and-syscalls.md` is updated to reflect the new fields
- [ ] Existing `sys_ipc_call` invocations with `n_caps = 0` behave identically to pre-Phase-74 behavior
- [ ] The deferral comment at `kernel/src/ipc/mod.rs:34` is replaced with `// Capability grants: delivered in Phase 74`

### A.2 — Capability copy in the kernel IPC path

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `ipc_transfer_caps`
**Why it matters:** The kernel must atomically validate and transfer capability entries; a partial transfer (first cap succeeds, second fails) must roll back.

**Acceptance:**
- [ ] `ipc_transfer_caps(sender, receiver, msg)` validates all `n_caps` handles before copying any
- [ ] On validation failure, the entire IPC call returns `EINVAL` with no caps transferred
- [ ] On success, each capability is inserted into the receiver's table and the new index written into the receiver's reply registers
- [ ] Receiver's table-full condition returns `ENOSPC`; sender is notified; IPC call fails atomically

### A.3 — `syscall-lib` bindings for cap-slot IPC

**File:** `userspace/syscall-lib/src/ipc.rs`
**Symbol:** `ipc_call_with_caps`, `ipc_recv_caps`
**Why it matters:** Userspace servers need ergonomic wrappers that hide the raw register protocol.

**Acceptance:**
- [ ] `ipc_call_with_caps(endpoint, msg, caps)` serializes the cap slots into the syscall arguments and returns received cap indices
- [ ] `ipc_recv_caps(endpoint)` returns `(msg, received_caps)`
- [ ] A host-side unit test in `kernel-core` validates the serialization round-trip

---

## Track B — Page-Grant Bulk Transfer

### B.1 — `PageGrant` kernel object and `sys_page_grant_send`

**File:** `kernel/src/ipc/page_grant.rs`
**Symbol:** `PageGrant`, `sys_page_grant_send`
**Why it matters:** The absence of zero-copy bulk transport is the primary IPC performance bottleneck for the Phase 72 compositor's large surface buffers.

**Acceptance:**
- [ ] `sys_page_grant_send(endpoint, pages_vaddr, n_pages)` unmaps the specified virtual range from the sender's page table, validates that all pages are present and owned by the sender, and creates a `PageGrant` kernel object
- [ ] The sender's TLB is shot down (IPI to all cores running sender threads) before the grant is marked ready
- [ ] The `PageGrant` object is registered as a capability in the kernel's capability table; its handle is delivered to the receiver via the IPC message's `cap_slots`
- [ ] The frame allocator records a grant epoch on each transferred frame; any attempt to free a frame with a pending grant returns `EBUSY`

### B.2 — `sys_page_grant_recv` and IOMMU domain update

**Files:**
- `kernel/src/ipc/page_grant.rs`
- `kernel/src/iommu/mod.rs`

**Symbol:** `sys_page_grant_recv`, `iommu_remap_grant`
**Why it matters:** The receive side must map transferred pages into the receiver's address space and update IOMMU translation tables atomically where present.

**Acceptance:**
- [ ] `sys_page_grant_recv(grant_cap)` maps the granted pages into the receiver's address space at a kernel-chosen virtual address returned in a register
- [ ] Where Phase 55a's IOMMU substrate is active, `iommu_remap_grant` updates the receiver's IOMMU translation domain inside a single IOMMU domain lock critical section
- [ ] On non-IOMMU platforms, identity-map fallback is used (no IOMMU call)
- [ ] After `sys_page_grant_recv` returns, the `PageGrant` capability is consumed and cannot be received a second time

### B.3 — Page-grant correctness test

**File:** `kernel/tests/page_grant.rs`
**Symbol:** `test_page_grant_transfer`
**Why it matters:** A bug here causes silent data corruption or use-after-free in the compositor's surface buffers.

**Acceptance:**
- [ ] Test allocates 1024 pages (4 MB), writes a sentinel pattern, grants them to a child process, and verifies the sentinel is readable by the child without copying
- [ ] Sender's virtual mapping is absent after `sys_page_grant_send` returns (SIGSEGV on access)
- [ ] Double-receive of the same grant cap returns `EINVAL`
- [ ] Test passes under `cargo xtask test --test page_grant`

---

## Track C — IPC Timeouts

### C.1 — `sys_ipc_call_timeout` and `sys_ipc_recv_timeout`

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `sys_ipc_call_timeout`, `sys_ipc_recv_timeout`
**Why it matters:** Closes the Phase 6 deferral (noted at `ipc/mod.rs:35`) and the Phase 55c "Timed recv" deferral; prevents servers from blocking indefinitely on slow clients.

**Acceptance:**
- [ ] `sys_ipc_call_timeout(endpoint, msg, deadline_ns)` returns `ETIMEDOUT` if no receiver picks up the message before `deadline_ns` (absolute `CLOCK_MONOTONIC`)
- [ ] `sys_ipc_recv_timeout(endpoint, deadline_ns)` returns `ETIMEDOUT` if no message arrives before the deadline
- [ ] Both syscalls register a timer entry in the Phase 57a timer wheel at entry time
- [ ] The deferral comment at `kernel/src/ipc/mod.rs:35` is replaced with `// IPC timeouts: delivered in Phase 74`

### C.2 — Race-free timeout and IPC completion interaction

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `ipc_timeout_cancel`
**Why it matters:** A timeout that fires simultaneously with a successful IPC delivery must not leave the thread in an inconsistent state.

**Acceptance:**
- [ ] When an IPC message arrives at the same tick as a timeout expiry, the message delivery wins; the timeout entry is cancelled
- [ ] When the timeout fires first, the thread is removed from the endpoint's blocked queue before being woken; no dangling pointer remains in the queue
- [ ] `ipc_timeout_cancel(thread)` is called from the IPC completion path and from the timeout wheel fire path; both sides hold the endpoint lock

### C.3 — `syscall-lib` timeout bindings

**File:** `userspace/syscall-lib/src/ipc.rs`
**Symbol:** `ipc_call_timeout`, `ipc_recv_timeout`
**Why it matters:** Userspace servers cannot safely use raw register syscalls for timeout semantics.

**Acceptance:**
- [ ] `ipc_call_timeout(endpoint, msg, timeout_ns: u64)` converts to an absolute deadline via `clock_gettime(CLOCK_MONOTONIC)` and calls `sys_ipc_call_timeout`
- [ ] `ipc_recv_timeout(endpoint, timeout_ns: u64)` does the same for the recv side
- [ ] A unit test in `kernel-core` validates that a 0 ns timeout returns `ETIMEDOUT` immediately

---

## Track D — Many-to-One Notification Binding

### D.1 — `sys_notif_bind` implementation

**File:** `kernel/src/ipc/notification.rs`
**Symbol:** `sys_notif_bind`
**Why it matters:** Closes the Phase 55c explicit deferral; servers that handle both IPC messages and hardware notifications must block on a single receive call.

**Acceptance:**
- [ ] `sys_notif_bind(endpoint, notif_cap)` adds the notification object to the endpoint's receive set
- [ ] A thread blocked on `ipc_recv` for that endpoint wakes when any bound notification fires
- [ ] The notification source is identified in the return value (a discriminant indicates "message" vs "notification N")
- [ ] Binding the same notification to the same endpoint twice returns `EEXIST`

### D.2 — `syscall-lib` binding and documentation

**File:** `userspace/syscall-lib/src/notification.rs`
**Symbol:** `notif_bind`
**Why it matters:** The Phase 55c deferred item notes this as needed for the `audio_server` IRQ + IPC multiplexing pattern.

**Acceptance:**
- [ ] `notif_bind(endpoint, notif_cap)` wraps `sys_notif_bind` with error propagation
- [ ] `docs/roadmap/55c-ring-3-driver-correctness-closure.md` "Deferred" section updated to note closure in Phase 74
- [ ] A smoke-test binary demonstrates one thread waking on either of two bound notification objects

---

## Track E — Documentation Updates

### E.1 — Remove deferral comments from `kernel/src/ipc/mod.rs`

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** N/A (comments at lines 34–35)
**Why it matters:** Stale deferral comments mislead future readers into thinking the features are still absent.

**Acceptance:**
- [ ] Lines 34–35 comments replaced with Phase 74 closure references
- [ ] No other `// TODO Phase 7+` or `// deferred` comments remain in `kernel/src/ipc/`

### E.2 — Update Phase 6, Phase 50, and Phase 55c design docs

**Files:**
- `docs/roadmap/06-ipc-core.md` (or equivalent)
- `docs/roadmap/50-capability-system.md` (or equivalent)
- `docs/roadmap/55c-ring-3-driver-correctness-closure.md`

**Symbol:** N/A
**Why it matters:** The audit noted that Phase 6's deferred items have no tracking entry pointing to their resolution; this creates the formal closure link.

**Acceptance:**
- [ ] Phase 6 "Deferred Until Later" section lists cap-grant-via-IPC and page-grant as closed in Phase 74
- [ ] Phase 55c "Deferred Until Later" section lists `ipc_recv_timeout` and `sys_notif_bind` as closed in Phase 74

---

## Track F — Optional Bulk-Path Migration

### F.1 — `display_server` surface buffer transport via page-grant

**File:** `userspace/display_server/src/surface.rs`
**Symbol:** `receive_surface_buffer`
**Why it matters:** The Phase 72 compositor copies 8 MB per frame for 1080p surfaces; page-grant eliminates this copy and is the primary Phase 74 use-case motivator.

**Acceptance:**
- [ ] `receive_surface_buffer` uses `ipc_recv_caps` to receive a page-grant capability from the client
- [ ] It calls `sys_page_grant_recv(grant_cap)` to map the surface buffer without copying
- [ ] Existing clients that use inline copy are unaffected (negotiated by the protocol version field)
- [ ] A before/after frame time measurement shows > 30% reduction in compositor CPU time at 1080p/60

### F.2 — `audio_server` DMA buffer transport via page-grant (optional)

**File:** `userspace/audio_server/src/dma.rs`
**Symbol:** `map_client_audio_buffer`
**Why it matters:** Audio clients currently copy PCM data into the server; page-grant allows direct DMA hand-off matching the Phase 63 architecture intent.

**Acceptance:**
- [ ] `map_client_audio_buffer` accepts a page-grant for a PCM ring buffer from the client
- [ ] The DMA descriptor in the AC'97 driver points directly at the transferred pages
- [ ] Audio playback quality is unchanged after the migration

---

## Documentation Notes

- Track A's `IpcMessage` struct change is an ABI break; all in-tree callers must be audited before the PR merges. The audit list should be attached as a comment in the commit.
- Track B's frame-allocator epoch tracking must integrate cleanly with the Phase 53a slab allocator; confirm that slab-backed frame metadata supports the grant-epoch field.
- Track C's race between timeout and IPC delivery is the most subtle correctness concern in this phase; the acceptance criteria require both orderings to be tested.
- Track D closes two items from the Phase 55c "Deferred Until Later" section; those doc updates belong in Track E.
- Track F is explicitly optional for Phase 74 completion; Tracks A–E are the blocking deliverables.
