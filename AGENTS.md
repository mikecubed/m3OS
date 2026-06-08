# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**m3OS** (technical name: `m3os`) is a bootable microkernel OS in Rust: x86_64, UEFI boot, kernel **v0.86.2**. Ring 0 handles memory, scheduling, IPC/capabilities, interrupt routing, and in-kernel drivers; ring 3 hosts everything else.

Capabilities now present in the tree:

- **Userspace**: init (PID 1), shell (sh0) + ion, coreutils, multi-user (login/su/passwd/adduser), editor, service manager, PTY, telnet/SSH servers, crypto.
- **Networking & storage**: IPv4/TCP/UDP stack, AF_UNIX sockets, NVMe + AHCI/SATA ring-3 block drivers (single-queue, IOMMU-routed command list/FIS/PRDT, FLUSH-CACHE-EXT durability, presenting as `RemoteBlockDevice` over the shared block protocol — a SATA disk mounts the root off `ahci.block`) and modern NIC ring-3 drivers — Intel e1000 (82540EM), e1000e/igb/igc and Realtek RTL8111/8168 (r8169) + RTL8125 2.5G — with device-ID matching over a bounded multi-NIC registry, on a VirtIO baseline.
- **Wireless**: ring-3 MediaTek mt792x Wi-Fi driver (MT7921/MT7922 connac2, MT7925 in the same registry) — firmware-blob download, WM MCU command ring, WFDMA TX/RX rings, soft-MAC 802.11 mgmt FSM + WPA2-PSK 4-way handshake (host crypto in `wifi-core`/`crypto-lib`) with chipset CCMP offload, presenting as an L2 `RemoteNic`; no QEMU mt76 model, so logic is host-tested and the radio is VFIO/bare-metal validated.
- **IOMMU substrate**: ACPI DMAR/IVRS parsing, per-device VT-d / AMD-Vi domains, IOMMU-routed `DmaBuffer<T>`, fault ISRs, VT-d queued invalidation.
- **Ring-3 driver hosting**: capability-gated device-host syscalls, supervised userspace NVMe/e1000 with `RemoteBlockDevice`/`RemoteNic` facades.
- **USB host stack**: ring-3 xHCI host driver (MSI-X, BME, TRB/event rings) + `usb-core`/hub + a HID Boot-Protocol class driver (`usb-hid`) injecting keyboard/mouse into `kbd_server`/`mouse_server` — modern PS/2-less machines get USB keyboard/mouse input.
- **Package management**: content-addressed prebuilt-package substrate — a relocatable `.m3pkg` format + portable content key (`pkg-format`, host-tested), an `xtask` seal-after-install / resolve-before-build pkgcache (`target/pkgcache/`, strip-before-seal, zero-rebuild gate), and an offline in-OS `pkg install`/`remove`/`upgrade`/`list`/`verify` installer with a transitive dependency solver (resolving each package's `/usr/pkg/<name>.meta` `DEPS=` in dependency-first topological order) reading a local `/usr/pkg/` repo + `/var/lib/pkg/db`. The ncurses-class ports (+zlib) build once and install as artifacts; **git** (Phase 85b → 86c — a static musl `git`: local-only in 85b, then **HTTPS-capable** in 86c, rebuilt **with** a static `libcurl --with-mbedtls` (`NO_CURL` removed; `NO_OPENSSL` kept — the TLS backend is mbedTLS-via-curl) so `git clone https://github.com/…` validates GitHub's TLS 1.3 cert chain + hostname against the Phase 86a CA bundle; git-over-SSH is served separately by the dropbear `ssh` port (86b)) and **Python** (Phase 85c — a two-stage musl-cross **CPython 3.12**: a same-version build interpreter via `--with-build-python`, then a **fully static** `python3` (`MODULE_BUILDTYPE=static` → every C extension builtin, `-static` → musl libc embedded; no `lib-dynload`/`dlopen`, since m3OS's custom `ld-musl` has no real `libc.so`), with the comprehensive non-networked stdlib — every C extension whose dependency is already ported is builtin, incl. `zlib`/`gzip` and `_curses`/`_curses_panel` against the ported wide ncurses — frozen into a single `lib/python312.zip` for the slow ring-3 VFS, `pkg install python`), and **Clang/LLVM/LLD** (Phase 85d — an opt-in, X86-only, statically musl-linked **clang + lld**, host-cross-built with the host clang as the cross-compiler (musl-tools ships no C++ compiler) over a self-contained libc++ (abi + unwinder merged), `MinSizeRel`, no `opt`/`llc`/sanitizers/self-hosting; bundled behind the `M3OS_WITH_CLANG` image feature so default images omit the several-hundred-MB artifact; `pkg install clang` then **compiles + links + runs C/C++ on m3OS** via clang + lld + a bundled musl/libc++ sysroot, the resource dir resolved relative to the binary — m3OS's first on-device native C/C++ toolchain) are the cross-compiled toolchains delivered through the substrate; networked `git` transport is delivered (SSH in 86b, HTTPS/TLS via mbedTLS+curl in 86c), while Python's TLS/DNS/`pip`/`asyncio` remain deferred within Phase 86, and `ctypes`/`dlopen` to Phase 91 (Dynamic C Runtime — needs a real `libc.so`); `hashlib` keeps working via built-in HACL\* `_md5`/`_sha*` (no OpenSSL).
- **Graphical stack**: `display_server` (framebuffer owner, focus-aware input, layer-shell surface roles, damage tracking, animations/decorations), `kbd_server`/`mouse_server`, compositor clients (`wallpaper`, `bar`, `launcher`, `notifyd`, `lockscreen`), `greeter` GUI login, `session_manager` lifecycle supervision.
- **Audio**: out-of-process ring-3 audio drivers — `ac97` and `hda` (Intel HD Audio controller + generic zero-quirk widget-graph codec, CORB/RIRB IOVA rings, BDL/`SDnFMT` output stream) — behind a `driver_ipc::audio` seam; `audio_server` is a pure policy/mixer (32-ch DMX→S16LE mix, DOOM audio + bell) that forwards PCM over a persistent `sys_shm` ring.
- **Terminal**: `term` emulator, full termios/line-discipline, UTF-8 + TTF/Nerd Font glyphs, ncurses + less/htop/tmux ports.
- **Dynamic linking**: `ld-musl-x86_64.so.1` with `ldso_core`, PT_INTERP support, dlopen/dlsym/dlclose, PLT lazy resolve, DT_GNU_HASH, symbol versioning, LD_BIND_NOW, W^X enforcement.
- **CPU hardening**: SMEP + SMAP enforced on every core, per-CPU microcode application (AMD container), PT_TLS-backed pthreads, RFC 6298 TCP retransmission. **Spectre-v2**: retpoline kernel codegen (`-Zretpoline`, objdump-gated) + `IA32_SPEC_CTRL` family (eIBRS set-once / IBPB on cross-process switch / STIBP opt-in), behind a `mitigations=off|auto|full` policy with `m3ctl mitigations status`. KPTI (Meltdown) is designed + scaffolded; its CR3-trampoline activation is a tracked bare-metal-validated follow-up (Phase 84 Track A).

> **Phase history is NOT maintained here.** For the detailed per-phase record (Phase 55a → 76d and onward) and the full workspace/source layout, read `docs/roadmap/README.md` and `docs/appendix/codebase-map.md`. Read the relevant phase doc under `docs/roadmap/` before changing a subsystem.

> **Maintenance policy for this file — keep it small.** This file loads on every session, so its size is a recurring token cost. Do **not** append phase summaries, changelogs, or implementation diaries here — that record belongs in `docs/roadmap/`. When a phase lands, the only edits permitted in this file are: bump the kernel version above, and add a bullet to the capability inventory **only if it introduces a new capability class** (not for changes within an existing one). Prefer rewriting an existing bullet over adding prose. If a section starts listing internal symbols or per-change detail, move it to `docs/` and link instead.

## Build & Run

Uses the `xtask` pattern — always build through `cargo xtask`, never `cargo build` directly.

```bash
cargo xtask run          # build + launch in QEMU (headless, serial output)
cargo xtask run --fresh  # same, but recreate data disk first
cargo xtask run-gui      # build + launch in QEMU (GUI with framebuffer)
cargo xtask run-gui --fresh  # same, but recreate data disk first
cargo xtask image        # build bootable disk image (UEFI raw + VHDX)
cargo xtask image --sign # build + sign EFI binary for Secure Boot
cargo xtask check        # clippy (-D warnings) + rustfmt + host tests for kernel-core (incl. storage::{ahci,ata}, spectre, kpti), passwd, driver_runtime, audio_client, audio_server, surface_buffer, crypto-lib, term, audio_mixer, audio_client_ffi, session_manager, wifi-core, mt792x_driver, ahci_driver, m3ctl, pkg-format (Phase 85a .m3pkg pack/unpack/verify + content-key), pkg (Phase 85a installed-file DB), xtask (Phase 85a Portfile parser + package_key + pkgcache seal/resolve) + the Phase 84 retpoline objdump indirect-branch gate
cargo xtask fmt --fix    # auto-format all workspace source
cargo xtask test         # run all kernel tests in QEMU via ISA debug exit
cargo xtask test --test <name>  # run a single QEMU test binary
cargo xtask test --timeout 120  # custom timeout (default 60s)
cargo xtask test --display      # show QEMU window for debugging
cargo xtask sign         # sign EFI binary with Secure Boot keys
cargo xtask clean        # delete disk.img so next run recreates it
cargo test -p kernel-core       # run kernel-core host-side unit tests directly
```

After adding new service configs to the ext2 data disk, run `cargo xtask clean` to force disk recreation.

Tests cannot use `cargo test` on the kernel — it is `no_std` and tests run inside QEMU via the xtask harness. Pure-logic code lives in `kernel-core` and is testable on the host. The workspace's `.cargo/config.toml` defaults the build target to the bare-metal `x86_64-unknown-none`, so to run kernel-core host tests you must force the host target:

```bash
cargo test -p kernel-core --target x86_64-unknown-linux-gnu        # all host tests
cargo test -p kernel-core --target x86_64-unknown-linux-gnu --lib <filter>   # by name
```

(`cargo xtask check` already runs these correctly; the explicit `--target` is only needed for direct `cargo test`.)

## Headless framebuffer screenshots (QMP / VNC)

The serial-only smoke harness is **blind to graphical / TUI rendering** — it cannot tell a populated screen from a black one. To verify what the framebuffer actually shows (compositor output, `term`, `htop`, DOOM, etc.) **headlessly** (no host display), drive QEMU over QMP and capture PPM screenshots. This is how `less-render-probe` and `compositor-stress` work; reuse that plumbing rather than reinventing it:

- **Launch flags** (see `cmd_compositor_stress` / `less-render-probe` in `xtask/src/main.rs`): start from `qemu_args_with_devices(.., QemuDisplayMode::Headless, ..)`, then replace `-display none` with `-display vnc=unix:<vnc.sock>`, add `-qmp unix:<qmp.sock>,server,nowait`, and `-vga std`. Use `qmp::fresh_socket_path()` for both sockets. (A human can also attach a viewer with `M3OS_GUI_BACKEND=vnc cargo xtask run-gui` → `vncviewer localhost:5900`, but the programmatic path is QMP.)
- **Drive + capture** (`xtask/src/qmp.rs`): `QmpClient::connect(&qmp_sock, deadline)` → `client.send_key(..)` to inject PS/2 keystrokes (the same path real keys take) → `client.screendump(&path)` writes a binary **P6 PPM** of the current framebuffer.
- **Analyze** (`xtask/src/ppm.rs`): parse the PPM and assert on pixels — non-black-region checks, row/column occupancy (e.g. counting populated text rows for a `htop` process list), hashing/diffing between frames, etc.

Use this for any acceptance criterion of the form "the screen shows X" — a serial `Wait` on a sentinel only proves the program ran, not that it rendered.

## Git Workflow

All work must happen on a feature branch with a pull request to `main`. Never commit directly to `main`.

```bash
git checkout -b feat/my-feature       # 1. create feature branch
# ... make changes ...
git add <files> && git commit         # 2. commit
git push -u origin feat/my-feature    # 3. push
gh pr create --base main              # 4. open PR to main
# 5. user merges PR after review
```

Branch naming: `feat/`, `fix/`, `refactor/`, `docs/` prefixes as appropriate.

## First-Time Setup

After cloning, install the git hooks so quality gates run before commits and pushes:

```bash
./setup.sh
```

This sets `core.hooksPath` to `.githooks/`. **pre-commit** runs `cargo xtask check`. **pre-push** runs `cargo xtask check` + `smoke-test` + `regression`, plus these opt-in gates when their env var is set:

| Gate | Env var |
|---|---|
| `ssh-e1000-banner-check` | `M3OS_E1000_REGRESSION=1` |
| `doom-audio-smoke` | `M3OS_DOOM_AUDIO_REGRESSION=1` |
| `termios-smoke` | `M3OS_TERMIOS_REGRESSION=1` |
| `tui-app-smoke` | `M3OS_TUI_APP_REGRESSION=1` |
| `doom-concurrent-smoke` | `M3OS_DOOM_CONCURRENT_REGRESSION=1` |
| `tiling-smoke` | `M3OS_TILING_REGRESSION=1` |
| `htop-render-probe` | `M3OS_HTOP_REGRESSION=1` |
| `xhci-bringup-smoke` + `xhci-enum-smoke` + `usb-smoke` | `M3OS_USB_REGRESSION=1` |
| `tls-smoke` PASS (not SKIP) | `M3OS_TLS_REGRESSION=1` |
| `dns-smoke` PASS (not SKIP) | `M3OS_DNS_REGRESSION=1` |
| `multi-nic-smoke` (e1000 + e1000e + igb arms) | `M3OS_MULTI_NIC_REGRESSION=1` |
| `hda-smoke` (`-device intel-hda -device hda-duplex`, non-silent WAV) | `M3OS_HDA_REGRESSION=1` |
| `wifi-smoke` (no QEMU mt76 model — skip-with-reason; radio validated via VFIO) | `M3OS_WIFI_REGRESSION=1` |
| `ahci-smoke` (`-device ich9-ahci` + scratch `ide-hd`; IDENTIFY/write/read-back-compare/flush/IDENTIFY-after-write/induced-TFES-recovery; BOHC/SSS/hot-plug skip-with-reason on QEMU, validated via VFIO) **and** `ahci-root-smoke` (real ext2 data disk routed to AHCI; asserts the root mounts off `ahci.block` end-to-end — virtio root absent → driver MBR/ext2 probe → owner-gate accept → `init: / mounted (ext2 via ring-3 ahci.block)` → login prompt) | `M3OS_AHCI_REGRESSION=1` |
| `mitigations-status-smoke` (Phase 84: boots + asserts the `[sec] mitigations=… global_kernel_ptes=0` boot-policy log and the `m3ctl mitigations status` reporter output — per-vuln Meltdown line + compiled-in retpoline line + UNADDRESSED enumeration; KPTI-independent default boot) | `M3OS_MITIGATIONS_REGRESSION=1` |
| `pkgcache-hit-check` (Phase 85a: second build of a warmed-cache port performs zero compiler invocations — pure `.m3pkg` hit; requires a musl cross-compiler for the initial warm build) | `M3OS_PKGCACHE_REGRESSION=1` |
| `pkg-smoke` (Phase 85a: boots m3OS, then exercises the offline `pkg` manager against the bundled `/usr/pkg/` repo — `install`/`list`/`verify`/`upgrade`/`remove` of the dependency-free `libevent` leaf (verifying `/usr/local/lib/libevent.a`), then `pkg install tmux` to prove the dependency solver auto-installs tmux's `libevent` dep; proves the `.m3pkg` round-trips build → image → in-OS install) | `M3OS_PKG_REGRESSION=1` |
| `git-local-smoke` (Phase 85b: cross-builds the local-only `git` + zlib into `.m3pkg`s, boots m3OS, `pkg install git` from the bundled `/usr/pkg/` repo — exercising the solver's `DEPS=zlib` auto-install — then drives a scripted local repo workflow: `init` → add/commit → edit + `diff` shows the change → second commit → `log --oneline` shows two commits → `checkout -b feature` + commit → `checkout main` + `merge feature` (both files tracked) → `status` reports a clean tree. Requires a musl cross-compiler for the initial git build) | `M3OS_GIT_REGRESSION=1` |
| `git-ssh-smoke` (Phase 86b: cross-builds the static client-only **dropbear** `ssh` (`dbclient`) into an `ssh.m3pkg`, reuses the Phase 85b `git` **unchanged**, builds a fresh image with both bundled into `/usr/pkg/` + GitHub's ed25519 host key seeded into `/root/.ssh/known_hosts`, boots m3OS, then over serial: `pkg install ssh` + `pkg install git` succeed, `dbclient -V` / `ssh -V` report `Dropbear v2024.86` (the static client *runs* on m3OS), and `cat /root/.ssh/known_hosts` shows the seeded GitHub `ssh-ed25519` key (TOFU data round-trips the VFS). The live `git clone --depth 1 --single-branch ssh://git@github.com/...` (via `GIT_SSH`) and the host-key-**mismatch reject** negative test are **opt-in** — `M3OS_GIT_SSH_NET=1` runs the mismatch reject (egress to `github.com:22`, no key needed — host key compared at KEX before auth); `M3OS_GIT_SSH_KEY=<path>` stages a GitHub-registered key as `/root/.ssh/id_dropbear` and runs the clone — and **skip-with-reason** when unconfigured (mirroring `tls-smoke`/`dns-smoke` PASS-vs-SKIP). The mismatch-reject step is a `WaitEither` that passes on the real reject (`host key mismatch for`) **or** the formerly-localized non-blocking-`connect` blocker (`Connect failed`) and fails on a silent accept; with non-blocking `connect` now implemented in Phase 86b (`EINPROGRESS`/poll-`POLLOUT`/`getsockopt(SO_ERROR)`, proven by the always-on `connect-smoke` in the main `smoke-test` flow), `M3OS_GIT_SSH_NET=1` **matches the real reject** live against `github.com:22` — dropbear's non-blocking `connect()` establishes, reaches KEX, and the planted bad key is rejected and left on disk. The bundled Phase 85b git `.m3pkg` is **reused, not rebuilt**. Requires a musl cross-compiler for the initial dropbear build) | `M3OS_GIT_SSH_REGRESSION=1` |
| `git-https-smoke` (Phase 86c: cross-builds the HTTPS transport chain — a trimmed client-only static **mbedTLS** 3.6.2 (`sys_getrandom` entropy, no `/dev/urandom`), a static `libcurl --with-mbedtls --with-ca-bundle` 8.15.0, and **git rebuilt WITH curl** (`NO_CURL` removed; `git-remote-https` + `curl_multi_perform` + `mbedtls_ssl_handshake` present, `SSL_CTX_new` absent), bundles curl/mbedtls/ca-certificates/git into `/usr/pkg/`, boots m3OS, then over serial: `pkg install git` pulls the whole `zlib → mbedtls → ca-certificates → curl → git` chain via the solver, `curl --version` reports `mbedTLS/3.6.2` (the static TLS stack *runs* on m3OS), `git config` shows `http.sslVerify=true` + `http.sslCAInfo=/etc/ssl/certs/ca-certificates.crt`, and the installed CA bundle's `Bundle of CA Root Certificates` header round-trips the VFS. The live **bad-cert REJECT** (`self-signed.badssl.com` → TLS fails closed) and the public `git clone https://github.com/octocat/Hello-World.git` (`info/refs` validated by the `application/x-git-upload-pack-advertisement` Content-Type + the `# service=git-upload-pack` pkt-line magic, then a packfile transfer) are **opt-in** — `M3OS_GIT_HTTPS_NET=1` runs both (egress to `:443`, **no secret** — the cert is checked before auth) — and **skip-with-reason** when unconfigured (mirroring `git-ssh-smoke`). PAT auth is configured via `credential.helper store` (documented), not exercised by the anonymous positive arm. Runs at `--timeout 5400` (the ~27 MB install over the slow VFS + packfile transfer). Requires a musl cross-compiler for the initial mbedtls/curl/git build) | `M3OS_GIT_HTTPS_REGRESSION=1` |
| `python-smoke` (Phase 85c: two-stage cross-builds a **fully static** CPython 3.12 (+ zlib) into a `.m3pkg`, boots m3OS, `pkg install python` from the bundled `/usr/pkg/` repo — exercising the solver's `DEPS=zlib` auto-install — then over serial: `python3 --version` reports 3.12.8, a `-c` run imports `json,re,math,datetime,argparse,hashlib,dataclasses,pathlib,os,secrets` + checks `sys.platform=linux` + a HACL `hashlib.sha256` digest + `os.urandom`/`secrets`, runs the bundled `/usr/src/fibonacci.py`, and round-trips a `/tmp` file write+read. Runs at `--timeout 900` because cold imports over the slow ring-3 VFS take minutes. Python is static — m3OS's `/lib/ld-musl` is a custom loader with no real `libc.so`, so a dynamic interpreter can't run. Requires a musl cross-compiler for the initial build) | `M3OS_PYTHON_REGRESSION=1` |
| `clang-smoke` (Phase 85d: host-cross-builds an opt-in, X86-only, statically musl-linked **clang + lld** (+ a self-contained libc++) into a `.m3pkg`, builds a fresh image with the `M3OS_WITH_CLANG` feature so it bundles into `/usr/pkg/`, boots m3OS, `pkg install clang` (no deps), then INSIDE m3OS: `clang --version` reports 18.1.8, `clang -print-resource-dir` resolves under `/usr` (relocation contract), `clang -O2 /usr/src/hello.c` compiles + links (lld) + **runs** (`CLANG_C_OK`), `clang++ /usr/src/hello.cpp` links the bundled libc++ + runs (`CLANG_CPP_OK`), and `clang -fuse-ld=lld` links via LLD. Runs at `--timeout 5400` because the ≈125 MB install (the installer reads + SHA-verifies the whole 124 MiB `.m3pkg`, then writes ~1500 files) and each cold 64 MiB-clang invocation over the ~200 KB/s ring-3 VFS take tens of minutes — a deliberately heavy opt-in gate. Clang is static + X86-only; the host clang is the cross-compiler since musl-tools has no C++ compiler. Requires host clang + cmake + ninja for the initial build) | `M3OS_CLANG_REGRESSION=1` |

The `tls-smoke`/`dns-smoke` gates assert the musl-built smoke stage actually
`PASS`ed rather than `SKIP`ped — a `SKIP` means the musl cross-compiler was
absent at build time, which would otherwise let the Track C PT_TLS/pthread and
Track D.1 DNS fixes ride **unverified**. Set them on branches touching the
kernel clone path, `PT_TLS` loader, futex `CHILD_CLEARTID` wake, or the DNS
resolver / `recvmsg` / ephemeral-UDP path (they require a musl cross-gcc).

## Architecture

Microkernel: ring 0 kernel handles memory management, scheduling, IPC, interrupt routing, and device drivers. Userspace processes run in ring 3 and communicate through IPC and syscalls.

```
Ring 0 (kernel/):                Ring 3 (userspace/):
  - Frame allocator                - init (PID 1 daemon)
  - Page table manager             - sh0 (built-in shell)
  - Scheduler (SMP-aware)          - coreutils (cat, ls, grep, etc.)
  - IPC engine + capabilities      - ping (ICMP network tool)
  - IDT / APIC / interrupt router  - edit (text editor)
                                   - login, su, passwd, adduser
                                   - id, whoami
                                   - ion shell (external)
  - Syscall gate
  - VFS + FAT32 + tmpfs
  - Network stack (IPv4/TCP/UDP)
  - Unix domain sockets (AF_UNIX)
  - VirtIO drivers (blk, net)
  - ACPI / PCI enumeration
  - Framebuffer console
  - TTY + signal handling
  - SMP (multi-core boot + IPI)
```

See `docs/appendix/codebase-map.md` for workspace crates, ports tree, and source layouts.

### Adding a New Userspace Binary

Adding a new userspace binary requires changes in **four** places. Missing any one of these causes the binary to either not be built, not be embedded in the kernel image, or not be found at runtime.

1. **Workspace member** — add the crate to `Cargo.toml` `members` list
2. **xtask build pipeline** — add to the `bins` array in `xtask/src/main.rs` (`build_userspace` function, ~line 141). Set `needs_alloc = true` if the crate depends on `alloc` (e.g., uses `kernel-core` or `Vec`/`Box`/`String`). If `needs_alloc` is true, the binary must define a `#[global_allocator]` (use `syscall_lib::heap::BrkAllocator`) and enable the `alloc` feature on `syscall-lib`.
3. **Ramdisk embedding** — add an `include_bytes!` static and a `BIN_ENTRIES` tuple in `kernel/src/fs/ramdisk.rs`. Generated binaries are staged by `xtask` under `target/generated-initrd/`; checked-in static initrd assets remain under `kernel/initrd/`. Without the ramdisk entry, `execve` returns ENOENT.
4. **Service config (if daemon)** — add a `.conf` file to the ext2 data disk builder in `xtask/src/main.rs` (`populate_ext2_files` function) AND to the `KNOWN_CONFIGS` fallback list in `userspace/init/src/main.rs`. Run `cargo xtask clean` to recreate the disk.

### Adding a New Cross-Compiled Port (ncurses-style)

Ports live under `ports/<category>/<name>/Portfile` and are built host-side by `cargo xtask port build <name>`, which dispatches to a `build_<name>` function in `xtask/src/port_build.rs`. **Every new `build_*` function MUST route through the shared musl-toolchain plumbing or it will fail on toolchains that ship without empty static-compat archives** (Arch `musl-cross-tools`, raiden, hand-built `musl-cross-make`, anything that omits `libdl.a` / `libpthread.a` / `librt.a`). The "C compiler cannot create executables" configure error during the link probe is the symptom.

Required wiring in every port `build_*` function:

1. **Resolve the toolchain via `musl_toolchain()`** — which calls the shared `crate::find_musl_cc()` probe. Never invoke `x86_64-linux-musl-gcc` as a literal string.
2. **Compose LDFLAGS with `musl_extra_ldflags_joined()`**:
   ```rust
   let extra_ld = musl_extra_ldflags_joined();
   let ldflags = if extra_ld.is_empty() {
       "-static -L<stage>/lib".to_string()
   } else {
       format!("-static -L<stage>/lib {extra_ld}")
   };
   ```
   The `extra_ld` value is `-L<workspace>/target/musl-stub-libs/` when xtask auto-generated the empty archives. Without that `-L`, the configure script's `-static -ldl -lpthread -lrt` link probe fails and the build aborts with exit 77.
3. **Pass `--host=x86_64-linux-musl`** to `./configure` so autotools picks the correct cross triple.
4. **Use the `(cc, ar, ranlib)` tuple from `musl_toolchain()`** for `CC` / `AR` / `RANLIB` — the tuple's `ar`/`ranlib` already fall back to host `ar`/`ranlib` when the cross variants are absent (static archives are ELF-target-agnostic so this is safe).

To register a new port: add the name to `PORTS` in `xtask/src/main.rs` (~line 17446), add it to the `match name` dispatch in `xtask/src/port_build.rs`'s `fn port_build` (~line 873), implement `build_<name>` following the pattern above, and add the resulting binary path to `tui_app_smoke_steps` if the port participates in the gate.

## Critical Conventions

### Target flags — do not remove

In `.cargo/config.toml` / target spec:

- `"disable-redzone": true` — hardware interrupts use the stack; removing this causes silent stack corruption
- `"-mmx,-sse"` — kernel stays soft-float (no XMM in ring 0 / IRQ handlers). NOTE: per-task FPU/XSAVE state *is* already preserved across context switches (Phase 57e/60); enabling **userspace** SIMD is a tracked future task — see `docs/research/simd-enablement.md`
- `"panic-strategy": "abort"` — no unwinding; panics halt the machine

### `no_std` everywhere in kernel and userspace

All crates under `kernel/` and `userspace/` are `#![no_std]`. Only use `alloc` types (`Vec`, `Box`, `Arc`) after heap initialization. `kernel-core` supports both `no_std` (kernel) and `std` (host tests) via feature flags.

### `unsafe` only at hardware boundaries

Acceptable only for: hardware register/port I/O, page table/GDT/IDT setup, `enter_userspace()`/`switch_context()` asm stubs, global allocator initialization, APIC/ACPI MMIO access, VirtIO ring manipulation. Always wrap in a safe abstraction immediately.

All crates use Rust **edition 2024** — the body of an `unsafe fn` is *not* implicitly unsafe. You must wrap unsafe operations in explicit `unsafe {}` blocks inside unsafe functions.

### IPC model — read the doc before touching `kernel/src/ipc/`

Synchronous rendezvous + async notification objects (seL4-style):

- Server-to-server: sync `call`/`reply_recv`
- IRQ/vsync: `Notification` objects (word-sized bitfield, safe to signal from interrupt handlers)
- Bulk data: page capability grants, never IPC payloads
- Userspace servers must never share writable memory

### Interrupt handlers

Do the minimum: read scancode / ack interrupt / push to ring buffer / send EOI. No allocation, no blocking, no IPC from within an interrupt handler.

### Capabilities

Integer index into the current process's `CapabilityTable`. Kernel validates every handle on every syscall. Transfer via `sys_cap_grant` — never forge or copy raw capability values.

### Syscall ABI

| Register | Role |
|---|---|
| `rax` | Syscall number (in) / return value (out) |
| `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` | Arguments 1–6 |

`rcx` and `r11` are clobbered by `syscall` — never use them for arguments.

### Context switch

`switch_context(current, next)` saves/restores only callee-saved registers (`rbx`, `rbp`, `r12`–`r15`, `rsp`, `rip`). Do not change without auditing every call site.

### SMP conventions

- BSP (bootstrap processor) completes full kernel init before waking APs
- APs initialize their own GDT, IDT, APIC, and enter the scheduler idle loop
- Use IPI for TLB shootdown on page table updates affecting multiple cores
- Per-CPU data accessed via APIC ID — avoid global mutable state without proper locking

### QEMU test exit convention

```rust
// Write to I/O port 0xf4 (isa-debug-exit device)
// QEMU exit codes: 0x21 = success, 0x23 = failure
const QEMU_EXIT_SUCCESS: u32 = 0x10;
const QEMU_EXIT_FAILURE: u32 = 0x11;
```

### Userspace-first rule

New high-level policy defaults to userspace. Before adding policy-heavy code to ring 0, check the architecture review checklist in `docs/appendix/architecture-and-syscalls.md`.

### `BootInfo` is read-only after init

Parse memory regions, framebuffer, RSDP during `kernel_main` init and store in typed kernel structures. Do not hold long-lived references to `BootInfo`.

## Key Crates

| Crate | Purpose |
|---|---|
| `bootloader_api` | Kernel entry point macro, `BootInfo` |
| `x86_64` | `PageTable`, `IDT`, `GDT`, `PhysAddr`/`VirtAddr`, port I/O |
| `uart_16550` | Serial port driver — primary debug output |
| `pic8259` | 8259 PIC init and EOI |
| `spin` | `Mutex`/`RwLock` for `no_std` |
| `log` | Logging facade; backend writes to serial |
| `kernel-core` | Shared pure-logic library, host-testable |

## Documentation in `docs/`

Before making significant changes to a subsystem, read the corresponding phase doc. Full index in `docs/appendix/codebase-map.md`. Roadmaps and task lists live in `docs/roadmap/`.

### Documentation templates — all docs must conform

All roadmap docs must follow the templates in `docs/appendix/doc-templates.md`. When creating or updating docs, use the matching template:

| Doc type | Template section | Required fields |
|---|---|---|
| Phase design doc | `docs/roadmap/NN-slug.md` | Status, Source Ref, Depends on, Builds on, Primary Components, Milestone Goal, Why This Phase Exists, Learning Goals, Feature Scope, Important Components and How They Work, How This Builds on Earlier Phases, Implementation Outline, Acceptance Criteria, Companion Task List, How Real OS Implementations Differ, Deferred Until Later |
| Phase task doc | `docs/roadmap/tasks/NN-slug-tasks.md` | Status, Source Ref, Depends on, Goal, Track Layout table, per-track sections with tasks containing File/Symbol/Why it matters/Acceptance, Documentation Notes |
| Roadmap README row | `docs/roadmap/README.md` | Phase, Theme, Primary Outcome, Status, Source Ref, Milestone link, Tasks link |

Rules:

- Never create a task doc without all template sections populated.
- Never create a design doc missing Status, Source Ref, Depends on, or Builds on.
- Task acceptance items must be concrete and measurable — no vague "works correctly".
- Each task must have File, Symbol, and Why it matters fields.
- Update the roadmap README row when creating or completing a phase.
