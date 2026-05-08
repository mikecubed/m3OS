# 02 — Deferred Items and Documented Shortcuts

This document catalogues every item documented as deferred and every documented shortcut. It is organised by phase. Items marked **🔁 closed** are tracked elsewhere in the roadmap and should be considered remediated. Items without that mark are open.

The goal of this catalogue is to make the project's accepted technical debt visible in one place. Many of these items are honest and intentional (a small kernel deferring features so the core can be taught clearly). Others are gaps that warrant explicit owner phases or status corrections — those are surfaced separately in `03-red-flags-and-status-mismatches.md` and `06-pre-1.0-blocker-list.md`.

---

## Phase 01 — Boot Foundation
- Framebuffer output → 🔁 Phase 09
- Test harness integration beyond basic smoke testing
- Real-hardware boot beyond the documented QEMU path

## Phase 02 — Memory Basics
- Reclaiming freed physical frames → 🔁 Phase 17
- Demand paging → 🔁 Phase 36
- Sophisticated virtual memory policies

## Phase 03 — Interrupts
- APIC and SMP interrupt routing → 🔁 Phase 15
- Advanced driver interrupt models
- Complex deferred work queues

## Phase 04 — Tasking
- Priorities and deadline scheduling
- Sleep queues and timers beyond the basic tick
- SMP-aware run queues → 🔁 Phase 25

## Phase 05 — Userspace Entry
- Dynamic ELF loading → 🔁 Phase 11
- Shared libraries
- Full process lifecycle management → 🔁 Phase 11

## Phase 06 — IPC Core
- Large page-grant transfers → marked "deferred to Phase 7+" in code (`kernel/src/ipc/mod.rs:34-35`); **never delivered**. Capability grants via IPC, page-capability bulk transfers, IPC timeouts all currently absent.
- IPC timeouts and cancellation
- Advanced scheduling policies around IPC
- Capability table is a fixed 64-slot array (not dynamically sized) — partially relaxed by 52c's growable capability/endpoint pools, but the underlying model still treats this as a learning shortcut.
- Messages hold only 4 × u64 data words.

## Phase 07 — Core Servers
- Servers running in ring 3 (requires ELF loader from Phase 8+) → 🔁 Phase 11+ for ELF; Phase 52, Phase 54 for actual extraction
- String pointers via page grants for ring-3 IPC payloads → not delivered
- Service deregistration
- Automatic service restart policies → 🔁 Phase 46, Phase 51
- Complex capability delegation tooling
- Dynamic driver loading
- **Shortcut:** All servers (`init_task`, `console_server`, `kbd_server`) run as ring-0 kernel threads, not ring-3 processes (deliberate scope decision)
- **Shortcut:** Registry syscalls 9 and 10 wired but unused

## Phase 08 — Storage and VFS
- Writable filesystems → 🔁 Phase 13
- Page cache and buffering
- Permissions and access control → 🔁 Phase 27, Phase 38

## Phase 09 — Framebuffer and Shell
- Pipes and redirection → 🔁 Phase 14
- Job control → 🔁 Phase 20+
- Full-screen applications → 🔁 Phase 26
- Advanced graphics or windowing → 🔁 Phase 56

## Phase 10 — Secure Boot
- **Acceptance criterion not met:** "Kernel boots on real hardware with Secure Boot enabled" — explicitly unchecked because requires physical hardware
- README update (D.3) blocked on real-hardware boot test
- Key rotation and revocation
- Microsoft UEFI CA submission
- Measured boot / TPM attestation
- Module signing (N/A for monolithic + microkernel)

## Phase 11 — ELF Loader and Process Model
- Copy-on-write page faults → not delivered
- Dynamic linking and `ld.so` → 🔁 Phase 31+
- Process groups and sessions → 🔁 Phase 22
- `clone` with shared address spaces (threads) → 🔁 Phase 40
- `ptrace` and debugging support
- **Shortcut:** Static linking only at this stage
- Phase-11 deferred items absorbed by Phase 12 Track A: exception-handler context switch, execve address-space free, dead-task reaping, SFMASK

## Phase 12 — POSIX Compat
- `futex` and pthreads → 🔁 Phase 40
- `epoll` / `poll` / `select` → 🔁 Phase 37
- Linux signal-ABI delivery → 🔁 Phase 19
- `/proc` filesystem entries → 🔁 Phase 38
- Dynamic linker support (`PT_INTERP`, `LD_LIBRARY_PATH`)
- `mprotect` and memory permission changes → 🔁 Phase 36
- **Shortcut:** `getcwd` / `chdir` are stubs returning `/` (D.8)
- **Shortcut:** `ioctl` is TIOCGWINSZ stub only (D.9)
- **Shortcut:** Non-anonymous `mmap` rejected (only `MAP_ANONYMOUS`) (D.5)
- **Shortcut:** ~40 musl-required syscalls implemented vs hundreds in real Linux ABI

## Phase 13 — Writable FS
- **Doc gap:** No task doc exists
- Page cache and write-back buffering
- Journaling / CoW crash recovery
- File permissions and ownership bits → 🔁 Phase 27, Phase 38
- Hard and symbolic links → 🔁 Phase 38
- File-backed `mmap` of pages
- Extended attributes
- **Shortcut (verbatim):** "This phase writes through immediately and accepts the corruption risk, which is fine for a single-user development machine running in QEMU."
- **Shortcut:** `fsync` is a no-op on tmpfs

## Phase 14 — Shell and Userspace Tools
- stderr redirection (`2>&1`, `2>file`)
- Pipelines longer than two stages
- Subshells (`$(...)`, backticks)
- Here-documents (`<<EOF`)
- `trap` built-in
- Shell scripting (loops, conditionals, functions) — partially added in Phase 32 but never validated
- Tab completion
- Glob expansion
- Moving the shell to a userspace ELF binary → 🔁 Phase 20
- Per-process working directory (cwd is global) → 🔁 Phase 18
- Close-on-exec → 🔁 Phase 38, but **`O_CLOEXEC` is silently dropped on `open`/`openat` even now** (Phase 54a Track A)
- **Shortcut:** Pipe read/write use yield-loops (busy-wait), not proper blocking
- **Shortcut:** `sys_nanosleep` is a yield-loop tick count

## Phase 15 — Hardware Discovery
- ACPI AML interpreter and dynamic hardware events
- PCIe extended config space via MCFG (MMIO) → 🔁 Phase 55
- MSI/MSI-X → 🔁 Phase 55
- PCI device power management (D-states)
- ACPI S-states (sleep, hibernate)
- PCIe hotplug
- AP startup → 🔁 Phase 25
- IOMMU/DMA remapping → 🔁 Phase 55a (status drift)
- HPET as timer source
- Per-device PCI BAR MMIO mapping → 🔁 Phase 16
- **Shortcut:** Legacy 0xCF8/0xCFC port-I/O PCI enumeration (rather than MCFG)
- **Shortcut:** Static ACPI descriptor tables only (no AML)

## Phase 16 — Network
- TCP retransmission timer and congestion control (CUBIC, BBR)
- IPv6
- DNS resolution → 🔁 Phase 60
- TLS / DTLS → 🔁 Phase 43 partial
- `epoll`/`select`/`poll` for non-blocking sockets → 🔁 Phase 37
- Multiple simultaneous TCP connections
- Checksum offload via virtio
- DHCP client (static IP only)
- Scatter-gather DMA
- VLAN tagging
- Zero-copy `sendmsg`/`recvmsg`
- **Shortcut:** TCP has no retransmit timer in the first pass
- **Shortcut:** Single connection at a time
- **Shortcut:** UDP checksum may be zero
- **Shortcut:** Static IP `10.0.2.15/24`, gateway `10.0.2.2`
- **Doc gap:** Task doc uses planning-table format with no `[x]`/`[ ]` checkboxes

## Phase 17 — Memory Reclamation
- Stress test deferred to "Phase 18+" (no specific owner)
- Buddy allocator → 🔁 Phase 33
- Slab/SLUB allocator → 🔁 Phase 33 (infrastructure) / Phase 53a (modernization)
- Swap
- Huge pages
- NUMA awareness
- `mmap(MAP_PRIVATE)` copy-on-write
- Kernel stack guard pages
- OOM killer

## Phase 18 — Directory and VFS
- Inode-based hard links → 🔁 Phase 38
- Streaming `getdents64` for very large dirs
- Symlink resolution beyond first hop → 🔁 Phase 38
- VFS mount namespace
- tmpfs writeback
- `proc` filesystem → 🔁 Phase 38

## Phase 19 — Signal Handlers
- **🛑 Status mismatch:** design doc Complete, task doc all 6 tracks Not started
- Full `siginfo_t`
- `SA_SIGINFO` 3-arg form
- Queued real-time signals (`sigqueue`)
- Signal coalescing suppression for RT signals

## Phase 20 — Userspace Init and Shell
- Job control (`SIGTSTP`, `SIGCONT`, `bg`, `fg`)
- Pipeline stderr redirection
- `&&` / `||` conditional chaining
- Command history → 🔁 Phase 21 (sh0/ion gap)
- Tab completion → 🔁 Phase 22 (deferred again)
- `alias` / `function` builtins
- Environment variable export persistence across exec
- `/etc/profile` sourcing

## Phase 21 — Ion Shell
- **Milestone-goal inversion:** goal was ion as primary, sh0 as fallback; outcome is sh0 primary, ion fallback
- ion `-c 'cmd'` script mode → 🔁 Phase 22
- Ion interactive mode → 🔁 Phase 22
- History persistence (`~/.local/share/ion/history`) → 🔁 Phase 22 (re-deferred)
- Tab completion with reedline → 🔁 Phase 22 (re-deferred)
- Vendoring of ion source

## Phase 22 — TTY and Terminal Control
- History persistence — re-deferred from Phase 21 with no successor phase
- Tab completion — re-deferred from Phase 21 with no successor phase

## Phase 22b — ANSI Escape Sequences
- **Doc gap:** No design doc exists (template violation)
- **Entire Track F validation deferred (manual QEMU visual test):** ion prompt redraw, ion prompt position after command, `\x1b[2J\x1b[H` clears screen, backspace visual erase, long-line wrap, external command output display, **sh0 regression check (P22b-T046)**

## Phase 23 — Socket API
- `SO_REUSEADDR` / `SO_REUSEPORT`
- `SO_LINGER`
- `sendmsg` / `recvmsg`
- `select` / `poll` / `epoll` → 🔁 Phase 37
- Non-blocking sockets (`O_NONBLOCK` on AF_UNIX)
- Unix socket credential passing (`SCM_CREDENTIALS`)
- Abstract namespace sockets
- Datagram sockets (`SOCK_DGRAM`)

## Phase 24 — Persistent Storage
- **Primary acceptance criterion (files survive reboot) explicitly deferred** to "interactive QEMU"
- `rename(2)` syscall — deferred entirely
- Shell `mount` builtin — deferred entirely
- Host-side ext2 image visibility via `losetup`
- Multi-cluster file test
- QEMU integration test

## Phase 25 — SMP
- **Acceptance criterion not met:** TLB shootdown coherence after `munmap` — `munmap` does not call the TLB shootdown hook (correctness hazard)
- **Acceptance criterion not met:** "syscalls issued from any core handled correctly" — per-core syscall dispatch deferred (BSP-only)
- Spinlock audit "BSP-only mitigation" rather than full per-core correctness
- Per-core syscall dispatch → 🔁 Phase 35 (claimed but see Phase 35 finding)

## Phase 26 — Text Editor
- Syntax highlighting → Phase 26b (planned)
- Multiple file editing (split views, tabs)
- Undo/redo beyond single-character
- Copy/paste
- Mouse support
- ncurses / TUI library
- terminfo / termcap
- Line wrapping mode
- Full Unicode multi-byte editing
- Plugin or macro system
- Configuration file (`.kibirc`)
- Colorscheme / theme

## Phase 27 — User Accounts
- **Shortcut:** TSC-seeded PRNG for password salts (not crypto-secure) — caveat documented but not assigned to a remediation phase
- Proper entropy source
- ext2 storage → 🔁 Phase 28
- `sudo` (su is sufficient at this stage)
- PAM / pluggable authentication
- Password aging / expiry
- Group management beyond static `/etc/group`
- LDAP / NIS integration
- Full POSIX ACLs

## Phase 28 — ext2 Filesystem
- Triple-indirect block addressing — files limited to ~64 MB; reads of triple-indirect blocks return `Err(Ext2Error::CorruptedEntry)` (`kernel/src/fs/ext2.rs:355`)
- Full directory entry deletion
- Symlink creation → 🔁 Phase 38
- **Shortcut:** Track I — `.m3os_permissions` FAT32 overlay was a stated cleanup target; was retained as a "fallback" rather than removed

## Phase 29 — PTY Subsystem
- `/dev/tty` device node
- Packet mode (`TIOCPKT`)
- SIGWINCH from kernel resize events (only manual `TIOCSWINSZ`)
- Terminal multiplexer (screen / tmux)
- Dynamic PTY allocation beyond fixed 16-slot pool
- PTY ownership and permission enforcement (`grantpt()` is a no-op)
- Orphaned process group handling (full POSIX job control)
- `SIGTTIN` / `SIGTTOU` for background-process terminal access
- Multiple line disciplines (only N_TTY)
- `/dev/pts` filesystem (slaves opened by path convention)

## Phase 30 — Telnet Server
- **Entire Track F (16 manual-QEMU validation items) unchecked**: telnetd boot, login prompt, valid auth → shell, basic commands, pipe commands, edit over telnet, ≥4 concurrent sessions, session independence, SIGHUP on disconnect, PTY freeing, NAWS, run-gui boot, README update

## Phase 31 — Compiler Bootstrap
- **Entire Tracks D, E, F (~15 items) deferred:**
  - D: TCC `--version`, `tcc -run hello.c`, hello world output, fibonacci, multi-file
  - E: TCC compiles itself; TCC-compiled-by-TCC works; self-hosted binary passes basic tests; self-hosting documented
  - F: `cargo xtask check` passage, QEMU boot, regression tests, README update

## Phase 32 — Build Tools
- **Entire Tracks B (partial), C (partial), D, E, F (~20 items) deferred:**
  - B: `make --version`, simple Makefile parses and executes
  - C: `time` utility
  - D: sh0 for-loop, sh0 `if`/`else`, sh0 `$(...)`, sh0 `$?`
  - E: full build, incremental rebuild, `make clean`, `ar` archive, `build.sh`
  - F: all 9 validation items

## Phase 33 — Kernel Memory Improvements
- **Headline deliverable not done:** C.4 slab migration — most kernel allocations still flow through the global linked-list heap
- A.4: OOM stress test
- D.4: mmap/munmap loop binary
- E.3: Heap coalescing test
- G.2: Memory stress test

## Phase 34 — Real-Time Clock
- E.2: QEMU RTC accuracy test (verified by boot log only)

## Phase 35 — True SMP Multitasking
- **Headline behaviour not active:** E.2 — `maybe_load_balance()` hook commented out in scheduler dispatch loop
- **In-doc admission:** global `SCHEDULER` lock acquired on every dispatch — negates per-CPU queue contention reduction
- E.1: Per-run-queue length atomic counter
- G.2: Pipe wait-queue attachment
- G.3: IPC wait-queue attachment
- H.3: Child `tms_cutime`/`tms_cstime` reporting (stubbed zero)
- **Doc gap:** Design doc missing `Status:` and `Source Ref:` header fields

## Phase 37 — I/O Multiplexing
- **Documented accuracy gap:** Timeout granularity is ~10ms (no proper timer subsystem); affects `poll`/`select`/`epoll` across the system

## Phase 39 — Unix Domain Sockets
- **Correctness gap:** Named sockets create regular file markers, not socket inodes — `stat()` returns wrong `st_mode` type bits
- Dedicated socket node type in tmpfs/ext2
- J.1 automated AF_UNIX integration test (3 acceptance items unchecked)

## Phase 42 — Crypto Primitives
- **Documented:** "Crypto implementations are for learning. They have not been audited and should not be used to protect real secrets."
- **Documented:** CSPRNG is not cryptographically secure; SSH (43) depends on it
- Hardening to RDRAND/RDSEED — not assigned to any remediation phase

## Phase 42b — Async Executor
- **Doc gap:** No design doc exists
- **Doc gap:** Task doc has zero checked checkboxes across all 7 tracks despite `Status: Complete`

## Phase 43 — SSH Server
- **Documented incompatibility:** Pubkey format is hex-encoded Ed25519 (64 hex chars), not OpenSSH wire format / `authorized_keys`. Standard `ssh` clients cannot authenticate against standard `authorized_keys` files
- E.5: Window-change (SIGWINCH) propagation to PTY
- G.1: End-to-end password auth (Manual, unchecked)
- G.2: End-to-end pubkey auth (Manual, unchecked)
- G.3: Wireshark traffic inspection (Manual, unchecked)

## Phase 43b — Kernel Trace Ring
- **Doc gap:** Zero checked checkboxes across all 9 tracks despite `Status: Complete`

## Phase 43c — Regression and Stress CI
- **Doc gap:** Zero checked checkboxes across all 11 tracks despite `Status: Complete`

## Phase 45 — Ports System
- Binary package format
- Package signing and verification
- Version conflict resolution
- Automatic updates
- Mirror / repository support
- Network fetching of source tarballs
- Cross-compilation of ports on the host
- `/var/run → /run` compatibility symlink — routed from `docs/debug/54-followups.md` item 3, no current owner phase

## Phase 46 — System Services
- **Doc gap:** Zero checked checkboxes across all 8 tracks (22 tasks) despite `Status: Complete`
- Socket activation (systemd-style)
- `sd_notify` readiness protocol
- Service sandboxing (cgroups, namespaces)
- Journal (structured logging)
- Log rotation
- Remote syslog
- NTP time synchronization
- systemd-compatible unit files
- Runlevels / targets

## Phase 47 — DOOM
- **Doc gap:** Zero checked checkboxes across all 6 tracks despite `Status: Complete`
- Multi-application composition and windowing → 🔁 Phase 56
- Pointer-driven GUI policy → 🔁 Phase 56
- Audio output for graphical session → 🔁 Phase 57
- Hardware-accelerated rendering

## Phase 48 — Security Foundation
- Full privilege separation across all network services
- Advanced key management, rotation, audit
- Multi-factor or hardware-backed auth
- General sandboxing
- **Open:** Pre-seeded image still ships single-iteration `$sha256$` root password (only post-`passwd` users get `$sha256i$10000$`); two-format situation explicitly accepted by Phase 53

## Phase 50 — IPC Completion
- Deep performance tuning of zero-copy paths
- Rich typed service IDLs / code-generated bindings
- Advanced delegation patterns

## Phase 51 — Service Model Maturity
- **🛑 Status: In Progress, no task doc exists**
- Advanced service sandboxing and capability confinement
- Socket activation and readiness protocols
- Rich health probes, backoff tuning, multi-instance orchestration
- Structured journaling and long-term log retention

## Phase 52 — First Service Extractions
- **🛑 Status: In Progress, parent phase never formally closed (sub-phases 52a/b/c/d did the work)**

## Phase 52a — Kernel Reliability Fixes
- Track B.2.2: large SSH output stress (acknowledged unverified)
- The `restore_caller_context` mechanism — superseded by 52b before 52a's docs were finalised; aftermath documented in 52d
- Task-owned `syscall_user_rsp`, typed `UserBuffer`, `AddressSpace` → 🔁 Phase 52b

## Phase 52b — Kernel Structural Hardening
- Half-migrated `UserReturnState`: state still saved at block points (not syscall entry); `kernel_stack_top` / `fs_base` split → 🔁 Phase 52d
- VMA tree → 🔁 Phase 52c
- Per-core scheduler with work-stealing → 🔁 Phase 52c (queues landed; lock-free dispatch deferred)
- Dynamic IPC resource pools → 🔁 Phase 52c (endpoints/caps/registry growable; notification pool fixed)
- ISR-direct notification wakeup → 🔁 Phase 52c

## Phase 52c — Kernel Architecture Evolution
- Full fair scheduler with virtual runtime (CFS / EEVDF / WAVL)
- **True per-core scheduling (lock-free dispatch hot path) — explicitly re-deferred by 52d with no assigned phase**
- **Growable notification pool — `MAX_NOTIFS = 64` remains; re-deferred by 52d (ISR-safety constraint)**
- Atomic `reply_recv` (seL4-style)
- Preemptive scheduling from interrupt context
- Dynamic PTY pool

## Phase 52d — Kernel Completion and Roadmap Alignment
- Full fair / EEVDF / CFS-style runtime accounting
- Cluster-aware / NUMA-aware work stealing
- Lock-free per-core dispatch hot path (re-stated)
- Growable ISR-safe notification pool (re-stated)
- Broader cleanup of compatibility/debugging syscalls (e.g., termios register-return wrappers retained as `#[deprecated]`)

## Phase 53a — Kernel Memory Modernization
- NUMA-aware per-domain slab and page caches
- Constructor / destructor object caching
- Memory debugging suite (red zones, poison fill, KFENCE-style)
- Memory pressure callbacks (shrinker interface) — deferred to Phase 54; **not observed delivered there**
- Full GFP-like context flags
- Type-state `Frames<Free/Allocated/Mapped>` wrappers

## Phase 53 — Headless Hardening
- Broad outbound developer networking (HTTPS/TLS, DNS, git, GitHub) → 🔁 Phase 60
- GUI / display compositor / graphical session / local desktop → 🔁 Phase 56, Phase 57
- Mouse input, audio output → 🔁 Phase 56, Phase 57
- Large third-party runtime ecosystems (Python, Node, JVM) → 🔁 Phase 59, Phase 61
- Broad hardware certification beyond QEMU x86_64 with OVMF
- Package feeds, remote repositories, dynamic linking
- Full POSIX compliance testing

## Phase 54 — Deep Serverization
- Broader filesystem matrix beyond first migrated `/etc/...` slice
- Full network-service ecosystem (TCP and other protocols still in-kernel)
- Aggressive performance tuning
- Complete POSIX policy removal from kernel
- MOUNT_OP_LOCK yielding primitive (long-term, no owner phase)
- Scheduler diagnostic threshold tuning (no owner phase)
- Full epoll module extraction (Phase 54a only moves cleanup helper)
- virtio_blk IRQ completion → 🔁 Phase 55 Track C.5
- `/var/run → /run` symlink → 🔁 Phase 45 deferred list
- **Code-side:** `fat_server` is a permanent ENOSYS stub — replies `-ENOSYS` to every request; no FAT32 I/O ever migrated to ring-3

## Phase 54a — Post-Serverization Kernel Hygiene
- **Status: Planned**
- Track A: CLOEXEC/NONBLOCK plumbing — `open`/`openat`/`openat2`/`vfs_service_open` silently drop O_CLOEXEC
- Track B: Four `arch::x86_64::syscall::*_pub` layer-crossing wrappers in `kernel/src/process/mod.rs`
- Full epoll extraction (only cleanup helper moved)
- AGENTS.md version string still stale at v0.51.0

## Phase 55 — Hardware Substrate
- Broad laptop/desktop certification
- Wide Wi-Fi, GPU, USB peripheral matrices
- IOMMU isolation → 🔁 Phase 55a (status drift)
- Ring-3 driver extraction → 🔁 Phase 55b (status drift)
- Hardware-acceleration features
- All physical-hardware validation deferred (matrix entries say "Physical target deferred")
- **Shortcut (acknowledged TCB widening):** Phase 55 places NVMe and e1000 drivers in ring 0 for bring-up simplicity

## Phase 55a — IOMMU Substrate
- **🛑 Status drift: Design doc says "Planned" while 55c/56/57 list 55a as ✅ dependency and AGENTS.md treats IOMMU as operational**
- VFIO / device passthrough
- SR-IOV virtual-function support
- IOMMU-group enforcement policies beyond per-device domains
- ARM SMMU
- Dynamic IOVA compaction and large-page promotion
- Interrupt remapping
- VT-d scalable mode (hardcoded false in `kernel/src/iommu/intel.rs:178`)
- Queued invalidation (deferred — `kernel/src/iommu/intel.rs:722` uses register-based path)
- AMD-Vi multi-BDF domains (deferred — `kernel/src/iommu/amd.rs:143`)
- AMD-Vi fault-dispatch ISR (`kernel/src/iommu/amd.rs:938` Track E TODO; **no handler installed today**)
- "Known Open Bug" section in 55a doc (VT-d MMIO `CTRL.RST` drops) claims-closed-by-55c-R2 but the 55a doc text never updated

## Phase 55b — Ring-3 Driver Host
- **🛑 Status drift: Design doc says "Planned" while 55c/56/57 list 55b as ✅ dependency and AGENTS.md treats ring-3 NVMe + e1000 as operational**
- Driver-side seccomp / syscall sandbox beyond default posture
- Hot-plug / surprise-removal handling
- VirtIO-blk / VirtIO-net extraction (only NVMe and e1000 covered)
- Driver live-update / zero-downtime restart (cold-restart only)
- Multi-queue NVMe beyond single I/O queue pair
- LOC-metric targets missed (target ≤ −1800; actual +1917 net kernel LOC delta)

## Phase 55c — Ring-3 Driver Correctness Closure
- Many-to-one notification binding
- Timed `recv` (`ipc_recv_timeout`)
- NVMe migration to bound-notification model
- IOMMU coverage for MSI-X table regions
- Generalised EAGAIN over block I/O
- Secondary e1000 IRQ-coalescing concerns from 55b residuals (multiple RX descriptor-ring wraparounds)
- **Open follow-up: 3 RX-path tests in `kernel/src/net/remote.rs` use `encode_net_send` where they should use `encode_net_rx_notify`** (fix is documented, ~20 min, blocked on PR #124 frame-allocator fix)
- **Code-side: 4 isolation tests are 100% `todo!()` scaffolding** (cross-device MMIO denied, cross-device DMA denied, capability forge denied, post-crash handles invalid)

## Phase 56 — Display and Input Architecture
- **🛑 Status drift: Design doc says "Planned" but completion-gaps doc shows 100% closed (267/279 ticked, all 9 closing checkboxes)**
- D-B4 zero-copy via page-grant capabilities → Phase 56b or later
- D-F1a `mouse_server` dependency direction (init manifest parser doesn't support comma-separated `depends=`) → Phase 57+ session-manifest pass
- D-F1b distinct `on-restart=` supervisor directive → Phase 51
- D-D1 standalone modifier-key edges (additive when client needs)
- D-A0 L/R modifier chord differentiation (wire-format change)
- **D-E4 server-initiated subscription event push — `TODO(subscription-push)` in code (`userspace/display_server/src/control.rs:670,690,696,703`); 4 `publish_*` functions queue events but never transmit them to subscribers** — no owner phase
- Compositor damage tracking (every cursor motion forces full repaint of all surfaces) — `userspace/display_server/src/compose.rs:173`
- DOOM `sys_fb_acquire` migration — Phase 56 wrap-up follow-on

## Phase 57 — Audio and Local Session
- Rich desktop audio routing and mixing
- Media playback, recording, advanced codecs
- Multiple graphical sessions
- Full desktop shell, notifications, settings panels, app ecosystems
- **Code-side:** terminal hardcodes `HOME=/root` (multi-user graphical session future-phase)

## Phase 57a — Scheduler Block/Wake Protocol Rewrite
- **🛑 Status drift: Design doc says "Planned" but git log + AGENTS.md treat as complete**
- Per-CPU runqueues with per-CPU locks
- Priority inheritance
- Wait-queue helper layer (`prepare_to_wait` / `finish_wait`)
- Loom-style formal interleaving search
- Refactoring `userspace-init`'s boot fork burst
- Migration of < 1 ms branch of `sys_nanosleep` away from TSC busy-spin
- **Code-side: 4 `TODO(57a-C/D): route through pi_lock + with_block_state` markers in scheduler.rs** at sites that bypass the abstraction with bare `task.state = ...` stores

## Phase 57b — Preemption Foundation
- **Status: "Complete pending soak (PR #132)"** — soak result document not in any accessible doc
- Per-CPU placement of `preempt_count`
- Tracing variants (`preempt_disable_notrace`)
- Hardirq / softirq sub-counts
- Replacing `switch_context` with unified preempt-aware switch

## Phase 57c — Kernel Busy-Wait Conversion
- Lockdep equivalent
- `might_sleep()`-style instrumentation
- Loom-style formal interleaving search
- Per-CPU load balancing of converted-syscall-task placement
- **`preempt_disable` wrappers at annotated sites are deferred to 57e Track B** — currently comments only

## Phase 57d — Voluntary Preemption
- Per-CPU `preempt_count`
- Explicit reschedule points (`might_resched`-style)
- Priority inheritance
- CFS / EEVDF-style fair scheduling
- Kernel-mode preemption → 🔁 Phase 57e

## Phase 57e — Full Kernel Preemption
- Per-CPU runqueues with per-CPU locks
- Priority inheritance
- Real-time scheduling policies (SCHED_FIFO, SCHED_RR)
- Lockdep equivalent
- Loom-style formal interleaving search
- `PREEMPT_RT` parity
- AVX-512 in `XCR0`
- XSAVE fallback for pre-2011 CPUs
- Memory protection keys (PKRU) save/restore

---

## Code-side architectural deferrals (across all phases)

The following are not phase-specific but are universal architectural shortcuts visible in the code:

- **W^X is absent.** `kernel/src/mm/user_space.rs:135,143` — code pages mapped WRITABLE | USER_ACCESSIBLE with no NO_EXECUTE enforcement. Documented as "deferred to Phase 6+", but never delivered. Every userspace code page is currently writable.
- **Capability grants via IPC are absent.** `kernel/src/ipc/mod.rs:34-35` — "Deferred to Phase 7+: capability grants via IPC, page-capability bulk transfers, IPC timeouts." Fundamental seL4-style features missing from the IPC engine.
- **Per-core lock-free dispatch is absent.** `kernel/src/task/scheduler.rs:28-30` — "True per-core scheduling (where the dispatch hot path never acquires a global lock) is deferred to a future phase." Phase 35 was claimed Complete with this gap; 52d re-deferred with explicit rationale.
- **PAT slot management is absent.** `kernel/src/pci/bar.rs:428` — BAR MMIO mappings use `NO_CACHE | WRITE_THROUGH` blanket fallback; correct UC- / WC / WB per BAR type cannot be expressed.
- **Triple-indirect ext2 reads are absent.** `kernel/src/fs/ext2.rs:355` — files requiring triple-indirect blocks (>~8 MB) silently return `Err(Ext2Error::CorruptedEntry)`.
- **`fat_server` is permanently ENOSYS.** `userspace/fat_server/src/main.rs:67` — replies `-ENOSYS` to every request; never migrated. Phase 54 declared Complete with this stub in place.
- **Compositor has no damage tracking.** `userspace/display_server/src/compose.rs:173` — every cursor motion or surface update forces full composite of all surfaces.
- **Display server subscription push is missing.** 4 `publish_*` functions queue events but never transmit them. `userspace/display_server/src/control.rs:670,690,696,703`.
- **Three termios register-return syscalls retained as `#[deprecated]`.** `userspace/syscall-lib/src/lib.rs:957-991` and matching kernel-side syscalls — no in-tree binary calls them after 52d, but the ABI surface is still live.
- **~~5 tick-multiplier bugs latent.~~ Closed by PR #136 (validation pass 2026-05-08).** Phase 57a Track G.3 fixed all five sites; comments now read `— G.3 fix` at `kernel/src/task/scheduler.rs:3878,4343` and `kernel/src/arch/x86_64/syscall/mod.rs:15763,16024,16436`.
- **`unsafe` block density:** 526 in kernel, 328 in userspace, 25 in kernel-core (881 total). Safety-comment coverage in kernel ~59% — ~217 unsafe blocks lack adjacent `// SAFETY:` rationale, concentrated in `syscall/mod.rs` (57), `interrupts.rs` (45), `scheduler.rs` (58), and userspace `syscall-lib/src/lib.rs` (137).
