# m³OS — Bootable OS in Rust

A serious, still-maturing bootable operating system built in Rust, following a
**microkernel-inspired architecture** targeting **x86_64**. Designed for
learning and experimentation, with a real userspace, networking, remote access,
and a roadmap toward stronger service isolation and broader platform support.

## Documentation Index

### Phase-Aligned Learning Docs

| Document | Phase | Description |
|---|---|---|
| [Boot Process](./01-boot.md) | 1 | UEFI boot flow, `bootloader` crate, kernel entry |
| [Memory Management](./02-memory.md) | 2 | Frame allocator, page tables, kernel heap |
| [Interrupts & Exceptions](./03-interrupts.md) | 3 | IDT, PIC, exception handlers, hardware IRQs |
| [Tasking & Scheduling](./04-tasking.md) | 4 | Task model, context switching, scheduler |
| [Userspace Entry](./05-userspace-entry.md) | 5 | Ring 3 transition, syscall gate, first userspace binary |
| [IPC](./06-ipc.md) | 6 | Synchronous rendezvous, capabilities, notifications |
| [Core Servers](./07-core-servers.md) | 7 | init, console_server, kbd_server, service registry |
| [Storage and VFS](./08-storage-and-vfs.md) | 8 | VFS layer, ramdisk, file IPC protocol |
| [Framebuffer and Shell](./09-framebuffer-and-shell.md) | 9 | Pixel console, keyboard IPC, shell |
| [Secure Boot](./10-secure-boot.md) | 10 | Host-side signing, UEFI Secure Boot |
| [ELF Loader & Process Model](./11-elf-loader-and-process-model.md) | 11 | ELF loading, per-process page tables, fork, process lifecycle |
| [POSIX Compatibility Layer](./12-posix-compatibility-layer.md) | 12 | Linux syscall ABI, musl libc, TLS, C runtime startup |
| [Writable Filesystem](./13-writable-filesystem.md) | 13 | tmpfs at /tmp, file mutation syscalls |
| [Shell and Tools](./14-shell-and-tools.md) | 14 | Pipes, redirection, job control, coreutils |
| [Hardware Discovery](./15-hardware-discovery.md) | 15 | ACPI, PCI enumeration, APIC |
| [Network Stack](./16-network.md) | 16 | virtio-net, Ethernet, ARP, IPv4, TCP, UDP |
| [Memory Reclamation](./17-memory-reclamation.md) | 17 | Free-list allocator, CoW fork, heap growth |
| [Directory VFS](./18-directory-vfs.md) | 18 | getdents64, directory fds, per-process cwd |
| [Signal Handlers](./19-signal-handlers.md) | 19 | rt_sigaction, sigframe, sigreturn |
| [Userspace Init and Shell](./20-userspace-init.md) | 20 | Ring-3 PID 1, remove kernel shell |
| [Ion Shell Integration](./21-ion-shell.md) | 21 | Redox OS ion shell, cross-compilation |
| [TTY and Terminal Control](./22-tty-terminal.md) | 22 | termios, line discipline, cooked/raw mode |
| [ANSI Escape Sequences](./22b-ansi-escape.md) | 22b | VT100 CSI parser, cursor, SGR colors |
| [Socket API](./23-socket-api.md) | 23 | BSD socket syscalls, userspace ping, poll for sockets |
| [Persistent Storage](./24-persistent-storage.md) | 24 | virtio-blk, FAT32 read/write, /data mount |
| [SMP](./25-smp.md) | 25 | AP startup, per-core scheduler, TLB shootdown |
| [Text Editor](./26-text-editor.md) | 26 | Full-screen editor (kibi-style) |
| [User Accounts](./27-user-accounts.md) | 27 | Login, UID/GID, file permissions, passwd/shadow |
| [ext2 Filesystem](./28-ext2-filesystem.md) | 28 | Native Unix permissions, replaces FAT32 |
| [PTY Subsystem](./29-pty-subsystem.md) | 29 | Pseudo-terminal pairs, session management |
| [Telnet Server](./30-telnet-server.md) | 30 | Remote shell access over TCP |
| [Compiler Bootstrap](./31-compiler-bootstrap.md) | 31 | TCC compiles C programs inside the OS |
| [Build Tools](./32-build-tools.md) | 32 | make, ar, multi-file C projects |
| [Kernel Memory](./33-kernel-memory.md) | 33 | Buddy allocator, slab caches, working munmap |
| [Timekeeping](./34-timekeeping.md) | 34 | CMOS RTC, wall-clock time, CLOCK_REALTIME |
| [True SMP](./25-smp.md) | 35 | Per-core syscalls, priority scheduling, load balancing |
| [Expanded Memory](./33-kernel-memory.md) | 36 | Demand paging, mprotect, large mmap, 1 GB RAM/disk |
| [Crash Diagnostics](./43a-crash-diagnostics.md) | 43a | Enriched panic/fault handlers, scheduler/fork/IPC assertions |
| [DOOM Port](./47-doom.md) | 47 | Framebuffer mapping, raw scancodes, doomgeneric integration, and real-world input/performance lessons |
| [Security Foundation](./48-security-foundation.md) | 48 | Kernel-enforced credentials, RDRAND entropy, iterated password hashing, first-boot provisioning |
| [Architectural Declaration](./49-architectural-declaration.md) | 49 | Syscall decomposition, keep/move/transition matrix, userspace-first rule |
| [IPC Completion](./50-ipc-completion.md) | 50 | Capability grants, bulk-data transport, ring-3-safe registry, server-loop failure semantics |
| [Service Model Maturity](./51-service-model-maturity.md) | 51 | Stable service contract, restart backoff, crash classification, shutdown ordering, admin hardening |
| [First Service Extractions](./52-first-service-extractions.md) | 52 | Console and keyboard extracted to ring-3 services, restart behavior, IPC-based input/output |
| [Kernel Reliability Fixes](./52a-kernel-reliability-fixes.md) | 52a | Stale resume-state, sunset waker, clear_child_tid, and exec signal-reset fixes |
| [Kernel Structural Hardening](./52b-kernel-structural-hardening.md) | 52b | AddressSpace, typed user buffers, task-owned return state, batched TLB, frame zeroing |
| [Kernel Architecture Evolution](./52c-kernel-architecture-evolution.md) | 52c | VMA tree, growable IPC tables, kernel line-discipline infrastructure, ISR wakeups |
| [Kernel Completion and Roadmap Alignment](./52d-kernel-completion-and-roadmap-alignment.md) | 52d | Return-state closure, keyboard convergence, bootfixes, and release-gate/initrd cleanup |
| [Kernel Memory Modernization](./53a-kernel-memory-modernization.md) | 53a | Per-CPU page cache, magazine slabs, size-class allocator cutover, cross-CPU frees, and allocator-local reclaim |
| [Headless Hardening](./53-headless-hardening.md) | 53 | Supported headless/reference workflow, validation gates, operator model, and non-goals |
| [Deep Serverization](./54-deep-serverization.md) | 54 | Userspace-owned storage/VFS and UDP policy slices, degraded-mode fallback contracts, and the signal/IPC shutdown fix needed to make them operable |
| [Hardware Substrate](./55-hardware-substrate.md) | 55 | PCIe MCFG + MSI/MSI-X, reusable hardware-access layer (BAR mapping, DMA, device IRQ), NVMe storage driver, Intel 82540EM classic e1000 network driver, reference hardware matrix |
| [Ring-3 Driver Host](./55b-ring-3-driver-host.md) | 55b | Device-host capability primitives, MMIO bounds-checking, IOMMU-gated DMA, notification-forwarded IRQs, NVMe and e1000 extracted to supervised ring-3 processes |
| [Display and Input Architecture](./56-display-and-input-architecture.md) | 56 | Ring-3 `display_server` owns the framebuffer; focus-aware input dispatch via `kbd_server` / `mouse_server`; layer-shell-equivalent surface roles; control socket for `m3ctl`-style tooling; supervised crash recovery and text-mode fallback |
| [Audio and Local Session](./57-audio-and-local-session.md) | 57 | First audio path (Intel AC'97 ring-3 driver, single-client PCM-out via `audio_server`); fixed-boot graphical session orchestrator (`session_manager`) with `text-fallback` recovery contract and `m3ctl` control-socket verbs; first useful graphical client (`term`) composing PTY + ANSI parser + Phase 56 display-server client + audio bell |
| [Release 1.0 Gate](./83-release-1-0-gate.md) | 83 | The 1.0 release contract — closed status legend, target×workflow [support matrix](./release/1.0-release-gate.md) with a mutually-exhaustive evidence trail, QEMU/host/bare-metal honesty tiering, first-class non-goals, and the phase-tracked `0.83.0` versioning posture ("1.0" is quality-bar language, not SemVer `1.0.0`) |
| [Spectre / KPTI Mitigations](./84-spectre-mitigations.md) | 84 | Post-1.0 transient-execution hardening — KPTI (Meltdown), retpoline (Spectre-v2 BTI), IBRS/eIBRS/IBPB/STIBP, `mitigations=off\|auto\|full` boot policy, host-tested CPUID/MSR decode, and an honest accounting of the UNADDRESSED classes (Spectre-v1, MDS, L1TF, SSB, Retbleed, Downfall) |
| [Cross-Compiled Toolchains](./85-cross-compiled-toolchains.md) | 85 (85a–d) | Build-once / install-prebuilt developer-toolchain family — the 85a content-addressed cache + relocatable `.m3pkg` + offline in-OS `pkg` installer, the relocation contract, and git (85b, local-only), Python (85c, fully-static CPython 3.12), and Clang/LLVM/LLD (85d, opt-in X86-only static) with their disk/RAM budget. Links the four 85a–d design + task docs |
| [Networking and GitHub](./86-networking-and-github.md) | 86 (86a–f) | Authenticated outbound developer workflows — the 86a CSPRNG/wall-clock/CA trust foundation, SSH (86b) and HTTPS/TLS (86c) git transports with their contrasting trust models, the Go runtime (86d, `mmap` `MAP_FIXED` + edge-`epoll` + `SIGURG`), the GitHub CLI (86e, two coexisting TLS stacks), and the 86f userspace SIMD / AES-NI capstone (soft-float-kernel / hard-float-userspace split, signal-frame FPU, AES-NI ≈27×). Links all six 86a–f design + task docs |
| [Node.js](./89-nodejs.md) | 89 | V8 jitless W^X model (`--v8-options=--jitless`, Ignition interpreter only — modern V8 removed the `mprotect` RW↔RX JIT path; NOT `--v8-lite-mode`, which compiles WASM out and aborts Node 22 startup), libuv `timerfd` event loop integration (Phase 89 A.1, the only new kernel primitive), the `signalfd` self-pipe fallback decision, the static-musl host-clang C++ cross build (reusing `build_llvm`'s sysroot), `small-icu`/bundled-OpenSSL/npm configuration, and the TLS/DNS/`npm install` package path the Phase 90 Claude Code milestone depends on |
| [Memory Protection Keys (PKU)](./90a-memory-protection-keys.md) | 90a | The x86 PKU hardware model (PTE protection-key bits 59–62, the PKRU register's per-key access/write-disable bits, `RDPKRU`/`WRPKRU`, `CR4.PKE`, and **why key rights are per-thread while page tags are per-mapping**); **W^X v1 → v2** as a case study in evolving a security invariant without abandoning it — the verbatim 4-clause v2 contract note + the enforcement-point audit (every path that can produce a W+X PTE, including the `mmap(W+X)` hole that was closed); PKRU on XSAVE component 9 (runtime RFBM `0x207`, Linux-default init `0x55555554`, the xsaveopt init-state / fork-inherit subtleties); V8's runtime PKU-adoption mechanics (the three musl-static blockers + port-side remedies) and the graceful no-PKU fallback; and the real-OS comparison (Linux pkeys, OpenBSD `wxallowed`, Apple `MAP_JIT`, Windows ACG). Links the [90a design](./roadmap/90a-memory-protection-keys.md) + [task](./roadmap/tasks/90a-memory-protection-keys-tasks.md) docs; the JIT V8 variant it unblocks feeds the Phase 90b Claude Code TUI |
| [Claude Code](./90b-claude-code.md) | 90b | Running Anthropic's CLI coding agent natively inside m3OS (install + launch + headless `claude -p` + the **interactive TUI rendering on the 90a JIT node**) — the supported-workflow decision and the **native-binary divergence** (why the pin is `@anthropic-ai/claude-code@2.1.112`, the last `cli.js` + `yoga.wasm` + `vendor/ripgrep/` version; 2.1.113+ repackaged into a native Bun binary that does not use Node), the **pre-bundled `.m3pkg` install path** (and why live `npm install -g` is not supported — npm's thousands of tiny files over the slow VFS), the runtime dependency chain (`DEPS=node` → jitless node by default / 90a JIT node opt-in → `/usr/bin/claude` launcher → 86a CA bundle), the launcher env contract line-by-line (`NODE_EXTRA_CA_CERTS`/`DISABLE_AUTOUPDATER`/`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`), the subscription-first 0600 OAuth-token/key credential posture (the `gh`-pattern, value never on serial) + the `/login` paste-flow, the vendored ripgrep static-pie finding, the W^X-v2 cross-thread PKU read-recovery kernel fix, the small-icu→**full-icu** build fix (small-icu lacked the ICU break-iterator data `Intl.Segmenter` needs → `JSSegments::Create` null-deref), and the `claude-smoke` gate (always-on offline install+launch core + a `SEG_OK` `Intl.Segmenter` guard, CI-viable on the jitless default; the automated QMP/PPM interactive-TUI render arm on the JIT node — 592 changed scanlines; opt-in live API/agent arms). Links the [90b design](./roadmap/90b-claude-code.md) + [task](./roadmap/tasks/90b-claude-code-tasks.md) docs |

### Roadmap

| Document | Description |
|---|---|
| [Roadmap Guide](./roadmap/README.md) | Detailed learning-first milestones with per-phase pages and Mermaid diagrams |
| [Roadmap Task Lists](./roadmap/tasks/README.md) | Actionable per-phase task breakdowns for implemented and near-term phases; later phases add task docs when implementation planning begins |

### Appendix (cross-cutting and historical)

| Document | Description |
|---|---|
| [Architecture & Syscalls](./appendix/architecture-and-syscalls.md) | Microkernel design, privilege model, syscall ABI, address space layout |
| [Testing](./appendix/testing.md) | QEMU-based test harness, exit conventions, writing tests |
| [Legacy C Kernel Comparison](./appendix/legacy-os-comparison.md) | Comparison with a prior x86 C kernel |
| [State Analysis (March 2026)](./appendix/state-analysis-march-2026.md) | Historical snapshot of OS state before Phases 17-34 |
| [Phase 21 Handoff](./appendix/phase-21-handoff.md) | Ion shell integration PR handoff notes |

### Standalone Roadmaps

| Document | Description |
|---|---|
| [Clang/LLVM Roadmap](./clang-llvm-roadmap.md) | Clang/LLVM cross-compilation strategy — revived for [Phase 85d](./roadmap/85d-clang-llvm.md) |
| [Python Roadmap](./python-roadmap.md) | Python cross-compilation strategy — revived for [Phase 85c](./roadmap/85c-python.md) |
| [git Roadmap](./git-roadmap.md) | git cross-compilation strategy — revived for [Phase 85b](./roadmap/85b-git-local.md) |
| [Node.js Roadmap](./nodejs-roadmap.md) | Node.js cross-compilation strategy — revived for [Phase 89](./roadmap/89-nodejs.md) |
| [GitHub CLI Roadmap](./archived/github-cli-roadmap.md) | gh CLI cross-compilation strategy (archived; [Phase 86](./roadmap/86-networking-and-github.md)) |
| [Claude Code Roadmap](./claude-code-roadmap.md) | Claude Code on m3OS strategy — revived for [Phases 90a/90b](./roadmap/90b-claude-code.md) |
| [Rust Crate Acceleration](./archived/rust-crate-acceleration.md) | Rust crate porting strategy (archived) |

### Evaluation

| Document | Description |
|---|---|
| [Project Evaluation](./evaluation/README.md) | Repo-wide review of current state, security, usability gaps, GUI path, and Rust OS comparisons |
| [Evaluation Roadmap](./evaluation/roadmap/README.md) | Release-oriented path to 1.0 and beyond, tied back to the official implementation roadmap |

## Quick Start

```bash
# Build and run in QEMU (requires nightly Rust, QEMU, OVMF)
cargo +nightly xtask run

# Build a bootable disk image (UEFI raw + VHDX for Hyper-V)
cargo +nightly xtask image

# Run tests
cargo +nightly xtask test
```

## Design Principles

- **Minimal trusted computing base** -- The kernel does as little as possible.
- **Safety by default** -- `unsafe` is used only at hardware boundaries, always wrapped in safe abstractions.
- **Incremental** -- Each phase produces a runnable artifact; nothing is left in a broken state.
- **Self-contained** -- No large third-party runtimes; the crate ecosystem is used for hardware abstractions only.
