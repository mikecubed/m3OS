# Cross-Compiled Toolchains (Phase 85)

**Aligned Roadmap Phase:** Phase 85 (sub-phases 85a–85d)
**Status:** Complete
**Source Ref:** phase-85
**Supersedes Legacy Doc:** new

## Overview

Phase 85 turns m3OS from "an OS that can run a few hand-ported TUI programs"
into "an OS with a supported, post-1.0 developer toolchain" — **git**, **Python**,
and **Clang/LLVM/LLD** — without making every image build pay to recompile them.
It ships as four sequenced sub-phases: **85a** (the packaging substrate), then
**85b** (git), **85c** (Python), and **85d** (Clang + the family's release
closeout). The family closes at kernel `0.85.3`.

The central lesson is **why a build-once / install-prebuilt substrate has to come
first**. The early ports (ncurses, less, htop, tmux) are small enough that
rebuilding them from source on each image build is merely annoying. The new
toolchains are not: a Clang + LLD build links with **many gigabytes of RAM over a
multi-hour compile**, and even a stripped, static, X86-only install runs to
**≈125 MB**. Recompiling that on every `cargo xtask image` is
intolerable, so Phase 85 first builds a content-addressed package cache (85a),
then makes each toolchain a *consumer* of that cache. Build the artifact once;
every later build installs it for free.

The second lesson is **the relocation contract**. A prebuilt package is only
useful if it works after being unpacked into `/usr` on the target — not just from
the exact prefix it was built in. That forces every toolchain to locate its
support files *relative to its own executable* rather than at a hard-coded build
path. Clang is the hardest case in the whole family: it must find its resource
directory (built-in headers + `compiler-rt` builtins under `lib/clang/<ver>/`)
relative to the `clang` binary, with a fixed `--sysroot` supplying libc
headers/CRT. Getting that right is what makes the `.m3pkg` genuinely relocatable.

The third lesson is **honest scope**. These are deliberately *local* tools. git
is built `NO_CURL NO_OPENSSL` — local repository workflows only. Python is a
fully static interpreter with a non-networked stdlib — no `ssl`, no `pip`, no
`ctypes`/`dlopen`. Clang is opt-in, X86-only, statically linked against
musl + libc++, with no `opt`/`llc`/sanitizers and no self-hosting. The networked
half of each tool is a deliberate Phase 86 (DNS + TLS) / Phase 93 (dynamic
`libc.so`) handoff, not an oversight.

## What This Doc Covers

- **The content-addressed build cache (85a)** — how a port is keyed on a hash of
  its inputs, sealed once into a relocatable `.m3pkg`, and reused on a cache hit
  with **zero** compiler invocations.
- **The `.m3pkg` format + offline `pkg` installer (85a)** — the relocatable
  package artifact, the offline in-OS `pkg install`/`remove`/`upgrade`/`list`/
  `verify`, and the transitive dependency solver — all with **no network**.
- **The relocation contract** — why binaries must resolve their support files
  relative to the executable, why Clang's resource dir is the worst case, and how
  the DESTDIR-at-`/usr` staging keeps entry paths prefix-relative.
- **git (85b)** — a local-only musl `git` (`NO_CURL NO_OPENSSL` + zlib), the
  first real tool to exercise the substrate end-to-end.
- **Python (85c)** — a two-stage cross-built **fully static** CPython 3.12 with a
  comprehensive non-networked stdlib frozen into `python312.zip`.
- **Clang/LLVM/LLD (85d)** — the heavyweight, opt-in, X86-only static toolchain
  whose rebuild cost is the entire reason 85a exists.
- **The disk / RAM budget** — why git and Python fit the default 1 GB image but
  Clang is feature-gated, and the host-build memory implications.
- **Where the family stops** — the networked-transport and dynamic-linking
  deferrals to Phases 86 and 93.

## Core Implementation

### The packaging substrate (85a) — build once, install prebuilt

Before Phase 85, the ports system already cached two things *within one machine*:
source tarballs by SHA-256 under `target/port-src/`, and built stage trees by a
`target/port-stage/<name>/.stamp` fingerprint (a fingerprint match skipped
configure/make/install entirely). What it lacked was a **portable** artifact, a
**cross-machine** key, and any way to install a tool into a running m3OS short of
rebuilding the whole image.

85a generalizes the `.stamp` fingerprint into a **content-addressed package key**:

```
key = hash(source rev/tarball SHA
           + musl toolchain identity
           + build flags
           + dependency artifact keys)
```

This is the same idea behind Yocto's sstate cache, Bazel's action cache, and the
Nix/Guix store path. The build pipeline runs a **seal-after-install** step: after
a port's DESTDIR install completes, it strips the ELF executables/`.so`s, computes
the key, and packs `target/port-stage/<name>/` into
`target/pkgcache/<key>.m3pkg`. Before any build, a **resolve-before-build** step
checks the cache for a matching key and short-circuits straight to
install-from-artifact — no rebuild. The headline acceptance result is proven on
the heaviest artifact: a second image build of an unchanged tool performs **zero**
compiler invocations.

### The `.m3pkg` format and the offline `pkg` installer (85a)

A **`.m3pkg`** is a small relocatable package: a binary header (magic + version +
a reserved ed25519 signature field + a per-entry index of path / mode / SHA-256)
followed by a data blob. It is modeled on Redox's `pkgar` but deliberately
simpler. The v1 implementation uses **SHA-256** (a compact pure-`u32`
implementation) rather than BLAKE3, and a custom binary header rather than
`.tar.zst`, because `blake3`/`zstd` are unavailable in the offline build
environment and the RustCrypto `sha2` crate does not codegen on the soft-float /
no-SSE `x86_64-unknown-none` target the installer is built for. The ed25519 field
is reserved (zeroed) for the Phase 86 signed *networked* repo; offline/local
install needs only the hash because the artifact already sits on a trusted disk.

The userspace **`pkg`** binary installs a `.m3pkg` from a **local on-disk repo**
(`/usr/pkg/`) into `/usr`, recording an installed-file database at
`/var/lib/pkg/db` — **no network required**, which is what keeps the whole thing
inside the pre-networking Phase 85 boundary. It supports `install` / `remove` /
`upgrade` / `list` / `verify`, and a **transitive dependency solver** that reads
each package's `/usr/pkg/<name>.meta` `DEPS=` line and installs dependencies first
in topological order (e.g. `pkg install git` auto-installs its `zlib` dependency;
`pkg install python` does the same).

### The relocation contract — locate support files relative to the executable

A package built at one prefix and installed at another is only usable if nothing
inside it depends on the *build* path. The DESTDIR/relocation contract every
recipe must honor:

1. **Build at the final runtime prefix** (`--prefix=/usr` for the toolchains) and
   **stage** via `make install DESTDIR=<target/port-stage/<name>>`, so the packed
   tree's entry paths are prefix-relative (`usr/...`) and the installer lays them
   under `/` unchanged.
2. **Strip executables and shared objects before sealing**, to keep artifacts
   small — modest on the ncurses-class binaries, but the large payoff is the
   multi-hundred-MB Clang artifact.
3. **Keep a relocatable internal layout** — no build-prefix-absolute paths baked
   into installed files. Each tool resolves its support files *relative to its own
   binary*:
   - **Clang** resolves its **resource dir** (`lib/clang/<ver>/{include,lib}` —
     built-in headers + `compiler-rt` builtins) relative to the `clang`
     executable; a fixed `--sysroot` supplies libc headers/CRT. This is the
     hardest relocation case in the family, because if the resource dir is not
     found relative to the binary, Clang cannot locate its own headers and the
     package is not installable. Inside m3OS, `clang -print-resource-dir` resolves
     under `/usr`.
   - **Python** relies on the `sys.prefix` landmark search, keeping `bin/` +
     `lib/pythonX.Y/` in a fixed relative layout.
   - **git** uses `RUNTIME_PREFIX` so its `libexec/git-core` subcommands and
     `share/git-core/templates` are found relative to the `git` binary.

### git (85b) — the first real tool through the substrate

git is the smallest of the three toolchains and lands first to de-risk the rest.
It is a musl `git` built `NO_CURL=1 NO_OPENSSL=1` (plus the other `NO_*` knobs that
carve out gettext/tcltk/perl/python/iconv/expat), statically linked against the
existing `ports/lib/zlib` — its one mandatory dependency, since git's SHA-1/SHA-256
are built-ins. The build flows through the full 85a path: cross-build → DESTDIR
stage at `/usr` → strip → `.m3pkg` seal → `pkg install git` (with the solver
auto-installing `zlib`). It supports the documented **local** repository
workflows: `init`, `add`, `commit`, `log`, `diff`, `status`, `branch`, `merge`,
`checkout`. Networked transport (clone/fetch/push over HTTPS) is deliberately
deferred to Phase 86; git's SSH transport — which shells out to an `ssh` client —
is tracked in Phase 86's secure-transport work, not here.

### Python (85c) — a fully static CPython 3.12

Python is the fiddly middle case: a **two-stage cross build**. CPython's build
needs a same-version interpreter to run its own build scripts, so 85c first builds
a host CPython of the exact target version, then cross-configures the target
interpreter with `--with-build-python=<host>` (plus `--host=x86_64-linux-musl`,
`--build=$(gcc -dumpmachine)`, `--disable-shared`, `--disable-ipv6`,
`--without-ensurepip`, and the `CONFIG_SITE`/`ac_cv_*` cache answers a cross build
needs).

The interpreter is **fully static**: `MODULE_BUILDTYPE=static` makes every C
extension a builtin (no `lib-dynload`/`dlopen`), and `-static` embeds musl libc.
This is not a stylistic choice — m3OS's `ld-musl-x86_64.so.1` is a custom loader
with **no real `libc.so` to load**, so a dynamic interpreter simply cannot run
(this finding is exactly what motivates Phase 93). "Comprehensive stdlib" means
every C extension whose dependency is *already ported* is builtin: `zlib`/`gzip`
against `ports/lib/zlib`; `_curses`/`_curses_panel` against the ported wide
ncurses (the same archives less/htop/tmux link); and `hashlib` via the built-in
HACL\* `_md5`/`_sha*` (no OpenSSL). The whole stdlib `.py` tree is frozen into a
single `lib/python312.zip` so the slow ring-3 VFS does a few large reads instead
of thousands of tiny ones. Networking/TLS modules (`ssl`, socket DNS, `pip`,
`asyncio`) and the `dlopen`-only `ctypes` are out of scope by design.

### Clang/LLVM/LLD (85d) — the heavyweight, opt-in artifact

Clang is the artifact the whole substrate exists for, and it lands last, on a
proven cache. It is a host-cross-built static **Clang + LLD**, configured for size:

> **Why the host *clang* is the cross-compiler here.** Every other port cross-builds
> with the musl-gcc wrapper — but `musl-tools` ships **no C++ compiler**, and
> LLVM/Clang is a large C++ codebase. So 85d drives the **host `clang`** as the
> cross-compiler (`--target=x86_64-linux-musl` over an assembled musl sysroot).
> That exposes a chicken-and-egg: compiling LLVM's *own* C++ for musl needs a musl
> C++ standard library that does not exist yet. The fix is a two-stage build —
> first cross-build `libc++`/`libc++abi`/`libunwind` for the target (the runtimes
> need only C headers), making a **self-contained `libc++.a`** (abi + unwinder
> merged in, so a bare `-lc++` links); then build `clang`+`lld` against it. Because
> the build host and target are the same arch (only the libc differs), the
> just-built static-musl `tblgen` tools run directly on the host, sidestepping the
> usual native-tooling sub-build. The built clang bakes in m3OS-friendly defaults —
> `lld` as the linker (m3OS has no GNU `ld`), `compiler-rt`/`libc++`/`libunwind`,
> and a fixed `DEFAULT_SYSROOT` — so a bare in-OS `clang hello.c` just works.

```
cmake -DLLVM_ENABLE_PROJECTS="clang;lld" \
      -DLLVM_ENABLE_RUNTIMES="libcxx;libcxxabi;libunwind" \
      -DLLVM_TARGETS_TO_BUILD="X86" \
      -DCMAKE_BUILD_TYPE=MinSizeRel \
      -DLLVM_ENABLE_THREADS=OFF \
      -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF \
      -DLLVM_ENABLE_TERMINFO=OFF \
      -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF \
      -DCLANG_ENABLE_STATIC_ANALYZER=OFF
```

The size levers matter: the single `X86` target alone saves the bulk of the
build, `MinSizeRel` optimizes for binary size, and tests/benchmarks/static-analyzer
are off. (`LLVM_ENABLE_THREADS=OFF` is chosen because m3OS's compile target is
single-threaded, not as a size lever.) The build runs `ninja clang lld`, both
binaries are `install/strip`-ped, and the package bundles everything a sample
program needs to actually *link*: the musl sysroot (`libc.a`, CRT objects), the
Clang built-in headers, the `compiler-rt` builtins, **and** the C++ runtime
(`libc++.a`, `libc++abi.a`, `libunwind.a` + the `c++/v1` headers) — without the
C++ runtime a `hello.cpp` would compile but not link. A working `clang++` is
provided (symlink, or `argv[0]` driver-mode dispatch if ext2 symlinks prove
unreliable).

Because the artifact is large, the Clang `.m3pkg` is **bundled only behind an
opt-in image feature** (e.g. `M3OS_WITH_CLANG`); default images omit it and stay
small. The validation gate compiles and runs C and C++ inside m3OS:
`clang -O2 /usr/src/hello.c -o hello && ./hello` prints "hello, world",
`clang++ /usr/src/hello.cpp` links against the bundled libc++ and runs, and
`clang -fuse-ld=lld` links via LLD.

### The disk / RAM budget — why Clang is gated and the others are not

The two costs are distinct: **on-image disk** vs **host-build RAM**.

| Artifact | On-image install (approx.) | Fits default 1 GB image? |
|---|---|---|
| 85a retrofit (ncurses/less/htop/tmux/libevent/zlib) | ~34 MB (~3% of the 1 GB disk) | Yes |
| git (85b) | tens of MB | Yes |
| Python (85c) | tens of MB | Yes |
| Clang/LLD (85d) | **≈125 MB** packed `.m3pkg` (130,541,744 B); ≈125 MB installed under `/usr` | Fits, but **opt-in only** |

The Clang numbers come from a real opt-in build of LLVM 18.1.8 (`MinSizeRel`,
X86-only, static, stripped): the sealed `clang.m3pkg` is **130,541,744 bytes
(≈125 MB)**, and because the `.m3pkg` format is **uncompressed** (raw bytes + a
per-entry SHA-256 index, no deflate), `pkg install clang` writes a comparable
≈125 MB under `/usr` — the stripped static `clang` + `lld` plus the bundled
musl + libc++ sysroot and the Clang resource headers/builtins.

A ≈125 MB install does technically fit the default 1 GB data disk — but the
artifact is kept **opt-in (`M3OS_WITH_CLANG`)** anyway, because the repo `.m3pkg`
and the unpacked install coexist (≈250 MB of data-disk footprint after install),
and a quarter-gigabyte of compiler is dead weight on every default image for a
tool most users will not install. By contrast, 85a measured the six-port retrofit
at ~34 MB and git + Python are each tens of MB, so they ride the default image
without a second thought. (Compressing the `.m3pkg` payload — a real size win for
this artifact — is tracked in the VFS bulk-I/O phase.)

The **host-build** memory story is the real reason the cache exists. The
ncurses-class ports and git/Python build in well under 1 GB of host RAM. The
Clang + LLD link is the outlier: it can require **many GB of RAM (≈8–16 GB+)** on
the build host or CI runner, over a multi-hour compile. That is a one-time cost
the 85a content-addressed cache pays exactly once — every later build (and, via
the Phase 86 hosted repo, every other developer) installs the prebuilt `.m3pkg`
instead of re-linking gigabytes.

### How the toolchains fit the post-1.0 developer story

These tools are intentionally additive growth *after* the Phase 83 1.0 boundary,
not part of the 1.0 promise. The kernel stays phase-tracked (`0.85.x`), never
SemVer `1.0.0`. The progression is deliberate: git gives local version control
(and is the prerequisite for remote git in Phase 86); Python gives a scripting and
automation runtime (and is the prerequisite for later tooling); Clang gives a real
optimizing C/C++ compiler beyond the Phase 31 TCC. Each is "supported developer
workflow" rather than "experiment that happens to run once" — installed via
`pkg install`, living under `/usr/bin` + `/usr/lib`, and validated by a dedicated
serial gate.

## Key Files

| File | Purpose |
|---|---|
| `xtask/src/port_build.rs` | Port `build_*` functions (`build_git`, `build_python`, `build_llvm`); the 85a seal-after-install + resolve-before-build cache steps; `strip_stage` |
| `xtask/src/main.rs` | Image staging from `.m3pkg` artifacts, `DISK_SIZE`, the opt-in Clang image feature gate, the per-tool serial validation gates |
| `pkg-format/src/lib.rs` | The `.m3pkg` v1 byte layout (magic + version + reserved ed25519 + per-entry path/mode/SHA-256 index + data blob); host-tested pack/unpack/verify + content-key |
| `userspace/pkg/` | The offline in-OS `pkg` installer — `install`/`remove`/`upgrade`/`list`/`verify` + transitive `DEPS=` solver, reading `/usr/pkg/` and `/var/lib/pkg/db` |
| `ports/util/git/Portfile` | Pinned git version + SHA-256; `NO_CURL NO_OPENSSL` + zlib build recipe |
| `ports/lang/python/Portfile` | Pinned CPython 3.12 version + SHA-256; two-stage static cross-build recipe |
| `ports/lang/llvm/Portfile` | Pinned LLVM version + SHA-256; X86-only `MinSizeRel` static CMake cross-build recipe |
| `ports/lib/zlib/` | The shared zlib dependency git and Python both link |

## How This Phase Differs From Later Toolchain Work

- **Networked git transport** (clone/fetch/push over HTTPS) and git's GitHub
  integration are **Phase 86** (DNS + TLS). 85b is local-only.
- **Python TLS/DNS, `pip`, and `asyncio`** are **Phase 86**; `ctypes`/`dlopen`
  and a *dynamic* `python3` with real `lib-dynload` `.so` extensions are
  **Phase 93** (Dynamic C Runtime — m3OS needs a real `libc.so` first).
- **Networked `pkg install`/`update` over HTTPS** from the hosted `m3os-pkgs`
  static repo, `/etc/pkg.d/` remote registration, and signed `.m3pkg`
  verification (populating the reserved ed25519 field) are **Phase 86**. 85a is
  offline/local only.
- **Self-hosting LLVM** (building LLVM *on* m3OS) needs C++ exception handling,
  threading, and build infra (CMake/Ninja) and is `docs/clang-llvm-roadmap.md`
  Stage 2 — deferred beyond Phase 85.
- **A full LLVM toolchain** — additional targets beyond X86, `opt`/`llc`,
  sanitizers, the clang-tools suite, dynamic linking of the toolchain, and
  multi-threaded compilation (`LLVM_ENABLE_THREADS=ON`) — is out of scope; 85d
  ships clang + lld only.
- **Binary delta packages**, transactional/atomic install+rollback, and multiple
  repositories are beyond the flat-install 85a model.

## Related Roadmap Docs

- [Phase 85 umbrella design doc](./roadmap/85-cross-compiled-toolchains.md) —
  theme, sub-phase decomposition, and the shared packaging architecture
- [Phase 85a — Package & Build-Cache Infrastructure](./roadmap/85a-package-infrastructure.md)
- [Phase 85a Task List](./roadmap/tasks/85a-package-infrastructure-tasks.md)
- [Phase 85b — git (Local)](./roadmap/85b-git-local.md)
- [Phase 85b Task List](./roadmap/tasks/85b-git-local-tasks.md)
- [Phase 85c — Python (CPython)](./roadmap/85c-python.md)
- [Phase 85c Task List](./roadmap/tasks/85c-python-tasks.md)
- [Phase 85d — Clang/LLVM/LLD (+ Release)](./roadmap/85d-clang-llvm.md)
- [Phase 85d Task List](./roadmap/tasks/85d-clang-llvm-tasks.md)
- Standalone per-tool roadmaps: [git](./git-roadmap.md), [Python](./python-roadmap.md),
  [Clang/LLVM](./clang-llvm-roadmap.md)

## Deferred or Later-Phase Topics

- Networked `pkg install`/`update`, networked git, Python TLS/DNS/`pip`/`asyncio`,
  signed remote `.m3pkg` repos — Phase 86
- `ctypes`/`dlopen` + a dynamic `python3` with real `lib-dynload` extensions,
  and a real musl `libc.so` — Phase 93 (Dynamic C Runtime)
- Self-hosting LLVM inside m3OS; additional LLVM targets; `opt`/`llc`; runtime
  sanitizers; dynamic linking of the toolchain — beyond Phase 85
  (`docs/clang-llvm-roadmap.md` Stage 2)
- Broader language/runtime stacks (Node.js, etc.) beyond git/Python/Clang —
  Phase 89+
- Binary delta packages, transactional/atomic installs, and multiple repositories
