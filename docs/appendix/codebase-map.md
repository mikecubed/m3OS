# Codebase Map

Reference file for workspace layout, source structure, and documentation index.
Extracted from AGENTS.md to keep active guidance lean.

## Workspace Crates

```
Cargo.toml                # workspace root (default-members = ["kernel"])
kernel/                   # main OS kernel binary (no_std)
kernel-core/              # shared library — host-testable pure logic (no_std + std feature)
xtask/                    # build system (host, std)
userspace/
  syscall-lib/            # syscall wrapper library for userspace Rust binaries
  exit0/                  # test binary: simple exit
  fork-test/              # test binary: fork behavior
  echo-args/              # test binary: argument echo
  init/                   # PID 1 init daemon
  shell/                  # sh0 shell (binary name: sh0)
  ping/                   # ICMP ping utility
  edit/                   # full-screen text editor (kibi-style)
  login/                  # login authentication (Phase 27)
  su/                     # switch user (Phase 27)
  passwd/                 # change password (Phase 27)
  adduser/                # create user account (Phase 27)
  pty-test/               # PTY subsystem test (Phase 29)
  unix-socket-test/       # Unix domain socket test (Phase 39)
  thread-test/            # Threading primitives test (Phase 40)
  crypto-lib/             # Cryptography library (Phase 42)
  crypto-test/            # Crypto integration test (Phase 42)
  telnetd/                # Telnet server daemon (Phase 30)
  sshd/                   # SSH server daemon (Phase 43)
  syslogd/                # System logging daemon (Phase 46)
  crond/                  # Cron scheduler daemon (Phase 46)
  coreutils/              # C implementations: cat, cp, echo, env, grep, id, ls, mkdir, mv, pwd, rm, rmdir, sleep, true, false, prompt, whoami, touch, stat, wc, ar, install
  coreutils-rs/           # Rust implementations: true, false, echo, pwd, sleep, rm, mkdir, rmdir, mv, touch, stat, wc, ar, install, meminfo, date, uptime, sha256sum, genkey, service, logger, shutdown, reboot, hostname, who, w, last, crontab
  demo-project/           # Multi-file C demo project for make testing (Phase 32)
  hello-c/                # C hello world test
  signal-test/            # C signal handling test
  stdin-test/             # C stdin test
  tmpfs-test/             # C tmpfs test
  # Phase 44: musl-linked Rust std programs (standalone crates, NOT workspace members)
  hello-rust/             # Rust std hello world (musl cross-compiled, Phase 44)
  sysinfo-rust/           # System info tool via std::fs (Phase 44)
  httpd-rust/             # Minimal HTTP server via std::net (Phase 44)
  calc-rust/              # Interactive calculator via std::io (Phase 44)
  todo-rust/              # Persistent todo list via std::fs (Phase 44)
```

## Ports Tree Layout (Phase 45)

```
ports/
  port.sh                 # port command (installed at /usr/bin/port)
  lang/lua/               # Lua 5.4.7 scripting language port
  lib/zlib/               # zlib 1.3.1 compression library port
  math/bc/                # bc calculator port
  core/sbase/             # suckless Unix tools port (basename, seq, rev, etc.)
  doc/mandoc/             # man page formatter port
  util/minizip/           # zlib-dependent test port
  <category>/<program>/
    Portfile              # metadata: NAME, VERSION, DESCRIPTION, CATEGORY, DEPS
    Makefile              # targets: fetch, patch, build, install, clean
    src/                  # bundled source code
    patches/              # m3OS-specific patches
```

## Kernel Source Layout

```
kernel/src/
  main.rs              # entry point, boot sequence
  serial.rs            # serial I/O + log backend
  pipe.rs              # inter-process pipes
  pty.rs               # PTY pair table and lifecycle (Phase 29)
  rtc.rs               # CMOS real-time clock driver (Phase 34)
  signal.rs            # POSIX-style signal handling
  stdin.rs             # stdin abstraction
  tty.rs               # TTY/terminal subsystem
  testing.rs           # QEMU test framework
  arch/x86_64/         # GDT, IDT (APIC-based), paging, syscall gate
  acpi/                # ACPI table parsing (RSDP, MADT)
  blk/                 # block devices: VirtIO-blk, MBR parsing
  fb/                  # framebuffer console driver
  fs/                  # VFS layer, FAT32, tmpfs, ramdisk, protocol
  ipc/                 # endpoints, capabilities, messages, notifications, registry
  mm/                  # buddy frame allocator, paging, heap, slab caches, user_space, ELF loader
  net/                 # IPv4, ARP, Ethernet, ICMP, TCP, UDP, Unix domain sockets, VirtIO-net, dispatch
  pci/                 # PCI device enumeration
  process/             # process management (fork, exec, exit, wait, threads, futex)
  smp/                 # AP boot, IPI, TLB shootdown
  task/                # scheduler (SMP-aware round-robin)
kernel/initrd/           # static initrd assets checked into source
target/generated-initrd/ # xtask-staged generated binaries embedded by ramdisk
```

## kernel-core Source Layout

```
kernel-core/src/
  lib.rs               # module declarations
  types.rs             # shared types
  buddy.rs             # buddy frame allocator (Phase 33)
  slab.rs              # slab cache allocator (Phase 33)
  time.rs              # time conversion library (Phase 34)
  fb.rs                # framebuffer abstractions
  pipe.rs              # pipe abstractions
  pty.rs               # PTY pair state, ring buffers (Phase 29)
  tty.rs               # TTY abstractions
  fs/                  # FAT32, MBR, tmpfs abstractions
  ipc/                 # capability, message, registry abstractions
  net/                 # ARP, Ethernet, ICMP, IPv4, TCP, UDP abstractions
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

Phase-specific roadmaps and task lists live in `docs/roadmap/` (phases 01-48) with corresponding `docs/roadmap/tasks/` breakdowns.
