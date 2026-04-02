# Phase 41 - Expanded Coreutils

**Status:** Planned
**Source Ref:** phase-41
**Depends on:** Phase 14 (Shell and Tools) ✅, Phase 27 (User Accounts) ✅, Phase 38 (Filesystem Enhancements) ✅
**Builds on:** Extends the minimal coreutils from Phase 14 with a comprehensive set of
text processing, file inspection, system administration, and developer workflow tools.
Phase 38's `/proc` filesystem and symlink/permissions infrastructure provide the kernel
interfaces that the new system and file tools read from.
**Primary Components:** userspace/coreutils, userspace/coreutils-rs, kernel/src/fs/procfs.rs, kernel/src/serial.rs, xtask/src/main.rs

## Milestone Goal

The OS ships with a comprehensive set of Unix utilities sufficient for daily development
work. Moving beyond the minimal set from Phase 14, this phase adds text processing,
file inspection, and system administration tools that developers expect.

## Why This Phase Exists

After Phase 14, the OS has a working shell with basic tools (`cat`, `ls`, `grep`, `cp`,
`mv`, `rm`, `echo`, `mkdir`, `rmdir`, `pwd`, `env`, `sleep`, `wc`, `touch`, `stat`).
That set is enough for simple file manipulation but not for real development work.
Developers expect text processing pipelines (`sort | uniq -c`), file search
(`find | xargs`), system introspection (`ps`, `free`, `dmesg`), permission management
(`chmod`, `chown`), and a pager (`less`). Without these, the OS feels like a demo rather
than a usable environment. This phase closes that gap by porting or writing ~30 tools.

## Learning Goals

- Understand how small, composable Unix tools work together via pipes.
- Learn the Unix philosophy: each tool does one thing well.
- See how porting existing tools (vs. writing from scratch) accelerates OS development.

## Feature Scope

### Text Processing Tools

| Tool | Purpose | Source |
|---|---|---|
| `head` | Print first N lines | Port from sbase or write (~50 lines C) |
| `tail` | Print last N lines | Port from sbase or write (~80 lines C) |
| `sort` | Sort lines | Port from sbase (~200 lines C) |
| `uniq` | Filter duplicate lines | Port from sbase (~60 lines C) |
| `cut` | Extract fields/columns | Port from sbase (~80 lines C) |
| `tr` | Translate characters | Port from sbase (~100 lines C) |
| `sed` | Stream editor | Port from sbase (~400 lines C) |
| `diff` | Compare files | Port from sbase or OpenBSD diff |
| `patch` | Apply diffs | Port from sbase or write minimal version |
| `tee` | Duplicate stdin to file and stdout | Write (~30 lines C) |

### File and Directory Tools

| Tool | Purpose | Source |
|---|---|---|
| `find` | Search for files | Port from sbase (~300 lines C) |
| `xargs` | Build commands from stdin | Port from sbase (~100 lines C) |
| `du` | Disk usage | Write or port (~60 lines C) |
| `df` | Filesystem free space | Write (~40 lines C) |
| `ln` | Create links (hard/symlinks when supported) | Write (~30 lines C) |
| `file` | Identify file type (basic) | Write (~100 lines C) |
| `hexdump` / `xxd` | Hex dump of binary files | Write (~80 lines C) |

### System Tools

| Tool | Purpose | Source |
|---|---|---|
| `ps` | List running processes | Write (reads kernel proc info) |
| `kill` | Send signals to processes | Write (thin wrapper over `kill(2)`) |
| `uptime` | System uptime | Extend existing Rust utility or add C equivalent |
| `free` | Memory usage summary | Write (~30 lines C) |
| `dmesg` | Kernel log buffer | Write (reads kernel ring buffer) |
| `mount` / `umount` | Mount/unmount filesystems | Write (wraps mount syscall) |
| `chmod` | Change file permissions | Write (~40 lines C) |
| `chown` | Change file ownership | Write (~40 lines C) |

### Developer Tools

| Tool | Purpose | Source |
|---|---|---|
| `less` / `more` | Pager for viewing files | Port sbase more or write minimal pager |
| `strings` | Extract printable strings from binaries | Write (~40 lines C) |
| `cal` | Calendar display | Port from sbase (~60 lines C) |

### Porting Strategy: sbase

[sbase](https://git.suckless.org/sbase/) from suckless.org is a collection of minimal
Unix utilities in clean, portable C. Most tools are 30-200 lines and have no dependencies
beyond libc. This is the ideal source for porting:

1. Clone sbase on the host.
2. Cross-compile individual tools with `x86_64-linux-musl-gcc -static`.
3. Add working binaries to the disk image.
4. Test each tool inside the OS.

Not all sbase tools will work immediately — some may require syscalls we haven't
implemented. Prioritize tools that work with our existing syscall set and add missing
syscalls as needed.

### Kernel Support (if needed)

- Kernel log ring buffer in `kernel/src/serial.rs` so `dmesg` can read boot messages.
- `/proc/kmsg` virtual file in `kernel/src/fs/procfs.rs` to expose the ring buffer.
- `sys_umount2()` syscall for the `umount` binary.
- `pipe2` with O_CLOEXEC for better xargs/find support (stretch goal).

Note: `/proc` (Phase 38) already provides `meminfo`, `uptime`, `mounts`, and per-PID
`status`/`cmdline`. The `chmod`/`chown`/`fchmod`/`fchown`/`mount`/`kill`/`link`/`symlink`
syscalls also already exist in `syscall-lib`.

## Important Components and How They Work

### sbase cross-compilation pipeline

sbase tools are self-contained C files (30–400 lines each) with no dependencies beyond
libc. The `xtask/src/main.rs` function `build_musl_bins()` already cross-compiles C
sources with `musl-gcc -static` and copies the resulting ELFs to `kernel/initrd/`. New
tools are added by appending `(source_path, binary_name)` tuples to its `bins` array.

### `/proc` as the system introspection interface

Phase 38 built a full procfs with per-PID directories, `/proc/meminfo`, `/proc/uptime`,
`/proc/mounts`, and `/proc/stat`. System tools like `ps`, `free`, and `mount` (no args)
read these virtual files using standard `open()`/`read()` calls — no custom syscalls needed.
The `kernel/src/fs/procfs.rs` module generates file content on the fly from kernel data
structures.

### Permission and ownership syscalls

The `chmod()`, `chown()`, `fchmod()`, and `fchown()` syscalls (and their `syscall-lib`
wrappers) were implemented in Phase 27/38. The `chmod` and `chown` binaries are thin
argument-parsing wrappers around these existing syscalls.

## How This Builds on Earlier Phases

- Extends Phase 14 by adding ~30 tools to the original ~15 basic coreutils.
- Reuses Phase 38's `/proc` filesystem for `ps`, `free`, `mount`, and `uptime` output.
- Reuses Phase 27's UID/GID model and Phase 38's permission enforcement for `chmod`/`chown`.
- Reuses Phase 22's termios raw mode for `less` keyboard input handling.
- Reuses Phase 34's `clock_gettime()`/`gettimeofday()` for `cal` and `uptime`.

## Implementation Outline

1. Set up sbase cross-compilation with musl on the host.
2. Port text processing tools first (head, tail, sort, uniq — most immediately useful).
3. Port file tools (find, xargs, tee).
4. Build the system tools around existing procfs and syscall support: `ps`,
   `free`, `mount`, and `uptime` read the Phase 38 `/proc` files, while new
   kernel work stays limited to the `dmesg` ring buffer and `umount`.
5. Port diff and patch — essential for development workflows.
6. Port or write a pager (less/more).
7. Port remaining tools.
8. Test each tool with shell pipelines to verify composability.

## Acceptance Criteria

- All listed tools are present in `/bin` or `/usr/bin` and work from the shell.
- Pipelines work: `cat file | sort | uniq -c | sort -rn | head -10`.
- `find . -name "*.c" | xargs grep "main"` works.
- `ps` shows running processes with PID, name, and status.
- `free` shows total and available memory.
- `uptime` reports time since boot and is available from the default shell PATH.
- `diff file1 file2` shows differences; `patch` can apply the diff.
- `less` provides scrollable file viewing with search.

## Companion Task List

- [Phase 41 Task List](./tasks/41-expanded-coreutils-tasks.md)

## How Real OS Implementations Differ

Real systems use GNU coreutils (or BusyBox on embedded systems), which have decades
of feature additions, locale support, and edge case handling. Our sbase-based utilities
are intentionally minimal — they handle the common cases and skip exotic options.

Real systems also have a `/proc` filesystem providing rich kernel introspection. Our
approach may use either a simplified `/proc` or dedicated syscalls, depending on what
is simpler to implement.

## Deferred Until Later

- Full GNU coreutils compatibility
- Locale and internationalization support
- BusyBox-style multicall binary
- man pages
- `bc` calculator (complex parser; deferred to a later follow-up once the text and file toolchain lands)
