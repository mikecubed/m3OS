# Phase 31 - Build Tools and Scripting

## Milestone Goal

Multi-file C projects can be built inside the OS using a `make`-compatible build tool
and shell scripts. This transforms the OS from a "compile one file" environment into
one where real software projects can be developed.

## Learning Goals

- Understand how `make` uses dependency graphs and file timestamps to do incremental builds.
- Learn how shell scripting automates repetitive development tasks.
- See why build tools are essential infrastructure for any self-hosting system.

## Feature Scope

### Build Tool: Port `make`

Port a minimal `make` implementation. Candidates (in order of preference):

1. **`pdpmake`** (Public Domain POSIX make) — ~3000 lines of C, POSIX-compliant,
   no dependencies beyond libc. Perfect for our needs.
2. **`omake`** (OpenBSD make) — well-documented, BSD-licensed, POSIX make.
3. **Custom minimal make** — if porting proves too complex, write a ~500 line
   make-subset that handles basic rules, dependencies, and `$(CC)` variable expansion.

Required Makefile features:
- Inference rules (`.c.o:` or `%.o: %.c`)
- Variable assignment (`CC = tcc`, `CFLAGS = -Wall`)
- Phony targets (`all`, `clean`)
- File timestamp comparison for incremental builds
- Pattern substitution (`$(SRC:.c=.o)`)

### Shell Scripting Improvements

Ion shell already supports scripting, but verify and fix:
- `for` loops: `for f in *.c; ... end`
- Conditionals: `if test -f Makefile; ... end`
- Command substitution: `$(command)` or `` `command` ``
- Exit status checking: `$?`
- `test` / `[` builtin for file and string tests

### Additional Utilities

- **`ar`** — create static libraries (`.a` archives). Port `sbase` ar or write a minimal one.
- **`install`** — copy files with permission setting. Simple C utility.
- **`time`** — measure command execution time (requires `clock_gettime`).
- **`wc`** — word/line/byte count (simple but frequently needed during development).
- **`touch`** — update file timestamps (needed for make).
- **`stat`** — display file metadata.

### Demonstration: Multi-file Project

Build a non-trivial C project inside the OS:
```
project/
  Makefile
  main.c
  util.c
  util.h
```
`make` compiles both `.c` files and links them. `make clean` removes object files.
Editing one file and re-running `make` only recompiles the changed file.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 26 (Text Editor) | Edit Makefiles and source files |
| Phase 30 (Compiler) | TCC for compilation |
| Phase 24 (Persistent Storage) | File timestamps for make |

## Implementation Outline

1. Cross-compile pdpmake (or chosen make) with musl.
2. Add `make` binary to disk image at `/usr/bin/make`.
3. Verify basic Makefile parsing and rule execution.
4. Cross-compile `ar`, `install`, `touch`, `wc`, `stat` utilities.
5. Add them to the disk image.
6. Create the demo multi-file project in the disk image.
7. Boot, run `make` in the project directory, verify incremental builds work.
8. Test shell scripting: write a `build.sh` that automates a build-and-test cycle.

## Acceptance Criteria

- `make --version` (or equivalent) runs inside the OS.
- A Makefile with inference rules, variables, and phony targets is parsed correctly.
- Incremental builds work: only modified files are recompiled.
- `make clean` removes generated files.
- `ar` can create a static library from `.o` files.
- Shell scripts with loops, conditionals, and command substitution work.
- The multi-file demo project builds, runs, and incrementally rebuilds correctly.

## Companion Task List

- [Phase 31 Task List](./tasks/31-build-tools-tasks.md)

## How Real OS Implementations Differ

Real build systems have evolved far beyond POSIX make:
- GNU make extensions (pattern rules, functions, recursive make)
- CMake, Meson, Ninja for cross-platform builds
- Autoconf/Automake for portability
- Package managers (apt, pkg) handle fetching, building, and installing software

We implement the POSIX make subset because it is the minimum viable build tool
and enables porting most C projects with simple Makefiles.

## Deferred Until Later

- GNU make extensions
- CMake, Meson, or other meta-build systems
- `ld` as a standalone linker (TCC has a built-in linker)
- `nm`, `objdump`, or other binary inspection tools
- Autoconf/configure script support
