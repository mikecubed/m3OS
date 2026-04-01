# Phase 21 — Ion Shell Integration: Task List

**Status:** Complete
**Source Ref:** phase-21
**Depends on:** Phase 20 ✅
**Goal:** Integrate
[ion](https://github.com/redox-os/ion) — the shell built for Redox OS — into the
system as a non-interactive/script-mode shell at `/bin/ion`, while keeping the
Phase 20 shell as the interactive shell that userspace init spawns (`/bin/sh0`)
and continuing to use it as a regression harness. Ion becomes the interactive
login shell in Phase 22 (Termios).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Kernel syscall stubs for ion/musl | — | ✅ Done |
| B | Build pipeline: cross-compile ion, embed in ramdisk | — | ✅ Done |
| C | Rename Phase 20 shell to sh0, update init with fallback | B | ✅ Done |
| D | Runtime debugging: boot ion, fix crashes iteratively | A, B, C | ✅ Done |
| E | Validation and documentation | D | ✅ Done |

---

## Track A — Kernel Syscall Stubs

Ion's runtime (via musl libc and the `nix` crate) calls syscalls that our
kernel doesn't yet handle. Most can be stubbed with harmless return values;
a few need minimal implementation.

### A.1 — fcntl stub

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_fcntl`
**Why it matters:** musl uses fcntl for `F_DUPFD_CLOEXEC` and `F_SETFD`; ion cannot start without it.

**Acceptance:**
- [x] `fcntl` (72) handles F_DUPFD, F_DUPFD_CLOEXEC, F_GETFD/SETFD, F_GETFL/SETFL

### A.2 — User/group ID stubs

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Ion's `users` crate queries uid/gid at startup; stubbing as root (0) is sufficient.

**Acceptance:**
- [x] `getuid` (102), `geteuid` (107), `getgid` (104), `getegid` (108) all return 0

### A.3 — getpgrp stub

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** The `nix` crate calls `getpgrp` during process group management.

**Acceptance:**
- [x] `getpgrp` (111) delegates to `getpgid(0)`

### A.4 — access stub

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_access`
**Why it matters:** Ion uses `access` for PATH searching to check if commands exist before exec.

**Acceptance:**
- [x] `access` (21) checks ramdisk/tmpfs/dev paths

### A.5 — mprotect stub

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl uses mprotect for stack guard pages; a no-op is safe here.

**Acceptance:**
- [x] `mprotect` (10) returns 0 (no-op)

### A.6 — set_robust_list stub

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl thread initialization calls this; a no-op prevents startup failure.

**Acceptance:**
- [x] `set_robust_list` (273) returns 0 (no-op)

### A.7 — prlimit64 stub

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl queries resource limits at startup; returning ENOSYS is the correct behavior for an unimplemented syscall.

**Acceptance:**
- [x] `prlimit64` (302) returns `-ENOSYS`

### A.8 — getrandom implementation

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_getrandom`
**Why it matters:** The `rand` crate needs entropy for initialization; a TSC-seeded PRNG provides sufficient randomness for a toy OS.

**Acceptance:**
- [x] `getrandom` (318) implemented with TSC-seeded xorshift64* PRNG

### A.9 — Terminal ioctl stubs

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_ioctl`
**Why it matters:** Ion's TTY detection probes these ioctls; returning ENOTTY tells it no terminal is available.

**Acceptance:**
- [x] TCGETS/TCSETS/TIOCGPGRP/TIOCSPGRP return `-ENOTTY`

### A.10 — clone stub

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl may use clone(SIGCHLD) instead of fork; delegating to sys_fork handles this case.

**Acceptance:**
- [x] `clone` (56) with SIGCHLD delegates to `sys_fork`

### A.11 — pipe2 and dup3 stubs

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl prefers pipe2/dup3 over pipe/dup2; delegating to the existing implementations avoids duplication.

**Acceptance:**
- [x] `pipe2` (293) delegates to `sys_pipe`
- [x] `dup3` (292) delegates to `sys_dup2`

### A.12 — Bonus syscall stubs

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Additional syscalls discovered during runtime testing.

**Acceptance:**
- [x] futex (202), clock_gettime (228), gettimeofday (96), socketpair (53), /dev/null all handled
- [x] `cargo xtask check` passes

---

## Track B — Build Pipeline

Cross-compile ion for musl and embed it in the ramdisk alongside existing
binaries. Ion is ~3.1 MB so this significantly increases the kernel image size.

### B.1 — build_ion() in xtask

**File:** `xtask/src/main.rs`
**Symbol:** `build_ion`
**Why it matters:** Ion must be cross-compiled with musl for a statically linked, non-PIE binary that our ELF loader can handle.

**Acceptance:**
- [x] `build_ion()` clones, builds with `-C relocation-model=static`, strips, and caches
- [x] musl target already present in CI
- [x] Vendoring deferred — `build_ion()` caches `ion.elf` between builds

### B.2 — Ramdisk entries for ion

**File:** `kernel/src/fs/ramdisk.rs`
**Why it matters:** Ion must be discoverable at `/bin/ion` for exec to find it.

**Acceptance:**
- [x] `/bin/ion` and `/bin/ion.elf` entries added to ramdisk

### B.3 — Build verification

**Why it matters:** Confirms the enlarged image builds and ion binary has the right format.

**Acceptance:**
- [x] `cargo xtask image` builds successfully with ion (3.1 MB stripped)
- [x] Ion is ET_EXEC (non-PIE), no relocations, no PT_INTERP

---

## Track C — Init and Shell Rename

Rename the Phase 20 minimal shell to `/bin/sh0` and update init to launch
ion with a fallback to sh0.

### C.1 — Shell rename

**Files:** `userspace/shell/Cargo.toml`, `xtask/src/main.rs`
**Why it matters:** The Phase 20 shell needs a distinct name to coexist with ion.

**Acceptance:**
- [x] Shell binary renamed to `sh0` in Cargo.toml + xtask
- [x] Ramdisk has `/bin/sh0` and `/bin/sh0.elf` entries

### C.2 — Init fallback logic

**File:** `userspace/init/src/main.rs`
**Why it matters:** Init must boot even if ion is missing; sh0 provides a reliable fallback.

**Acceptance:**
- [x] Init execs `/bin/sh0` first, falls back to `/bin/ion`
- [x] CI boot assertions verified via piped QEMU test
- [x] `cargo xtask check` + `cargo xtask image` pass

---

## Track D — Runtime Debugging

Boot ion in QEMU and iteratively fix kernel-side issues. This track is
inherently iterative — each boot attempt may reveal new missing syscalls
or unexpected behavior.

### D.1 — First boot and PIE fix

**File:** `xtask/src/main.rs`
**Why it matters:** The initial PIE binary crashed because our ELF loader expected fixed addresses.

**Acceptance:**
- [x] Switched to non-PIE (ET_EXEC) build

### D.2 — Catch-all syscall logger

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Unhandled syscalls must be logged to identify gaps quickly during bring-up.

**Acceptance:**
- [x] `log::warn` for unhandled syscall numbers

### D.3 — musl startup verification

**Why it matters:** musl's `__libc_start_main` calls several syscalls during C runtime init; all must succeed.

**Acceptance:**
- [x] `arch_prctl`, `set_tid_address`, `mprotect` all verified working

### D.4 — Ion interactive mode

**Why it matters:** Ion starts, detects non-TTY, and enters a degraded but functional loop.

**Acceptance:**
- [x] Ion starts, detects non-TTY, prints errors gracefully, enters loop
- [x] Ion script mode (`ion -c`) deferred to Phase 22 (ENOTTY)

### D.5 — sh0 regression and fallback

**Why it matters:** The existing shell must keep working while ion is being debugged.

**Acceptance:**
- [x] Pipeline testing (`ls | cat`) works via sh0
- [x] `cd /tmp && pwd` works via sh0
- [x] sh0 fallback verified when ion not available

### D.6 — Critical bug fixes

**File:** `kernel/src/arch/x86_64/syscall.rs`, `kernel/src/process/mod.rs`, `kernel/src/mm/`
**Why it matters:** Ion's complex runtime exposed latent kernel bugs that would affect any large userspace binary.

**Acceptance:**
- [x] Fixed critical futex context restore bug (init CR3 corruption)
- [x] Fixed fork child caller-saved register corruption (RDI/RSI/RDX/R8/R9/R10)
- [x] Added demand paging for stack region (8 MiB above ELF_STACK_TOP)
- [x] `getrandom` implemented with TSC-seeded xorshift64* PRNG

---

## Track E — Validation and Documentation

### E.1 — Acceptance tests

**Why it matters:** Validates ion integration and sh0 backward compatibility.

**Acceptance:**
- [x] `cargo xtask image` produces a disk image containing `/bin/ion` without manual intervention
- [x] `echo hello` prints `hello` (via sh0)
- [x] `ls | cat` produces directory listing via pipeline (via sh0)
- [x] `cd /tmp && pwd` prints `/tmp` (via sh0)
- [x] `/bin/sh0` still boots and works as a fallback
- [x] `readelf` confirms ion binary is statically linked with no `PT_INTERP`
- [x] Phase 20 acceptance criteria still pass when using `/bin/sh0`
- [x] `cargo xtask check` passes (clippy + fmt + 63 host tests)
- [x] QEMU boot validation — no panics, no regressions

### E.2 — Deferred acceptance tests (require Phase 22)

**Why it matters:** Ion interactive mode requires termios support.

**Acceptance (deferred to Phase 22):**
- [x] Booting in QEMU presents the ion prompt (resolved by P22-T046)
- [x] `let x = world; echo $x` prints `world` (resolved by P22-T051)
- [x] `for i in a b c { echo $i }` prints three lines (resolved by P22-T052)
- [x] `Ctrl-C` during `sleep 10` kills the child (resolved by P22-T054)

### E.3 — Documentation

**File:** `docs/19-ion-shell.md`
**Why it matters:** Documents the ion integration approach and remaining work.

**Acceptance:**
- [x] `docs/19-ion-shell.md` written

---

## Deferred Until Phase 22

These items require `tcgetattr`/`tcsetattr` (termios) support:

- Ion's interactive raw-mode line editor (arrow keys, history recall)
- History persistence (`~/.local/share/ion/history`)
- Tab completion with reedline-style highlighting
- `SIGWINCH` / window size change notifications
- Proper `isatty()` that returns true for the console fd

---

## Documentation Notes

- Ion is cross-compiled for `x86_64-unknown-linux-musl` as a statically linked ET_EXEC binary
- Phase 20 shell renamed from `sh` to `sh0` to coexist with ion
- Multiple critical kernel bugs fixed during ion bring-up (futex CR3 corruption, fork register corruption, demand paging)
- Ion interactive mode deferred to Phase 22 pending termios support
- `docs/19-ion-shell.md` written as the phase documentation
