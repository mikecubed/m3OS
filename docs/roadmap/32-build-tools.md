# Phase 32 — Build Tools and Scripting

**Status:** Complete
**Source Ref:** phase-32
**Depends on:** Phase 26 (Text Editor) ✅, Phase 31 (Compiler Bootstrap) ✅
**Builds on:** Extends the compiler from Phase 31 with a build system (make) and utility programs so that multi-file C projects can be developed entirely inside the OS
**Primary Components:** userspace/coreutils/ (ar, install, touch, stat, wc), userspace/coreutils-rs/ (ar, install, touch, stat, wc), userspace/demo-project/

## Milestone Goal

Multi-file C projects can be built inside the OS using a `make`-compatible build tool
and shell scripts. This transforms the OS from a "compile one file" environment into
one where real software projects can be developed.

## Why This Phase Exists

Phase 31 delivered a working C compiler, but compiling one file at a time is impractical
for real projects. Any non-trivial C program has multiple source files, header
dependencies, and a build process that should be automated. Without a build tool,
developers must manually track which files changed and re-run the compiler for each one.
`make` solves this with dependency graphs and timestamp-based incremental builds. The
additional coreutils (`ar`, `touch`, `stat`, `wc`) fill gaps that `make` and general
development workflows depend on.

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

## Important Components and How They Work

### pdpmake

The chosen `make` implementation is pdpmake (Public Domain POSIX make), a ~3000-line C
program that implements the POSIX make specification. It is cross-compiled with musl
and placed in the disk image at `/usr/bin/make`. It parses Makefiles, builds a
dependency graph, compares file timestamps, and executes shell commands for out-of-date
targets.

### ar (Archive Tool)

`ar` creates and manipulates static library archives (`.a` files). It reads `.o` object
files and packs them into an archive with a symbol table. TCC's built-in linker can
then link against these archives. Both C and Rust implementations are provided.

### Demo Project

A multi-file C project (`userspace/demo-project/`) is included in the disk image to
demonstrate and test the full build workflow: editing, compiling, linking, incremental
rebuilds, and `make clean`.

## How This Builds on Earlier Phases

- **Extends Phase 31 (Compiler Bootstrap):** Adds build automation on top of the TCC compiler; `make` invokes `tcc` to compile and link.
- **Extends Phase 26 (Text Editor):** Developers use the editor to modify Makefiles and source files before running `make`.
- **Reuses Phase 24 (Persistent Storage):** File timestamps stored on the FAT32 filesystem are used by `make` for incremental build decisions.

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

- [Phase 32 Task List](./tasks/32-build-tools-tasks.md)

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
