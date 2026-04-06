# Phase 45 — Ports and Package System: Task List

**Status:** Planned
**Source Ref:** phase-45
**Depends on:** Phase 31 (Compiler) ✅, Phase 32 (Build Tools) ✅, Phase 41 (Coreutils) ✅, Phase 24 (Persistent Storage) ✅
**Goal:** A simple BSD-style ports system that builds and installs third-party
software from source recipes inside m3OS, with a `port` command for lifecycle
management, dependency resolution, and package tracking.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Portfile format and ports tree structure | — | Planned |
| B | `port` command implementation | A | Planned |
| C | xtask integration for ports tree packaging | A | Planned |
| D | Initial ports: Lua and zlib | A, B, C | Planned |
| E | Remaining ports collection | D | Planned |
| F | Dependency resolution and package tracking | B, D | Planned |
| G | Integration testing and documentation | A–F | Planned |

---

## Track A — Portfile Format and Ports Tree Structure

Define the metadata format and directory layout that every port follows.

### A.1 — Design the Portfile format

**File:** `ports/lua/Portfile`
**Symbol:** `Portfile` (format specification)
**Why it matters:** The Portfile is the single source of truth for a port's
identity, version, dependencies, and description. It uses shell variable syntax
so the `port` command can source it directly. Getting this right first avoids
rework across every port later.

**Acceptance:**
- [ ] Portfile format documented with required fields: `NAME`, `VERSION`, `DESCRIPTION`, `CATEGORY`, `DEPS`
- [ ] Format uses shell variable assignments (e.g., `NAME=lua`, `VERSION=5.4.7`)
- [ ] At least one example Portfile exists (Lua)
- [ ] Optional fields defined: `URL`, `SHA256`, `MAINTAINER`

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
- [ ] `ports/` directory exists at the project root with `category/program/` hierarchy
- [ ] Each port directory contains at minimum `Portfile` and `Makefile`
- [ ] Optional `patches/` subdirectory for m3OS-specific source patches
- [ ] At least two categories (`lang/`, `lib/`) with placeholder ports

### A.3 — Design the Makefile target contract

**File:** `ports/lang/lua/Makefile`
**Symbol:** `fetch`, `patch`, `build`, `install`, `clean` (make targets)
**Why it matters:** Every port Makefile must implement the same set of targets
so the `port` command can drive any port generically. The contract defines what
each target does: `fetch` extracts bundled source, `patch` applies patches,
`build` compiles with TCC/make, `install` copies to `/usr/local/`, and `clean`
removes build artifacts.

**Acceptance:**
- [ ] Standard targets defined: `fetch`, `patch`, `build`, `install`, `clean`
- [ ] Each target is documented with expected behavior
- [ ] Makefile uses variables from Portfile (sourced or passed via environment)
- [ ] `DESTDIR` or `PREFIX` variable controls install location (default `/usr/local`)

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
- [ ] `port.sh` script exists with subcommand dispatch (`install`, `remove`, `list`, `info`, `clean`)
- [ ] `port` with no arguments prints usage help
- [ ] `port` with an unknown subcommand prints an error
- [ ] Script is executable and uses `#!/bin/sh` shebang

### B.2 — Implement `port list` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_list`
**Why it matters:** `port list` scans `/usr/ports/` and displays all available
ports. This is the simplest subcommand and validates that the ports tree is
correctly laid out and accessible from the running OS.

**Acceptance:**
- [ ] `port list` enumerates all ports under `/usr/ports/` by scanning `category/program/Portfile`
- [ ] Output shows port name, version, and category in a tabular format
- [ ] Works with the `find` utility from Phase 41 coreutils

### B.3 — Implement `port info <name>` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_info`
**Why it matters:** `port info` reads a port's Portfile and displays its
metadata. This validates Portfile parsing — sourcing the shell variables and
printing them in a human-readable format.

**Acceptance:**
- [ ] `port info lua` sources the Portfile and prints NAME, VERSION, DESCRIPTION, CATEGORY, DEPS
- [ ] Reports an error if the port name is not found
- [ ] Searches all categories for the given port name

### B.4 — Implement `port install <name>` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_install`
**Why it matters:** This is the core subcommand. It locates the port directory,
sources the Portfile, then runs `make fetch`, `make patch`, `make build`, and
`make install` in sequence. Any failure aborts the install and reports which
step failed.

**Acceptance:**
- [ ] `port install lua` runs the full fetch → patch → build → install lifecycle
- [ ] Each make target is run in the port's directory
- [ ] Failure at any stage aborts with a clear error message
- [ ] Skips already-installed ports (checks `/var/db/ports/installed`)

### B.5 — Implement `port remove <name>` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_remove`
**Why it matters:** Removal is essential for iterating on ports during
development. The `port` command reads the install manifest for a package and
deletes the installed files, then removes the tracking entry.

**Acceptance:**
- [ ] `port remove lua` deletes files listed in the package's install manifest
- [ ] Removes the entry from `/var/db/ports/installed`
- [ ] Reports an error if the port is not currently installed
- [ ] Warns if other installed ports depend on the port being removed

### B.6 — Implement `port clean <name>` subcommand

**File:** `ports/port.sh`
**Symbol:** `cmd_clean`
**Why it matters:** Build artifacts in the port's `work/` directory consume
disk space. `port clean` runs `make clean` in the port directory and removes
the work directory, reclaiming space after a successful install.

**Acceptance:**
- [ ] `port clean lua` runs `make clean` in the port directory
- [ ] Removes the `work/` build directory if it exists
- [ ] `port clean` with no argument cleans all ports

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
- [ ] `populate_ports_tree()` mirrors `ports/` into `/usr/ports/` on the ext2 partition
- [ ] Portfiles, Makefiles, patches, and bundled source tarballs are all included
- [ ] Called from the image build pipeline alongside `populate_tcc_files()`
- [ ] `/usr/ports/` is browsable from the m3OS shell after boot

### C.2 — Install the `port` command into the image

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ports_tree` (port script installation)
**Why it matters:** The `port` shell script must be installed at `/usr/bin/port`
(or `/usr/local/bin/port`) with execute permissions so users can invoke it
directly from the shell.

**Acceptance:**
- [ ] `port.sh` is copied to `/usr/bin/port` in the ext2 image
- [ ] File has execute permission set via `debugfs sif` command
- [ ] `port list` works from the m3OS shell after boot

### C.3 — Create `/usr/local/` and `/var/db/ports/` directories

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ports_tree` (directory creation)
**Why it matters:** Ports install to `/usr/local/bin` and `/usr/local/lib`, and
the package database lives at `/var/db/ports/installed`. These directories must
exist in the image before any port is installed.

**Acceptance:**
- [ ] `/usr/local/bin`, `/usr/local/lib`, `/usr/local/include` directories created in ext2
- [ ] `/var/db/ports/` directory created in ext2
- [ ] Directories are writable by root

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
- [ ] Lua 5.4.x source tarball bundled in `ports/lang/lua/`
- [ ] Portfile has correct NAME, VERSION, DESCRIPTION, DEPS (none)
- [ ] `make fetch` extracts source to a `work/` directory
- [ ] `make patch` applies any m3OS-specific patches (e.g., disable `os.execute` if needed)
- [ ] `make build` compiles Lua with TCC (`tcc -o lua *.c`)
- [ ] `make install` copies `lua` binary to `/usr/local/bin/`
- [ ] `lua -e "print('hello from m3os')"` runs successfully inside m3OS

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
- [ ] zlib source bundled in `ports/lib/zlib/`
- [ ] Portfile has correct metadata with DEPS empty
- [ ] `make build` compiles zlib as a static library (`libz.a`) with TCC
- [ ] `make install` copies `libz.a` to `/usr/local/lib/` and headers to `/usr/local/include/`
- [ ] The installed library can be linked by other ports

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
- [ ] `port install lua` completes without errors from the m3OS shell
- [ ] `lua -e "print('hello from m3os')"` outputs `hello from m3os`
- [ ] `/var/db/ports/installed` contains an entry for lua
- [ ] `port list` shows lua as available; `port info lua` shows its metadata

---

## Track E — Remaining Ports Collection

Port additional programs to demonstrate breadth and exercise dependency resolution.

### E.1 — Create bc port (math/bc)

**Files:**
- `ports/math/bc/Portfile`
- `ports/math/bc/Makefile`

**Symbol:** `bc` (port)
**Why it matters:** A calculator utility useful for scripting. Simple C
codebase with no external dependencies — validates another independent port
build.

**Acceptance:**
- [ ] bc source bundled and Portfile created
- [ ] `port install bc` builds and installs bc
- [ ] `echo "2 + 3" | bc` outputs `5`

### E.2 — Create sbase port (core/sbase)

**Files:**
- `ports/core/sbase/Portfile`
- `ports/core/sbase/Makefile`

**Symbol:** `sbase` (port)
**Why it matters:** sbase is the suckless coreutils — a complete set of Unix
tools in minimal C. Porting the full sbase validates building a multi-binary
project and installing many executables from one port.

**Acceptance:**
- [ ] sbase source bundled and Portfile created
- [ ] `port install sbase` builds and installs at least 10 sbase utilities
- [ ] Installed sbase tools run correctly (e.g., `sbase-cat`, `sbase-ls`)

### E.3 — Create mandoc port (doc/mandoc)

**Files:**
- `ports/doc/mandoc/Portfile`
- `ports/doc/mandoc/Makefile`

**Symbol:** `mandoc` (port)
**Why it matters:** A man page viewer enables reading documentation inside
m3OS. mandoc is a self-contained BSD man page compiler/viewer that can format
and display man pages from the terminal.

**Acceptance:**
- [ ] mandoc source bundled and Portfile created
- [ ] `port install mandoc` builds and installs mandoc
- [ ] `mandoc` can format and display a sample man page

### E.4 — Create a port that depends on zlib

**Files:**
- Port TBD (e.g., `ports/net/curl-lite/Portfile` or `ports/util/minizip/Portfile`)
- Corresponding `Makefile`

**Symbol:** (zlib-dependent port)
**Why it matters:** This port's primary purpose is to validate dependency
resolution. Its Portfile declares `DEPS=zlib`, and `port install` must
automatically build and install zlib first if it is not already present.

**Acceptance:**
- [ ] Port's Portfile lists `DEPS=zlib`
- [ ] `port install <name>` installs zlib first if not already installed
- [ ] The port links against `/usr/local/lib/libz.a` successfully
- [ ] Build succeeds only after zlib is available

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
- [ ] `port install` reads `DEPS` from the Portfile
- [ ] Missing dependencies are installed automatically before the main port
- [ ] Already-installed dependencies are skipped
- [ ] Circular dependency detection prevents infinite loops (error message)

### F.2 — Implement installed-package tracking

**File:** `ports/port.sh`
**Symbol:** `track_install`, `is_installed`
**Why it matters:** The installed-package database at `/var/db/ports/installed`
records which ports are installed, their versions, and which files they own.
This enables `port remove`, prevents duplicate installs, and lets dependency
resolution check what is already present.

**Acceptance:**
- [ ] Successful `port install` writes an entry to `/var/db/ports/installed`
- [ ] Entry includes port name, version, install date, and file manifest
- [ ] `port remove` reads the manifest to know which files to delete
- [ ] `is_installed` check is used by both `install` (skip) and `resolve_deps`

### F.3 — Implement install manifest generation

**File:** `ports/port.sh`
**Symbol:** `generate_manifest`
**Why it matters:** During `make install`, the files copied to `/usr/local/`
must be recorded so `port remove` can cleanly delete them. The manifest is
generated by comparing the filesystem before and after install, or by having
the Makefile's install target log each file it copies.

**Acceptance:**
- [ ] Each installed port has a file manifest at `/var/db/ports/<name>.manifest`
- [ ] Manifest lists every file installed by the port (one path per line)
- [ ] `port remove` deletes exactly the files in the manifest
- [ ] Empty directories left after removal are cleaned up

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
- [ ] `port install lua` succeeds; `lua -e "print('hello')"` works
- [ ] `port install zlib` succeeds; `libz.a` is installed
- [ ] `port install bc` succeeds; `echo "2+3" | bc` outputs `5`
- [ ] `port install sbase` succeeds; sbase tools run
- [ ] `port install mandoc` succeeds; mandoc renders a man page
- [ ] Dependency resolution works for the zlib-dependent port

### G.2 — Verify `port remove` and `port clean`

**Files:**
- `ports/port.sh`

**Symbol:** `cmd_remove`, `cmd_clean`
**Why it matters:** Package removal and cleanup must work cleanly to avoid
leaving orphaned files or corrupted state in the package database.

**Acceptance:**
- [ ] `port remove lua` removes the lua binary and manifest entry
- [ ] `lua` command no longer found after removal
- [ ] `port clean lua` removes build artifacts from the work directory
- [ ] Re-installing after removal works correctly

### G.3 — Verify no regressions in existing tests

**Files:**
- `kernel/tests/*.rs`
- `xtask/src/main.rs`

**Symbol:** (all existing tests)
**Why it matters:** Adding the ports tree to the disk image and any new
directories or xtask code must not break existing functionality.

**Acceptance:**
- [ ] `cargo xtask check` passes (clippy + fmt)
- [ ] `cargo xtask test` passes (all existing QEMU tests)
- [ ] `cargo test -p kernel-core` passes (host-side unit tests)

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
- [ ] Design doc updated with Status set to `Complete` and Companion Task List linked
- [ ] Roadmap README row updated with task list link and status
- [ ] AGENTS.md updated with ports tree references and documentation pointers
- [ ] A "How to create a new port" section exists in the design doc or a companion doc

---

## Documentation Notes

- Phase 45 introduces a BSD-style ports system modeled after FreeBSD/OpenBSD
  ports. The implementation is much simpler: no binary packages, no signing,
  no version conflict resolution.
- The `port` command is a shell script (not C) to keep it simple and
  self-documenting. It leverages the ion/sh shell scripting capabilities
  from Phase 14.
- All port sources are bundled in the disk image (no network fetching).
  Network fetching is deferred until an HTTP client is ported.
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
