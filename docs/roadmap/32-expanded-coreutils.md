# Phase 32 - Expanded Coreutils

## Milestone Goal

The OS ships with a comprehensive set of Unix utilities sufficient for daily development
work. Moving beyond the minimal set from Phase 14, this phase adds text processing,
file inspection, and system administration tools that developers expect.

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
| `kill` | Send signals to processes | Already exists via shell builtin; add standalone |
| `uptime` | System uptime | Write (~20 lines C) |
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
| `bc` | Calculator | Port sbase bc or write minimal version |

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

- `/proc` filesystem (or equivalent) for `ps`, `free`, `uptime` to read kernel state.
  Alternatively, add dedicated syscalls for process listing and memory stats.
- `symlink` / `readlink` syscalls if symlinks are added.
- `fchmod` / `fchown` syscalls.
- `pipe2` with O_CLOEXEC for better xargs/find support.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 14 (Shell and Tools) | Existing basic coreutils as foundation |
| Phase 27 (User Accounts) | chmod/chown need UID/GID support |
| Phase 30 (Compiler) | Can compile new tools inside the OS (stretch goal) |

## Implementation Outline

1. Set up sbase cross-compilation with musl on the host.
2. Port text processing tools first (head, tail, sort, uniq — most immediately useful).
3. Port file tools (find, xargs, tee).
4. Write system tools (ps, free, uptime) — these need kernel info, so implement
   a `/proc`-like interface or info syscalls.
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
- `diff file1 file2` shows differences; `patch` can apply the diff.
- `less` provides scrollable file viewing with search.

## Companion Task List

- [Phase 32 Task List](./tasks/32-expanded-coreutils-tasks.md)

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
- `/proc` filesystem (full implementation)
- BusyBox-style multicall binary
- man pages
