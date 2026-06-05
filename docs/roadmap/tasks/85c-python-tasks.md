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
- [x] `build_python` first builds a host CPython of the target version under `target/port-build/python/build-host/` (out-of-tree VPATH build; the `build-host/python` binary feeds `--with-build-python`).
- [x] The Portfile pins the CPython version (3.12.8) + SHA-256 (`c909157b…`, verified against python.org) and declares `DEPS=zlib`.

### A.2 — Cross-configure for the musl target

**File:** `xtask/src/port_build.rs` (`build_python` stage 2)
**Symbol:** the CPython `configure` invocation
**Why it matters:** the cross flags + `CONFIG_SITE` cache answers are the error-prone heart of a CPython cross build.

**Acceptance:**
- [x] CPython cross-configures with `--host=x86_64-linux-musl --build=$(cc -dumpmachine) --with-build-python=<host python> --disable-shared --disable-ipv6 --without-ensurepip --without-pymalloc` and the `ac_cv_file__dev_ptmx=no ac_cv_file__dev_ptc=no` cache answers (plus `ac_cv_buggy_getaddrinfo=no`), routed through `musl_toolchain()`. Reproducibility hazard fixed: every external-lib stdlib module (`_ctypes`, `_ssl`, `_curses`, `readline`, …) is forced to `py_cv_module_*=n/a` so a build host that ships the `-dev` package can't change what cross-builds (only `n/a`, not `disabled`, survives configure's per-module overwrite).
- [x] `--disable-test-modules` excludes the large `test` package. **No source patches were required** — the upstream musl-cross SOABI/`lib-dynload` hazards were avoided in build logic instead: the `py_cv_module_*=n/a` set keeps the build off host `-dev` libs, and the one cross-hostile `make` step (`checksharedmods`, which runs the glibc build-python to import the musl target `.so`) is neutered to a no-op echo (`patches/` is empty).

---

## Track B — stdlib staging + relocation

### B.1 — Comprehensive non-networked stdlib + `lib-dynload`

**File:** `xtask/src/port_build.rs` (`build_python`), CPython `Modules/Setup`
**Symbol:** the DESTDIR install + `Modules/Setup` extension selection
**Why it matters:** "comprehensive" means building every C extension whose dependency is already present, while explicitly excluding networking/TLS — getting the static-vs-`lib-dynload` split right is what makes the stdlib usable.

> **⚠ Architectural finding (changed from the plan).** The plan assumed a
> dynamic interpreter with `lib-dynload/*.so` loaded via `dlopen`. The first
> in-m3OS gate run proved that infeasible: m3OS's `/lib/ld-musl-x86_64.so.1` is a
> *custom Rust loader reimplementation* (`userspace/ld-musl-x86_64.so.1/`) and
> m3OS ships **no real musl `libc.so`** (its userland is `no_std` Rust), so a
> dynamic CPython faults at startup — `ldso: DT_NEEDED not found: libc.so`. The
> interpreter is therefore built **fully static** (`MODULE_BUILDTYPE=static` →
> every C extension builtin; `LDFLAGS=-static` → musl libc embedded; no
> `PT_INTERP`, no `lib-dynload`, no `dlopen`) — the same model the static `git`
> port uses, and the only one that runs on m3OS today. The substance of every
> B.1 item is preserved; only the *packaging* of the extensions changed
> (builtin, not `.so`). Lifting the static constraint — a real `libc.so` + a
> dynamic `python3` with real `lib-dynload` + `ctypes` — is tracked as
> [Phase 91 (Dynamic C Runtime)](../91-dynamic-c-runtime.md).
>
> **⚠ Second finding — frozen `python312.zip`.** The first static gate run also
> showed m3OS's ring-3 VFS is slow (`vfs_server: slow req … STAT_PATH
> elapsed_us=80000-200000` — 80-200 ms *per* path stat), so shipping ~1700 loose
> stdlib files made `pkg install python` and every cold `import` (a per-module
> `sys.path` stat storm) take minutes (install timed out at 360 s). Fix:
> `freeze_stdlib_zip` byte-compiles the stdlib and packs it into a single
> `lib/python312.zip` of `.pyc` (already on CPython's default `sys.path` —
> zipimport reads the archive directory once). The package drops from ~1700
> files to a few hundred, install + imports become fast, and only the `os.py`
> getpath landmark is kept loose.

**Acceptance:**
- [x] `make install DESTDIR=<stage>` lays `bin/python3` (→`python3.12`) + `lib/python3.12/` (stdlib `.py`); **every** stdlib C extension is compiled **into** the interpreter (`MODULE_BUILDTYPE=static`) rather than as `lib-dynload/*.so`; the `zlib`/`gzip` extensions build against the staged `ports/lib/zlib` (now `-fPIC`) and `hashlib` works via CPython's built-in HACL\*-backed `_md5`/`_sha*` (no OpenSSL) — host-validated: `import json,re,math,…,zlib,gzip,socket` all succeed, `hashlib.sha256(b'abc')` = `ba7816bf…`.
- [x] The interpreter is **stripped** before sealing (`seal_package`→`strip_stage`; stripped static `python3.12` ≈ 9.8 MB). `prune_python_stage` removes the demo-only `lib-dynload/` (and hard-fails if any *real* extension leaked there as shared — a static-build correctness probe).
- [x] The TLS/name-resolution extensions (`_ssl`, `_hashlib`-OpenSSL, `getaddrinfo`/DNS) are **not** built: every external-lib module is forced `py_cv_module_*=n/a`, and `assert_python_layout` proves the interpreter is static (no `/lib/ld-musl…` interp string). `_socket` *is* builtin (TCP/UDP + AF_UNIX); only DNS resolution + TLS are deferred to Phase 86.

### B.2 — Relocation contract (`sys.prefix` landmark)

**File:** the 85a relocation-contract doc + `build_python`
**Symbol:** the staged layout
**Why it matters:** CPython finds its stdlib by searching upward from the executable for `os.py`; the package is only relocatable if `bin/` + `lib/pythonX.Y/` stay in fixed relative layout.

**Acceptance:**
- [x] The `.m3pkg` installs to `/usr` and `python3` resolves `sys.prefix` to `/usr` with the stdlib found, with no hardcoded build-prefix path baked in. Confirmed in-m3OS by the `python-smoke` gate (after `pkg install python`, `python3 --version` + every `import` resolve against the `/usr`-relative `os.py` landmark + `python312.zip`); host-validated that `sys.prefix` tracks the executable location (no build-prefix baked).

---

## Track C — Packaging + validation + version

### C.1 — Seal + install via `pkg`

**File:** `xtask/src/port_build.rs` (85a seal step) + `xtask/src/main.rs` (staging)
**Symbol:** 85a `seal_package` + `pkg install python`
**Why it matters:** Python's larger, multi-file layout is a stronger test of the 85a substrate than git.

**Acceptance:**
- [x] `cargo xtask port build python` produces a `.m3pkg` (sealed `9aff35bc…m3pkg`, 22.5 MB); a second build logs `PKGCACHE: hit … zero compiler invocations`.
- [x] `pkg install python` lays the interpreter + stdlib into `/usr` and `python3` runs — confirmed by the `python-smoke` gate (`pkg install: resolving python + dependencies` → solver auto-installs `zlib` → `pkg install: python: OK` → `python3 --version` prints `Python 3.12.8`).

### C.2 — REPL + script validation gate

**Files:** `xtask/src/main.rs` (`python-smoke` serial gate), `AGENTS.md` (opt-in row `M3OS_PYTHON_REGRESSION=1`), a bundled `/usr/src/fibonacci.py`
**Symbol:** `cmd_python_smoke`
**Why it matters:** proves the interpreter + stdlib actually work inside m3OS.

**Acceptance:**
- [x] Inside m3OS (`python-smoke` gate, PASSED 26 steps in 375 s): a `-c` run imports `json, re, math, datetime, argparse, hashlib, dataclasses, pathlib` (+`os, secrets`) and `print()`s; `/usr/src/fibonacci.py` prints `0 1 1 2 3 5 8 13 21 34`; a `/tmp` file write+read round-trips; `sys.platform` reports `linux`. (The gate prints **runtime-constructed** `PYSMOKE:`/`PYIO:` sentinels rather than the literal `print('hello from m3OS')` — a `Wait` must not match the serial-echoed command, the same discipline as the git gate; the substance, that `print()` + the imports work, is what's asserted.)
- [x] The `/usr/src/fibonacci.py` fixture is written into the data disk via `populate_ext2_files`; `cmd_python_smoke` force-recreates the disk every run (equivalent to `cargo xtask clean`), so the fixture is always fresh.
- [x] `os.urandom`/`secrets` are validated-**working** (the imports `-c` prints `PYSMOKE:rand=` from `os.urandom(4).hex()+secrets.token_hex(2)` — the m3OS `getrandom` syscall).
- [x] The gate is wired as an opt-in pre-push regression (`M3OS_PYTHON_REGRESSION=1`) in `AGENTS.md` (opt-in table row) and `.githooks/pre-push` (`cargo xtask python-smoke --timeout 900`).

### C.3 — Bump kernel crate `0.85.1` → `0.85.2`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.85.2"`
**Why it matters:** the 85c cut is the third Phase 85 sub-phase (mirrors 78c `0.78.2`).

**Acceptance:**
- [x] `kernel/Cargo.toml` reads `0.85.2` (+ `Cargo.lock`); `cargo xtask check` clean (clippy `-D warnings` + rustfmt + host tests); the `python-smoke` boot banner reports `kernel v0.85.2`.

---

## Documentation Notes

- **What changed relative to the standalone roadmap.** `docs/python-roadmap.md` Stage 1 is this sub-phase; its Stage 2 (networking/`ssl`/pip/threading) is Phase 86+.
- **Honesty.** No `ssl`/DNS resolution/`pip`/`asyncio` here; the docs must state these are deferred, not present-but-broken.
- **Prefer exact targets.** Reference the exact cross-configure flags + `CONFIG_SITE` cache answers, not "the cross flags".
