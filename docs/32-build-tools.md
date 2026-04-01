# Phase 32 - Build Tools and Scripting

## Overview

Phase 32 adds a POSIX-compatible `make` build tool and supporting utilities,
enabling multi-file C projects to be built inside the OS. This transforms the OS
from a "compile one file" environment into one where real software projects can
be developed with incremental builds.

## Components Added

### pdpmake (POSIX make)

**pdpmake** (Public Domain POSIX make) is a ~4000-line C implementation of
POSIX make. It was chosen for its simplicity, lack of external dependencies,
and public domain license.

- Cross-compiled with `musl-gcc -static -O2` (~143 KB binary)
- Available as `/bin/make` in the ramdisk
- Supports: target rules, inference rules (`.c.o:`), variable assignment and
  expansion (`CC = tcc`, `$(CC)`), phony targets (`.PHONY`), file timestamp
  comparison for incremental builds, pattern substitution (`$(SRCS:.c=.o)`)

### New Utilities

All implemented as Rust `no_std` coreutils (with C source files kept for
reference and for TCC compilation inside the OS):

| Utility | Purpose |
|---------|---------|
| `touch` | Create files or update modification timestamps |
| `stat`  | Display file metadata (size, timestamps, permissions) |
| `wc`    | Count lines, words, and bytes |
| `ar`    | Create static library archives (`.a` files) |
| `install` | Copy files and create directories |

### Kernel Changes

- **Stat timestamps**: `sys_stat`/`sys_fstat`/`sys_fstatat` now populate
  `st_atime`, `st_mtime`, and `st_ctime` from ext2 inode timestamps
- **`sys_utimensat` (syscall 280)**: Update file modification timestamps.
  Supports `UTIME_NOW` and `UTIME_OMIT` flags
- **Write mtime updates**: Writing to ext2 files now updates `st_mtime` and
  `st_ctime` automatically
- **Kernel version**: bumped to 0.32.0

## How `make` Uses File Timestamps

`make` determines what to rebuild by comparing modification times:

1. For each target, `make` checks if the target file exists
2. If it exists, `make` compares its `st_mtime` to each dependency's `st_mtime`
3. If any dependency is newer than the target, `make` re-runs the recipe
4. If the target doesn't exist, `make` always runs the recipe

This is why the kernel timestamp support (Track A) was a prerequisite:
- `stat()` must return meaningful `st_mtime` values
- `write()` must update `st_mtime` when a file is modified
- `touch` must be able to update `st_mtime` via `utimensat()`

## Demo Project

A multi-file C project is included on the ext2 disk at `/home/project/`:

```
/home/project/
  Makefile     — build rules using TCC
  main.c       — entry point, calls util functions
  util.c       — utility functions (add, factorial)
  util.h       — header declarations
  build.sh     — manual build script
```

### Usage

```sh
cd /home/project
make          # builds main.o, util.o, links to demo
./demo        # runs the program
touch util.c  # mark util.c as modified
make          # only recompiles util.o + relinks
make clean    # removes .o files and demo binary
```

### Static Library Workflow

```sh
tcc -static -c util.c -o util.o
ar rcs libutil.a util.o
tcc -static -o demo main.c libutil.a
```

## ar Archive Format

The `ar` utility creates standard Unix archive files:

```
!<arch>\n                    (8-byte magic)
[for each member:]
  name/           padding   (16 bytes, '/' terminated)
  timestamp       padding   (12 bytes)
  uid             padding   (6 bytes)
  gid             padding   (6 bytes)
  mode            padding   (8 bytes, octal)
  size            padding   (10 bytes)
  `\n                        (2-byte end marker)
  [file data]
  [\n if odd size]           (padding to even boundary)
```

TCC can link `.a` archives directly. The `s` flag (symbol index) is a no-op
stub since TCC doesn't require it.

## Shell Scripting

Ion shell (already present from Phase 22) supports the scripting constructs
needed for build automation:

- `for` loops: `for f in *.c; echo $f; end`
- Conditionals: `if test -f Makefile; echo found; end`
- Command substitution: `$(command)`
- Exit status: `$?`
- `test` builtin: `-f`, `-d`, `-e`, `-z`, `-n`, `=`, `!=`

The `build.sh` script in the demo project demonstrates a manual build-and-test
cycle without `make`.

## How This Differs from Production Build Systems

| Feature | m3OS Phase 32 | Production Systems |
|---------|--------------|-------------------|
| Make variant | pdpmake (POSIX) | GNU make with extensions |
| Build generator | None | CMake, Meson, Ninja |
| Configure | None | Autoconf/Automake |
| Package manager | None | apt, pkg, ports |
| Linking | TCC built-in | standalone ld, lld |
| Binary tools | None | nm, objdump, readelf |
| Libraries | Static only | Dynamic + static |

Phase 44 (Ports System) will add package management capabilities.

## Files Changed

### Kernel
- `kernel/Cargo.toml` — version bump to 0.32.0
- `kernel/src/arch/x86_64/syscall.rs` — stat timestamps, sys_utimensat, write mtime
- `kernel/src/fs/ramdisk.rs` — register new binaries

### Userspace
- `userspace/coreutils-rs/src/{touch,stat_cmd,wc,ar,install}.rs` — Rust implementations
- `userspace/coreutils/{touch,stat,wc,ar,install}.c` — C reference implementations
- `userspace/syscall-lib/src/lib.rs` — Stat struct, utimensat/stat wrappers
- `userspace/demo-project/` — multi-file C demo project

### Build System
- `xtask/src/main.rs` — pdpmake build, demo project population, new coreutils
