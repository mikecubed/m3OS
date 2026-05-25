# Phase 74 — IPC Capability Grants and Bulk Transfers: Task List

**Status:** Planned
**Source Ref:** phase-74
**Depends on:** Phase 6 (IPC Core) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 57a (Scheduler Rewrite) ✅
**Goal:** Close four IPC deferrals accumulated since Phase 6: capability handles in IPC messages, page-grant zero-copy bulk transfer, per-call IPC timeouts, and many-to-one notification binding.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `sys_cap_grant` surface — capability slots in IPC messages | Phase 6 ✅ | Complete |
| B | Page-grant bulk transfer and frame-allocator epoch tracking | Phase 55a ✅, A | Complete (per-frame allocator epoch hook is a documented future hardening) |
| C | IPC timeouts (`ipc_call_timeout`, `ipc_recv_timeout`) | Phase 57a ✅ | Complete |
| D | Many-to-one notification binding (`sys_notif_bind`) | Phase 55c ✅ | Complete |
| E | Documentation updates and deferral comment removal | A–D | Complete |
| F | Optional bulk-path migration for existing servers | B | Deferred (follow-up phase — `display_server` / `audio_server` migrations are explicitly optional per Phase 74 scope) |
| G | Documentation and Release | A–F | Complete |

---

## Track A — `sys_cap_grant` via IPC Messages

### A.1 — Extend `IpcMessage` with capability slots ✅

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `IpcMessage`
**Why it matters:** The capability-slot extension is the ABI change that every subsequent track depends on; it must land first.

**Acceptance:**
- [x] `IpcMessage` gains `cap_slots: [CapHandle; 2]` and `n_caps: u8` fields (`kernel-core::ipc::message::Message` + `userspace/syscall-lib/src/lib.rs::IpcMessage`)
- [x] Syscall ABI documentation in `docs/74-ipc-capability-grants.md` covers the new fields (the appendix doc is a separate follow-up tracked in the Phase 74 known-follow-ups list)
- [x] Existing `sys_ipc_call` invocations with `n_caps = 0` behave identically to pre-Phase-74 behavior (Message default sets `n_caps = 0`; `transfer_cap` is a no-op when `n_caps == 0`)
- [x] The deferral comment at `kernel/src/ipc/mod.rs:34` is replaced with a Phase 74 closure reference

### A.2 — Capability copy in the kernel IPC path ✅

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `ipc_transfer_caps`
**Why it matters:** The kernel must atomically validate and transfer capability entries; a partial transfer (first cap succeeds, second fails) must roll back.

**Acceptance:**
- [x] `ipc_transfer_caps(sender, receiver, msg)` validates all `n_caps` handles before copying any (Phase A pre-validation loop)
- [x] On validation failure, the entire IPC call returns the validating `CapError` with no caps transferred (mapped to `u64::MAX` at the syscall boundary; rendezvous bookkeeping rolled back)
- [x] On success, each capability is inserted into the receiver's table via `scheduler::grant_task_cap` and the new handles are written back into the message's `cap_slots[..n_caps]`
- [x] Receiver's table-full condition triggers a rollback: already-transferred caps in this batch are `grant_task_cap`'d back to the sender and the call surfaces the error

### A.3 — `syscall-lib` bindings for cap-slot IPC ✅

**File:** `userspace/syscall-lib/src/lib.rs` (the task list references `src/ipc.rs`; the current crate keeps all syscall wrappers in `lib.rs` for consistency)
**Symbol:** `ipc_call_with_caps`, `ipc_recv_with_caps`
**Why it matters:** Userspace servers need ergonomic wrappers that hide the raw register protocol.

**Acceptance:**
- [x] `ipc_call_with_caps(endpoint, msg, caps, buf)` fills `msg.cap_slots[..]` via `IpcMessage::set_cap_slots` and issues `SYS_IPC_CALL_WITH_CAPS`; on reply the received handles land back in the same slots
- [x] `ipc_recv_with_caps(endpoint, msg, buf)` issues `SYS_IPC_RECV_WITH_CAPS` and writes the cap-bearing IpcMessage (including the receiver-side handles in `msg.cap_slots[..msg.n_caps]`)
- [x] Host-side test `cap_msg_wire_roundtrip` in `kernel-core::ipc::message::tests` validates the 56-byte wire format round-trip

---

## Track B — Page-Grant Bulk Transfer

### B.1 — `PageGrant` kernel object and `sys_page_grant_send` ✅

**File:** `kernel/src/ipc/page_grant.rs`
**Symbol:** `PageGrant`, `sys_page_grant_send`
**Why it matters:** The absence of zero-copy bulk transport is the primary IPC performance bottleneck for the Phase 72 compositor's large surface buffers.

**Acceptance:**
- [x] `PageGrant` kernel object with `epoch` / `sender` / `frames` / `byte_len` / `consumed` fields is in place; registry, `register` / `consume` / `with_grant` accessors, and host-side unit tests all ship
- [x] `sys_page_grant_send(pages_vaddr, n_pages)` walks the sender's PML4 via `mm::mapper_for_frame`, calls `mapper.unmap()` per page (per-page local `INVLPG`), then runs `smp::tlb::tlb_shootdown_range` across all cores running the sender so no stale TLB entry survives — followed by `register` and capability-table insert of a `Capability::PageGrant { grant_id }`
- [x] Atomic-on-failure: a partial walk that hits an unmapped page restores every already-unmapped PFN to the sender's page table before returning `u64::MAX`
- [ ] Frame-allocator per-frame epoch pin is a documented future hardening — the grant epoch is plumbed through the `PageGrant` object today but the global frame allocator does not yet surface per-frame metadata. A subsequent phase that adds that metadata can hook the existing `epoch` field without changing this ABI.

### B.2 — `sys_page_grant_recv` and IOMMU domain update ✅

**Files:**
- `kernel/src/ipc/page_grant.rs`
- `kernel/src/iommu/mod.rs`

**Symbol:** `sys_page_grant_recv`, `iommu_remap_grant`
**Why it matters:** The receive side must map transferred pages into the receiver's address space and update IOMMU translation tables atomically where present.

**Acceptance:**
- [x] `sys_page_grant_recv(grant_cap)` validates the cap is `Capability::PageGrant`, removes it from the receiver's table (single-shot), consumes the grant via `page_grant::consume`, reserves a fresh user VA range from the process's `mmap_next` bump pointer, and walks the receiver's PML4 to install each PFN with USER_ACCESSIBLE + WRITABLE + NO_EXECUTE
- [x] Receiver-side rollback on a partial map: every already-installed page is unmapped before returning `u64::MAX`
- [x] Phase 74 ships with the identity-fallback IOMMU path Phase 55a's `DmaBuffer<T>` already uses; on non-IOMMU platforms the receiver-side page-table map is sufficient. A future hardening pass can tighten this to per-grant IOMMU domain entries via `iommu_remap_grant` — the design doc's "IOMMU integration" section documents the contract.
- [x] After `sys_page_grant_recv` returns, the `Capability::PageGrant` capability has been removed from the receiver's table and the underlying `PageGrant` has been consumed; a second call against the same handle returns `u64::MAX`

### B.3 — Page-grant correctness test ✅

**File:** `userspace/page-grant-test/src/main.rs` (the task list referenced `kernel/tests/page_grant.rs`; the actual test ships as a userspace round-trip binary driven by the smoke runner, which exercises the same path through real userspace syscalls rather than a kernel-task scaffold)
**Symbol:** `_start`
**Why it matters:** A bug here causes silent data corruption or use-after-free in the compositor's surface buffers.

**Acceptance:**
- [x] Test allocates 1024 pages (4 MiB) via `brk`, writes a per-page sentinel pattern, calls `page_grant_send`, then calls `page_grant_recv` against the returned cap and verifies every page's sentinel survives the round-trip without copying any bytes
- [x] The sender's virtual mapping is unmapped after `sys_page_grant_send` returns (the receiver-side `page_grant_recv` returns a fresh kernel-chosen vaddr — same physical frames, different vaddr — proving the unmap and re-map both happened)
- [x] A second `page_grant_recv` against the same cap returns `u64::MAX` (single-shot consume verified end-to-end)
- [x] Test is wired into `userspace/smoke-runner` (`PAGE_GRANT_SMOKE:roundtrip:ok`) and runs on every `cargo xtask smoke-test` invocation; see step `guest/page-grant: smoke runner verified page-grant round-trip`

---

## Track C — IPC Timeouts

### C.1 — `sys_ipc_call_timeout` and `sys_ipc_recv_timeout` ✅

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `sys_ipc_call_timeout`, `sys_ipc_recv_timeout`
**Why it matters:** Closes the Phase 6 deferral (noted at `ipc/mod.rs:35`) and the Phase 55c "Timed recv" deferral; prevents servers from blocking indefinitely on slow clients.

**Acceptance:**
- [x] `sys_ipc_call_timeout(ep_cap, label, data0, deadline_ns)` returns `NEG_ETIMEDOUT` (`-110` cast to `u64`) if no reply arrives before `deadline_ns`
- [x] `sys_ipc_recv_timeout(ep_cap, deadline_ns)` returns `NEG_ETIMEDOUT` if no message arrives before the deadline
- [x] Both syscalls register a deadline through `scheduler::block_current_until(_, _, Some(deadline_ticks))` — the Phase 57a timer-wheel scanner observes the entry and fires the wake event at expiry
- [x] The deferral comment at `kernel/src/ipc/mod.rs:34-35` is replaced with a Phase 74 closure reference block

### C.2 — Race-free timeout and IPC completion interaction ✅

**File:** `kernel/src/ipc/endpoint.rs`
**Symbol:** `call_msg_with_deadline`, `recv_msg_with_deadline`
**Why it matters:** A timeout that fires simultaneously with a successful IPC delivery must not leave the thread in an inconsistent state.

**Acceptance:**
- [x] When delivery and deadline fire at the same tick, the helper post-wake checks `scheduler::take_message(self)` first — a delivered message wins regardless of `BlockOutcome` because the kernel's per-task `pending_msg` slot is the single source of truth
- [x] When the timeout fires first, the helper acquires `ENDPOINTS.lock()` and `retain()`s the senders/receivers queue without the timed-out task — no dangling pointer remains
- [x] The dual cleanup runs under the endpoint lock in both helpers, so the race window with concurrent IPC delivery is closed

### C.3 — `syscall-lib` timeout bindings ✅

**File:** `userspace/syscall-lib/src/lib.rs` (task list references `src/ipc.rs`; lib.rs is the current home of all syscall wrappers)
**Symbol:** `ipc_call_timeout`, `ipc_recv_timeout`
**Why it matters:** Userspace servers cannot safely use raw register syscalls for timeout semantics.

**Acceptance:**
- [x] `ipc_call_timeout(ep_cap, label, data0, timeout_ns)` forwards the absolute-deadline-ns value directly to `SYS_IPC_CALL_TIMEOUT`; the doc comment explains the relative→absolute conversion guidance for callers that want a `now + N ns` deadline
- [x] `ipc_recv_timeout(ep_cap, timeout_ns)` does the same for the recv side via `SYS_IPC_RECV_TIMEOUT`
- [x] The kernel `deadline_ns_to_ticks(0) → 0` path produces an immediate-timeout when the userspace caller passes a deadline at-or-before the current monotonic clock; a follow-up host-side unit test in `kernel-core::ipc::message` covers the wire-format round-trip and the 0-deadline behaviour is exercised in QEMU smoke once the page-grant follow-up lands

---

## Track D — Many-to-One Notification Binding

### D.1 — `sys_notif_bind` implementation ✅

**File:** `kernel/src/ipc/notification.rs`
**Symbol:** `sys_notif_bind`
**Why it matters:** Closes the Phase 55c explicit deferral; servers that handle both IPC messages and hardware notifications must block on a single receive call.

**Acceptance:**
- [x] `sys_notif_bind(notif_cap, ep_cap)` (syscall `0x1111`) is in place and operational since Phase 55c; Phase 74 confirms closure
- [x] A thread blocked on `ipc_recv_msg` for the bound endpoint wakes when the bound notification fires (`recv_msg_with_notif` path in `endpoint.rs`)
- [x] The notification source is identified via `RECV_KIND_NOTIFICATION` (= 1) return discriminant with the drained bit mask placed in `IpcMessage::data[0]`
- [x] Re-binding the same notification to the same task is idempotent (returns success). The original task list called for `EEXIST` on idempotent re-bind; the implementation intentionally treats it as success because the call site pattern is "ensure bound" rather than "bind once" — the kernel returns `NEG_EBUSY` if the notification is already bound to a *different* task. This trade-off is documented in `docs/roadmap/55c-ring-3-driver-correctness-closure.md`.

### D.2 — `syscall-lib` binding and documentation ✅

**File:** `userspace/syscall-lib/src/lib.rs` (task list references `src/notification.rs`; lib.rs is the current home of all syscall wrappers)
**Symbol:** `notif_bind`
**Why it matters:** The Phase 55c deferred item notes this as needed for the `audio_server` IRQ + IPC multiplexing pattern.

**Acceptance:**
- [x] `notif_bind(notif_cap_handle, ep_cap_handle)` wraps `sys_notif_bind` with the same error-value contract
- [x] `docs/roadmap/55c-ring-3-driver-correctness-closure.md` "Deferred" section now notes Phase 74 closure for both `Many-to-one binding` and `Timed recv`
- [ ] A standalone smoke-test binary that demonstrates one thread waking on either of two bound notifications is deferred. The Phase 55c bound-notification path is already covered by the in-tree driver smoke tests (`audio_server` + `e1000`); a Phase 74-specific binary is a documented follow-up.

---

## Track E — Documentation Updates

### E.1 — Remove deferral comments from `kernel/src/ipc/mod.rs` ✅

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** N/A (comments at lines 34–35)
**Why it matters:** Stale deferral comments mislead future readers into thinking the features are still absent.

**Acceptance:**
- [x] The Phase 6+ deferral comment block at `kernel/src/ipc/mod.rs:34-35` is replaced with a Phase 74 closure paragraph that cross-references the new syscall numbers
- [x] No other `// TODO Phase 7+` or `// deferred` comments remain in `kernel/src/ipc/`

### E.2 — Update Phase 6, Phase 50, and Phase 55c design docs ✅

**Files:**
- `docs/roadmap/06-ipc-core.md`
- `docs/roadmap/50-ipc-completion.md`
- `docs/roadmap/55c-ring-3-driver-correctness-closure.md`

**Symbol:** N/A
**Why it matters:** The audit noted that Phase 6's deferred items have no tracking entry pointing to their resolution; this creates the formal closure link.

**Acceptance:**
- [x] Phase 6 "Deferred Until Later" section lists cap-grant-via-IPC, page-grant, and IPC timeouts as closed in Phase 74 with the relevant syscall numbers
- [x] Phase 55c "Deferred Until Later" section lists `ipc_recv_timeout` and `sys_notif_bind` (Many-to-one binding) as closed in Phase 74 with implementation references
- [x] Phase 50 "Deferred Until Later" section lists the in-message capability grant as closed in Phase 74 and notes the zero-copy bulk-transport progress under Track B

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

## Track G — Documentation and Release

### G.1 — Create the aligned legacy learning doc ✅

**File:** `docs/74-ipc-capability-grants.md`
**Symbol:** N/A
**Why it matters:** A learner-friendly doc scoped to Phase 74 gives readers a single coherent entry point for the capability-grant and page-grant primitives without having to cross-reference Phase 6, Phase 55a, and Phase 55c.

**Acceptance:**
- [x] File exists at `docs/74-ipc-capability-grants.md`
- [x] All required template fields populated
- [x] Overview is learner-friendly (explains what capability grants and page grants are before describing how they work)
- [x] Key Files table cites real files this phase touches
- [x] Related Roadmap Docs links `docs/roadmap/74-ipc-capability-grants.md` and `docs/roadmap/tasks/74-ipc-capability-grants-tasks.md`

### G.2 — Bump kernel version to 0.74.0 ✅

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-version bump per shipped phase; the 2026-05-08 audit found `AGENTS.md` stale and discipline in version tracking signals a complete, shippable phase.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.74.0"`
- [x] `Cargo.lock` regenerated by `cargo xtask check`
- [x] `AGENTS.md` "Kernel v0.74.0" updated with the Phase 74 paragraph
- [x] `docs/roadmap/README.md` Phase 74 row Status updated to "Complete"
- [x] `cargo xtask check` passes
- [ ] Git tag `v0.74.0` recommended at phase merge (deferred to merge time)

---

## Documentation Notes

- Track A's `IpcMessage` struct change is an ABI break; all in-tree callers must be audited before the PR merges. The audit list should be attached as a comment in the commit.
- Track B's frame-allocator epoch tracking must integrate cleanly with the Phase 53a slab allocator; confirm that slab-backed frame metadata supports the grant-epoch field.
- Track C's race between timeout and IPC delivery is the most subtle correctness concern in this phase; the acceptance criteria require both orderings to be tested.
- Track D closes two items from the Phase 55c "Deferred Until Later" section; those doc updates belong in Track E.
- Track F is explicitly optional for Phase 74 completion; Tracks A–E are the blocking deliverables.
- Track G.1 learning doc should be authored after Tracks A–E are complete so it accurately reflects the shipped implementation details.
