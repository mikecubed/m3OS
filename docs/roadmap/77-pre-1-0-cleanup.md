# Phase 77 - Pre-1.0 Correctness, Cheap Security, and Network Polish

**Status:** Planned
**Source Ref:** phase-77
**Depends on:** Phase 74 (IPC Capability Grants) ✅, Phase 75 (W^X Enforcement) ✅
**Builds on:** Closes the small but real correctness and security gaps that the Phase 74a audit promoted out of "post-1.0 acceptable" into pre-1.0 must-fix, without committing to any large new subsystem
**Primary Components:** `userspace/sshd/`, `kernel/src/arch/x86_64/syscall/`, `kernel/src/arch/x86_64/cpu.rs`, `kernel/src/mm/elf.rs`, `kernel/src/net/tcp.rs`, `userspace/syscall-lib/src/net/`, `kernel/src/fs/procfs.rs`, kernel boot path (`kernel/src/main.rs`), `docs/roadmap/56-*.md` deferred lists

## Milestone Goal

Land the bundle of small, well-scoped correctness, security, and networking fixes the Phase 74a audit identified as the cheap pre-1.0 wins. After this phase, the §1 audit blocker count drops from 8 to 4 (USB, NIC, HDA, Wi-Fi remaining), every PR #118 residual is closed, the dev laptop's CPU has microcode loaded, SMEP+SMAP are on, `PT_TLS` works, DNS resolves names, TCP survives packet loss on the real internet, `htop` / `ps` / `top` actually show processes, and the doc-drift in earlier phase deferral lists is gone.

## Why This Phase Exists

Phase 74a §7 catalogues ten separate tracks that individually look small but collectively represent the difference between "1.0 runs on QEMU" and "1.0 is honest on a real network connected to a real Strix Halo laptop." Each item is too small to justify its own phase, but together they would silently slip past Phase 83 (Release 1.0 Gate) if not bundled. The htop / `/proc` compatibility work (Track H) gets its own track because it touches `kernel/src/fs/procfs.rs` and breaks a Phase 69d-shipped TUI app, but the work is still small enough to belong inside the bundle.

This phase exists to land them as one coherent pre-1.0 cleanup pass, with each track scoped tightly enough to ship in under a week, and no track allowed to expand into adjacent subsystems.

## Learning Goals

- See how a release-gate phase is the wrong place to discover small correctness bugs — they need to land before the gate, not in it
- Understand which security mitigations are cheap CR4 flips (SMEP, SMAP) versus expensive page-table reshapes (KPTI, retpoline — deferred to Phase 84)
- Learn why TCP retransmission and DNS resolution are pre-1.0 concerns even though "the network already works in QEMU"
- See how `PT_TLS` parsing is the difference between fragile and reliable multi-threaded musl programs
- Practice running a multi-track cleanup phase without scope creep

## Feature Scope

### Track A — PR #118 residuals

- **A.1** — SSH disconnect hang fix (`userspace/sshd/src/session.rs:1474`). The 2026-04-25 handoff documents the symptom: a clean client `exit` leaves the sshd session task spinning. The fix is in the session-shutdown ordering, not a protocol issue.
- **A.2** — `sys_nanosleep` busy-yield (`kernel/src/arch/x86_64/syscall/mod.rs:3174-3191`). Today the implementation `yield_now()`s in a loop until the deadline; under load it can starve PID 1. Replace with `block_current_until(deadline)` from the Phase 57a/74 deadline infrastructure.

### Track B — Cheap security mitigations (SMEP + SMAP)

- **B.1** — Enable `CR4.SMEP` (bit 20) on every CPU during boot if `CPUID.07h.EBX.SMEP` reports support. ~50 LOC including AP path. Causes a kernel #PF if ring 0 ever tries to fetch an instruction from a user page — an entire class of "smash userspace shellcode" exploits goes away.
- **B.2** — Enable `CR4.SMAP` (bit 21) similarly if `CPUID.07h.EBX.SMAP` reports support. Add the `stac` / `clac` instruction pair around the existing `copy_from_user` / `copy_to_user` helpers (they already centralize user-memory access). ~150 LOC total.

KPTI, retpoline, and IBRS are explicitly out of scope here — they go in Phase 84.

### Track C — `PT_TLS` parsing in the ELF loader

- **C.1** — `kernel/src/mm/elf.rs` currently skips `PT_TLS` segments. Parse the segment header, copy the initialized portion plus zero-init the BSS portion into a fresh TLS image at exec time, and place the image address into the `AT_*` aux vector in the form musl's `__init_tls` expects. ~200 LOC.
- **C.2** — Verify with a multi-threaded tcc-built test program that uses `__thread int x = 42;` and `pthread_create` to spawn 4 worker threads — each thread should see its own copy of `x`.

### Track D — Networking polish

- **D.1** — DNS resolution via the prebuilt musl resolver. There is no in-tree musl source tree — musl is a prebuilt cross-toolchain (`find_musl_cc`) whose libc already ships `getaddrinfo` / `gethostbyname` / `__dns_query`. The work is therefore wire-and-verify, not a C port: stage `/etc/resolv.conf` (only `passwd` / `shadow` / `group` are staged today) and exercise the resolver against the Phase 23 `socket(AF_INET, SOCK_DGRAM)` path (`sys_socket`, `udp::bind`), filling any missing syscall surface only as the resolver demands it. ~tens of LOC of xtask staging plus verification, not ~600 LOC of new C.
- **D.2** — TCP retransmission timer + multi-connection slot lift. The 4-slot fixed array in `kernel/src/net/tcp.rs` becomes a `BoundedVec` of N configurable slots (target N=64); retransmission honors RFC 6298 RTO. Without this, leaving QEMU's perfect LAN hangs the first time a packet is lost.

### Track E — Microcode loading

- **E.1** — Parse the SDM/AMD-defined microcode update header from a vendor-supplied blob (Intel: 48-byte header; AMD: `cpu_id_match` table → patch blob), write to `IA32_BIOS_UPDT_TRIG` / `MSR_AMD64_PATCH_LOADER` on every CPU during AP bring-up. ~300 LOC. Blob lives under `kernel/initrd/lib/firmware/`.

### Track F — `epoll_*` verify-and-implement-if-missing

- **F.1** — Source verification (2026-05-28) confirms `sys_epoll_create1` / `sys_epoll_ctl` / `sys_epoll_wait` **already exist and are fully implemented** (`kernel/src/arch/x86_64/syscall/mod.rs:18453` / `:18496` / `:18593`, dispatched at lines 1900-1913, backed by the `kernel/src/epoll.rs` module with FD-table integration, interest lists, and `WaitQueue`-backed blocking). Audit §2 had flagged them PARTIAL. So this track is verify-only: add a `userspace/epoll-smoke` regression and update the audit doc from PARTIAL to "wired and verified." No new implementation unless the smoke test surfaces a genuine gap, in which case it is filled against the existing `WaitQueue` infrastructure.

### Track G — Open-handoff resolution

- **G.1** — Doc-drift PR (Phase 74a §6). Walk each Phase-56-and-earlier "Deferred Until Later" list and strike items shipped in later phases, citing the shipping phase. Bookkeeping but matters for trust before Phase 83.
- **G.2** — Confirm the 2026-05-22 multi-term OOM reproducer is fixed (`cargo xtask run-gui --kvm --fresh`, launch 4× terminals at 4K). Record evidence in `docs/handoffs/`.
- **G.3** — Resolve the 2026-05-04 virtio-input migration "status unclear" handoff. Either close it as "shipped via Phase 56" or schedule the remaining work explicitly.
- **G.4** — Verify the 2026-04-28 graphical-stack-startup handoff on current main (cursor pinned at (0,0) on display_server start when display_server lands on an AP). Likely closed by the Phase 57a / 57b / 57e SMP discipline hardening (`pi_lock`, `with_block_state`, `wake_task_v2` precondition closure); if still reproducible, root-cause and fix. Otherwise mark the handoff resolved.
- **G.5** — Capture log on next occurrence of the 2026-05-13 mouse-reset-top-left handoff (PS/2 cursor enters sticky bad state after boot, resets to (0,0) on tiny motion). Build a serial-log dump path that survives reboot for after-the-fact analysis if the bug is too rare to reproduce reliably. Fix if a root cause emerges; otherwise document as a known intermittent issue.

### Track H — `/proc` compatibility for `htop` / `ps` / `top`

- **H.1** — Root-cause the 2026-05-20 htop-zero-processes handoff. The 2026-05-20 handoff lists five suspected causes; the implementation work likely centers on `kernel/src/fs/procfs.rs` (`getdents64` semantics, `/proc/<pid>/stat` field order/format, `/proc/<pid>/status` field set, `openat`-on-`/proc`-dir-fd) plus the userspace `getdents64` → `readdir` glue in the musl tree the on-target build pulls.
- **H.2** — Wire a `cargo xtask htop-smoke` (or fold into the existing `tui-app-smoke`) that boots m3OS, runs `htop`, captures the rendered cell grid, and asserts at least N>1 process rows. Without an automated gate this regression will silently come back.
- **H.3** — Verify `ps aux` and `top` (BusyBox or coreutils equivalents) also show non-zero process lists — they consume the same `/proc` files and should now work as a side effect.

## Important Components and How They Work

### `sys_nanosleep` deadline path

The Phase 57a `block_current_until(deadline, queue)` primitive is the existing mechanism the rest of the kernel uses to wait without busy-yielding. Re-wiring `sys_nanosleep` onto it removes the only remaining busy-yield syscall.

### CR4 bit gating

SMEP/SMAP are pure CR4 flips guarded by a CPUID check. The AP boot path applies them in the same place it sets PAE, PGE, and OSFXSR. STAC/CLAC are needed only where the kernel deliberately reads or writes user memory — every such site already routes through `copy_from_user_safe` / `copy_to_user_safe`, so the change is centralized.

### `PT_TLS` and musl's `__init_tls`

musl reads the TLS template from `AT_PHDR` + `AT_PHENT` + `AT_PHNUM` plus a runtime calculation. Today m3OS supplies the first three correctly but the TLS image is left unmapped — musl works around this with a reserved-stack-space hack. After Track C the kernel maps the TLS image properly and musl's `__init_tls` finds it the standard way.

### TCP retransmission

A per-connection RTO timer (one-shot, rescheduled on every ack). The four-slot array becomes a `BoundedVec<TcpConnection, MAX_TCP_CONNECTIONS>` where `MAX_TCP_CONNECTIONS = 64`. Connection-state machine is unchanged.

## How This Builds on Earlier Phases

- Extends Phase 74's deadline path (`block_current_until`) by using it for `sys_nanosleep`.
- Extends Phase 23's socket API with the DNS resolver — no new syscalls needed, just a userspace library.
- Extends Phase 16's TCP implementation with retransmission and a wider connection cap.
- Closes the W+X dead-code follow-up to Phase 75 by making sure all of the boot path correctly applies SMEP/SMAP for completeness.
- Closes the Phase 11 ELF-loader gap that Phase 40 (Threading) silently relied on.

## Implementation Outline

1. Land Track A (SSH hang + nanosleep) — pure bug fixes, no API surface change.
2. Land Track B (SMEP + SMAP) — CR4 + STAC/CLAC; verify with a deliberate ring-0 dereference of a user page in a debug-only test that should fault.
3. Land Track C (`PT_TLS`) — write the host-test against `kernel-core` first, then wire into the kernel loader.
4. Land Track D.1 (DNS) and D.2 (TCP) in parallel.
5. Land Track E (microcode loading) on BSP first, then AP path.
6. Land Track F (`epoll_*` verify, implement if absent).
7. Land Track H (htop / `/proc` compatibility) — root-cause from the five suspects in the 2026-05-20 handoff, fix, wire a smoke gate.
8. Land Track G (open-handoff resolution) last — the doc-drift PR should reflect what actually shipped in this bundle; the open-handoff verification motions confirm the cleanup is real.
9. Bump kernel to `0.77.0`.

## Acceptance Criteria

- `userspace/sshd` cleanly terminates when an SSH client sends `exit` — no spinning task, no hung session.
- `cargo xtask regression --test serverization-fallback` passes 10/10 consecutive runs (the flake from PR #118 is gone).
- `CR4.SMEP` and `CR4.SMAP` are visible in `/proc/cpuinfo`-equivalent on every CPU where supported; a debug-only kernel test that fetches an instruction from a user page faults with `#PF`.
- A new `userspace/tls-smoke` test exercises `__thread` variables across 4 threads — each thread sees its own value.
- `getaddrinfo("github.com", ...)` returns at least one address on a network that has DNS reachable.
- A new `userspace/tcp-loss-smoke` test exercises a 100-MB transfer through a QEMU netem-style loss filter (5% drop) and completes without hang.
- A microcode `log::info!` reports the loaded patch level on every CPU at boot.
- Either `epoll_*` syscalls exist and a new `userspace/epoll-smoke` passes, or the audit doc is updated to record that they were already wired and the verification is done.
- `htop` shows a non-empty process list when launched from `term` (both as root and as an unprivileged user). A new `htop-smoke` (or `tui-app-smoke` extension) asserts at least N>1 process rows.
- `ps aux` and the chosen `top` variant show the same non-empty list.
- The 2026-04-28 graphical-stack-startup handoff is closed (either verified resolved on current main, or root-caused and fixed if still reproducible).
- The 2026-05-13 mouse-reset-top-left handoff has at least one captured-log instance OR is downgraded to "known intermittent issue, post-1.0 follow-up."
- The Phase 74a §6 doc-drift items are struck from their owning phase docs with citations to the shipping phase.
- Kernel bumped to `0.77.0`.

## Companion Task List

- [Phase 77 Task List](./tasks/77-pre-1-0-cleanup-tasks.md)

## How Real OS Implementations Differ

- Mature OSes ship SMEP/SMAP/KPTI/retpoline as a single coordinated set; m3OS is splitting cheap (this phase) from expensive (Phase 84) explicitly to keep the 1.0 scope honest.
- Real OSes maintain microcode update packages as distribution artifacts with signed updates; m3OS embeds a static blob and expects the user to rebuild the disk image for updates.
- `PT_TLS` handling in Linux is much more elaborate (`dl_tls`, dtv, lazy initialization, `tlsdesc`); m3OS does the simple static-TLS-only case and lets musl handle the rest.
- Production TCP stacks have per-connection congestion control state machines (CUBIC, BBR); m3OS will ship Reno-style RTO retransmission only.
- Real DNS resolvers cache, handle search domains, parse `nsswitch.conf`; the stub here just does the `/etc/resolv.conf` → UDP → first-A-record path.

## Deferred Until Later

- KPTI, retpoline, IBRS toggling — Phase 84
- TCP congestion control beyond Reno-style retransmission — post-1.0
- IPv6 and DHCPv6 — Phase 89
- DNS caching and DNSSEC — post-1.0
- Microcode update via online package management — never (rebuild the disk image)
- `tlsdesc` and `PT_TLS` for `dlopen`-loaded `.so` modules — Phase 76 (dynamic linker) territory
