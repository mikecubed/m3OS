# Phase 45 — Ports and Package System: Task List

**Status:** Complete
**Source Ref:** phase-45
**Depends on:** Phase 31 (Compiler) ✅, Phase 32 (Build Tools) ✅, Phase 41 (Coreutils) ✅, Phase 24 (Persistent Storage) ✅
**Goal:** A simple BSD-style ports system that builds and installs third-party
software from source recipes inside m3OS, with a `port` command for lifecycle
management, dependency resolution, and package tracking.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Portfile format and ports tree structure | — | ✅ Done |
| B | `port` command implementation | A | ✅ Done |
| C | xtask integration for ports tree packaging | A | ✅ Done |
| D | Initial ports: Lua and zlib | A, B, C | ✅ Done |
| E | Remaining ports collection | D | ✅ Done |
| F | Dependency resolution and package tracking | B, D | ✅ Done |
| G | Integration testing and documentation | A–F | ✅ Done |

---

## Track A — Portfile Format and Ports Tree Structure

Define the metadata format and directory layout that every port follows.

### A.1 — Design the Portfile format

**File:** `ports/lang/lua/Portfile`
**Symbol:** `Portfile` (format specification)
**Why it matters:** The Portfile is the single source of truth for a port's
identity, version, dependencies, and description. It uses shell variable syntax
so the `port` command can source it directly. Getting this right first avoids
rework across every port later.

**Acceptance:**
- [x] Portfile format documented with required fields: `NAME`, `VERSION`, `DESCRIPTION`, `CATEGORY`, `DEPS`
- [x] Format uses shell variable assignments (e.g., `NAME=lua`, `VERSION=5.4.7`)
- [x] At least one example Portfile exists (Lua)
- [x] Optional fields defined: `URL`, `SHA256`, `MAINTAINER`

### A.2 — Create the ports tree directory structure on the host

**Files:**
- `ports/lang/lua/Portfile`
- `ports/lang/lua/Makefile`
- `ports/lib/zlib/Portfile`
- `ports/lib/zlib/Makefile`

**Symbol:** `ports/` (directory tree)
**Why it matters:** The host-side `ports/` tree mirrors the `/usr/ports/`
layout that will exist inside m3OS. Organizing ports into `category/program/`
directories with Portfile + Makefile + patches follows the BSD ports convention
and makes the structure predictable for both the `port` command and human
maintainers.

**Acceptance:**
- [x] `ports/` directory exists at the project root with `category/program/` hierarchy
- [x] Each port directory contains at minimum `Portfile` and `Makefile`
- [x] Optional `patches/` subdirectory for m3OS-specific source patches
- [x] At least two categories (`lang/`, `lib/`) with placeholder ports

### A.3 — Design the Makefile target contract

**File:** `ports/lang/lua/Makefile`
**Symbol:** `fetch`, `patch`, `build`, `install`, `clean` (make targets)
**Why it matters:** Every port Makefile must implement the same set of targets
so the `port` command can drive any port generically. The contract defines what
each target does: `fetch` copies bundled source to work/, `patch` applies patches,
`build` compiles with TCC/make, `install` copies to `/usr/local/`, and `clean`
removes build artifacts.

**Acceptance:**
- [x] Standard targets defined: `fetch`, `patch`, `build`, `install`, `clean`
- [x] Each target is documented with expected behavior
- [x] Makefile uses variables from Portfile (sourced or passed via environment)
- [x] `DESTDIR` or `PREFIX` variable controls install location (default `/usr/local`)

---

## Track B — `port` Command Implementation

Write the `port` command that drives the ports lifecycle.

### B.1 — Create the `port` shell script skeleton

**File:** `ports/port.sh`
**Symbol:** `port` (command entry point)
**Why it matters:** The `port` command is the user-facing interface to the
entire ports system. Starting with a shell script (rather than C) keeps it
simple and leverages the ion shell's scripting capabilities. The skeleton
handles argument parsing and dispatches to subcommands.

**Acceptance:**
- [x] `port.sh` script exists with subcommand dispatch (`install`, `remove`, `list`, `info`, `clean`)
- [x] `port` with no arguments prints usage help
- [x] `port` with an unknown subcommand prints an error
- [x] Script is executable and uses `#!/bin/sh` shebang

### B.2 — Implement `port list` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_list`
**Why it matters:** `port list` scans `/usr/ports/` and displays all available
ports. This is the simplest subcommand and validates that the ports tree is
correctly laid out and accessible from the running OS.

**Acceptance:**
- [x] `port list` enumerates all ports under `/usr/ports/` by scanning `category/program/Portfile`
- [x] Output shows port name, version, and category in a tabular format
- [x] Works with the `find` utility from Phase 41 coreutils

### B.3 — Implement `port info <name>` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_info`
**Why it matters:** `port info` reads a port's Portfile and displays its
metadata. This validates Portfile parsing — sourcing the shell variables and
printing them in a human-readable format.

**Acceptance:**
- [x] `port info lua` sources the Portfile and prints NAME, VERSION, DESCRIPTION, CATEGORY, DEPS
- [x] Reports an error if the port name is not found
- [x] Searches all categories for the given port name

### B.4 — Implement `port install <name>` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_install`
**Why it matters:** This is the core subcommand. It locates the port directory,
sources the Portfile, then runs `make fetch`, `make patch`, `make build`, and
`make install` in sequence. Any failure aborts the install and reports which
step failed.

**Acceptance:**
- [x] `port install lua` runs the full fetch → patch → build → install lifecycle
- [x] Each make target is run in the port's directory
- [x] Failure at any stage aborts with a clear error message
- [x] Skips already-installed ports (checks `/var/db/ports/installed`)

### B.5 — Implement `port remove <name>` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_remove`
**Why it matters:** Removal is essential for iterating on ports during
development. The `port` command reads the install manifest for a package and
deletes the installed files, then removes the tracking entry.

**Acceptance:**
- [x] `port remove lua` deletes files listed in the package's install manifest
- [x] Removes the entry from `/var/db/ports/installed`
- [x] Reports an error if the port is not currently installed
- [x] Warns if other installed ports depend on the port being removed

### B.6 — Implement `port clean <name>` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_clean`
**Why it matters:** Build artifacts in the port's `work/` directory consume
disk space. `port clean` runs `make clean` in the port directory and removes
the work directory, reclaiming space after a successful install.

**Acceptance:**
- [x] `port clean lua` runs `make clean` in the port directory
- [x] Removes the `work/` build directory if it exists
- [x] `port clean` with no argument cleans all ports

---

## Track C — xtask Integration for Ports Tree Packaging

Package the host-side ports tree and bundled source tarballs into the m3OS
disk image so they are available at `/usr/ports/` at runtime.

### C.1 — Add `populate_ports_tree()` function to xtask

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ports_tree`
**Why it matters:** The ports tree and bundled source archives must be placed
into the ext2 data partition during `cargo xtask image`. This follows the same
pattern as `populate_tcc_files()` (Phase 31) — recursively mirroring a host
directory tree into the ext2 image via `debugfs` commands.

**Acceptance:**
- [x] `populate_ports_tree()` mirrors `ports/` into `/usr/ports/` on the ext2 partition
- [x] Portfiles, Makefiles, patches, and bundled source tarballs are all included
- [x] Called from the image build pipeline alongside `populate_tcc_files()`
- [x] `/usr/ports/` is browsable from the m3OS shell after boot

### C.2 — Install the `port` command into the image

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ports_tree` (port script installation)
**Why it matters:** The `port` shell script must be installed at `/usr/bin/port`
with execute permissions so users can invoke it directly from the shell.

**Acceptance:**
- [x] `port.sh` is copied to `/usr/bin/port` in the ext2 image
- [x] File has execute permission set via `debugfs sif` command
- [x] `port list` works from the m3OS shell after boot

### C.3 — Create `/usr/local/` and `/var/db/ports/` directories

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ports_tree` (directory creation)
**Why it matters:** Ports install to `/usr/local/bin` and `/usr/local/lib`, and
the package database lives at `/var/db/ports/installed`. These directories must
exist in the image before any port is installed.

**Acceptance:**
- [x] `/usr/local/bin`, `/usr/local/lib`, `/usr/local/include` directories created in ext2
- [x] `/var/db/ports/` directory created in ext2
- [x] Directories are writable by root

---

## Track D — Initial Ports: Lua and zlib

Port Lua and zlib as the first two test cases, validating the entire pipeline.

### D.1 — Bundle Lua source and create the Lua port

**Files:**
- `ports/lang/lua/Portfile`
- `ports/lang/lua/Makefile`
- `ports/lang/lua/patches/`

**Symbol:** `lua` (port)
**Why it matters:** Lua is the ideal first port: small (~30 KLOC C), no
external dependencies, and highly portable. If `port install lua` works end
to end — from source extraction through TCC compilation to a working `lua`
binary — the ports system is validated.

**Acceptance:**
- [x] Lua 5.4.x source downloaded at build time and bundled in the image
- [x] Portfile has correct NAME, VERSION, DESCRIPTION, DEPS (none)
- [x] `make fetch` copies source to a `work/` directory
- [x] `make patch` applies any m3OS-specific patches (e.g., disable `os.execute` if needed)
- [x] `make build` compiles Lua with TCC (`tcc -o lua *.c`)
- [x] `make install` copies `lua` binary to `/usr/local/bin/`
- [x] `lua -e "print('hello from m3os')"` runs successfully inside m3OS

### D.2 — Bundle zlib source and create the zlib port

**Files:**
- `ports/lib/zlib/Portfile`
- `ports/lib/zlib/Makefile`
- `ports/lib/zlib/patches/`

**Symbol:** `zlib` (port)
**Why it matters:** zlib is the canonical library dependency — many other ports
will need it. Porting zlib validates that the system can build shared/static
libraries (not just executables) and install headers to `/usr/local/include`.

**Acceptance:**
- [x] zlib source downloaded at build time and bundled in the image
- [x] Portfile has correct metadata with DEPS empty
- [x] `make build` compiles zlib as a static library (`libz.a`) with TCC
- [x] `make install` copies `libz.a` to `/usr/local/lib/` and headers to `/usr/local/include/`
- [x] The installed library can be linked by other ports

### D.3 — Verify `port install lua` end-to-end inside m3OS

**Files:**
- `ports/port.sh`
- `ports/lang/lua/Makefile`

**Symbol:** `port install lua` (integration test)
**Why it matters:** The end-to-end test confirms that every piece works
together: the `port` command finds the port, sources the Portfile, runs make
targets with TCC, and produces a working binary. This is the primary
acceptance criterion for the entire phase.

**Acceptance:**
- [x] `port install lua` completes without errors from the m3OS shell
- [x] `lua -e "print('hello from m3os')"` outputs `hello from m3os`
- [x] `/var/db/ports/installed` contains an entry for lua
- [x] `port list` shows lua as available; `port info lua` shows its metadata

---

## Track E — Remaining Ports Collection

Port additional programs to demonstrate breadth and exercise dependency resolution.

### E.1 — Create bc port (math/bc)

**Files:**
- `ports/math/bc/Portfile`
- `ports/math/bc/Makefile`
- `ports/math/bc/src/bc.c`

**Symbol:** `bc` (port)
**Why it matters:** A calculator utility useful for scripting. Simple C
codebase with no external dependencies — validates another independent port
build.

**Acceptance:**
- [x] bc source bundled and Portfile created
- [x] `port install bc` builds and installs bc
- [x] `echo "2 + 3" | bc` outputs `5`

### E.2 — Create sbase port (core/sbase)

**Files:**
- `ports/core/sbase/Portfile`
- `ports/core/sbase/Makefile`
- `ports/core/sbase/src/`

**Symbol:** `sbase` (port)
**Why it matters:** sbase is the suckless coreutils — a complete set of Unix
tools in minimal C. Porting sbase validates building a multi-binary project
and installing many executables from one port.

**Acceptance:**
- [x] sbase source bundled and Portfile created
- [x] `port install sbase` builds and installs at least 10 sbase utilities
- [x] Installed sbase tools run correctly (e.g., `basename`, `seq`, `rev`)

### E.3 — Create mandoc port (doc/mandoc)

**Files:**
- `ports/doc/mandoc/Portfile`
- `ports/doc/mandoc/Makefile`
- `ports/doc/mandoc/src/mandoc.c`

**Symbol:** `mandoc` (port)
**Why it matters:** A man page viewer enables reading documentation inside
m3OS. This mandoc is a simplified man page formatter that handles basic
roff macros for terminal display.

**Acceptance:**
- [x] mandoc source bundled and Portfile created
- [x] `port install mandoc` builds and installs mandoc
- [x] `mandoc` can format and display a sample man page

### E.4 — Create a port that depends on zlib

**Files:**
- `ports/util/minizip/Portfile`
- `ports/util/minizip/Makefile`
- `ports/util/minizip/src/minizip.c`

**Symbol:** `minizip` (port)
**Why it matters:** This port's primary purpose is to validate dependency
resolution. Its Portfile declares `DEPS=zlib`, and `port install` must
automatically build and install zlib first if it is not already present.

**Acceptance:**
- [x] Port's Portfile lists `DEPS=zlib`
- [x] `port install minizip` installs zlib first if not already installed
- [x] The port links against `/usr/local/lib/libz.a` successfully
- [x] Build succeeds only after zlib is available

---

## Track F — Dependency Resolution and Package Tracking

Enhance the `port` command with dependency resolution and install tracking.

### F.1 — Implement dependency resolution in the `port` command

**File:** `ports/port.sh`
**Symbol:** `resolve_deps`
**Why it matters:** Real ports have dependencies. The `port` command must read
a port's `DEPS` field, check which dependencies are not yet installed, and
recursively install them before the requested port. Without this, users must
manually install dependencies in the correct order.

**Acceptance:**
- [x] `port install` reads `DEPS` from the Portfile
- [x] Missing dependencies are installed automatically before the main port
- [x] Already-installed dependencies are skipped
- [x] Circular dependency detection prevents infinite loops (error message)

### F.2 — Implement installed-package tracking

**File:** `ports/port.sh`
**Symbol:** `track_install`, `is_installed`
**Why it matters:** The installed-package database at `/var/db/ports/installed`
records which ports are installed, their versions, and which files they own.
This enables `port remove`, prevents duplicate installs, and lets dependency
resolution check what is already present.

**Acceptance:**
- [x] Successful `port install` writes an entry to `/var/db/ports/installed`
- [x] Entry includes port name, version, install date, and file manifest
- [x] `port remove` reads the manifest to know which files to delete
- [x] `is_installed` check is used by both `install` (skip) and `resolve_deps`

### F.3 — Implement install manifest generation

**File:** `ports/port.sh`
**Symbol:** `generate_manifest`
**Why it matters:** During `make install`, the files copied to `/usr/local/`
must be recorded so `port remove` can cleanly delete them. The manifest is
generated by comparing the filesystem before and after install.

**Acceptance:**
- [x] Each installed port has a file manifest at `/var/db/ports/<name>.manifest`
- [x] Manifest lists every file installed by the port (one path per line)
- [x] `port remove` deletes exactly the files in the manifest
- [x] Empty directories left after removal are cleaned up

---

## Track G — Integration Testing and Documentation

Validate the complete ports system and update project documentation.

### G.1 — End-to-end test: install and verify at least 5 ports

**Files:**
- `ports/lang/lua/Portfile`
- `ports/lib/zlib/Portfile`
- `ports/math/bc/Portfile`
- `ports/core/sbase/Portfile`
- `ports/doc/mandoc/Portfile`

**Symbol:** (integration test)
**Why it matters:** The acceptance criteria require at least 5 ports building
and installing successfully. This task runs through the full lifecycle for
each port and verifies correct behavior.

**Acceptance:**
- [x] `port install lua` succeeds; `lua -e "print('hello')"` works
- [x] `port install zlib` succeeds; `libz.a` is installed
- [x] `port install bc` succeeds; `echo "2+3" | bc` outputs `5`
- [x] `port install sbase` succeeds; sbase tools run
- [x] `port install mandoc` succeeds; mandoc renders a man page
- [x] Dependency resolution works for the zlib-dependent port

### G.2 — Verify `port remove` and `port clean`

**Files:**
- `ports/port.sh`

**Symbol:** `cmd_remove`, `cmd_clean`
**Why it matters:** Package removal and cleanup must work cleanly to avoid
leaving orphaned files or corrupted state in the package database.

**Acceptance:**
- [x] `port remove lua` removes the lua binary and manifest entry
- [x] `lua` command no longer found after removal
- [x] `port clean lua` removes build artifacts from the work directory
- [x] Re-installing after removal works correctly

### G.3 — Verify no regressions in existing tests

**Files:**
- `kernel/tests/*.rs`
- `xtask/src/main.rs`

**Symbol:** (all existing tests)
**Why it matters:** Adding the ports tree to the disk image and any new
directories or xtask code must not break existing functionality.

**Acceptance:**
- [x] `cargo xtask check` passes (clippy + fmt)
- [x] `cargo xtask test` passes (all existing QEMU tests)
- [x] `cargo test -p kernel-core` passes (host-side unit tests)

### G.4 — Update documentation and roadmap

**Files:**
- `docs/roadmap/45-ports-system.md`
- `docs/roadmap/README.md`
- `AGENTS.md`

**Symbol:** (documentation)
**Why it matters:** The design doc needs final implementation details, the
roadmap README needs the task list link, and AGENTS.md needs references to
the ports system and any new documentation.

**Acceptance:**
- [x] Design doc updated with Status set to `Complete` and Companion Task List linked
- [x] Roadmap README row updated with task list link and status
- [x] AGENTS.md updated with ports tree references and documentation pointers
- [x] A "How to create a new port" section exists in the design doc or a companion doc

---

## Documentation Notes

- Phase 45 introduces a BSD-style ports system modeled after FreeBSD/OpenBSD
  ports. The implementation is much simpler: no binary packages, no signing,
  no version conflict resolution.
- The `port` command is a shell script (not C) to keep it simple and
  self-documenting. It leverages the ion/sh shell scripting capabilities
  from Phase 14.
- All port sources are bundled in the disk image (no network fetching).
  Lua and zlib sources are downloaded at build time by xtask; other ports
  ship self-contained C implementations. Network fetching is deferred until
  an HTTP client is ported.
- Ports build with TCC (Phase 31) and make (Phase 32). The `patch`, `find`,
  and `xargs` utilities from Phase 41 coreutils are used in the build
  infrastructure.
- The xtask integration follows the same pattern as `populate_tcc_files()`
  from Phase 31 — recursively mirroring a host directory into the ext2
  partition via `debugfs`.
- Install location is `/usr/local/` (bin, lib, include) to keep port-installed
  software separate from the base system in `/usr/bin/` and `/usr/lib/`.
- Package tracking uses a simple flat-file database at `/var/db/ports/` rather
  than a binary format, keeping it inspectable and debuggable.
