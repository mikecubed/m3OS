# Phase 85c — Python (CPython): Task List

**Status:** In Progress (feat/phase-85c-python)
**Source Ref:** phase-85c
**Depends on:** Phase 85a (Package & Build-Cache Infrastructure), Phase 36 (Expanded Memory) ✅, Phase 45 (Ports System) ✅
**Goal:** Two-stage cross-build a CPython interpreter + comprehensive non-networked standard library, package it via the Phase 85a `.m3pkg` substrate, install it with `pkg install python`, and validate REPL + script workloads inside m3OS.

> **Implementation underway on `feat/phase-85c-python`.** Acceptance items are checked `[x]` as each is implemented and validated. Builds on the 85a substrate.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Host interpreter + cross `configure` | 85a | Planned |
| B | stdlib + `lib-dynload` staging + relocation | A | Planned |
| C | Packaging + install + validation gate + version bump | B, 85a | Planned |

---

## Track A — Two-stage cross build

### A.1 — Build the host interpreter + add the python Portfile

**Files:**
- `ports/lang/python/Portfile` (new — pinned CPython version + SHA-256)
- `xtask/src/port_build.rs` (new `build_python`, registered in `PORTS` + dispatch)

**Symbol:** `build_python` (stage 1: host build)
**Why it matters:** a cross build of CPython needs a build-platform interpreter of the exact same version (`--with-build-python`); without it the cross configure cannot run target-version bytecode.

**Acceptance:**
- [ ] `build_python` first builds a host CPython of the target version under `target/port-build/python/build-host/`.
- [ ] The Portfile pins the CPython version + SHA-256 and declares `DEPS=zlib`.

### A.2 — Cross-configure for the musl target

**File:** `xtask/src/port_build.rs` (`build_python` stage 2)
**Symbol:** the CPython `configure` invocation
**Why it matters:** the cross flags + `CONFIG_SITE` cache answers are the error-prone heart of a CPython cross build.

**Acceptance:**
- [ ] CPython cross-configures with `--host=x86_64-linux-musl --build=$(host triple) --with-build-python=<host python> --disable-shared --disable-ipv6 --without-ensurepip --without-pymalloc` and the `ac_cv_file__dev_ptmx=no ac_cv_file__dev_ptc=no` cache answers (plus any others required), routed through `musl_toolchain()`.
- [ ] `--disable-test-modules` (or equivalent) excludes the large `test` package; any musl-cross SOABI/`lib-dynload` patches needed (upstream cpython#95855 / #115382 class) are applied via the Portfile `patches/` and noted.

---

## Track B — stdlib staging + relocation

### B.1 — Comprehensive non-networked stdlib + `lib-dynload`

**File:** `xtask/src/port_build.rs` (`build_python`), CPython `Modules/Setup`
**Symbol:** the DESTDIR install + `Modules/Setup` extension selection
**Why it matters:** "comprehensive" means building every C extension whose dependency is already present, while explicitly excluding networking/TLS — getting the static-vs-`lib-dynload` split right is what makes the stdlib usable.

**Acceptance:**
- [ ] `make install DESTDIR=<stage>` lays `bin/python3` + `lib/pythonX.Y/` (stdlib `.py` + `lib-dynload/*.so`); the `zlib`/`gzip` extensions build (zlib via `ports/lib/zlib`) and `hashlib` works via CPython's built-in HACL\*-backed `_md5`/`_sha*` modules (no OpenSSL).
- [ ] The interpreter and `lib-dynload/*.so` are **stripped** before sealing (the 85a seal contract).
- [ ] The TLS/name-resolution extensions (`_ssl`, `_hashlib`-OpenSSL, and `getaddrinfo`/DNS resolution) are **not** provided; their absence is recorded, not silently broken. (`_socket` itself may build against the existing TCP/UDP + AF_UNIX stack — it is DNS resolution and TLS that are deferred, not the extension wholesale.)

### B.2 — Relocation contract (`sys.prefix` landmark)

**File:** the 85a relocation-contract doc + `build_python`
**Symbol:** the staged layout
**Why it matters:** CPython finds its stdlib by searching upward from the executable for `os.py`; the package is only relocatable if `bin/` + `lib/pythonX.Y/` stay in fixed relative layout.

**Acceptance:**
- [ ] The `.m3pkg` installs to `/usr` and `python3` resolves `sys.prefix` to `/usr` with the stdlib found, with no hardcoded build-prefix path baked in.

---

## Track C — Packaging + validation + version

### C.1 — Seal + install via `pkg`

**File:** `xtask/src/port_build.rs` (85a seal step) + `xtask/src/main.rs` (staging)
**Symbol:** 85a `seal_package` + `pkg install python`
**Why it matters:** Python's larger, multi-file layout is a stronger test of the 85a substrate than git.

**Acceptance:**
- [ ] `cargo xtask port build python` produces a `.m3pkg`; a second build is a pkgcache hit (zero compiler invocations).
- [ ] `pkg install python` lays the interpreter + stdlib into `/usr` and `python3` runs.

### C.2 — REPL + script validation gate

**Files:** `xtask/src/main.rs` (`python-smoke` serial gate), `AGENTS.md` (opt-in row `M3OS_PYTHON_REGRESSION=1`), a bundled `/usr/src/fibonacci.py`
**Symbol:** `cmd_python_smoke`
**Why it matters:** proves the interpreter + stdlib actually work inside m3OS.

**Acceptance:**
- [ ] Inside m3OS: `python3 -c "print('hello from m3OS')"` prints; `import json, re, math, datetime, argparse, hashlib, dataclasses, pathlib` succeeds; `/usr/src/fibonacci.py` prints the sequence; a `/tmp` file write+read round-trips; `sys.platform` reports the expected value.
- [ ] The `/usr/src/fibonacci.py` fixture is written into the data disk via `populate_ext2_files`, with `cargo xtask clean` run to recreate the disk.
- [ ] The status of `os.urandom`/`secrets` is validated-working (via the existing `getrandom` syscall) **or** documented-absent — not left silently broken.
- [ ] The gate is wired as an opt-in pre-push regression (`M3OS_PYTHON_REGRESSION=1`) in `AGENTS.md`.

### C.3 — Bump kernel crate `0.85.1` → `0.85.2`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.85.2"`
**Why it matters:** the 85c cut is the third Phase 85 sub-phase (mirrors 78c `0.78.2`).

**Acceptance:**
- [ ] `kernel/Cargo.toml` reads `0.85.2` (+ `Cargo.lock`); `cargo xtask check` clean; boot banner / `uname` report `0.85.2`.

---

## Documentation Notes

- **What changed relative to the standalone roadmap.** `docs/python-roadmap.md` Stage 1 is this sub-phase; its Stage 2 (networking/`ssl`/pip/threading) is Phase 86+.
- **Honesty.** No `ssl`/DNS resolution/`pip`/`asyncio` here; the docs must state these are deferred, not present-but-broken.
- **Prefer exact targets.** Reference the exact cross-configure flags + `CONFIG_SITE` cache answers, not "the cross flags".
