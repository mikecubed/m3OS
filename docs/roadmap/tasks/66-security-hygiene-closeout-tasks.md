# Phase 66 — Security and Hygiene Closeout: Task List

**Status:** Planned
**Source Ref:** phase-66
**Depends on:** Phase 48 (Security Foundation) ✅, Phase 27 (User Accounts) ✅, Phase 28 (Extended Filesystem) ✅. Phase 54a (Post-Serverization Kernel Hygiene) is Planned and completed-by-this-phase via Track F.2.
**Goal:** Close the five deferred security and hygiene items from Phase 48 and Phase 54a: sticky-bit enforcement in VFS unlink/rename; atomic shadow-file writes in passwd/adduser; O_CLOEXEC and O_NONBLOCK honored at descriptor construction; four layer-crossing wrapper bodies relocated from `kernel/src/arch/x86_64/syscall/mod.rs` into their owning modules; pre-seeded image password hash format upgraded. Flip Phase 54a status to Complete.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `/tmp` sticky-bit S_ISVTX enforcement in `unlink` and `rename` | None | Planned |
| B | Atomic shadow file writes in `passwd` and `adduser` | None | Planned |
| C | `O_CLOEXEC` / `O_NONBLOCK` plumbing at descriptor construction | None | Planned |
| D | Relocate four layer-crossing wrapper bodies out of `arch/x86_64/syscall/mod.rs` into their owning modules | None | Planned |
| E | Pre-seeded image password hash format upgrade | B | Planned |
| F | Phase 48 + Phase 54a design docs + task docs updated; Phase 54a flipped Complete | A, B, C, D, E | Planned |
| G | Documentation and Release | F | Planned |

---

## Track A — Sticky-Bit Enforcement

### A.1 — Add `S_ISVTX` constant and `check_sticky` helper

**Files:**
- `kernel-core/src/fs/mode.rs` (new module; add `pub mod mode;` to `kernel-core/src/fs/mod.rs`)
- `kernel/src/fs/vfs.rs` (re-export / call-site)

**Symbol:** `check_sticky`
**Why it matters:** Without the sticky-bit check any user can delete another user's `/tmp` file — a local privilege escalation surface.

**Acceptance:**
- [ ] `S_ISVTX = 0o1000` is defined as a named constant in the new `kernel-core::fs::mode` module.
- [ ] `check_sticky(parent_mode: u16, file_uid: u32, dir_uid: u32, caller_uid: u32, caller_is_root: bool) -> Result<(), Errno>` returns `Ok` if S_ISVTX is clear, caller is root, file owner matches caller, or directory owner matches caller; otherwise `-EACCES`.
- [ ] At least four `cargo test -p kernel-core` host-side unit tests: bit clear (always ok), bit set + owner match, bit set + dir owner match, bit set + neither match.

### A.2 — Wire `check_sticky` into `sys_linux_unlink` and `sys_linux_rename`

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_linux_unlink` at ~line 12222, `sys_linux_rename` at ~line 12365)
- `kernel/src/fs/vfs.rs`

**Symbol:** `sys_linux_unlink`, `sys_linux_rename`
**Why it matters:** The check must occur before any inode mutation to prevent TOCTOU races.

**Acceptance:**
- [ ] `sys_linux_unlink` calls `check_sticky` on the parent directory before removing the directory entry across all three backends (tmpfs, ext2, fat32 fallback).
- [ ] `sys_linux_rename` calls `check_sticky` on the source parent directory before the rename across all backends.
- [ ] `cargo xtask test --test sticky_bit` passes: non-owner non-root cannot unlink; owner can.
- [ ] `/tmp` is already created in tmpfs with `0o1777` by `populate_mountpoints` in `kernel/src/fs/tmpfs.rs:47`; verify no regression and that the tmpfs unlink/rename path consults `check_sticky` against the in-memory parent-dir mode.

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
- `userspace/passwd/src/main.rs` and `userspace/passwd/src/lib.rs` (current shadow-write call sites)
- `userspace/adduser/src/main.rs` (current shadow-write call site; this crate has no `lib.rs`)
- `userspace/passwd/Cargo.toml`, `userspace/adduser/Cargo.toml` (add `shadow` dep)

**Symbol:** shadow-write helper in each binary (rename whatever direct-write helper exists today to call `shadow::shadow_write_atomic`)
**Why it matters:** The previous direct-write path is a torn-shadow risk on crash.

**Acceptance:**
- [ ] Both binaries import `shadow::shadow_write_atomic` and remove any direct `open("/etc/shadow")` write paths.
- [ ] A `cargo xtask test --test shadow_atomic` integration test injects a fault mid-write and verifies `/etc/shadow` is unmodified; `/etc/shadow.new` contains partial data.

---

## Track C — CLOEXEC and NONBLOCK Plumbing

**Current state (verified):** `FdEntry` (the kernel's per-FD struct in `kernel/src/process/mod.rs:216`) already has a `cloexec: bool` field, and `execve` already closes CLOEXEC FDs via `close_cloexec_fds` (line 262). `sys_pipe_with_flags` (pipe2) and the `dup`/`dup2` helpers wire CLOEXEC correctly. The remaining gaps are:

1. `sys_linux_open` / `sys_linux_openat` accept the `O_CLOEXEC` flag from userspace but never propagate it into the resulting `FdEntry.cloexec` — the bit is silently discarded.
2. `FdEntry` has no `nonblock` field, so `O_NONBLOCK` is silently discarded even though the call accepts it.

### C.1 — Add `nonblock: bool` to `FdEntry`

**File:** `kernel/src/process/mod.rs` (`FdEntry` struct ~line 216, plus the three `new_fd_table` stdin/stdout/stderr defaults ~lines 524–545 and any `FdEntry { … }` literal in the file)
**Symbol:** `FdEntry`, `FdEntry.nonblock`
**Why it matters:** `O_CLOEXEC` is already tracked on `FdEntry`; the missing parallel field for `O_NONBLOCK` is what blocks Track C.3.

**Acceptance:**
- [ ] `FdEntry` has a new `pub nonblock: bool` field defaulting to `false`.
- [ ] All in-tree `FdEntry { … }` constructions compile (stdin/stdout/stderr defaults, fork-clone paths, `dup`/`dup2`).
- [ ] `dup` and `dup2` preserve `nonblock` on the new FD (POSIX semantics: dup copies status flags, only `FD_CLOEXEC` is forced clear).

### C.2 — Honor `O_CLOEXEC` and `O_NONBLOCK` at open-family entry points

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` — `sys_linux_open` (~line 7753), `sys_linux_openat` (~line 7767), and the shared `open_user_path` helper they both delegate to

**Symbol:** `sys_linux_open`, `sys_linux_openat`, `open_user_path`
**Why it matters:** Per POSIX, `O_CLOEXEC` must be set atomically at `open()` time to avoid race conditions between `open()` and `execve()` on another thread. `O_NONBLOCK` must take effect on the FD that `open()` returns, not only after a follow-up `fcntl`.

**Acceptance:**
- [ ] `open_user_path` extracts `O_CLOEXEC` (0x80000) and `O_NONBLOCK` (0x800) from the `flags` argument and stores them on the resulting `FdEntry`.
- [ ] `cargo xtask test --test cloexec_fd` passes: an FD opened with `O_CLOEXEC` is absent after `execve`.
- [ ] Note: there is no `openat2` syscall in this kernel; if one is added later it inherits the same plumbing.

### C.3 — Honor `O_NONBLOCK` on the blocking-capable read/write paths

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_linux_read`, `sys_linux_write` (and the pipe / pty / unix-socket / tty read paths they dispatch to)
**Why it matters:** Without NONBLOCK enforcement a caller that opened with `O_NONBLOCK` still blocks on I/O, defeating its purpose.

**Acceptance:**
- [ ] If `FdEntry.nonblock` is `true` and the underlying source would block (empty pipe, empty pty master, empty unix-socket receive queue, empty tty input), the syscall returns `-EAGAIN` immediately instead of parking.
- [ ] At least one integration test: open a blocking-capable FD with `O_NONBLOCK`, attempt a read that would block, receive `-EAGAIN`.

---

## Track D — Layer-Crossing Wrapper Relocation

**Current state (verified):** The four wrapper *bodies* live in `kernel/src/arch/x86_64/syscall/mod.rs`, not in `kernel/src/process/mod.rs`. `process/mod.rs` only contains the *call sites* (the FD-teardown loops at lines 334/340/343/346 and the duplicate set at 426/432/435/438). The layer-crossing smell is that `syscall/mod.rs` reaches across into net, epoll, ext2, and vfs-service internals; relocating each body into the module that owns the data structure tightens the boundary. Only `kernel/src/net/mod.rs` exists today as a clean target. The other three target modules must be carved out as part of this track (epoll currently lives entirely inside `syscall/mod.rs`; vfs-service likewise; ext2 lives in the flat file `kernel/src/fs/ext2.rs`).

### D.1 — Move `release_socket_pub` to `kernel/src/net/mod.rs`

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (remove `pub fn release_socket_pub` body at ~line 17127)
- `kernel/src/net/mod.rs` (add `pub fn release_socket_pub`)
- `kernel/src/process/mod.rs` (update both call sites to `crate::net::release_socket_pub(h)`)

**Symbol:** `release_socket_pub`
**Why it matters:** A socket-handle teardown function in the syscall layer is an architectural boundary violation — the net subsystem owns the table.

**Acceptance:**
- [ ] Function body exists only in `kernel/src/net/mod.rs`.
- [ ] `grep -n 'fn release_socket_pub' kernel/src/arch/x86_64/syscall/mod.rs` returns zero lines.
- [ ] `kernel/src/process/mod.rs` calls `crate::net::release_socket_pub` (not `crate::arch::x86_64::syscall::release_socket_pub`).
- [ ] `cargo xtask check` passes.

### D.2 — Move `epoll_free_pub` to a new `kernel/src/epoll.rs`

**Files:**
- `kernel/src/lib.rs` or `kernel/src/main.rs` (add `pub mod epoll;`)
- `kernel/src/epoll.rs` (new module; add `pub fn epoll_free_pub` body)
- `kernel/src/arch/x86_64/syscall/mod.rs` (remove `pub fn epoll_free_pub` body at ~line 16574; leave the rest of the epoll syscall handlers in place until a later phase carves out the full epoll subsystem)
- `kernel/src/process/mod.rs` (update both call sites to `crate::epoll::epoll_free_pub(id)`)

**Symbol:** `epoll_free_pub`
**Why it matters:** Epoll instance teardown belongs in the module that owns the epoll instance table.

**Acceptance:**
- [ ] `kernel/src/epoll.rs` exists and contains `pub fn epoll_free_pub`.
- [ ] `grep -n 'fn epoll_free_pub' kernel/src/arch/x86_64/syscall/mod.rs` returns zero lines.
- [ ] `cargo xtask check` passes.

### D.3 — Move `reap_unused_ext2_inode` to `kernel/src/fs/ext2.rs`

**Files:**
- `kernel/src/fs/ext2.rs` (add `pub fn reap_unused_ext2_inode`)
- `kernel/src/arch/x86_64/syscall/mod.rs` (remove `pub(crate) fn reap_unused_ext2_inode` at ~line 7784; update the internal call site at ~line 7894 to `crate::fs::ext2::reap_unused_ext2_inode`)
- `kernel/src/process/mod.rs` (update both call sites)

**Symbol:** `reap_unused_ext2_inode`
**Why it matters:** Ext2 inode reclamation belongs next to the ext2 volume table it mutates.

**Acceptance:**
- [ ] Function body exists in `kernel/src/fs/ext2.rs`.
- [ ] `grep -n 'fn reap_unused_ext2_inode' kernel/src/arch/x86_64/syscall/mod.rs` returns zero lines.
- [ ] `cargo xtask check` passes.

### D.4 — Move `vfs_service_close_pub` to a new `kernel/src/fs/vfs_service.rs`

**Files:**
- `kernel/src/fs/mod.rs` (add `pub mod vfs_service;`)
- `kernel/src/fs/vfs_service.rs` (new module; add `pub fn vfs_service_close_pub` body)
- `kernel/src/arch/x86_64/syscall/mod.rs` (remove `pub(crate) fn vfs_service_close_pub` at ~line 7802; `vfs_service_close` itself at ~line 7183 stays in the syscall layer until a later phase carves out the full vfs-service module)
- `kernel/src/process/mod.rs` (update both call sites)

**Symbol:** `vfs_service_close_pub`
**Why it matters:** The public teardown wrapper belongs next to a real `vfs_service` module rather than masquerading as syscall infrastructure.

**Acceptance:**
- [ ] `kernel/src/fs/vfs_service.rs` exists and contains `pub fn vfs_service_close_pub`.
- [ ] `grep -n 'fn vfs_service_close_pub' kernel/src/arch/x86_64/syscall/mod.rs` returns zero lines.
- [ ] `cargo xtask check` passes.

---

## Track E — Password Hash Format Upgrade

### E.1 — Upgrade pre-seeded image hash format

**Current state (verified):** Password hashing is currently scattered across `userspace/passwd/src/main.rs`, `userspace/passwd/src/lib.rs`, `userspace/adduser/src/main.rs`, `xtask/src/main.rs`, and the SHA-256 primitive in `userspace/syscall-lib/src/sha256.rs`. There is no `hash.rs` file in either binary crate yet.

**Files:**
- `userspace/passwd/src/lib.rs` (consolidate the hashing helper here; both `passwd` main and `adduser` import from it)
- `userspace/adduser/src/main.rs` (replace its local hashing path with the call into `passwd_lib::hash_password` — or extract into a shared `userspace/lib/credhash` crate if a separate crate is cleaner)
- `xtask/src/main.rs` (regenerate pre-seeded `/etc/shadow` entries through the canonical helper rather than an inline literal)
- `userspace/syscall-lib/src/sha256.rs` (unchanged primitive, but referenced)

**Symbol:** `hash_password`, `HASH_FORMAT_PREFIX`
**Why it matters:** The Phase 48 format string is undocumented inline; the format must be named and the bootstrap regenerated.

**Acceptance:**
- [ ] `hash_password` is documented with a code comment naming the algorithm (SHA-256 iterative) and iteration count.
- [ ] The format string is defined as a named constant (`HASH_FORMAT_PREFIX`), not an inline literal.
- [ ] Pre-seeded hashes baked by `xtask` into `/etc/shadow` on the data disk are generated by calling `hash_password` rather than by pasting a static hash string.
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
- [ ] Key Files table cites `kernel/src/fs/vfs.rs`, `kernel-core/src/fs/mode.rs`, `userspace/lib/shadow/src/lib.rs`, `kernel/src/process/mod.rs` (FdEntry), `kernel/src/arch/x86_64/syscall/mod.rs`, and `userspace/passwd/src/main.rs`.
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
- The CLOEXEC/NONBLOCK changes must not break existing tests that pass `0` for flags to `open`-family syscalls; `FdEntry { cloexec: false, nonblock: false, … }` is the default for non-flag-bearing call sites.
- Track D should land as one PR (four small commits, one per wrapper) so the small handful of process/mod.rs call sites are updated in lock-step with each body move. No `pub(crate) use` shim layer — `process/mod.rs` already routes through `crate::arch::x86_64::syscall::*`; the relocations simply change those paths to `crate::net::*`, `crate::epoll::*`, `crate::fs::ext2::*`, `crate::fs::vfs_service::*`.
- Track D.2 and D.4 create new top-level modules (`kernel/src/epoll.rs`, `kernel/src/fs/vfs_service.rs`) that intentionally start small — they hold only the relocated wrapper. Carving the *rest* of the epoll and vfs-service code out of `syscall/mod.rs` is a follow-up phase, not part of Phase 66.
