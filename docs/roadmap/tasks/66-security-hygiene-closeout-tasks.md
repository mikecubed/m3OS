# Phase 66 — Security and Hygiene Closeout: Task List

**Status:** Planned
**Source Ref:** phase-66
**Depends on:** Phase 48 (Security Foundation) ✅, Phase 54a (Post-Serverization Kernel Hygiene) ✅, Phase 27 (User Accounts) ✅, Phase 28 (Extended Filesystem) ✅
**Goal:** Close the five deferred security and hygiene items from Phase 48 and Phase 54a: sticky-bit enforcement in VFS unlink/rename; atomic shadow-file writes in passwd/adduser; O_CLOEXEC and O_NONBLOCK honored at descriptor construction; four layer-crossing wrappers relocated from process/mod.rs; pre-seeded image password hash format upgraded. Flip Phase 54a status to Complete.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `/tmp` sticky-bit S_ISVTX enforcement in `unlink` and `rename` | None | Planned |
| B | Atomic shadow file writes in `passwd` and `adduser` | None | Planned |
| C | `O_CLOEXEC` / `O_NONBLOCK` plumbing at descriptor construction | None | Planned |
| D | Relocate four layer-crossing wrappers out of `process/mod.rs` | None | Planned |
| E | Pre-seeded image password hash format upgrade | B | Planned |
| F | Phase 48 + Phase 54a design docs + task docs updated; Phase 54a flipped Complete | A, B, C, D, E | Planned |
| G | Documentation and Release | F | Planned |

---

## Track A — Sticky-Bit Enforcement

### A.1 — Add `S_ISVTX` constant and `check_sticky` helper

**File:** `kernel/src/fs/vfs.rs`
**Symbol:** `check_sticky`
**Why it matters:** Without the sticky-bit check any user can delete another user's `/tmp` file — a local privilege escalation surface.

**Acceptance:**
- [ ] `S_ISVTX = 0o1000` is defined as a named constant in `kernel-core::fs::mode`.
- [ ] `check_sticky(parent_mode, file_uid, caller_uid, caller_is_root) -> Result<(), Errno>` returns `Ok` if S_ISVTX is clear, caller is root, file owner matches caller, or directory owner matches caller; otherwise `-EACCES`.
- [ ] At least four unit tests: bit clear (always ok), bit set owner match, bit set directory owner match, bit set neither match.

### A.2 — Wire `check_sticky` into `sys_unlink` and `sys_rename`

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (unlink/rename handlers)
- `kernel/src/fs/vfs.rs`

**Symbol:** `sys_unlink`, `sys_rename`
**Why it matters:** The check must occur before any inode mutation to prevent TOCTOU races.

**Acceptance:**
- [ ] `sys_unlink` calls `check_sticky` on the parent directory before removing the directory entry.
- [ ] `sys_rename` calls `check_sticky` on the source parent directory before the rename.
- [ ] `cargo xtask test --test sticky_bit` passes: non-owner non-root cannot unlink; owner can.
- [ ] `/tmp` directory in the disk image builder has mode `0o1777`.

---

## Track B — Atomic Shadow File Writes

### B.1 — Create `userspace/lib/shadow` crate with `shadow_write_atomic`

**File:** `userspace/lib/shadow/src/lib.rs`
**Symbol:** `shadow_write_atomic`
**Why it matters:** Both `passwd` and `adduser` need the same temp-file + rename pattern; duplicating it is a DRY violation.

**Acceptance:**
- [ ] `shadow_write_atomic(path: &str, content: &[u8]) -> Result<(), ShadowError>` opens `{path}.new` with `O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC`, writes all content, calls `fsync`, then calls `rename("{path}.new", path)`.
- [ ] On any write error the function unlinks `{path}.new` and returns `Err`; the original file is untouched.
- [ ] At least two tests: success path (rename committed), simulated write failure (original file unchanged).

### B.2 — Update `passwd` and `adduser` to use `shadow_write_atomic`

**Files:**
- `userspace/passwd/src/main.rs`
- `userspace/adduser/src/main.rs`

**Symbol:** `write_shadow`
**Why it matters:** The previous direct-write path is a torn-shadow risk on crash.

**Acceptance:**
- [ ] Both binaries import `shadow::shadow_write_atomic` and remove any direct `open("/etc/shadow")` write paths.
- [ ] A test injects `kill -9` mid-write and verifies `/etc/shadow` is unmodified; `/etc/shadow.new` contains partial data.

---

## Track C — CLOEXEC and NONBLOCK Plumbing

### C.1 — Add `FdFlags` to `FileDescription`

**File:** `kernel/src/fs/file.rs`
**Symbol:** `FileDescription`, `FdFlags`
**Why it matters:** Flags silently discarded at `open()` time mean `O_CLOEXEC` is advisory only, which is a security gap.

**Acceptance:**
- [ ] `FdFlags` bitfield has `CLOEXEC` and `NONBLOCK` bits.
- [ ] `FileDescription::new` accepts `FdFlags` and stores them.
- [ ] Existing call sites pass `FdFlags::empty()` by default (no behavior change for existing code).

### C.2 — Honor `O_CLOEXEC` in `open`, `openat`, `openat2`, `vfs_service_open`

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (open/openat/openat2 arms)
- `kernel/src/fs/vfs_service.rs`

**Symbol:** `sys_open`, `sys_openat`, `sys_openat2`, `vfs_service_open`
**Why it matters:** Per POSIX, `O_CLOEXEC` must be set atomically at `open()` time to avoid race conditions between `open()` and `execve()`.

**Acceptance:**
- [ ] Each syscall arm extracts `O_CLOEXEC` from the flags argument and passes `FdFlags::CLOEXEC` to `FileDescription::new`.
- [ ] `cargo xtask test --test cloexec_fd` passes: FD opened with `O_CLOEXEC` is absent after `execve`.

### C.3 — Honor `O_NONBLOCK` in `read` and `write`

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_read`, `sys_write`
**Why it matters:** Without NONBLOCK enforcement a caller that opened with `O_NONBLOCK` still blocks on I/O, defeating its purpose.

**Acceptance:**
- [ ] If `FileDescription::flags` has `NONBLOCK` set and the I/O would block, the syscall returns `-EAGAIN` immediately.
- [ ] At least one integration test: open a blocking-capable FD with `O_NONBLOCK`, attempt a read that would block, receive `-EAGAIN`.

---

## Track D — Layer-Crossing Wrapper Relocation

### D.1 — Move `release_socket_pub` to `kernel/src/net/mod.rs`

**Files:**
- `kernel/src/process/mod.rs` (remove body; add `pub(crate) use`)
- `kernel/src/net/mod.rs` (add function body)

**Symbol:** `release_socket_pub`
**Why it matters:** A socket teardown function in the process module is an architectural boundary violation.

**Acceptance:**
- [ ] Function body exists only in `kernel/src/net/mod.rs`.
- [ ] `kernel/src/process/mod.rs` contains only `pub(crate) use net::release_socket_pub`.
- [ ] All existing callers compile without change.

### D.2 — Move `epoll_free_pub`, `reap_unused_ext2_inode`, `vfs_service_close_pub`

**Files:**
- `kernel/src/process/mod.rs` (remove bodies)
- `kernel/src/epoll.rs`, `kernel/src/fs/ext2/mod.rs`, `kernel/src/fs/vfs_service.rs` (add bodies)

**Symbol:** `epoll_free_pub`, `reap_unused_ext2_inode`, `vfs_service_close_pub`
**Why it matters:** Each function belongs in the module that owns its data structures.

**Acceptance:**
- [ ] Each function body lives in its owning module.
- [ ] `grep -n 'fn release_socket_pub\|fn epoll_free_pub\|fn reap_unused_ext2_inode\|fn vfs_service_close_pub' kernel/src/process/mod.rs` returns zero lines.
- [ ] All existing callers compile without change.

---

## Track E — Password Hash Format Upgrade

### E.1 — Upgrade pre-seeded image hash format

**Files:**
- `xtask/src/main.rs` (disk image bootstrap)
- `userspace/passwd/src/hash.rs`
- `userspace/adduser/src/hash.rs`

**Symbol:** `hash_password`
**Why it matters:** The Phase 48 format string is undocumented inline; the format must be named and the bootstrap regenerated.

**Acceptance:**
- [ ] `hash_password` is documented with a code comment naming the algorithm (SHA-256, PBKDF2 or equivalent) and iteration count.
- [ ] The format string is defined as a named constant (`HASH_FORMAT_PREFIX`), not an inline literal.
- [ ] Pre-seeded hashes in the disk image builder are regenerated using the current `hash_password` function.
- [ ] `cargo xtask test --test shadow_login` passes: a login with the pre-seeded password succeeds.

---

## Track F — Documentation Closure

### F.1 — Update Phase 48 design doc

**File:** `docs/roadmap/48-security-foundation.md`
**Symbol:** (document section `## Deferred Until Later`)
**Why it matters:** Phase 48 explicitly deferred sticky-bit and atomic shadow writes; the doc must note they are closed.

**Acceptance:**
- [ ] A closure note is appended naming Phase 66 as the phase that delivered sticky-bit and atomic shadow writes.

### F.2 — Flip Phase 54a status to Complete

**Files:**
- `docs/roadmap/54a-post-serverization-kernel-hygiene.md`
- `docs/roadmap/tasks/54a-post-serverization-kernel-hygiene-tasks.md`

**Symbol:** `**Status:**`
**Why it matters:** Phase 54a has been Planned since it was written; Phase 66 delivers its entire scope.

**Acceptance:**
- [ ] Both files have `**Status:** Complete`.
- [ ] A closure note references Phase 66 for the implementation.

---

---

## Track G — Documentation and Release

### G.1 — Create the aligned legacy learning doc

**File:** `docs/66-security-hygiene-closeout.md`
**Symbol:** (new document)
**Why it matters:** Learners need a focused reference for the five security trust-floor items — sticky-bit, atomic shadow writes, CLOEXEC, wrapper relocation, hash format — without mixing in Phase 48 foundation context or future MAC/ACL work.

**Acceptance:**
- [ ] `docs/66-security-hygiene-closeout.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 66`, `**Status:** Planned`, `**Source Ref:** phase-66`, `**Supersedes Legacy Doc:** new`).
- [ ] Overview is one learner-friendly paragraph explaining the security trust-floor gaps closed in this phase.
- [ ] Key Files table cites `kernel/src/fs/vfs.rs`, `userspace/lib/shadow/src/lib.rs`, `kernel/src/fs/file.rs`, `kernel/src/process/mod.rs`, and `userspace/passwd/src/main.rs`.
- [ ] Related Roadmap Docs links `docs/roadmap/66-security-hygiene-closeout.md` and `docs/roadmap/tasks/66-security-hygiene-closeout-tasks.md`.

### G.2 — Bump kernel version to 0.66.0

**Files:** `kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md`, `docs/roadmap/README.md`
**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-bump per shipped phase; keeping the version cursor accurate ensures `AGENTS.md` and the README reflect the real state of the kernel at any given phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.66.0"`
- [ ] `Cargo.lock` regenerated (run `cargo check` or `cargo xtask check` to trigger)
- [ ] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.66.0`
- [ ] `cargo xtask check` passes after the bump
- [ ] Git tag `v0.66.0` recommended at phase merge

---

## Documentation Notes

- The `userspace/lib/shadow` crate is a new workspace member; add it to the root `Cargo.toml` members list.
- The CLOEXEC changes must not break existing tests that pass `0` for flags to `open`-family syscalls; `FdFlags::empty()` is the default.
- The four wrapper relocations in Track D should land in a single commit to minimize review surface; the `pub(crate) use` re-exports can be removed in a follow-up once callers are updated.
