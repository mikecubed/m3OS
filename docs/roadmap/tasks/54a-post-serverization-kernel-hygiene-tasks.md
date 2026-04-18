# Phase 54a — Post-Serverization Kernel Hygiene: Task List

**Status:** Planned
**Source Ref:** phase-54a
**Depends on:** Phase 54 (Deep Serverization) ✅
**Goal:** Close the two cross-cutting kernel-hygiene items carried forward from Phase 54's closure review — missing CLOEXEC / NONBLOCK plumbing at non-pipe / non-socket fd construction sites, and the four `arch::x86_64::syscall::*_pub` wrappers that keep `kernel/src/process/mod.rs` on an arch-specific dependency — without changing process-cleanup or VFS-refcounting behavior.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `FdEntry` CLOEXEC / NONBLOCK plumbing | None | Planned |
| B | Relocate `arch::x86_64::syscall::*_pub` wrappers into owning subsystems | None | Planned |
| C | Phase closure: backlog routing and version bump | A, B | Planned |

---

## Track A — `FdEntry` CLOEXEC / NONBLOCK plumbing

### A.1 — Introduce `FdEntry::from_open_flags(backend, flags)` helper

**File:** `kernel/src/process/mod.rs`
**Symbol:** `FdEntry::from_open_flags`
**Why it matters:** Every non-pipe / non-socket / non-epoll open path currently hardcodes `cloexec: false, nonblock: false`, silently dropping the `O_CLOEXEC` guarantee requested by userspace. A single helper that maps syscall-level flags to `FdEntry` fields keeps every construction site consistent and prevents new paths from re-introducing the bug.

**Acceptance:**
- [ ] `FdEntry::from_open_flags(backend: FdBackend, flags: u32) -> FdEntry` accepts the raw `flags` argument from the originating syscall
- [ ] Helper correctly sets `cloexec = flags & O_CLOEXEC != 0` and `nonblock = flags & O_NONBLOCK != 0`
- [ ] At least 2 host-testable unit tests in `kernel-core` (or an equivalent host-testable seam) cover the O_CLOEXEC and O_NONBLOCK mappings

### A.2 — Convert every hardcoded `cloexec: false, nonblock: false` site to use the helper

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/process/mod.rs`

**Symbol:** `vfs_service_open`, `sys_linux_openat`, `sys_linux_openat2`, and every other fd-creating syscall that takes flags
**Why it matters:** The helper is only useful if every construction site routes through it. Without an audit-style conversion, new call sites drift back to hardcoded `false, false` because the helper's existence is easy to miss.

**Acceptance:**
- [ ] Every `FdEntry` construction site in `kernel/src/arch/x86_64/syscall` that has access to a `flags` argument routes it through `FdEntry::from_open_flags`
- [ ] Phase 54's `vfs_service_open` (≈ `kernel/src/arch/x86_64/syscall/mod.rs:5720`) is converted
- [ ] `grep -rn "cloexec: false, nonblock: false" kernel/src` returns only sites that deliberately create a non-CLOEXEC fd (e.g., stdin/stdout inheritance); each remaining site carries an inline comment explaining why
- [ ] All existing QEMU tests still pass

### A.3 — Regression test: `O_CLOEXEC` clears fd across `execve` for every backend

**File:** `userspace/cloexec-test/src/main.rs` (new userspace test binary)
**Symbol:** `main`
**Why it matters:** The only reliable way to test `O_CLOEXEC` is to actually `execve` and observe whether the child still sees the fd. Without a per-backend regression, A.2's audit can regress without detection.

**Acceptance:**
- [ ] Userspace test opens an fd with `O_CLOEXEC` for each of: regular file (`open` / `openat`), VFS-service-backed file (open via a serverized path that routes through `vfs_service_open`), and at least one additional backend if applicable
- [ ] Test execs into a helper that walks `/proc/self/fd` (or equivalent) and verifies the CLOEXEC fds are gone while non-CLOEXEC sentinel fds remain
- [ ] Test binary is wired into the ramdisk, the xtask build pipeline, and the QEMU test harness per the four-place userspace-binary rule in `AGENTS.md`
- [ ] `cargo xtask test --test cloexec-test` passes in QEMU

---

## Track B — Relocate `arch::x86_64::syscall::*_pub` wrappers

### B.1 — Move `release_socket_pub` into `crate::net`

**Files:**
- `kernel/src/net/mod.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/process/mod.rs`

**Symbol:** `release_socket_pub`, new `crate::net::release_socket`
**Why it matters:** The Phase 54 review thread explicitly called out this wrapper. Moving it into `crate::net` removes the last reason `kernel/src/process/mod.rs` needs to import from `arch::x86_64::syscall` for socket cleanup.

**Acceptance:**
- [ ] `crate::net::release_socket(socket_handle)` exists with the same signature and semantics as the current `release_socket_pub`
- [ ] `arch::x86_64::syscall::release_socket_pub` is deleted
- [ ] `kernel/src/process/mod.rs` calls `crate::net::release_socket` directly
- [ ] All existing QEMU tests still pass

### B.2 — Move `epoll_free_pub` into `crate::epoll`

**Files:**
- `kernel/src/epoll/mod.rs` (new module if epoll cleanup helpers are not already hoisted)
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/process/mod.rs`

**Symbol:** `epoll_free_pub`, new `crate::epoll::free`
**Why it matters:** Same rationale as B.1. If epoll currently lives inside `arch/x86_64/syscall/mod.rs`, the minimum work is hoisting just the cleanup helper into a new `crate::epoll` module; full extraction of the epoll syscall surface remains deferred.

**Acceptance:**
- [ ] `crate::epoll::free(epoll_handle)` exists and replaces the wrapper
- [ ] `arch::x86_64::syscall::epoll_free_pub` is deleted
- [ ] `kernel/src/process/mod.rs` calls `crate::epoll::free` directly
- [ ] A follow-up note is added to the Phase 54a learning doc's `Deferred Until Later` section if full epoll extraction is not taken in this phase

### B.3 — Move `reap_unused_ext2_inode` into `crate::fs::ext2`

**Files:**
- `kernel/src/fs/ext2/mod.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/process/mod.rs`

**Symbol:** `reap_unused_ext2_inode`, new `crate::fs::ext2::reap_unused_inode`
**Why it matters:** Inode reap is pure fs-layer logic; the arch-syscall wrapper is a transitional artifact from before the ext2 module had a public surface.

**Acceptance:**
- [ ] `crate::fs::ext2::reap_unused_inode(inode_ref)` exists and replaces the wrapper
- [ ] `arch::x86_64::syscall::reap_unused_ext2_inode` is deleted
- [ ] `kernel/src/process/mod.rs` calls `crate::fs::ext2::reap_unused_inode` directly

### B.4 — Move `vfs_service_close_pub` into `crate::fs::vfs`

**Files:**
- `kernel/src/fs/vfs/mod.rs` (create if absent)
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/process/mod.rs`

**Symbol:** `vfs_service_close_pub`, new `crate::fs::vfs::service_close`
**Why it matters:** Phase 54 added this wrapper. Relocating it into the fs-layer matches the direction of the serverization work rather than leaving the close path as arch-syscall glue. The move must preserve the refcounting fix from PR #108 that only fires `VFS_CLOSE` on the last alias.

**Acceptance:**
- [ ] `crate::fs::vfs::service_close(handle)` exists and replaces the wrapper
- [ ] `arch::x86_64::syscall::vfs_service_close_pub` is deleted
- [ ] `kernel/src/process/mod.rs` calls `crate::fs::vfs::service_close` directly
- [ ] `VFS_CLOSE` is still emitted only when the last fd alias is removed (verified by the existing refcounting regression test path landed in PR #108)

### B.5 — Remove the arch-syscall import from `kernel/src/process/mod.rs`

**File:** `kernel/src/process/mod.rs`
**Symbol:** imports / `use` block at the top of the module
**Why it matters:** The final validation that Track B succeeded is that `process` no longer reaches into `arch::x86_64::syscall`. Leaving any residual import means the architectural hygiene goal is not met.

**Acceptance:**
- [ ] `grep -n "arch::x86_64::syscall" kernel/src/process/mod.rs` returns no matches
- [ ] `cargo xtask check` passes with no warnings about unused imports introduced by the move

---

## Track C — Phase closure: backlog routing and version bump

### C.1 — Trim `docs/debug/54-followups.md` to long-term backlog items only

**File:** `docs/debug/54-followups.md`
**Symbol:** Document content
**Why it matters:** Items 1 and 2 of the followups file are covered by Tracks A and B. Items 3, 5, and 7 are routed to other phases or already done (C.2, C.3). Items 4 and 6 remain as long-term backlog; the file should carry only those, each with an owner note, so the backlog's scope is obvious.

**Acceptance:**
- [ ] Items 1 (CLOEXEC), 2 (arch wrappers), 3 (`/var/run`), 5 (virtio IRQ), and 7 (parent-doc cleanup) are removed from `docs/debug/54-followups.md`
- [ ] Items 4 (MOUNT_OP_LOCK yielding) and 6 (scheduler thresholds) remain, each annotated with its long-term owner and the revisit condition
- [ ] File's self-referential `Parent doc cleanup` section is removed (the superseded files are already gone)

### C.2 — Confirm virtio_blk IRQ completion is routed to Phase 55

**File:** `docs/roadmap/tasks/55-hardware-substrate-tasks.md`
**Symbol:** Track C.5 acceptance list
**Why it matters:** Item 5 of the followups file (spin-poll → IRQ-driven completion on virtio_blk / virtio-net) is exactly the kind of VirtIO migration Track C.5 already owns. Folding the work into Track C.5 keeps it visible and enforces that Phase 55 closure addresses it.

**Acceptance:**
- [ ] Phase 55 Track C.5 gains an acceptance item covering IRQ-driven completion for both virtio_blk and virtio-net
- [ ] The Phase 54 followups file no longer mentions item 5

### C.3 — Confirm `/var/run` symlink is routed to Phase 45

**File:** `docs/roadmap/45-ports-system.md`
**Symbol:** `Deferred Until Later`
**Why it matters:** The `/var/run → /run` symlink is an opportunistic port-compatibility fix with no current consumer. The right home is the ports-system phase's deferred list so the first port that demands `/var/run` has an obvious owner.

**Acceptance:**
- [ ] Phase 45 design doc's `Deferred Until Later` section gains a bullet for the `/var/run → /run` symlink with the revisit condition from the followups file
- [ ] The Phase 54 followups file no longer mentions item 3

### C.4 — Version bump to 0.54.1

**Files:**
- `kernel/Cargo.toml`
- `AGENTS.md` (project-overview version string — currently stuck at `v0.51.0`, two bumps behind the tree)
- `README.md` (project overview / release notes, if a kernel version is mentioned)
- `docs/roadmap/README.md` (Phase 54a status column)
- `docs/roadmap/tasks/README.md` (Phase 54a status column)

**Symbol:** `version` field (Cargo.toml) and prose version mentions (docs)
**Why it matters:** Phase 54a is a patch-level aftermath phase on top of the `v0.54.0` baseline that closed Phase 54. Incrementing to `v0.54.1` signals that kernel hygiene has landed without implying the larger feature surface expected at `v0.55.0`, and gives downstream release work a distinct tag. Bumping here also surfaces the existing drift in `AGENTS.md`, which was never updated past `v0.51.0` — correcting it is in scope for this closure task.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `[package].version` is `0.54.1`
- [ ] `AGENTS.md` project-overview paragraph reflects kernel `v0.54.1` (corrects the stale `v0.51.0`)
- [ ] `README.md` project description reflects the new kernel version if it mentions one
- [ ] `docs/roadmap/README.md` Phase 54a row status is `Complete`
- [ ] `docs/roadmap/tasks/README.md` Phase 54a row status is `Complete`
- [ ] A repo-wide search for the previous `0.54.0` version string returns no user-facing references that should have been bumped (generated lockfiles excepted)

---

## Documentation Notes

- The two files noted as superseded in `docs/debug/54-followups.md` (`54-remaining-smp-race.md`, `54-review-findings.md`) were already deleted as part of PR #108; Phase 54a does not re-do that work.
- Track B preserves behavior; no task in this track is allowed to change process-cleanup semantics or the VFS close refcounting landed in PR #108.
- The CLOEXEC exposure is bounded: security-sensitive fd creation paths (`pipe2`, `socket(SOCK_CLOEXEC)`, `epoll_create1`, `accept4`, `socketpair`, `fcntl F_SETFD`) already honor the flag. Phase 54a closes the remaining `open`-family paths and the Phase 54 `vfs_service_open`.
- Phase 54a is named `54a` to follow the `52a / 52b / 52c / 52d / 53a` aftermath-phase precedent.
- Phase 54a closes the tree at `v0.54.1`, a patch-level bump on top of `v0.54.0`. Phase 55 takes over from that baseline and bumps to `v0.55.0` at its own close.
