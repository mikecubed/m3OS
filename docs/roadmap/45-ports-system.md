# Phase 45 - Ports and Package System

**Status:** Complete
**Source Ref:** phase-45
**Depends on:** Phase 31 (Compiler) ✅, Phase 32 (Build Tools) ✅, Phase 41 (Coreutils) ✅, Phase 24 (Persistent Storage) ✅
**Builds on:** Uses TCC from Phase 31, make from Phase 32, and coreutils (find, patch, xargs) from Phase 41 to compile and install third-party software from source recipes
**Primary Components:** ports tree, port command, xtask ports integration

## Milestone Goal

A simple BSD-style ports system enables building and installing third-party software
inside the OS from source recipes. A port is a directory containing a Makefile, patches,
and a Portfile that automates extracting, compiling, and installing a piece of software.
At least 5 ports build and install successfully, with dependency resolution.

## Why This Phase Exists

With a working compiler (TCC), build tools (make), and persistent storage, the OS
can compile C programs from source. But users need a structured way to discover,
build, install, track, and remove third-party software. Without a package system,
each program requires manual compilation steps, dependency tracking, and cleanup.
The ports system provides a reproducible, scriptable, and inspectable workflow for
all of this.

## Learning Goals

- Understand how BSD ports and Gentoo portage work: source-based package management.
- Learn what "porting" software means: identifying dependencies, patching for
  compatibility, and verifying correct behavior on a new platform.
- See how a package system provides reproducible builds.
- Understand install manifests and package databases for tracking installed software.

## Feature Scope

### Ports Tree Structure

```
/usr/ports/
  category/
    program/
      Portfile          # metadata: name, version, description, deps
      Makefile          # build rules: fetch, patch, build, install
      src/              # bundled source code
      patches/          # any source patches needed for m3os
        01-fix-syscall.patch
```

### Port Lifecycle

The `port install <name>` command drives the full lifecycle internally by
running make targets in sequence:

```
make fetch    → copy bundled source to work directory
make patch    → apply m3os-specific patches
make build    → compile with tcc/make
make install  → copy binaries to /usr/local/bin, libs to /usr/local/lib
make clean    → remove build artifacts (via `port clean`)
```

### Package Manager (`port` command)

A shell script installed at `/usr/bin/port` that:
- Reads `Portfile` for metadata and dependencies.
- Resolves dependencies and builds them first.
- Executes the Makefile targets in order.
- Tracks installed packages in `/var/db/ports/installed`.
- Generates file manifests for clean removal.
- Supports `port list`, `port info`, `port install`, `port remove`, `port clean`.

### Initial Ports Collection

| Port | Category | Why |
|---|---|---|
| `lua` | lang | Lightweight scripting language, simple to port |
| `zlib` | lib | Compression library, dependency for many tools |
| `bc` | math | Calculator, useful for scripting |
| `sbase` | core | Suckless Unix tools (basename, dirname, seq, etc.) |
| `mandoc` | doc | Man page viewer |
| `minizip` | util | Zlib-dependent utility for dependency validation |

### Source Bundling

All port sources are bundled in the disk image. Lua and zlib sources are downloaded
at build time by xtask and staged into the ext2 image. Other ports (bc, sbase,
mandoc, minizip) ship self-contained C implementations checked into the repository.
Network fetching is deferred until an HTTP client is ported.

## Important Components and How They Work

### Portfile Format

Each port's `Portfile` uses shell variable syntax so the `port` command can source
it directly:

```sh
NAME=lua
VERSION=5.4.7
DESCRIPTION="Lightweight scripting language"
CATEGORY=lang
DEPS=
```

Required fields: `NAME`, `VERSION`, `DESCRIPTION`, `CATEGORY`, `DEPS`.
Optional fields: `URL`, `SHA256`, `MAINTAINER`.

### Makefile Target Contract

Every port Makefile implements five standard targets:

| Target | Behavior |
|---|---|
| `fetch` | Copy bundled source from `src/` to `work/` |
| `patch` | Apply patches from `patches/*.patch` |
| `build` | Compile with TCC |
| `install` | Copy outputs to `$(PREFIX)` (default `/usr/local`) |
| `clean` | Remove `work/` build directory |

### Package Database

Installed packages are tracked in flat files at `/var/db/ports/`:
- `installed` — one line per package: `name version date`
- `<name>.manifest` — one file path per line, listing every installed file

### xtask Integration

The `populate_ports_tree()` function in xtask mirrors the host-side `ports/`
directory into `/usr/ports/` on the ext2 partition via debugfs, following the
same pattern as `populate_tcc_files()` from Phase 31. It also installs the
`port` command at `/usr/bin/port` and creates `/usr/local/` and `/var/db/ports/`
directories. Source for Lua and zlib is fetched at build time and staged alongside.

## How This Builds on Earlier Phases

- Uses TCC from Phase 31 as the C compiler for building port source code.
- Uses make (pdpmake) from Phase 32 to drive the fetch/patch/build/install lifecycle.
- Uses find, patch, and xargs from Phase 41 coreutils in port scripts and Makefiles.
- Installs to the persistent ext2 filesystem from Phase 24.
- The `port` shell script runs in the ion shell from Phase 14/21.

## Implementation Outline

1. Design the Portfile format (shell variables) and Makefile target contract.
2. Create the `ports/` directory structure with category/program/ hierarchy.
3. Write the `port.sh` shell script with list/info/install/remove/clean subcommands.
4. Add `populate_ports_tree()` to xtask for mirroring ports into the ext2 image.
5. Fetch Lua and zlib sources at build time; bundle other port sources in-repo.
6. Create Portfiles and Makefiles for all initial ports.
7. Implement dependency resolution in the `port` command.
8. Implement install manifest generation and package tracking.
9. Verify end-to-end: `port install lua` produces a working Lua binary.

## Acceptance Criteria

- `port install lua` builds Lua from source inside the OS and installs it.
- `lua -e "print('hello from m3os')"` runs successfully.
- `port list` shows available ports.
- `port info lua` shows Lua's version and description.
- `port remove lua` uninstalls the package.
- At least 5 ports build and install successfully.
- Dependency resolution works: installing minizip builds zlib first.

## Companion Task List

- [Phase 45 Task List](./tasks/45-ports-system-tasks.md)

## How Real OS Implementations Differ

- Binary package distribution (apt, pacman, pkg) — precompiled packages avoid build times.
- Cryptographic signing for package integrity.
- Version constraints and conflict resolution.
- Automatic security updates.
- Mirror networks for distribution.
- Build farms for cross-compilation.
- BSD ports (FreeBSD, OpenBSD) are the closest model: source-based, Makefile-driven,
  with patches. Our version is much simpler: no version conflict resolution, no binary
  packages, no signing.

## How to Create a New Port

1. Choose a category (lang, lib, math, core, doc, util, net, etc.) and create the
   directory: `ports/<category>/<name>/`

2. Create a `Portfile` with metadata:
   ```
   NAME=myport
   VERSION=1.0
   DESCRIPTION="Short description"
   CATEGORY=<category>
   DEPS=                    # space-separated dependency names, or empty
   MAINTAINER=m3os
   ```

3. Place source code in `ports/<category>/<name>/src/`. For small programs, commit
   the source directly. For larger programs, add download logic to xtask's
   `fetch_port_sources()` function.

4. Create a `Makefile` with the five standard targets:
   ```makefile
   PORTDIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
   PREFIX ?= /usr/local
   WORKDIR := $(PORTDIR)/work

   .PHONY: fetch patch build install clean

   fetch:
   	mkdir -p $(WORKDIR)
   	cp -r $(PORTDIR)/src/* $(WORKDIR)/

   patch:
   	@if [ -d "$(PORTDIR)/patches" ]; then \
   		for p in $(PORTDIR)/patches/*.patch; do \
   			[ -f "$$p" ] && patch -d $(WORKDIR) -p1 < "$$p"; \
   		done; \
   	fi

   build:
   	cd $(WORKDIR) && tcc -o myport myport.c

   install:
   	mkdir -p $(PREFIX)/bin
   	cp $(WORKDIR)/myport $(PREFIX)/bin/myport

   clean:
   	rm -rf $(WORKDIR)
   ```

5. Optionally create a `patches/` directory for m3OS-specific source patches.

6. Rebuild the disk image with `cargo xtask image` (delete the existing disk.img first
   to regenerate it with the new port).

7. Test inside m3OS: `port install myport`

## Deferred Until Later

- Binary package format (precompiled packages)
- Package signing and verification
- Version conflict resolution
- Automatic updates
- Mirror/repository support
- Network fetching of source tarballs
- Cross-compilation of ports on the host
