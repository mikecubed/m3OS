# Phase 77 — Pre-1.0 Correctness, Cheap Security, and Network Polish: Task List

**Status:** Planned
**Source Ref:** phase-77
**Depends on:** Phase 74 (IPC Capability Grants) ✅, Phase 75 (W^X Enforcement) ✅
**Goal:** Land the bundle of small, well-scoped correctness, security, and networking fixes the Phase 74a audit promoted into pre-1.0 must-fix. After this phase the SSH session exits cleanly, `sys_nanosleep` never starves PID 1, SMEP+SMAP are on where supported, `PT_TLS` works, DNS resolves names, TCP survives packet loss, `htop`/`ps`/`top` show real processes, microcode loads on every CPU, `epoll_*` is verified, the earlier-phase deferral lists are de-drifted, and the kernel is bumped to `0.77.0`. No track may expand into an adjacent subsystem.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | PR #118 residuals (SSH hang, `sys_nanosleep`) | Phase 74 ✅ | Planned |
| B | Cheap security mitigations (SMEP + SMAP) | Phase 75 ✅ | Planned |
| C | `PT_TLS` parsing in the ELF loader | Phase 74 ✅ | Planned |
| D | Networking polish (DNS resolver, TCP retransmit + slot lift) | — | Planned |
| E | Microcode loading | — | Planned |
| F | `epoll_*` verify-and-implement-if-missing | — | Planned |
| G | Open-handoff resolution + doc-drift PR | A–F, H | Planned |
| H | `/proc` compatibility for `htop` / `ps` / `top` | — | Planned |
| I | Documentation and Release (learning doc + `0.77.0` bump) | A–H | Planned |

> **Review note (2026-05-28):** The Phase 77 design doc was source-verified before this task list was authored. Four claims drifted from current `main` and are corrected in-place below: (1) `sys_nanosleep` already blocks for ≥1 ms sleeps — A.2 is re-scoped to closing residual busy paths, not a rewrite; (2) `epoll_*` syscalls already exist and are fully implemented — F.1 is verify + smoke + audit-doc, not implementation; (3) the TCP table is **8** slots, not 4; (4) there is **no in-tree musl source tree** — musl is a prebuilt cross-toolchain, so D.1 stages `/etc/resolv.conf` and verifies the prebuilt resolver rather than porting `__dns_query` C source.

---

## Track A — PR #118 Residuals

### A.1 — SSH disconnect hang fix

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `async_session` (line 204), `cleanup` (line 1526) — the session-teardown ordering invoked from the `exit`/EOF path at line 281
**Why it matters:** The 2026-04-25 handoff (`docs/handoffs/2026-04-25-pr-118-residual-issues.md` + `-update.md`) documents that a clean client `exit` leaves the sshd session task spinning. The roadmap cited `session.rs:1474`; the teardown logic now lives in `cleanup` near line 1526, so the fix is in the shutdown ordering, not the SSH protocol. The current `cleanup` already escalates SIGTERM → 500 ms poll → SIGKILL, so the residual spin is most likely a channel/EOF or PTY-close ordering issue that leaves the task runnable after the shell PID reaps.

**Acceptance:**
- [ ] The 2026-04-25 spin is reproduced (or confirmed already-fixed) on current `main`, with the reproduction recipe recorded in the PR description
- [ ] After a client `exit`, the sshd session task terminates — no busy-spinning task remains (verified via `/proc` process count returning to the pre-connect baseline, or via serial-log absence of repeated session-loop log lines)
- [ ] The shell child PID is reaped exactly once and the PTY master/slave fds are closed in an order that does not leave the session loop runnable
- [ ] `cargo xtask regression --test serverization-fallback` passes 10/10 consecutive runs (the PR #118 flake is gone)
- [ ] The 2026-04-25 handoff doc is marked resolved with a citation to this task

### A.2 — `sys_nanosleep` residual busy-yield closure

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_nanosleep` (line 3754); deadline primitive `block_current_until` (`kernel/src/task/scheduler.rs:3209`)
**Why it matters:** The roadmap claimed `sys_nanosleep` busy-yields in a loop at lines 3174-3191 and asked to "replace with `block_current_until`." That replacement has **already shipped** for the common case: the sched-v2 path for sleeps ≥ 1 ms blocks on `block_current_until` (lines 3788-3803). Two residual non-blocking paths remain and are the actual scope of this task: (a) the uncalibrated-TSC fallback yield-loop (lines 3806-3816), and (b) the v1 (non-sched-v2) "long sleep ≥ 1 ms" yield path (lines 3829+). The sub-ms TSC busy-spin (lines 3817-3828) is intentional and documented — a context switch costs more than the sleep.

**Acceptance:**
- [ ] Static audit confirms the ≥ 1 ms sched-v2 path routes through `block_current_until` with an absolute tick deadline and honours `EINTR` on a pending signal (verification of existing behaviour)
- [ ] The uncalibrated-TSC fallback (lines 3806-3816) either blocks via `block_current_until` once ticks are available, or is documented in-code as a boot-window-only path that cannot occur after APIC/TSC calibration completes
- [ ] The v1 long-sleep yield path is either removed (if sched-v2 is the only live scheduler) or re-pointed at `block_current_until`; if v1 is dead, the dead branch is deleted rather than left in place
- [ ] The intentional sub-ms TSC busy-spin retains its existing explanatory comment and is **not** changed
- [ ] A regression demonstrates PID 1 is not starved: under N background tasks each issuing `nanosleep(50ms)` in a loop, PID 1 continues to make progress (measured via a liveness counter or serial heartbeat)

---

## Track B — Cheap Security Mitigations (SMEP + SMAP)

### B.1 — Enable `CR4.SMEP` (bit 20) on every CPU when supported

**Files:**
- `kernel/src/smp/boot.rs`
- `kernel/src/arch/x86_64/cpuid.rs`

**Symbol:** `save_bsp_cr4_for_aps` (BSP CR4 capture, `smp/boot.rs` ~line 216), `ap_entry` (AP CR4 load, `smp/boot.rs` line 408, CR4 store ~lines 409-416); `cpuid_raw` + the `XSaveFeatures`-style probe in `arch/x86_64/cpuid.rs`
**Why it matters:** CR4 is configured in `smp/boot.rs`, not `cpu.rs` as the roadmap's component list implied — the BSP saves its CR4 for the AP trampoline and each AP reloads it. Setting CR4.SMEP causes a kernel `#PF` if ring 0 ever fetches an instruction from a user page, eliminating an entire class of "smash userspace shellcode" exploits. ~50 LOC including the AP path.

**Acceptance:**
- [ ] A `CPUID.07h:EBX[7]` (SMEP) check is added following the existing `cpuid_raw`/feature-struct pattern in `arch/x86_64/cpuid.rs`
- [ ] When supported, `CR4` bit 20 is set on the BSP before APs are woken, and the saved-CR4 value the APs reload already carries the bit (so no separate AP code path is needed beyond confirming the bit propagates)
- [ ] A debug-only kernel test (gated behind a debug feature) deliberately fetches an instruction from a user-mapped page and asserts the resulting `#PF` — proving SMEP is live
- [ ] On hardware/QEMU configs without SMEP support, boot proceeds unchanged (the bit is not forced)
- [ ] A `log::info!` reports SMEP enabled (or "unsupported") on each CPU at boot

### B.2 — Enable `CR4.SMAP` (bit 21) + STAC/CLAC around user-memory copies

**Files:**
- `kernel/src/smp/boot.rs`
- `kernel/src/arch/x86_64/cpuid.rs`
- `kernel/src/mm/user_mem.rs`

**Symbol:** `copy_from_user` (`mm/user_mem.rs:41`), `copy_to_user` (`mm/user_mem.rs:116`) — the two centralized primitives behind `UserSliceRo::copy_to_kernel` (line 369) and `UserSliceWo::copy_from_kernel` (line 420)
**Why it matters:** SMAP faults if ring 0 reads or writes a user page without an explicit `stac`/`clac` window. Every deliberate user-memory access already funnels through the two `user_mem.rs` primitives, so the change is centralized. ~150 LOC total.

**Acceptance:**
- [ ] A `CPUID.07h:EBX[20]` (SMAP) check is added alongside the SMEP probe
- [ ] When supported, `CR4` bit 21 is set on the BSP and propagated to APs via the same saved-CR4 path as B.1
- [ ] `stac` precedes and `clac` follows the user-pointer dereference inside `copy_from_user` and `copy_to_user` only (not around the surrounding bounds checks); the asm is wrapped in an explicit `unsafe {}` block per edition-2024 rules
- [ ] No other call site touches user memory directly — a `grep` audit confirms all user reads/writes route through `user_mem.rs`; any stray site found is either routed through the helpers or given its own STAC/CLAC window with a comment
- [ ] A debug-only kernel test deliberately reads a user page *without* the STAC window and asserts a `#PF`, then reads through `copy_from_user` and asserts success
- [ ] `cargo xtask test` and `cargo xtask smoke-test` pass — confirming no syscall regressed from the SMAP window

> KPTI, retpoline, and IBRS are explicitly **out of scope** — Phase 84.

---

## Track C — `PT_TLS` Parsing in the ELF Loader

### C.1 — Parse `PT_TLS` and make the static-TLS template reachable by musl

**Files:**
- `kernel/src/mm/elf.rs`
- `kernel-core/src/elf/auxv.rs`

**Symbol:** `load_elf_into_with_interp` (`elf.rs:743`, phdr iteration ~796-814), `map_load_segment` (`elf.rs:315`), `LoadedElf` (`elf.rs:147`), `setup_abi_stack_with_envp` (`elf.rs:544`), `build_layout` (`kernel-core/src/elf/auxv.rs:96`)
**Why it matters:** The phdr loop today handles only `PT_LOAD` (1), `PT_DYNAMIC` (2), and `PT_INTERP` (3); `PT_TLS` (7) is silently ignored and has no constant. musl's `__init_tls` discovers the TLS template by walking the program headers via `AT_PHDR`/`AT_PHENT`/`AT_PHNUM` (which the kernel already supplies correctly), so the gap is in the kernel parsing the segment, recording its initialized image + bss size, and ensuring the phdr table and `.tdata` image are reachable in the user address space. The implementer must pin the exact mechanism against musl's `__init_tls`/`__copy_tls` during implementation — either (a) confirm the phdr-discovered static template works once mapped, or (b) stage a TLS image and thread its address through a new aux entry in `build_layout`.

**Acceptance:**
- [ ] A `PT_TLS = 7` constant is added and the phdr loop in `load_elf_into_with_interp` parses the `PT_TLS` header (alignment, `p_filesz`, `p_memsz`, `p_vaddr`/`p_offset`)
- [ ] The initialized portion (`p_filesz`) is preserved and the BSS portion (`p_memsz - p_filesz`) is zero-init in the TLS template image, mapped (or made reachable) in the user address space
- [ ] `LoadedElf` carries any new TLS metadata required by the chosen mechanism; if an aux entry is added, `build_layout` emits it with byte-exact ordering pinned by a `kernel-core` host test (matching the existing static/dynamic auxv ordering tests)
- [ ] `setup_abi_stack_with_envp` continues to supply `AT_PHDR`/`AT_PHENT`/`AT_PHNUM` correctly (regression-checked)
- [ ] The C.2 multi-threaded test passes — this is the load-bearing functional proof

### C.2 — Multi-threaded `__thread` TLS smoke test

**File:** `userspace/tls-smoke/` (new musl-built C binary; see "Adding a New Userspace Binary" in `AGENTS.md`)
**Symbol:** `main` (musl-built, links pthread), `__thread int x = 42;`
**Why it matters:** TLS correctness is only provable at runtime across real threads; a static audit cannot show each thread sees its own copy.

**Acceptance:**
- [ ] The binary declares `__thread int x = 42;`, spawns 4 worker threads via `pthread_create`, and each worker writes its thread index into `x` and reads it back
- [ ] Each thread observes its own value of `x` (no cross-thread bleed); the main thread still sees `42`
- [ ] The binary prints `TLS_SMOKE:PASS` on success and `TLS_SMOKE:FAIL <detail>` on mismatch, exiting 0 / non-zero respectively
- [ ] Wired into the `smoke-runner` gate so `cargo xtask smoke-test` execs `/bin/tls-smoke` and asserts `SMOKE:tls-smoke:PASS`
- [ ] All four ramdisk/workspace/xtask/embedding wiring points from `AGENTS.md` are completed

---

## Track D — Networking Polish

### D.1 — DNS resolution via the prebuilt musl resolver + `/etc/resolv.conf`

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files`, ~line 12298; `etc/passwd` staging at ~13102 is the pattern to follow)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`)
- `kernel/src/net/udp.rs` (`bind`, line 18) and `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_socket`, line 15638)

**Symbol:** `populate_ext2_files`, `sys_socket`, `udp::bind`
**Why it matters:** The roadmap framed this as "port ~600 LOC of C into the musl tree." That does not match this repo: there is **no in-tree musl source** — musl is a prebuilt cross-toolchain resolved via `find_musl_cc`, and `getaddrinfo`/`gethostbyname` already ship in its libc. The real gap is that `/etc/resolv.conf` is not staged (only `passwd`/`shadow`/`group` are) and the prebuilt resolver has never been exercised against m3OS's `socket(AF_INET, SOCK_DGRAM)` syscall path. `sys_socket` (UDP path lines 15642-15665) and `udp::bind` exist, so the work is wiring + verification, with any missing syscall surface filled only as the resolver demands it.

**Acceptance:**
- [ ] `/etc/resolv.conf` is staged onto the ext2 data disk by `populate_ext2_files` (and added to `KNOWN_CONFIGS` if init manages it), with a sane default nameserver for the QEMU user-net (e.g. `10.0.2.3`) overridable by the user
- [ ] A `cargo xtask clean` + boot confirms `/etc/resolv.conf` is present and readable at runtime
- [ ] `getaddrinfo("github.com", ...)` (or `gethostbyname`) returns at least one address on a network with DNS reachable, driven by a new `userspace/dns-smoke` binary
- [ ] Any syscall the musl resolver requires but m3OS lacks is identified and either filled minimally or documented as the blocker; the resolver's UDP traffic is confirmed to flow through `sys_socket`/`udp::bind`
- [ ] The roadmap's "~600 LOC C port" framing is corrected in the Phase 77 design doc's Feature Scope to reflect the prebuilt-toolchain reality

### D.2 — TCP retransmission timer + multi-connection slot lift

**File:** `kernel/src/net/tcp.rs`
**Symbol:** `MAX_TCP_CONNECTIONS` (line 291, **currently `8`** — the roadmap's "4-slot" claim is stale), `TcpConnections` (line 293), `TcpConnection` (line 66), `TCP_CONNS` (line 309)
**Why it matters:** The fixed 8-element `[Option<TcpConnection>; 8]` array caps concurrent connections far below a realistic 1.0 workload, and there is **no retransmission logic** today (only a comment about "retransmitting SYNs" at line 216 — no RTO timer, no resend queue). On QEMU's perfect LAN this is invisible; the first dropped packet on the real internet hangs the connection.

**Acceptance:**
- [ ] `MAX_TCP_CONNECTIONS` is raised to 64 and the backing storage becomes a `BoundedVec<TcpConnection, MAX_TCP_CONNECTIONS>` (or an equivalent bounded structure); the const-`new()` no longer hard-codes a fixed list of `None`s
- [ ] A per-connection one-shot RTO timer is added, rescheduled on every ACK, computing RTO per RFC 6298 (SRTT/RTTVAR estimation with the standard 1 s minimum / 60 s maximum clamps)
- [ ] On RTO expiry the oldest unacknowledged segment is retransmitted and RTO doubles (exponential backoff), capped at the RFC maximum
- [ ] The connection-state machine is otherwise unchanged (verified by the existing TCP tests still passing)
- [ ] A new `userspace/tcp-loss-smoke` test transfers 100 MB through a QEMU netem-style 5% drop filter and completes without hang
- [ ] 64 concurrent connections can be opened (a smoke or kernel test exercises the raised cap)

---

## Track E — Microcode Loading

### E.1 — Parse vendor microcode header and apply on every CPU at boot

**Files:**
- `kernel/src/smp/boot.rs` (`ap_entry`, line 408 — per-CPU init hook ~lines 415-424)
- `kernel/src/smp/mod.rs` (`wrmsr` pattern, e.g. `write_gs_base` line 522)
- `kernel/initrd/lib/firmware/` (new directory — does not exist today)

**Symbol:** `ap_entry`, a new microcode-apply helper modelled on the inline-asm `wrmsr` in `write_gs_base`
**Why it matters:** No microcode code exists (`grep` for `microcode`/`IA32_BIOS_UPDT_TRIG`/`PATCH_LOADER`/`ucode` is empty). The dev laptop's CPU boots with whatever patch level the firmware left. The AP bring-up path already runs a clean per-CPU init sequence (CR4, XCR0, GDT, IDT, MSRs), so microcode application slots in right after CR4/XCR0. ~300 LOC. The blob is a static, embedded artifact — updates require rebuilding the disk image.

**Acceptance:**
- [ ] A vendor-supplied microcode blob is embedded under `kernel/initrd/lib/firmware/` (Intel 48-byte header path **or** AMD `cpu_id_match` table → patch-blob path, matching the dev laptop's vendor)
- [ ] The header is parsed and validated (revision, processor signature / `cpu_id_match`) before any MSR write
- [ ] The patch is written to `IA32_BIOS_UPDT_TRIG` (0x79, Intel) or `MSR_AMD64_PATCH_LOADER` (0xC0010020, AMD) on the BSP first, then on every AP during bring-up via a new helper following the `write_gs_base` inline-asm convention
- [ ] A `log::info!` reports the loaded patch level (read back from `IA32_BIOS_SIGN_ID` / AMD `PATCH_LEVEL`) on every CPU at boot
- [ ] If the blob does not match the running CPU signature, the load is skipped with a `log::warn!` and boot continues unchanged

---

## Track F — `epoll_*` Verify (Implement Only If Missing)

### F.1 — Confirm `epoll_*` syscalls and add a smoke gate

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/epoll.rs`
- `docs/appendix/audit-status/74a-pre-1.0-audit.md`

**Symbol:** `sys_epoll_create1` (`syscall/mod.rs:18453`), `sys_epoll_ctl` (line 18496), `sys_epoll_wait` (line 18593); dispatch arms at lines 1900-1913; the `epoll` module at `kernel/src/epoll.rs`
**Why it matters:** Audit §2 flagged `epoll_*` as PARTIAL/possibly-absent. Source verification shows all three handlers **exist and are fully implemented** (FD-table integration, interest lists, wait-queue-backed blocking, close-on-exec cleanup). So this track is verification + a regression gate + an audit correction — **not** new implementation.

**Acceptance:**
- [ ] A new `userspace/epoll-smoke` binary registers a readable fd (pipe or socket) with `epoll_ctl(EPOLL_CTL_ADD)`, writes to it, and asserts `epoll_wait` reports the fd ready with the correct event mask; it also exercises `EPOLL_CTL_MOD`, `EPOLL_CTL_DEL`, and the timeout path
- [ ] The binary prints `EPOLL_SMOKE:PASS` / `EPOLL_SMOKE:FAIL <detail>` and is wired into the `smoke-runner` gate (`SMOKE:epoll-smoke:PASS`)
- [ ] `docs/appendix/audit-status/74a-pre-1.0-audit.md` §2 is updated from PARTIAL to "wired and verified," citing the three handler line numbers and the new smoke gate
- [ ] No new syscall handler is added unless the smoke test surfaces a genuine gap; if it does, the gap is implemented against the existing `WaitQueue` infrastructure (`kernel/src/task/wait_queue.rs`) and noted explicitly

---

## Track G — Open-Handoff Resolution + Doc-Drift PR

### G.1 — Doc-drift PR: strike shipped items from Phase-56-and-earlier deferral lists

**Files:** `docs/roadmap/50-*.md` through `docs/roadmap/56-*.md` "Deferred Until Later" sections (notably Phases 50, 51, 52a, 52b, 52c, 53a, 54, 55a, 55b, 56)
**Symbol:** N/A (doc bookkeeping)
**Why it matters:** Phase 74a §6 found substantial drift: deferral lists in phases 50-56 still claim items as deferred that later phases actually shipped. This matters for trust before the Phase 83 release gate.

**Acceptance:**
- [ ] Each Phase-56-and-earlier "Deferred Until Later" entry is walked; any item shipped in a later phase is struck (or annotated "Delivered in Phase NN") with a citation to the shipping phase
- [ ] No deferral entry remains that contradicts a shipped capability
- [ ] The PR description lists each struck item and the phase that delivered it, so the change is auditable

### G.2 — Confirm the 2026-05-22 multi-term OOM reproducer is fixed

**File:** `docs/handoffs/2026-05-22-compositor-shm-leak-multi-term-oom.md`
**Symbol:** N/A (verification + evidence capture)
**Why it matters:** The handoff documents an OOM when launching 4× terminals at 4K; it must be confirmed fixed (or re-opened) before 1.0.

**Acceptance:**
- [ ] `cargo xtask run-gui --kvm --fresh` is run and 4× terminals are launched at 4K
- [ ] System memory stays bounded (no OOM, no runaway SHM growth) — evidence (serial/memory trace or screenshot) recorded under `docs/handoffs/`
- [ ] The handoff is marked resolved with the evidence cited, or re-opened with a root-cause note if it still reproduces

### G.3 — Resolve the 2026-05-04 virtio-input migration "status unclear" handoff

**File:** `docs/handoffs/2026-05-04-virtio-input-migration.md`
**Symbol:** N/A
**Why it matters:** An unresolved "status unclear" handoff is a trust gap before the release gate.

**Acceptance:**
- [ ] The handoff is either closed as "shipped via Phase 56" with a citation, or the remaining work is scheduled explicitly into a named phase
- [ ] The handoff doc reflects the decision

### G.4 — Verify the 2026-04-28 graphical-stack-startup handoff on current `main`

**File:** `docs/handoffs/2026-04-28-graphical-stack-startup.md`
**Symbol:** N/A (verification; root-cause only if reproducible)
**Why it matters:** The handoff reports the cursor pinned at (0,0) when `display_server` lands on an AP. It is likely closed by the Phase 57a/57b/57e SMP discipline hardening (`pi_lock`, `with_block_state`, `wake_task_v2` precondition closure), but must be confirmed.

**Acceptance:**
- [ ] The graphical stack is started repeatedly (forcing `display_server` onto an AP) and the cursor-at-(0,0) symptom is checked
- [ ] If not reproducible, the handoff is marked resolved citing the Phase 57a/b/e hardening
- [ ] If still reproducible, it is root-caused and fixed, with the fix referenced in the handoff

### G.5 — Capture-log path for the 2026-05-13 mouse-reset-top-left handoff

**File:** `docs/handoffs/2026-05-13-mouse-reset-top-left-intermittent.md`
**Symbol:** N/A
**Why it matters:** The PS/2 cursor intermittently enters a sticky bad state and resets to (0,0) on tiny motion. It is too rare to reproduce on demand, so an after-the-fact capture path is needed.

**Acceptance:**
- [ ] A serial-log dump path that survives reboot is built (or an existing one is confirmed) so the bad state can be analysed after the fact
- [ ] At least one captured-log instance is recorded **OR** the handoff is explicitly downgraded to "known intermittent issue, post-1.0 follow-up"
- [ ] If a root cause emerges from a capture, it is fixed; otherwise the downgrade decision is documented in the handoff

---

## Track H — `/proc` Compatibility for `htop` / `ps` / `top`

### H.1 — Root-cause and fix the 2026-05-20 htop-zero-processes regression

**Files:**
- `kernel/src/fs/procfs.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_linux_getdents64`, lines 13392-13450; dispatch at line 1755)

**Symbol:** `list_dir` (`procfs.rs:160`), `render_pid_stat` (line 701), `render_status` (line 550), `render_comm_bytes` (line 661), `sys_linux_getdents64`
**Why it matters:** `htop` shows zero processes. The 2026-05-20 handoff lists five suspected causes, ranked: (1) `getdents64` semantics (`d_off`/`d_reclen` alignment), (2) `/proc/<pid>/stat` field count or `(comm)` paren escaping, (3) `/proc/<pid>/status` missing fields (`Tgid`, `VmRSS`, `VmData`, `VmStk`), (4) `openat(dirfd, "/proc/<pid>")` across a dir fd, (5) `/proc/cpuinfo` or `/proc/stat` cpu-line parser failure. `render_status` today emits only Name/State/Pid/PPid/Uid/Gid/Threads/VmSize/Cwd — the missing fields are a confirmed candidate.

**Acceptance:**
- [ ] The root cause is identified among the five suspects (or a sixth), with the diagnosis recorded in the PR and the handoff
- [ ] `/proc/<pid>/status` emits the fields htop reads, including `Tgid`, `VmRSS`, `VmData`, and `VmStk` (added where missing)
- [ ] `getdents64` over `/proc` returns correctly aligned `d_reclen`/`d_off` records for the root listing and per-pid directories, and per-pid dirs are openable via `openat` on a dir fd
- [ ] `/proc/<pid>/stat` field order/format and the `(comm)` parenthesization are confirmed correct (a process whose name contains `)` does not break the parse)
- [ ] `htop` launched from `term` shows a non-empty process list (verified manually and via the H.2 gate), both as root and as an unprivileged user

### H.2 — `htop-smoke` gate folded into `tui-app-smoke`

**File:** `xtask/src/main.rs`
**Symbol:** `tui_app_smoke_steps` (line 9831) — htop is already a step here; this task adds the process-row assertion
**Why it matters:** Without an automated gate this regression silently returns. The htop launch already exists in `tui-app-smoke`; what is missing is an assertion on the rendered process count.

**Acceptance:**
- [ ] The existing `tui-app-smoke` htop branch (or a new `htop-smoke` step) captures the rendered cell grid and asserts at least `N > 1` process rows are visible
- [ ] The assertion fails loudly (distinct exit code, e.g. the existing `SMOKE_EXIT_TUI_APP_SMOKE_FAILED=69`) when htop renders zero process rows
- [ ] The gate runs under `cargo xtask tui-app-smoke` and is exercised by the pre-push hook behind `M3OS_TUI_APP_REGRESSION=1`

### H.3 — Verify `ps aux` and `top` show non-empty process lists

**Files:** `userspace/` (BusyBox/coreutils `ps` and `top` equivalents), consuming `kernel/src/fs/procfs.rs`
**Symbol:** N/A (consumes the same `/proc` files fixed in H.1)
**Why it matters:** `ps` and `top` read the same `/proc` files as htop and should work as a side effect of H.1 — confirming this closes the compatibility story.

**Acceptance:**
- [ ] `ps aux` (the chosen BusyBox/coreutils variant) shows a non-empty process list matching the live process set
- [ ] `top` shows the same non-empty list and refreshes
- [ ] If either still shows zero after H.1, the residual `/proc` gap is identified and fixed (not deferred)

---

## Track I — Documentation and Release

### I.1 — Create the Phase 77 learning doc

**File:** `docs/77-pre-1-0-cleanup.md`
**Symbol:** N/A
**Why it matters:** A learner-friendly doc scoped to Phase 77 consolidates the cleanup story — SSH/nanosleep correctness, SMEP/SMAP, `PT_TLS`, DNS, TCP retransmission, microcode, epoll verification, and the `/proc` compatibility fix — so readers do not have to reconstruct the bundle from eight separate tracks. Follows the "aligned legacy learning doc" template in `docs/appendix/doc-templates.md`.

**Acceptance:**
- [ ] File exists at `docs/77-pre-1-0-cleanup.md`
- [ ] All required template fields populated: `**Aligned Roadmap Phase:** Phase 77`, `**Status:** Complete` (at merge), `**Source Ref:** phase-77`, `**Supersedes Legacy Doc:** new`
- [ ] Overview explains, learner-first, why a release-gate phase is the wrong place to discover small correctness bugs and which mitigations are cheap CR4 flips versus expensive page-table reshapes
- [ ] Key Files table cites the real files this phase touches: `userspace/sshd/src/session.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`, `kernel/src/smp/boot.rs`, `kernel/src/mm/user_mem.rs`, `kernel/src/mm/elf.rs`, `kernel/src/net/tcp.rs`, `kernel/src/net/udp.rs`, `kernel/src/fs/procfs.rs`
- [ ] "How This Phase Differs From Later Memory/Security Work" notes KPTI/retpoline/IBRS are Phase 84 and congestion control beyond Reno is post-1.0
- [ ] Related Roadmap Docs links `docs/roadmap/77-pre-1-0-cleanup.md` and `docs/roadmap/tasks/77-pre-1-0-cleanup-tasks.md`

### I.2 — Bump kernel version to `0.77.0`

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]` (currently `0.76.3` at line 3)
**Why it matters:** Project convention is one minor-version bump per shipped phase; disciplined version tracking signals a complete, shippable phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.77.0"`
- [ ] `Cargo.lock` regenerated (via `cargo xtask check`)
- [ ] `AGENTS.md` "Kernel v0.76.3" updated to `v0.77.0` and the project-overview summary extended with a Phase 77 sentence
- [ ] `docs/roadmap/README.md` Phase 77 row Status updated to "Complete" and the Tasks cell pointed at this task list at merge time
- [ ] `cargo xtask check` passes
- [ ] Git tag `v0.77.0` recommended at phase merge

---

## Documentation Notes

- **Scope discipline is the headline risk for this phase.** Each track is sized to ship in under a week; no track may expand into an adjacent subsystem. The design doc's "Implementation Outline" sequencing (A → B → C → D.1/D.2 → E → F → H → G → bump) should be followed so the doc-drift PR (G.1) reflects what actually shipped.
- **Four roadmap claims were stale and are corrected in this task list** (see the review note under Track Layout): `sys_nanosleep` already blocks for ≥1 ms (A.2 is residual-closure), `epoll_*` already exists (F.1 is verify-only), the TCP table is 8 slots not 4 (D.2), and there is no in-tree musl source so DNS is wire-and-verify not C-port (D.1). The Phase 77 **design doc** should be amended for D.1 and F.1 to match reality — flagged in D.1's acceptance and F.1's audit-doc update.
- CR4 configuration lives in `kernel/src/smp/boot.rs` (BSP saves CR4 for the AP trampoline; APs reload it), **not** `kernel/src/arch/x86_64/cpu.rs` as the design doc's Primary Components line implied. SMEP/SMAP (Track B) flip bits in that saved value so a single change propagates to all CPUs.
- Track B's STAC/CLAC windows must wrap **only** the user-pointer dereference inside `copy_from_user`/`copy_to_user`, not the surrounding bounds checks, and must be explicit `unsafe {}` blocks (edition 2024).
- Track C's functional proof is the C.2 multi-threaded test; the exact kernel mechanism (map phdrs so musl discovers `PT_TLS`, vs. stage a TLS image + new aux entry) must be pinned against musl's `__init_tls`/`__copy_tls` during implementation rather than assumed.
- Track H.1 should start from the five ranked suspects in the 2026-05-20 handoff; the `/proc/<pid>/status` missing-fields suspect (`Tgid`/`VmRSS`/`VmData`/`VmStk`) is the cheapest to confirm first.
- The learning doc (I.1) should be authored after Tracks A–H so it can cite the actual fixes and the corrected DNS/epoll scoping as concrete examples.
- After adding any new service config or staged file (e.g. `/etc/resolv.conf` in D.1), run `cargo xtask clean` to force ext2 disk recreation.
