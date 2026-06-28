# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**m3OS** (technical name: `m3os`) is a bootable microkernel OS in Rust: x86_64, UEFI boot, kernel **v0.98.0**. Ring 0 handles memory, scheduling, IPC/capabilities, interrupt routing, and in-kernel drivers; ring 3 hosts everything else.

Capabilities now present in the tree:

- **Userspace**: init (PID 1), shell (sh0) + ion, coreutils, multi-user login, editor, service manager, PTY, telnet/SSH servers, crypto. See `docs/appendix/codebase-map.md`.
- **Networking & storage**: IPv4/IPv6 dual-stack TCP/UDP + AF_UNIX sockets; NVMe + AHCI/SATA ring-3 block drivers; Intel + Realtek NIC ring-3 drivers on a VirtIO baseline. See `docs/roadmap/README.md`.
- **Wireless**: ring-3 MediaTek mt792x Wi-Fi driver (MT7921/7922/7925); soft-MAC 802.11 + WPA2-PSK; VFIO/bare-metal validated. See `docs/roadmap/README.md`.
- **IOMMU substrate**: ACPI DMAR/IVRS parsing, per-device VT-d / AMD-Vi domains, IOMMU-routed `DmaBuffer<T>`, fault ISRs, VT-d queued invalidation.
- **Ring-3 driver hosting**: capability-gated device-host syscalls; supervised userspace NVMe/e1000 with `RemoteBlockDevice`/`RemoteNic` facades.
- **USB host stack**: ring-3 xHCI + `usb-core` with HID (Boot+Report), mass storage, USB audio/video (isoch), USB-Ethernet (CDC-ECM/NCM), hub, hot-plug, and multi-controller concurrency. See `docs/roadmap/README.md`.
- **Package management**: content-addressed `.m3pkg` substrate + `pkg` in-OS installer with transitive dependency solver; cross-compiled ports include git, Python, Clang, Go, Node.js, Claude Code, coreutils, rustc. See `docs/roadmap/README.md`.
- **Graphical stack**: `display_server` (framebuffer, focus, damage, animations), `kbd_server`/`mouse_server`, compositor clients (wallpaper, bar, launcher, notifyd, lockscreen), `greeter` GUI login, `session_manager`. See `docs/roadmap/README.md`.
- **Audio**: ring-3 `ac97` + `hda` drivers behind a `driver_ipc::audio` seam; `audio_server` mixer (32-ch DMX→S16LE) forwards PCM over `sys_shm`. See `docs/roadmap/README.md`.
- **Terminal**: `term` emulator, full termios/line-discipline, UTF-8 + TTF/Nerd Font glyphs, ncurses + less/htop/tmux ports. See `docs/roadmap/README.md`.
- **Dynamic linking & a real `libc.so`**: Rust `ld-musl-x86_64.so.1` loader (PT_INTERP, dlopen/dlsym, PLT lazy resolve) + upstream musl 1.2.5 `libc.so`; enables dynamically-linked C and Python. See `docs/roadmap/README.md`.
- **CPU hardening**: SMEP/SMAP, Spectre-v2 retpoline + `IA32_SPEC_CTRL`, W^X v2 via x86 PKU (`pkey_alloc`/`pkey_mprotect`), AES-NI in userspace. See `docs/roadmap/README.md`.

> **Phase history is NOT maintained here.** For the detailed per-phase record (Phase 55a → 76d and onward) and the full workspace/source layout, read `docs/roadmap/README.md` and `docs/appendix/codebase-map.md`. Read the relevant phase doc under `docs/roadmap/` before changing a subsystem.

> **Maintenance policy for this file — keep it small.** This file loads on every session, so its size is a recurring token cost. Do **not** append phase summaries, changelogs, or implementation diaries here — that record belongs in `docs/roadmap/`. When a phase lands, bump the single workspace version in `[workspace.package]` in the root `Cargo.toml` and update the version in the header above. All other version lines in the tree are `version.workspace = true` and update automatically. See `docs/appendix/versioning-reform.md` for the full migration spec. Add a bullet to the capability inventory **only if it introduces a new capability class** (not for changes within an existing one). Prefer rewriting an existing bullet over adding prose. If a section starts listing internal symbols or per-change detail, move it to `docs/` and link instead.

## Build & Run

Uses the `xtask` pattern — always build through `cargo xtask`, never `cargo build` directly.

```bash
cargo xtask run          # build + launch in QEMU (headless, serial output)
cargo xtask run --fresh  # same, but recreate data disk first
cargo xtask run-gui      # build + launch in QEMU (GUI with framebuffer)
cargo xtask run-gui --fresh  # same, but recreate data disk first
cargo xtask image        # build bootable disk image (UEFI raw + VHDX)
cargo xtask image --sign # build + sign EFI binary for Secure Boot
cargo xtask check        # clippy (-D warnings) + rustfmt + all host-side unit tests
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

This sets `core.hooksPath` to `.githooks/`. **pre-commit** runs `cargo xtask check`. **pre-push** runs `cargo xtask check` + `smoke-test` + `regression`, plus these opt-in gates when their env var is set. Full per-gate descriptions live in [`docs/appendix/regression-gates.md`](docs/appendix/regression-gates.md).

| Gate | Env var | One-line purpose |
|---|---|---|
| `ssh-e1000-banner-check` | `M3OS_E1000_REGRESSION=1` | e1000 NIC initializes; SSH server banner answers on TCP/22. |
| `doom-audio-smoke` | `M3OS_DOOM_AUDIO_REGRESSION=1` | DOOM plays non-silent PCM through the ac97/hda `audio_server` mixer. |
| `termios-smoke` | `M3OS_TERMIOS_REGRESSION=1` | PTY/termios/line-discipline correct (raw mode, SIGWINCH, ICANON). |
| `tui-app-smoke` | `M3OS_TUI_APP_REGRESSION=1` | ncurses TUI apps (htop, tmux, less) render correctly over the terminal. |
| `doom-concurrent-smoke` | `M3OS_DOOM_CONCURRENT_REGRESSION=1` | DOOM runs concurrently with other ring-3 processes; no scheduler starvation. |
| `tiling-smoke` | `M3OS_TILING_REGRESSION=1` | `display_server` tiling layout and multi-window compositor work correctly. |
| `htop-render-probe` | `M3OS_HTOP_REGRESSION=1` | htop process list renders visible rows on the QMP/PPM framebuffer dump. |
| `xhci-bringup-smoke` + `xhci-enum-smoke` + `usb-smoke` + `usb-report-smoke` + `usb-hotplug-smoke` + `usb-storage-smoke` + `usb-hub-smoke` + `usb-mount-smoke` + `usb-unmount-smoke` + `usb-storage-dual-smoke` + `usb-multi-controller-smoke` + `usb-eth-smoke` | `M3OS_USB_REGRESSION=1` | Full xHCI USB suite: HID (Boot+Report), mass-storage, hub, hot-plug, multi-controller, USB-Ethernet. |
| `tls-smoke` PASS (not SKIP) | `M3OS_TLS_REGRESSION=1` | PT_TLS/pthread smoke PASSED (not skipped); musl cross-compiler was present at build. |
| `dns-smoke` PASS (not SKIP) | `M3OS_DNS_REGRESSION=1` | DNS/recvmsg smoke PASSED (not skipped); musl cross-compiler was present at build. |
| `multi-nic-smoke` | `M3OS_MULTI_NIC_REGRESSION=1` | e1000 + e1000e + igb NICs all initialize and register in the multi-NIC registry. |
| `ure-smoke` | `M3OS_URE_REGRESSION=1` | RTL8156 USB-Ethernet dongle brings up via `ure` driver on real silicon; skip without dongle. |
| `hda-smoke` | `M3OS_HDA_REGRESSION=1` | Intel HD Audio codec + `hda-duplex` drives a non-silent WAV output stream. |
| `usb-audio-smoke` | `M3OS_USB_AUDIO_REGRESSION=1` | USB UAC speaker receives PCM over isochronous OUT TRBs; captured WAV is non-silent. |
| `wifi-smoke` | `M3OS_WIFI_REGRESSION=1` | mt792x Wi-Fi driver loads; radio path VFIO-validated only (always skip-with-reason in CI). |
| `ahci-smoke` + `ahci-root-smoke` + `ahci-rw-smoke` + `ahci-persist-smoke` | `M3OS_AHCI_REGRESSION=1` | AHCI ring-3 suite: IDENTIFY/RW/flush, ext2 root mount, write round-trip, reboot-persistence. |
| `mitigations-status-smoke` | `M3OS_MITIGATIONS_REGRESSION=1` | `m3ctl mitigations status` reports correct Spectre-v2/retpoline/KPTI posture at boot. |
| `pkgcache-hit-check` | `M3OS_PKGCACHE_REGRESSION=1` | Second port build hits `.m3pkg` cache with zero compiler invocations. |
| `pkg-smoke` | `M3OS_PKG_REGRESSION=1` | In-OS `pkg` manager install/list/verify/upgrade/remove + transitive dependency solver works. |
| `git-local-smoke` | `M3OS_GIT_REGRESSION=1` | Static git + pkg solver runs local init/commit/branch/merge workflow in-OS. |
| `git-ssh-smoke` | `M3OS_GIT_SSH_REGRESSION=1` | dropbear SSH client installs and runs in-OS; live clone/mismatch-reject is opt-in. |
| `git-https-smoke` | `M3OS_GIT_HTTPS_REGRESSION=1` | mbedTLS+curl+git chain installs; TLS cert verify + live HTTPS clone is opt-in. |
| `python-smoke` | `M3OS_PYTHON_REGRESSION=1` | Static CPython 3.12 installs; stdlib imports, sha256, and file I/O run in-OS. |
| `coreutils-smoke` | `M3OS_COREUTILS_REGRESSION=1` | uutils/coreutils 0.9.0 multicall installs; GNU-compat battery + inode-identity passes. |
| `clang-smoke` | `M3OS_CLANG_REGRESSION=1` | Clang 18 + lld installs; compiles + links C and C++ natively in-OS. |
| `rustc-smoke` | `M3OS_RUST_REGRESSION=1` | Dynamic musl rustc 1.96.0 installs; `rustc hello.rs` compiles and runs (KVM-gated). |
| `go-runtime-smoke` | `M3OS_GO_REGRESSION=1` | Static Go 1.24 runtime starts, spawns goroutine, completes HTTP GET over TCP stack. |
| `gh-smoke` | `M3OS_GH_REGRESSION=1` | Static `gh` 2.82.1 runs in-OS; authenticated `gh pr list` / `gh issue create` opt-in. |
| `node-smoke` | `M3OS_NODE_REGRESSION=1` | Jitless Node 22 installs; local JS runtime + HTTP GET over in-kernel TCP always-on. |
| `userspace-simd-smoke` | `M3OS_SIMD_REGRESSION=1` | AES-NI + SSE binary runs fault-free in ring-3; kernel ELF confirmed to contain no XMM. |
| `pku-smoke` | `M3OS_PKU_REGRESSION=1` | PKU alloc/deny-fault/sigframe/W^X-v2 matrix passes; SKIPs on a no-PKU CPU. |
| `kstack-overflow-smoke` | `M3OS_KSTACK_OVERFLOW_REGRESSION=1` | Kernel-stack overflow kills the offending child via SIGSEGV; parent keeps running. |
| `smp-smoke` | `M3OS_SMP_REGRESSION=1` | 256 futex-heavy async ops complete across 8 cores (Phase 99 default, `M3OS_SMP=<N≥2>`-overridable; CI 2-vCPU runners set `M3OS_SMP=2`); no TLB-shootdown panics or lost wakeups. |
| `node-jit-smoke` | `M3OS_NODE_JIT_REGRESSION=1` | JIT Node: V8 TurboFan + WASM execute under W^X v2 PKU guard (requires KVM + PKU CPU). |
| `claude-smoke` | `M3OS_CLAUDE_REGRESSION=1` | claude-code 2.1.112 installs (DEPS=node), CLI runs; TUI render arm requires KVM + JIT node. |
| `vfs-throughput-smoke` | `M3OS_VFS_THROUGHPUT_REGRESSION=1` | 8 MiB VFS write+read IPC-call count stays under coalescing-path regression ceilings. |
| `vfs-bulkio-smoke` | `M3OS_VFS_BULKIO_REGRESSION=1` | mbedtls install read/write block-call deltas stay under thresholds after coalescing. |
| `ipv6-smoke` | `M3OS_IPV6_REGRESSION=1` | IPv6 link-local, NDP Neighbor Advertisement, AF_INET6 sockets, ICMPv6, TCP/UDP all pass. |
| `dynamic-hello-smoke` (+ `dynamic-python-smoke` opt-in) | `M3OS_DYNAMIC_C_REGRESSION=1` | Dynamic C binary loads via PT_INTERP + libc.so + dlopen; TLS and thread-fault arms pass. |

## Architecture

Microkernel: ring 0 kernel handles memory management, scheduling, IPC, interrupt routing, and device drivers. Userspace processes run in ring 3 and communicate through IPC and syscalls.

See `docs/appendix/codebase-map.md` for workspace crates, ports tree, and source layouts.

### Adding a New Userspace Binary

Adding a new userspace binary requires changes in **four** places. Missing any one of these causes the binary to either not be built, not be embedded in the kernel image, or not be found at runtime.

1. **Workspace member** — add the crate to `Cargo.toml` `members` list
2. **xtask build pipeline** — add to the `bins` array in `xtask/src/main.rs` (`build_userspace` function, ~line 141). Set `needs_alloc = true` if the crate depends on `alloc` (e.g., uses `kernel-core` or `Vec`/`Box`/`String`). If `needs_alloc` is true, the binary must define a `#[global_allocator]` (use `syscall_lib::heap::BrkAllocator`) and enable the `alloc` feature on `syscall-lib`.
3. **Ramdisk embedding** — add an `include_bytes!` static and a `BIN_ENTRIES` tuple in `kernel/src/fs/ramdisk.rs`. Generated binaries are staged by `xtask` under `target/generated-initrd/`; checked-in static initrd assets remain under `kernel/initrd/`. Without the ramdisk entry, `execve` returns ENOENT.
4. **Service config (if daemon)** — add a `.conf` file to the ext2 data disk builder in `xtask/src/main.rs` (`populate_ext2_files` function) AND to the `KNOWN_CONFIGS` fallback list in `userspace/init/src/main.rs`. Run `cargo xtask clean` to recreate the disk.

### Adding a New Cross-Compiled Port (ncurses-style)

Ports live under `ports/<category>/<name>/Portfile` and are built host-side by `cargo xtask port build <name>`, which dispatches to a `build_<name>` function in `xtask/src/port_build.rs`. Use `cargo xtask port list` to see every Portfile (version, deps, whether it has a host build recipe, whether it's built on this machine), and `cargo xtask port build all` to build the whole recipe set in `DEPS=`-topological order (skipping pkgcache hits, skipping dependents of a failed port, with a PASS/FAIL/SKIP summary). **Every new `build_*` function MUST route through the shared musl-toolchain plumbing or it will fail on toolchains that ship without empty static-compat archives** (Arch `musl-cross-tools`, raiden, hand-built `musl-cross-make`, anything that omits `libdl.a` / `libpthread.a` / `librt.a`). The "C compiler cannot create executables" configure error during the link probe is the symptom.

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
- `"-mmx,-sse"` — the **kernel** (`x86_64-unknown-none`) stays soft-float (no XMM in ring 0 / IRQ handlers); per-task FPU/XSAVE save/restore (`xsaveopt64`/`xsave64` around `switch_context`, Phase 57e/60) handles the task boundary. **Userspace** (`x86_64-m3os.json`) builds hardware-float SSE/SSE2 + AES-NI (Phase 86f); the two targets are deliberately decoupled so IRQ/exception handlers never emit XMM while ring-3 code gets hardware AES-NI.
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

### Documentation templates

All roadmap and appendix docs must follow the templates in
[`docs/appendix/doc-templates.md`](docs/appendix/doc-templates.md).
