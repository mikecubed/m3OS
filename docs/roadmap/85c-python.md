# Phase 85c - Python (CPython)

**Status:** Planned
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

Stage `bin/python3` + `lib/pythonX.Y/` (the stdlib `.py` + `lib-dynload/*.so`) in fixed relative layout; seal into a `.m3pkg`; `pkg install python`; validate REPL + scripts inside m3OS. Comprehensive scope = build every C extension whose dependency is already present (zlib via `ports/lib/zlib`; `hashlib` built-ins), explicitly excluding networking/TLS extensions (Phase 86).

## Important Components and How They Work

### `build_python` in `port_build.rs`

A new port `build_*` function: build the host interpreter, then the cross interpreter via the musl toolchain plumbing, DESTDIR-install the full prefix, and hand the staged tree to the 85a sealing step. Registered in `PORTS` + dispatch.

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
- Inside m3OS: `python3 -c "print('hello from m3OS')"` prints; `import json, re, math, datetime, argparse, hashlib` succeeds; a bundled `/usr/src/fibonacci.py` runs; a file write+read round-trips; `sys.platform` reports the expected value (serial-validated gate).
- Python is installed via `pkg install python` from a bundled `.m3pkg`.
- Networking/TLS modules (`ssl`, DNS/`getaddrinfo` name resolution, `pip`, `asyncio`) remain absent (deferred to Phase 86); their absence is documented, not silently failing. (`_socket` itself may build against the existing TCP/UDP + AF_UNIX stack; what is deferred is name resolution and TLS, not the extension as a whole.)

## Companion Task List

- [Phase 85c Task List](./tasks/85c-python-tasks.md)

## How Real OS Implementations Differ

- Distributions ship the full stdlib including `ssl`/`sqlite3`/`ctypes` and `pip`; 85c is the non-networked core (no TLS, no package installer).
- Known musl cross hazards (SOABI `-musl`/`-gnu` triple selection, `lib-dynload` `.so` collisions) are upstream CPython bugs that may need small patches — tracked in the task list.

## Deferred Until Later

- `ssl`/`_hashlib`-OpenSSL, DNS/`getaddrinfo` name resolution, `http.client`/`urllib`, `pip`, `venv`, `asyncio` — Phase 86. (`hashlib` itself works via CPython's built-in HACL\*-backed `_md5`/`_sha*` modules with no OpenSSL.)
- `threading`/`multiprocessing`, `ctypes`/cffi (needs dlopen at runtime), `sqlite3`, `readline`/`curses`, tkinter, NumPy/SciPy.
