# Phase 85c - Python (CPython)

**Status:** Implemented (kernel `0.85.2`; `python-smoke` gate green)
**Source Ref:** phase-85c
**Depends on:** Phase 85a (Package & Build-Cache Infrastructure), Phase 36 (Expanded Memory) ✅, Phase 45 (Ports System) ✅
**Builds on:** Adds a host-cross-built CPython interpreter + comprehensive non-networked standard library on top of the Phase 85a packaging substrate, using a two-stage (host-then-target) cross build.
**Primary Components:** `ports/lang/python/Portfile`, `xtask/src/port_build.rs` (`build_python`), CPython `configure` + `Modules/Setup`, the Phase 85a `.m3pkg` pipeline, `docs/python-roadmap.md`

## Milestone Goal

A `python3` interpreter runs inside m3OS for REPL use, script execution, and local automation, with a comprehensive non-networked standard library (`json`, `re`, `math`, `collections`, `itertools`, `functools`, `dataclasses`, `pathlib`, `datetime`, `argparse`, `csv`, `struct`, `base64`, `hashlib`, `enum`, `typing`, and the rest of the no-OS-dependency stdlib), installed from a Phase 85a `.m3pkg`.

## Why This Phase Exists

Python is the most broadly useful scripting runtime to bring up and a prerequisite for later tooling. Its cross build is the fiddly middle case: a two-stage build needing a same-version host interpreter (`--with-build-python`), `CONFIG_SITE` cache answers for the cross target, and careful stdlib + `lib-dynload` staging — a meaningful test of the 85a substrate beyond git's simpler layout.

## Learning Goals

- The CPython two-stage cross build (`--build` vs `--host`, `--with-build-python`, `CONFIG_SITE`, `_PYTHON_HOST_PLATFORM`).
- How `Modules/Setup`/`makesetup` decides static-vs-`lib-dynload` extensions and which C extension needs which external lib.
- How CPython finds its stdlib via the `sys.prefix` landmark search and what keeps an install relocatable.

## Feature Scope

### Area A — CPython two-stage cross build

Build a host CPython of the exact target version, then cross-configure: `--host=x86_64-linux-musl --build=$(gcc -dumpmachine) --with-build-python=../build-host/python --disable-shared --disable-ipv6 --without-ensurepip --without-pymalloc` with `ac_cv_file__dev_ptmx=no ac_cv_file__dev_ptc=no` (and any `CONFIG_SITE` cache answers needed). `--disable-test-modules` to drop the large `test` package.

### Area B — Comprehensive stdlib staging + validation

Stage `bin/python3` + `lib/pythonX.Y/` (the stdlib `.py`; all C extensions are builtin — see "Static interpreter" below, so there is no `lib-dynload`) in fixed relative layout; seal into a `.m3pkg`; `pkg install python`; validate REPL + scripts inside m3OS. Comprehensive scope = build every C extension whose dependency is already present — `zlib`/`gzip`/`zipfile` against `ports/lib/zlib`, `_curses`/`_curses_panel` against the ported wide `ports/lib/ncurses` (the same `libncursesw.a`/`libtinfow.a`/`libpanelw.a` archives less/htop/tmux link; `curses.ncurses_version` reports 6.5 inside m3OS), and `hashlib` via the HACL\* built-ins — explicitly excluding networking/TLS extensions (Phase 86) and `dlopen`-only extensions like `ctypes` (Phase 93). A module is deferred only when its external library is genuinely not yet ported (sqlite3, GNU readline, gdbm, Tk, libffi, libbz2, liblzma, libuuid) — never when the dependency is already in the tree.

## Important Components and How They Work

### `build_python` in `port_build.rs`

A new port `build_*` function: build the host interpreter, then the cross interpreter via the musl toolchain plumbing, DESTDIR-install the full prefix, and hand the staged tree to the 85a sealing step. Registered in the dispatch + `build_python_port()` entry point + bundled via `BUNDLE_ONLY_PORTS`.

### Static interpreter (the model that runs on m3OS)

The interpreter is **fully static**: `MODULE_BUILDTYPE=static` builds every stdlib C extension *into* `python3`, and `LDFLAGS=-static` embeds musl libc — no `PT_INTERP`, no `lib-dynload`, no `dlopen`. This is not the usual desktop CPython layout; it is forced by m3OS reality. m3OS's `/lib/ld-musl-x86_64.so.1` is a *custom Rust loader reimplementation* (`userspace/ld-musl-x86_64.so.1/`) and m3OS ships **no real musl `libc.so`** (the userland is `no_std` Rust). A dynamic CPython faults at startup the moment the loader hits the interpreter's `DT_NEEDED libc.so` — there is nothing to satisfy it, let alone the thousands of libc symbols a real C program needs. So Python is shipped static, exactly like the `git` port. (The dynamic build was implemented first and surfaced this in the first in-m3OS gate: `ldso: DT_NEEDED not found: libc.so`.) Lifting this — shipping a real musl `libc.so` + closing the syscall gaps a dynamic libc needs, then re-enabling a dynamic `python3` with real `lib-dynload` + `ctypes` — is [Phase 93 (Dynamic C Runtime)](./93-dynamic-c-runtime.md).

### Frozen stdlib in `python312.zip`

m3OS's ring-3 VFS is slow (`vfs_server: slow req … STAT_PATH elapsed_us=80000-200000` — 80-200 ms per path stat), so the ~1700 loose stdlib files made `pkg install python` and every cold `import` (a per-module `sys.path` stat storm) take minutes — the first static gate run timed out installing. `build_python` therefore byte-compiles the stdlib and freezes it into a single `lib/python312.zip` of `.pyc` (already on CPython's default `sys.path`; zipimport reads the archive directory once, no per-file stats). The package drops from ~1700 files to a few hundred, install + import become fast, and only the `os.py` getpath landmark is kept as a loose file so `sys.prefix` still resolves.

### Relocation contract

CPython derives `sys.prefix`/`sys.exec_prefix` by searching upward from the executable for the `os.py` landmark, so the package is relocatable as long as `bin/` and `lib/pythonX.Y/` stay in fixed relative layout — an 85a relocation-contract requirement.

## How This Builds on Earlier Phases

- Consumes the Phase 85a `.m3pkg` pipeline + offline installer + relocation contract.
- Reuses `ports/lib/zlib` for the `zlib`/`gzip` extensions; reuses the Phase 36 demand-paging/large-mmap baseline for the interpreter's working set.

## Implementation Outline

1. Add `ports/lang/python/Portfile` (pinned version + SHA-256) and `build_python`.
2. Build host interpreter; cross-configure + build; DESTDIR-install; exclude `test`.
3. Seal `.m3pkg`, bundle on disk, `pkg install python`.
4. Validate REPL + scripts; bump kernel to `0.85.2`.

## Acceptance Criteria

- CPython builds reproducibly via `cargo xtask port build python` and seals into a `.m3pkg`.
- Inside m3OS: `python3 -c "print('hello from m3OS')"` prints; `import json, re, math, datetime, argparse, hashlib, curses, curses.panel, threading` succeeds (the static `_curses`/`_curses_panel` link the ported ncurses — `curses.ncurses_version` reports 6.5; `threading` rides the `_thread` builtin); a bundled `/usr/src/fibonacci.py` runs; a file write+read round-trips; `sys.platform` reports the expected value (serial-validated gate).
- Python is installed via `pkg install python` from a bundled `.m3pkg`.
- Networking/TLS modules (`ssl`, DNS/`getaddrinfo` name resolution, `pip`, `asyncio`) remain absent (deferred to Phase 86); their absence is documented, not silently failing. (`_socket` itself may build against the existing TCP/UDP + AF_UNIX stack; what is deferred is name resolution and TLS, not the extension as a whole.)

## Companion Task List

- [Phase 85c Task List](./tasks/85c-python-tasks.md)

## How Real OS Implementations Differ

- Distributions ship the full stdlib including `ssl`/`sqlite3`/`ctypes` and `pip`; 85c is the non-networked core (no TLS, no package installer).
- Known musl cross hazards (SOABI `-musl`/`-gnu` triple selection, `lib-dynload` `.so` collisions) are upstream CPython bugs that may need small patches — tracked in the task list.

## Deferred Until Later

- **A dynamic interpreter** — a real musl `libc.so` + the syscall coverage a dynamic libc needs, then a dynamic `python3` with real `lib-dynload` `.so` extensions and `ctypes`/`dlopen` of arbitrary shared objects — is **[Phase 93 (Dynamic C Runtime)](./93-dynamic-c-runtime.md), now delivered**: a dynamic `python3` imports `lib-dynload/*.so` via the loader's `dlopen` and `ctypes.CDLL(...)` works. 85c ships fully static (every C extension builtin) because m3OS's Phase 76 loader had **no `libc.so`** to bind a dynamic C program against — the historical reason this port is static; Phase 93 lifted that by shipping the `libc.so` + the loader startup glue (TLS/TCB via `__init_tp`, the COPY-relocation-order fix, the ctor-queue patch). The static `python3` here stays the conservative fallback.
- `ssl`/`_hashlib`-OpenSSL, DNS/`getaddrinfo` name resolution, `http.client`/`urllib`, `pip`, `venv`, `asyncio` — Phase 86. (`hashlib` itself works via CPython's built-in HACL\*-backed `_md5`/`_sha*` modules with no OpenSSL.)
- `multiprocessing` (fork/exec + POSIX-semaphore IPC) — note `threading` itself is **not** deferred: the `_thread` builtin is on and pure-Python `threading` works single-process today.
- `ctypes`/cffi (needs `dlopen` at runtime → Phase 93), `sqlite3`, GNU `readline`, `tkinter`, `_bz2`/`_lzma`/`_uuid`/`_gdbm`, NumPy/SciPy — each deferred because its external library is not yet ported. (`curses` is **not** in this list: ncurses is already ported, so `_curses`/`_curses_panel` are built, per the Area B scope rule.)
