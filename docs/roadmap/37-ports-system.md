# Phase 37 - Ports and Package System

## Milestone Goal

A simple "ports" system enables building and installing third-party software inside
the OS from source recipes. A port is a directory containing a `Makefile`, patches,
and a description file that automates downloading (or bundling), compiling, and
installing a piece of software.

## Learning Goals

- Understand how BSD ports and Gentoo portage work: source-based package management.
- Learn what "porting" software means: identifying dependencies, patching for
  compatibility, and verifying correct behavior on a new platform.
- See how a package system provides reproducible builds.

## Feature Scope

### Ports Tree Structure

```
/usr/ports/
  category/
    program/
      Portfile          # metadata: name, version, description, deps
      Makefile          # build rules: fetch, patch, build, install
      patches/          # any source patches needed for m3os
        01-fix-syscall.patch
```

### Port Lifecycle

```
port fetch    → download/extract source (or use bundled tarball)
port patch    → apply m3os-specific patches
port build    → compile with tcc/make
port install  → copy binaries to /usr/local/bin, libs to /usr/local/lib
port clean    → remove build artifacts
```

### Package Manager (`port` command)

A shell script or small C program that:
- Reads `Portfile` for metadata and dependencies.
- Resolves dependencies and builds them first.
- Executes the Makefile targets in order.
- Tracks installed packages in `/var/db/ports/installed`.
- Supports `port list`, `port info`, `port install`, `port remove`.

### Initial Ports Collection

Port these programs to demonstrate the system:

| Port | Category | Why |
|---|---|---|
| `lua` | lang | Lightweight scripting language, simple to port |
| `nvi` | editors | Traditional vi editor, richer than kilo |
| `bc` | math | Calculator, useful for scripting |
| `curl` (or `wget-lite`) | net | HTTP client for fetching files |
| `zlib` | lib | Compression library, dependency for many tools |
| `sbase` (full) | core | Complete suckless coreutils |
| `mandoc` | doc | Man page viewer |
| `tmux` (stretch) | system | Terminal multiplexer (needs PTY, curses) |

### Network Fetching (if available)

If the OS has working TCP sockets (Phase 23) and a simple HTTP client, ports can
fetch source tarballs from the network. Otherwise, all port sources are bundled
in the disk image.

Initially, bundle all sources in the disk image. Add network fetching as a stretch goal
once an HTTP client is ported.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 31 (Compiler) | TCC to compile port sources |
| Phase 32 (Build Tools) | make for building ports |
| Phase 33 (Coreutils) | patch, find, xargs for port infrastructure |
| Phase 24 (Persistent Storage) | Install to persistent storage |

## Implementation Outline

1. Design the `Portfile` format (keep it simple: shell variables).
2. Write the `port` script/program.
3. Create the ports tree directory structure in the disk image.
4. Port Lua as the first test case (small, portable, no dependencies).
5. Verify: `port install lua` builds and installs Lua, `lua -e "print('hello')"` works.
6. Port remaining initial packages, adding patches as needed.
7. Implement dependency resolution in the `port` command.
8. Track installed packages.
9. Document how to create a new port.

## Acceptance Criteria

- `port install lua` builds Lua from source inside the OS and installs it.
- `lua -e "print('hello from m3os')"` runs successfully.
- `port list` shows available ports.
- `port info lua` shows Lua's version and description.
- `port remove lua` uninstalls the package.
- At least 5 ports build and install successfully.
- Dependency resolution works: installing a port that depends on `zlib` builds `zlib` first.

## Companion Task List

- [Phase 37 Task List](./tasks/37-ports-system-tasks.md)

## How Real OS Implementations Differ

Real package systems are far more sophisticated:
- Binary package distribution (apt, pacman, pkg) — precompiled packages avoid build times
- Cryptographic signing for package integrity
- Version constraints and conflict resolution
- Automatic security updates
- Mirror networks for distribution
- Build farms for cross-compilation

BSD ports (FreeBSD, OpenBSD) are the closest model to what we implement:
source-based, Makefile-driven, with patches. Our version is much simpler: no
version conflict resolution, no binary packages, no signing.

## Deferred Until Later

- Binary package format (precompiled packages)
- Package signing and verification
- Version conflict resolution
- Automatic updates
- Mirror/repository support
- Cross-compilation of ports on the host
