# Codebase Map

Reference file for workspace layout, source structure, and documentation index.
Reflects Phase 98 / kernel v0.97.0. Extracted from AGENTS.md to keep active guidance lean.

## Workspace Crates

**Regeneration command** — to get the authoritative list directly from `Cargo.toml`:

```bash
sed -n '/members = \[/,/\]/p' Cargo.toml | grep -oE '"[^"]+"' | tr -d '"'
```

When this file and `Cargo.toml` disagree, `Cargo.toml` wins.

### Core / build crates

```
kernel/                   # main OS kernel binary (no_std, x86_64-unknown-none)
kernel-core/              # shared pure-logic library — host-testable (no_std + std feature)
xtask/                    # build system and smoke harness (host, std)
pkg-format/               # .m3pkg format: pack/unpack/verify, content key (Phase 85a)
```

### Userspace — syscall lib and test binaries

```
userspace/syscall-lib/    # syscall wrapper library for all userspace Rust binaries
userspace/exit0/          # test binary: simple exit
userspace/fork-test/      # test binary: fork behavior
userspace/echo-args/      # test binary: argument echo
```

### Userspace — core daemons and tools

```
userspace/init/           # PID 1 init daemon + service supervisor
userspace/shell/          # sh0 built-in shell (binary: sh0)
userspace/ping/           # ICMP ping utility
userspace/ping6/          # ICMPv6 ping utility (Phase 91)
userspace/ipv6-smoke/     # IPv6 dual-stack smoke probe (Phase 91)
userspace/udp-smoke/      # UDP-path smoke probe
userspace/smoke-runner/   # drives smoke probes under xtask regression
userspace/edit/           # full-screen text editor (kibi-style)
userspace/login/          # login authentication daemon
userspace/su/             # switch user
userspace/passwd/         # change password
userspace/adduser/        # create user account
userspace/id/             # print user/group IDs
userspace/whoami/         # print current user
userspace/ktrace/         # kernel trace ring consumer (Phase 43b)
userspace/pty-test/       # PTY subsystem test
userspace/unix-socket-test/ # Unix domain socket test
userspace/thread-test/    # threading primitives test
userspace/crypto-lib/     # cryptography library (RustCrypto; AES-NI via cpufeatures)
userspace/crypto-test/    # crypto integration test + AES-NI benchmark
userspace/sshd/           # SSH server daemon (sunset integration)
userspace/async-rt/       # minimal async runtime for ring-3 services
userspace/coreutils-rs/   # Rust coreutils (echo, ls, cat, rm, mkdir, sort, sha256sum, …)
userspace/coreutils-tests/ # host-side tests for coreutils-rs
userspace/syslogd/        # system logging daemon
userspace/usb-logsink/    # USB log-sink daemon + GPT ext2 mount (Phase 96)
userspace/crond/          # cron scheduler daemon
```

### Userspace — core servers

```
userspace/console_server/ # console / framebuffer service
userspace/kbd_server/     # keyboard input service
userspace/stdin_feeder/   # stdin routing helper (console / keyboard split)
userspace/vfs_server/     # VFS policy service + ext2 engine
userspace/fat_server/     # FAT filesystem service backing vfs_server
userspace/net_server/     # UDP networking policy service
userspace/mouse_server/   # mouse input service
```

### Userspace — driver runtime lib and ring-3 drivers

```
userspace/lib/driver_runtime/  # shared driver IPC + DMA primitives
userspace/drivers/nvme/        # ring-3 NVMe driver (single-queue, IOMMU-routed)
userspace/drivers/e1000/       # ring-3 Intel 82540EM classic e1000 NIC driver
userspace/drivers/e1000e/      # ring-3 Intel e1000e NIC driver
userspace/drivers/igb/         # ring-3 Intel igb NIC driver
userspace/drivers/igc/         # ring-3 Intel igc NIC driver
userspace/drivers/r8169/       # ring-3 Realtek RTL8111/8168 NIC driver
userspace/drivers/r8125/       # ring-3 Realtek RTL8125 2.5G NIC driver
userspace/drivers/ure/         # ring-3 Realtek RTL8156 USB-Ethernet driver (Phase 96)
userspace/drivers/xhci/        # ring-3 xHCI USB host driver (MSI-X, TRB/event rings, hot-plug)
userspace/lib/usb-core/        # USB core library (enumeration, descriptors, class matching)
userspace/drivers/usbhub/      # USB hub walker (descriptor + per-port power/reset)
userspace/drivers/usb-hid/     # USB HID class driver (Boot + Report Protocol, LEDs)
userspace/drivers/usb-storage/ # USB mass storage (BOT/SCSI, /mnt/usb<n> mount, unmount-on-detach)
userspace/drivers/usb-net/     # USB CDC-ECM/NCM Ethernet class driver (Phase 92e)
userspace/drivers/usb-audio/   # USB audio class driver (UAC isoch OUT, Phase 92c)
userspace/drivers/usb-video/   # USB video class driver (UVC isoch IN, Phase 92c)
userspace/drivers/ac97/        # ring-3 Intel AC'97 audio driver
userspace/drivers/hda/         # ring-3 Intel HDA audio driver (CORB/RIRB, BDL stream)
userspace/drivers/ahci/        # ring-3 AHCI/SATA block driver (RemoteBlockDevice, Phase 82)
userspace/wifi-core/           # Wi-Fi policy + WPA2-PSK 4-way handshake
userspace/drivers/mt792x/      # ring-3 MediaTek mt792x Wi-Fi driver (MT7921/7922/7925)
```

### Userspace — driver smoke probes

```
userspace/nvme-crash-smoke/           # NVMe driver crash recovery smoke test
userspace/max-restart-smoke/          # service max-restart backoff smoke test
userspace/e1000-crash-smoke/          # e1000 driver crash recovery smoke test
```

### Userspace — GUI stack

```
userspace/lib/surface_buffer/         # shared framebuffer surface abstraction
userspace/display_server/             # compositor: framebuffer owner, focus dispatch, surface roles
userspace/gfx-demo/                   # simple graphics demo client
userspace/m3ctl/                      # OS admin tool (mitigations, display, audio, service control)
userspace/fb-takeover/                # framebuffer takeover client (DOOM Tier 3)
userspace/display-server-crash-smoke/ # display_server crash recovery smoke test
userspace/display-multi-client-smoke/ # display_server multi-client smoke test
userspace/grab-hook-smoke/            # input grab hook smoke test
userspace/session_manager/            # graphical session orchestrator (startup/shutdown lifecycle)
userspace/greeter/                    # GUI login client
userspace/wallpaper/                  # wallpaper compositor client
userspace/bar/                        # status bar compositor client
userspace/launcher/                   # application launcher compositor client
userspace/notifyd/                    # notification daemon compositor client
userspace/lockscreen/                 # lockscreen compositor client
```

### Userspace — audio

```
userspace/audio_server/        # audio policy server + 32-ch DMX→S16LE mixer
userspace/camera_server/       # camera capture server (UVC frames, Phase 92c)
userspace/lib/audio_client/    # audio client library
userspace/lib/audio_mixer/     # audio mixer library
userspace/lib/audio_client_ffi/  # audio client FFI shim (C-compatible)
userspace/audio-demo/          # audio demo / tone generator
userspace/audio-stats/         # audio statistics display
userspace/bell-test/           # terminal bell test
```

### Userspace — terminal

```
userspace/term/                # terminal emulator: UTF-8 + TTF/Nerd Font, ANSI, PTY client
```

### Userspace — lib crates

```
userspace/lib/shadow/          # shadow password file library
userspace/lib/display_client_ffi/  # display_server client FFI shim
userspace/lib/layout/          # GUI layout primitives
userspace/lib/desktop_client/  # desktop session client library
```

### Userspace — security / policy tools

```
userspace/ld-musl-x86_64.so.1/  # from-scratch Rust PT_INTERP loader (dlopen/dlsym, PLT, TLS)
userspace/pkg/                   # offline in-OS package manager (install/remove/upgrade/verify)
```

### Userspace — test / smoke / probe binaries

```
userspace/crash_stub/          # minimal crash fixture for restart-policy testing
userspace/tui-smoke/           # TUI app smoke gate (ncurses apps via xtask)
userspace/tcsmoke/             # termios/line-discipline smoke test
userspace/winsize-bang/        # TIOCGWINSZ/TIOCSWINSZ test
userspace/sendmsg-test/        # sys_sendmsg / sys_recvmsg test
userspace/page-grant-test/     # IPC page-capability grant test
userspace/wx-violation/        # W^X enforcement negative test
userspace/pku-smoke/           # PKU alloc/fault/asym/sigframe/W^X-v2 smoke (Phase 90a)
userspace/usb-mount-smoke/     # USB mass storage mount smoke test
userspace/kstack-overflow-test/ # kernel stack overflow controlled-kill test (Track D)
userspace/epoll-smoke/         # epoll/eventfd smoke test
userspace/doom-concurrent/     # DOOM concurrent input stress test
userspace/vfs-throughput-probe/ # VFS I/O throughput probe (Phase 95c)
```

### Non-member crates on disk

```
userspace/
  telnetd/              # Telnet server daemon (retained; not in workspace.members)
  coreutils/            # C musl coreutils (superseded by coreutils-rs)
  demo-project/         # Multi-file C demo (Phase 32 make testing)
  hello-c/              # C hello world fixture
  signal-test/          # C signal handling fixture
  stdin-test/           # C stdin fixture
  tmpfs-test/           # C tmpfs fixture
  mmap-leak-test/       # memory-map leak regression
  doom/                 # DOOM port (built via xtask-specific path)
  hello-rust/           # musl Rust std hello world (Phase 44)
  sysinfo-rust/         # musl Rust std sysinfo (Phase 44)
  httpd-rust/           # musl Rust std HTTP server (Phase 44)
  calc-rust/            # musl Rust std calculator (Phase 44)
  todo-rust/            # musl Rust std todo list (Phase 44)
```

## Ports Tree Layout

Regeneration: `ls ports/*/` to see all categories and port names.

```
ports/
  port.sh               # port command (installed at /usr/bin/port)
  core/
    sbase/              # suckless Unix tools (basename, seq, rev, …)
  lang/
    go/                 # Go 1.24 static runtime (Phase 86d)
    llvm/               # LLVM/Clang/LLD — reused sysroot for clang + rustc ports
    lua/                # Lua 5.4.7 scripting language
    node/               # Node.js 22 LTS jitless + JIT variants (Phase 89/90a)
    python/             # CPython 3.12 fully-static (Phase 85c)
    python-dynamic/     # CPython 3.12 dynamic (Phase 93; DEPS=musl)
    rust/               # rustc 1.96.0 dynamic musl (Phase 95; DEPS=musl)
  lib/
    ca-certificates/    # Mozilla CA bundle (Phase 86a)
    libevent/           # libevent (tmux dependency)
    libffi/             # libffi (dynamic Python ctypes, Phase 93)
    mbedtls/            # mbedTLS 3.6.2 static (Phase 86c)
    musl/               # musl 1.2.5 --enable-shared libc.so (Phase 93)
    ncurses/            # ncurses wide (tmux/htop/less dependency)
    zlib/               # zlib 1.3.1 compression library
  math/
    bc/                 # bc arbitrary-precision calculator
  util/
    claude-code/        # @anthropic-ai/claude-code@2.1.112 .m3pkg (Phase 90b)
    coreutils/          # uutils/coreutils 0.9.0 musl static multicall (Phase 94)
    curl/               # libcurl 8.15.0 --with-mbedtls static (Phase 86c)
    dropbear/           # Dropbear SSH client dbclient (Phase 86b)
    gh/                 # GitHub CLI gh 2.82.1 static Go (Phase 86e)
    git/                # git (local-only 85b; HTTPS-capable 86c via curl/mbedtls)
    htop/               # htop process monitor (ncurses)
    less/               # less pager (ncurses)
    minizip/            # minizip (zlib test port)
    tmux/               # tmux terminal multiplexer (ncurses + libevent)
  <category>/<program>/
    Portfile            # metadata: NAME, VERSION, DESCRIPTION, CATEGORY, DEPS
    Makefile            # targets: fetch, patch, build, install, clean
    src/                # bundled source (or fetched)
    patches/            # m3OS-specific patches
```

## Kernel Source Layout

```
kernel/src/
  main.rs              # entry point, boot sequence
  lib.rs               # crate root (no_std)
  serial.rs            # serial I/O + log backend (COM1)
  pipe.rs              # inter-process pipes
  pty.rs               # PTY pair table and lifecycle
  rtc.rs               # CMOS real-time clock driver
  signal.rs            # POSIX-style signal handling (sigaction, sigframe, sigreturn)
  stdin.rs             # stdin abstraction
  tty.rs               # TTY/terminal subsystem
  epoll.rs             # epoll (EPOLLET, EPOLLRDHUP, epoll_pwait)
  eventfd.rs           # eventfd2
  timerfd.rs           # timerfd (CLOCK_MONOTONIC, CLOCK_REALTIME)
  flock.rs             # advisory file locking (flock/fcntl F_SETLK)
  mitigations.rs       # Spectre/Meltdown mitigation policy (mitigations=off|auto|full)
  trace.rs             # per-core lock-free kernel trace ring
  fwcfg.rs             # QEMU fw_cfg device (test exit, debug)
  panic_diag.rs        # enriched panic / fault handler diagnostics
  testing.rs           # QEMU ISA-debug-exit test framework
  test_prelude.rs      # test prelude (no_std test helpers)
  arch/x86_64/         # GDT, IDT, APIC, syscall gate, XSAVE, SMEP/SMAP, PKU, PAT, microcode
  acpi/                # ACPI table parsing (RSDP, MADT, MCFG, DMAR/IVRS)
  blk/                 # block devices: virtio-blk, MBR, remote block façade
  fb/                  # framebuffer console driver
  fs/                  # VFS layer, ext2 engine, FAT32, tmpfs, ramdisk, procfs, metacache
  iommu/               # IOMMU substrate: VT-d (intel.rs), AMD-Vi (amd.rs), fault ISRs, per-device registry
  ipc/                 # endpoints, capabilities, messages, notifications, page grants, registry
  mm/                  # frame allocator, paging, heap, slab, shm, DMA, ELF loader, PKU (pkey.rs), user_space
  net/                 # IPv4/IPv6, ARP, NDP, ICMP/ICMPv6, TCP, UDP, DHCP/DHCPv6, AF_UNIX, dispatch
  pci/                 # PCI/PCIe enumeration, BAR mapping
  process/             # process management: fork, exec, exit, wait, threads, futex
  smp/                 # AP boot (boot.rs), IPI (ipi.rs), TLB shootdown (tlb.rs)
  syscall/             # syscall dispatch (mod.rs), device-host gate (device_host.rs), network (net.rs)
  task/                # scheduler (SMP-aware), blocking mutex, kstack, wait queues, watchdog
  time/                # CLOCK_MONOTONIC/REALTIME, TSC calibration
kernel/initrd/           # static initrd assets checked into source
target/generated-initrd/ # xtask-staged generated binaries embedded by ramdisk
```

## kernel-core Source Layout

```
kernel-core/src/
  lib.rs               # module declarations
  types.rs             # shared types
  buddy.rs             # buddy frame allocator
  slab.rs              # slab cache + magazine layer
  size_class.rs        # size-class allocator
  time.rs              # time conversion
  fb.rs                # framebuffer abstractions
  pipe.rs              # pipe abstractions
  pty.rs               # PTY pair state, ring buffers
  tty.rs               # TTY abstractions
  epoll.rs             # epoll model
  eventfd.rs           # eventfd model
  timerfd.rs           # timerfd model
  pkey.rs              # PKU pkey model
  trace_ring.rs        # trace ring model
  log_ring.rs          # log ring
  address_space.rs     # typed AddressSpace abstraction
  cred.rs              # credentials (UID/GID)
  csprng.rs            # CSPRNG (RDRAND/RDSEED seeded, Phase 86a)
  spectre.rs           # Spectre mitigation model
  kpti.rs              # KPTI model / scaffold
  e1000.rs             # e1000 NIC model
  r8169.rs             # r8169 NIC model
  nic_ids.rs           # NIC device-ID registry
  nvme.rs              # NVMe command model
  pci.rs               # PCI model
  preempt_frame.rs     # preemption trap-frame model
  preempt_model.rs     # preemption model
  sched_model.rs       # scheduler model
  xsave_model.rs       # XSAVE component model
  watchdog_policy.rs   # watchdog policy
  utf8.rs              # UTF-8 stream decoder
  mm.rs                # memory model helpers
  user_range.rs        # user-space address range validation
  cross_cpu_free.rs    # cross-CPU slab free
  magazine.rs          # magazine allocator
  microcode.rs         # CPU microcode model
  audio/               # audio IPC protocol (driver_ipc::audio)
  device_host/         # device-host capability model
  display/             # display protocol model
  driver_ipc/          # driver IPC seams (audio, NIC, block)
  driver_runtime/      # driver runtime model
  elf/                 # ELF model
  font/                # font model
  fs/                  # ext2, FAT32, MBR, tmpfs, VFS protocol, LRU cache
  hda/                 # HDA codec model
  init/                # init protocol model
  input/               # input event model
  iommu/               # IOMMU model
  ipc/                 # capability, message, registry, bound-notification model
  mt792x/              # mt792x Wi-Fi model
  net/                 # IPv4/IPv6, ARP, NDP, ICMP/ICMPv6, DHCP/DHCPv6, TCP, UDP, msghdr
  session/             # session model
  storage/             # AHCI, ATA model
  usb/                 # USB model: xHCI, HID Report, hub, CDC, mass-storage, UAC, UVC, enumerate
```

## Documentation Index

Read the relevant doc before making significant changes to that subsystem.

| File | When |
|---|---|
| `docs/appendix/architecture-and-syscalls.md` | Orientation — kernel vs. userspace split, syscall ABI |
| `docs/02-memory.md` | Before touching frame allocator, page tables, or heap |
| `docs/06-ipc.md` | Before touching `kernel/src/ipc/` or syscalls |
| `docs/08-storage-and-vfs.md` | Before touching `kernel/src/fs/` or block devices |
| `docs/appendix/testing.md` | Before writing kernel tests or modifying the xtask harness |
| `docs/11-elf-loader-and-process-model.md` | Before touching ELF loading or process lifecycle |
| `docs/12-posix-compatibility-layer.md` | Before adding syscalls or POSIX behavior |
| `docs/16-network.md` | Before touching `kernel/src/net/` |
| `docs/91-ipv6-dhcpv6.md` | Before touching `kernel/src/net/ipv6.rs`/`icmpv6.rs`/`ndp.rs`/`dhcpv6.rs` (and their `kernel-core` siblings) |
| `docs/92-usb-class-expansion.md` | Before touching the USB class drivers (`userspace/drivers/usb-{storage,audio,video,net,hid}/`, `usbhub`), the xHCI host stack (`userspace/drivers/xhci/`), or their `kernel-core/src/usb/{cdc,hub,hid_report,mass_storage,uac,uvc}.rs` siblings |
| `docs/93-dynamic-c-runtime.md` | Before touching `userspace/ld-musl-x86_64.so.1/` (the Rust loader), `ports/lib/musl/` (the companion `libc.so`), `ports/lib/libffi/`, `ports/lang/python-dynamic/`, or the kernel `mremap`/`arch_prctl` paths a dynamic libc exercises |
| `docs/94-rust-cargo-uutils.md` | Before touching `build_uutils` in `xtask/src/port_build.rs`, the `ports/util/coreutils/Portfile`, the `x86_64-unknown-linux-musl` Rust musl port class, or the `pkg-format` symlink round-trip |
| `docs/95-native-rust-toolchain.md` | Before touching `build_rust` in `xtask/src/port_build.rs`, the `ports/lang/rust/Portfile`, the userspace Rust sysroot/target + bundled `rust-lld`, the `M3OS_WITH_RUST` bundling, or `cmd_rustc_smoke` |
| `docs/95b-on-device-rustc.md` | Before touching the `ld-musl` loader's per-DSO load path (`userspace/ld-musl-x86_64.so.1/src/main.rs` `load_dso`/`load_dso_impl`), the kernel lazy file-backed mmap (`MAP_LAZY_FILE`, `sys_mmap_file_backed`), the page-fault demand-fill (`demand_map_vma_page` / `demand_map_user_page_from_buf_locked` / `shared_vma_demand_file` / `demand_read_file_page`), or the demand-fault TLB-shootdown skip |
| `docs/96-bare-metal-usb-ethernet.md` | Before touching the `ure` USB-Ethernet driver (`userspace/drivers/ure/`), the xHCI bulk-transfer consumer (`userspace/drivers/xhci/src/server.rs`) or bulk EP contexts (`kernel-core/src/usb/enumerate.rs`), the framebuffer write-combining path (`kernel/src/arch/x86_64/pat.rs`), the built-in PS/2 keyboard init (`kernel/src/arch/x86_64/ps2.rs` `init_keyboard`), the LAPIC/PIT calibration or COM1 RX drain bounds (`apic.rs` `calibrate_lapic_timer` / `serial.rs` `drain_uart_rx_locked`), the `usb-logsink` daemon + GPT mount probe (`userspace/usb-logsink/`, `usb_ext2_base_lba`), or the bare-metal `BUILTIN_CONFIGS` daemon wiring (`userspace/init/src/main.rs`) |
| `docs/19-signal-handlers.md` | Before touching signal delivery |
| `docs/22-tty-terminal.md` | Before touching TTY/terminal subsystem |
| `docs/25-smp.md` | Before touching SMP or multi-core code |
| `docs/26-text-editor.md` | Before touching the edit binary or userspace heap allocator |
| `docs/29-pty-subsystem.md` | Before touching PTY pairs, session management, or controlling terminals |
| `docs/30-telnet-server.md` | Before touching telnetd, socket refcounting, or network server architecture |
| `docs/32-build-tools.md` | Before touching make/pdpmake, ar, build utilities, or demo project |
| `docs/33-kernel-memory.md` | Before touching buddy allocator, slab caches, munmap, or meminfo |
| `docs/34-timekeeping.md` | Before touching RTC, clock_gettime, gettimeofday, or time conversion |
| `docs/roadmap/39-unix-domain-sockets.md` | Before touching Unix domain sockets, AF_UNIX, socketpair, or `kernel/src/net/unix.rs` |
| `docs/roadmap/42-crypto-primitives.md` | Before touching crypto-lib, sha256sum, genkey, or RustCrypto integration |
| `docs/roadmap/43-ssh-server.md` | Before touching sshd, sunset integration, host keys, or SSH authentication |
| `docs/43a-crash-diagnostics.md` | Before touching panic_diag, fault handler diagnostics, or scheduler/fork/IPC assertions |
| `docs/43b-kernel-trace-ring.md` | Before touching trace_ring, trace events, per-core trace rings, or sys_ktrace |
| `docs/43c-regression-stress-ci.md` | Before touching xtask regression/stress commands, CI workflows, or proptest/loom tests |
| `docs/roadmap/44-rust-cross-compilation.md` | Before touching musl Rust cross-compilation, xtask musl Rust builds, or custom target specs |
| `docs/roadmap/45-ports-system.md` | Before touching ports tree, port command, Portfile format, or xtask ports integration |
| `docs/roadmap/46-system-services.md` | Before touching init service manager, syslogd, crond, service command, or sys_reboot |
| `docs/appendix/sunset-local-fork.md` | Before modifying sunset-local/ or the sshd session event loop |
| `docs/roadmap/README.md` | Open design questions and per-phase scope |

Phase-specific roadmaps and task lists live in `docs/roadmap/`, with corresponding `docs/roadmap/tasks/` breakdowns.
