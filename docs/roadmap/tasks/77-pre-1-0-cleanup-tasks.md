# Phase 77 — Pre-1.0 Correctness, Cheap Security, and Network Polish: Task List

**Status:** Complete
**Source Ref:** phase-77
**Depends on:** Phase 74 (IPC Capability Grants) ✅, Phase 75 (W^X Enforcement) ✅
**Goal:** Land the bundle of small, well-scoped correctness, security, and networking fixes the Phase 74a audit promoted into pre-1.0 must-fix. After this phase the SSH session exits cleanly, `sys_nanosleep` never starves PID 1, SMEP+SMAP are on where supported, `PT_TLS` works, DNS resolves names, TCP survives packet loss, `htop`/`ps`/`top` show real processes, microcode loads on every CPU, `epoll_*` is verified, the earlier-phase deferral lists are de-drifted, and the kernel is bumped to `0.77.0`. No track may expand into an adjacent subsystem.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | PR #118 residuals (SSH hang, `sys_nanosleep`) | Phase 74 ✅ | Complete |
| B | Cheap security mitigations (SMEP + SMAP) | Phase 75 ✅ | Complete |
| C | `PT_TLS` parsing in the ELF loader | Phase 74 ✅ | Complete |
| D | Networking polish (DNS resolver, TCP retransmit + slot lift) | — | Complete |
| E | Microcode loading | — | Complete |
| F | `epoll_*` verify-and-implement-if-missing | — | Complete |
| G | Open-handoff resolution + doc-drift PR | A–F, H | Complete (5 handoffs resolved/downgraded; full line-by-line Phase 50-56 deferral-list sweep done — drift struck across 52/52a/52b/52c/53/55/56) |
| H | `/proc` compatibility for `htop` / `ps` / `top` | — | Complete — htop now shows a populated process list (root cause: missing `/proc/<pid>/task/<tid>/stat` subtree, fixed in commits `d3cc752`/`a012ab8`). `htop-render-probe` passes (~480 changed band scanlines vs. 0 before) and is gated under `M3OS_HTOP_REGRESSION=1`; `ps -e` covered by `smoke-test`; the 2026-05-20 handoff is resolved |
| I | Documentation and Release (learning doc + `0.77.0` bump) | A–H | Complete |

> **Review note (2026-05-28):** The Phase 77 design doc was source-verified before this task list was authored. Four claims drifted from current `main` and are corrected in-place below: (1) `sys_nanosleep` already blocks for ≥1 ms sleeps — A.2 is re-scoped to closing residual busy paths, not a rewrite; (2) `epoll_*` syscalls already exist and are fully implemented — F.1 is verify + smoke + audit-doc, not implementation; (3) the TCP table is **8** slots, not 4; (4) there is **no in-tree musl source tree** — musl is a prebuilt cross-toolchain, so D.1 stages `/etc/resolv.conf` and verifies the prebuilt resolver rather than porting `__dns_query` C source.

---

## Track A — PR #118 Residuals

### A.1 — SSH disconnect hang fix

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `async_session` (line 204), `cleanup` (line 1526) — the session-teardown ordering invoked from the `exit`/EOF path at line 281
**Why it matters:** The 2026-04-25 handoff (`docs/handoffs/2026-04-25-pr-118-residual-issues.md` + `-update.md`) documents that a clean client `exit` leaves the sshd session task spinning. The roadmap cited `session.rs:1474`; the teardown logic now lives in `cleanup` near line 1526, so the fix is in the shutdown ordering, not the SSH protocol. The current `cleanup` already escalates SIGHUP → 500 ms poll (20 × 25 ms `waitpid(WNOHANG)`) → SIGKILL, so the residual spin is most likely a channel/EOF or PTY-close ordering issue that leaves the task runnable after the shell PID reaps.

**Acceptance:**
- [x] The 2026-04-25 spin is root-caused on current `main`: the SSH-teardown freeze is the same lost-wakeup as the multithread-join hang — `sshd` reaps its session via musl pthreads, whose `__tl_lock` (a NON-private futex on `&__thread_list_lock`, the `CLONE_CHILD_CLEARTID` target) never received the kernel's lock-release wake because `do_clear_child_tid` woke only the private futex key. Fixed in commit `b6f517b` (Track C); the scheduler `on_cpu` defer + teardown ordering shipped in `6f57fbc`.
- [x] After a client `exit`, the sshd session task terminates — the EOF-driven `cleanup` (close PTY master first → shell EOF-exits → bounded `nanosleep` reap, SIGKILL last resort) shipped in `6f57fbc`; the futex fix removes the residual reaper hang
- [x] The shell child PID is reaped exactly once and the PTY master/slave fds are closed in an order that does not leave the session loop runnable (`6f57fbc`)
- [x] `cargo xtask regression --test serverization-fallback` passes 10/10 consecutive runs (verified 2026-05-28: 10 passed, 0 failed)
- [x] The 2026-04-25 handoff docs (`2026-04-25-pr-118-residual-issues.md` + `-update.md`) are marked **RESOLVED** with a citation to this task (commits `b6f517b` + `6f57fbc`).

### A.2 — `sys_nanosleep` residual busy-yield closure

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_nanosleep` (line 3754); deadline primitive `block_current_until` (`kernel/src/task/scheduler.rs:3209`)
**Why it matters:** The roadmap claimed `sys_nanosleep` busy-yields in a loop at lines 3174-3191 and asked to "replace with `block_current_until`." That replacement has **already shipped** for the common case: the sched-v2 path for sleeps ≥ 1 ms blocks on `block_current_until` (lines 3788-3803). Two residual non-blocking paths remain and are the actual scope of this task: (a) the uncalibrated-TSC fallback yield-loop (lines 3806-3816), and (b) the v1 (non-sched-v2) "long sleep ≥ 1 ms" yield path (lines 3829+). The sub-ms TSC busy-spin (lines 3817-3828) is intentional and documented — a context switch costs more than the sleep.

**Acceptance:** (all shipped in commit `6f57fbc`)
- [x] Static audit confirms the ≥ 1 ms sched-v2 path routes through `block_current_until` with an absolute tick deadline and honours `EINTR` on a pending signal (verified existing behaviour)
- [x] The uncalibrated-TSC fallback is documented in-code as a boot-window-only path that cannot occur after APIC/TSC calibration completes
- [x] The dead v1 long-sleep yield branch was deleted (sched-v2 is the only live scheduler)
- [x] The intentional sub-ms TSC busy-spin retains its existing explanatory comment and is unchanged
- [x] PID 1 starvation is ruled out: the SSH-teardown reaper and `sleep`-heavy coreutils issue `nanosleep` continuously across every smoke/regression boot, and PID 1 (init) keeps making progress (10/10 serverization-fallback + 3/3 smoke all reach completion)

---

## Track B — Cheap Security Mitigations (SMEP + SMAP)

### B.1 — Enable `CR4.SMEP` (bit 20) on every CPU when supported

**Files:**
- `kernel/src/smp/boot.rs`
- `kernel/src/arch/x86_64/cpuid.rs`

**Symbol:** `install_trampoline` (`smp/boot.rs:160`; BSP CR4 capture into the trampoline slot at line 216 — there is **no** `save_bsp_cr4_for_aps` function), `ap_entry` (AP CR4 load, `smp/boot.rs:408`, CR4 store lines 412-415); `cpuid_raw` (`arch/x86_64/cpuid.rs:232`) + the `XSaveFeatures`-style `probe()` pattern (line 120). Note: leaf `0x07` is not probed today (only leaves 1 and `0x0D`), so the SMEP/SMAP feature check is net-new but follows the established pattern. (`kernel/src/arch/x86_64/cpu.rs` does not exist — the design doc's Primary Components line is stale.)
**Why it matters:** CR4 is configured in `smp/boot.rs`, not `cpu.rs` as the roadmap's component list implied — the BSP saves its CR4 for the AP trampoline and each AP reloads it. Setting CR4.SMEP causes a kernel `#PF` if ring 0 ever fetches an instruction from a user page, eliminating an entire class of "smash userspace shellcode" exploits. ~50 LOC including the AP path.

**Acceptance:** (all shipped in commit `83d54fb`; verified by the 2026-05-28 boot log)
- [x] A `CPUID.07h:0.EBX[7]` (SMEP) check is added — `cpuid.rs::probe_smep_smap()` (line 232) tests `CPUID_07_EBX_SMEP = 1 << 7` (line 221). Leaf `0x07` was not probed before (only leaves 1 and `0x0D`), so this is net-new but follows the cached-`Once` feature-struct pattern.
- [x] `CR4` bit 20 is set on the BSP before APs are woken (`lib.rs:239` `enable_smep_smap()`), and the APs reload the bit from the trampoline-captured CR4 (`smp/boot.rs:426`) — no separate AP enable path, confirmed by the per-AP log below.
- [x] A debug-only self-test (`kernel/src/arch/x86_64/smap_test.rs::run_boot_self_test`, gated behind the `smep-smap-test` cargo feature, invoked from `lib.rs:257-261`) deliberately `jmp`s into a `USER_ACCESSIBLE` page and asserts the resulting `#PF` — proving SMEP is live; zero-cost/absent in production builds.
- [x] On configs without SMEP support the bit is not forced (`enable_smep_smap` is gated on `probe_smep_smap()`); boot proceeds unchanged.
- [x] A `log::info!` reports SMEP state on every CPU — verified 2026-05-28: `[sec] BSP CR4.SMEP enabled (supported=true)…` (`lib.rs:250`) + `[sec] AP CR4.SMEP enabled …` ×3 (`smp/boot.rs:435`).

### B.2 — Enable `CR4.SMAP` (bit 21); SMAP-clean via physmap-routed user access

**Files:**
- `kernel/src/smp/boot.rs`
- `kernel/src/arch/x86_64/cpuid.rs`
- `kernel/src/mm/user_mem.rs`

**Symbol:** `UserSliceRo::copy_to_kernel` / `UserSliceWo::copy_from_kernel` in `mm/user_mem.rs` — the centralized primitives every deliberate user-memory access funnels through.
**Why it matters:** SMAP faults if ring 0 reads or writes a user-accessible page. **Implementation note (mechanism deviation from the plan):** rather than open STAC/CLAC windows around a user-virtual dereference, m3OS reaches user bytes through the **physical-memory direct map** (physmap-routed) — the kernel never dereferences a `USER_ACCESSIBLE` *virtual* page, so SMAP is satisfied **by construction** and no STAC/CLAC is required (the cleaner of the two designs). The lone raw user-virtual writer (the dead `copy_to_user` at `user_space.rs:197`) was deleted during the audit, leaving the kernel SMAP-clean.

**Acceptance:** (all shipped in commit `83d54fb`; verified by the 2026-05-28 boot log)
- [x] A `CPUID.07h:0.EBX[20]` (SMAP) check is added alongside the SMEP probe — `CPUID_07_EBX_SMAP = 1 << 20` (`cpuid.rs:223`), returned by `probe_smep_smap()`.
- [x] `CR4` bit 21 is set on the BSP (`lib.rs:239`) and propagated to APs via the trampoline-captured CR4 (`smp/boot.rs:426`); BSP clears `EFLAGS.AC` (`lib.rs:242`) so the firmware's AC can't silently disable enforcement.
- [~] STAC/CLAC windows — **superseded by the physmap-routing mechanism above.** No `stac`/`clac` is needed because no user-virtual dereference occurs in ring 0; the acceptance intent (SMAP enforced without breaking syscalls) is met by construction.
- [x] No other call site touches user memory directly — the `grep` audit confirms all user reads/writes route through `user_mem.rs` (physmap-backed); the dead `copy_to_user` at `kernel/src/mm/user_space.rs:197` (raw `from_raw_parts_mut`, zero callers) was **deleted** (tombstone comment retained at that site).
- [x] A debug-only self-test (`smap_test.rs`, `smep-smap-test` feature) deliberately reads a `USER_ACCESSIBLE` page from ring 0 and asserts a `#PF`, proving SMAP is live.
- [x] `cargo xtask test` and `cargo xtask smoke-test` pass — no syscall regressed (the physmap path is the same one every live syscall already used, so there was no SMAP window to regress).

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
- [x] A `PT_TLS = 7` constant is added and the phdr loop in `load_elf_into_with_interp` parses the `PT_TLS` header (alignment, `p_filesz`, `p_memsz`, `p_vaddr`/`p_offset`)
- [x] The initialized portion (`p_filesz`) is preserved and the BSS portion (`p_memsz - p_filesz`) is zero-init in the TLS template image, mapped (or made reachable) in the user address space — mechanism (a): the `.tdata` image lives inside the already-mapped `PT_LOAD` segment, so it is reachable with no extra staging; musl's `__init_tls` copies it per-thread and zero-inits `.tbss`
- [x] `LoadedElf` carries any new TLS metadata required by the chosen mechanism — N/A: mechanism (a) needs no aux entry (musl rediscovers the template via the phdr table), so `build_layout`/`LoadedElf` are unchanged and the existing auxv ordering host tests still pin the layout
- [x] `setup_abi_stack_with_envp` continues to supply `AT_PHDR`/`AT_PHENT`/`AT_PHNUM` correctly (regression-checked — unchanged; `tls-smoke` passing proves musl's phdr-walk TLS discovery works end to end)
- [x] The C.2 multi-threaded test passes — this is the load-bearing functional proof

### C.2 — Multi-threaded `__thread` TLS smoke test

**File:** `userspace/tls-smoke/` (new musl-built C binary; see "Adding a New Userspace Binary" in `AGENTS.md`)
**Symbol:** `main` (musl-built, links pthread), `__thread int x = 42;`
**Why it matters:** TLS correctness is only provable at runtime across real threads; a static audit cannot show each thread sees its own copy.

**Acceptance:**
- [x] The binary declares `__thread int tls_x = 42;`, spawns 4 worker threads via `pthread_create`, and each worker writes its thread index into `tls_x` and reads it back
- [x] Each thread observes its own value of `tls_x` (no cross-thread bleed); the main thread still sees `42`
- [x] The binary prints `TLS_SMOKE:PASS` on success and `TLS_SMOKE:FAIL <detail>` on mismatch, exiting 0 / non-zero respectively
- [x] Wired into the `smoke-runner` gate so `cargo xtask smoke-test` execs `/bin/tls-smoke` and asserts `SMOKE:tls-smoke:PASS`
- [x] All four wiring points are completed (musl C binary: `build_musl_bins` entry in `xtask`, `ramdisk.rs` `BIN_ENTRIES` embedding, smoke-runner gate; no Cargo workspace member or service config needed for a C binary)

> **Implementation note (2026-05-28):** C.2 surfaced two latent threading bugs that made musl pthreads unusable, both fixed in this track's commit: (1) `make_fork_ctx_for_thread` zeroed the clone child's caller-saved GPRs including **r9**, but musl's `__clone` child does `call *%r9` (r9 = start fn) → every worker faulted at rip=0; (2) `do_clear_child_tid` woke only the **private** futex key `(0,addr)`, but musl's `__thread_list_lock` (the `CLONE_CHILD_CLEARTID` target) is waited as a **non-private** futex `(cr3,addr)` → the thread-list-lock release wake was lost, hanging `pthread_join` intermittently (and the Track A SSH-teardown freeze, since `sshd` reaps via pthreads). Also added the Linux-standard futex-waiter dequeue on `FUTEX_WAIT` return so stale entries can't absorb single-waiter wakes.

---

## Track D — Networking Polish

### D.1 — DNS resolution via the prebuilt musl resolver + `/etc/resolv.conf`

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files`, ~line 12298; `etc/passwd` staging at ~13102 is the pattern to follow)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`)
- `kernel/src/net/udp.rs` (`bind`, line 18) and `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_socket`, line 15639)

**Symbol:** `populate_ext2_files`, `sys_socket`, `udp::bind`
**Why it matters:** The roadmap framed this as "port ~600 LOC of C into the musl tree." That does not match this repo: there is **no in-tree musl source** — musl is a prebuilt cross-toolchain resolved via `find_musl_cc`, and `getaddrinfo`/`gethostbyname` already ship in its libc. The real gap is that `/etc/resolv.conf` is not staged (only `passwd`/`shadow`/`group` are) and the prebuilt resolver has never been exercised against m3OS's `socket(AF_INET, SOCK_DGRAM)` syscall path. `sys_socket` (UDP path lines 15642-15665) and `udp::bind` exist, so the work is wiring + verification, with any missing syscall surface filled only as the resolver demands it.

**Acceptance:**
- [x] `/etc/resolv.conf` is staged onto the ext2 data disk by `populate_ext2_files` (`nameserver 10.0.2.3` for QEMU SLIRP + `options timeout:5 attempts:3`; mode 0644, user-editable). It is a plain config, not an init-managed service, so `KNOWN_CONFIGS` is unchanged.
- [x] A `cargo xtask clean` + boot confirms `/etc/resolv.conf` is present and read at runtime — the resolver picks up `nameserver 10.0.2.3` and sends its query there (observed on the serial UDP trace)
- [x] `getaddrinfo("github.com", ...)` resolves end to end via the `userspace/dns-smoke` binary — **fixed and verified 2026-05-28.** The query egresses with a valid ephemeral source port and the reply (44 bytes from `10.0.2.3:53`) is delivered to the resolver, which accepts it (source-address validation passes) and `getaddrinfo` returns the A record; `dns-smoke` emits `DNS_SMOKE:PASS <ip>`. Verified by serial trace: `recvmsg` delivers `n=44`, the resolver's `poll` busy-spin collapsed from **11,918 iterations → 4**, and the process terminates immediately (no retransmit, no timeout). `cargo xtask smoke-test` PASSES with `SMOKE:dns-smoke:PASS`.
- [x] The missing syscall surface is identified and **filled minimally** — two parts. (1) musl's `__res_msend` `sendto`s on an unbound UDP socket (it `bind`s to `0.0.0.0:0`, leaving `local_port == 0`), which previously returned `EINVAL`; `net_server::handle_sendto` + `sys_sendto` now auto-assign and register an ephemeral source port so the reply is routable. (2) **The actual delivery blocker:** modern musl drains the DNS reply with **`recvmsg(2)`, not `recvfrom(2)`**, and `sys_recvmsg` returned `EOPNOTSUPP` for AF_INET `FdBackend::Socket` — so the resolver busy-looped `poll → recvmsg(fail) → poll` until its retry window expired (the earlier "userspace-delivery hop" note was correct in location but mis-attributed to `poll`/`recvfrom`; the prior probe instrumented `recvfrom`, a path musl never calls). Fixed by `sys_recvmsg_inet` (`kernel/src/arch/x86_64/syscall/mod.rs`): delivers the UDP/TCP payload into the iovecs and fills `msg_name` with the sender's `sockaddr_in`, which musl byte-compares (`memcmp(ns+j, &sa, 16)`) against its nameserver list. The reply source is now both delivered and accepted. (Adversarially reviewed: `msg_name` is pre-validated before the datagram is consumed; TCP/EOF report `msg_namelen = 0` per Linux connection-mode semantics; `cap == 0` is guarded.)
- [x] The roadmap's "~600 LOC C port" framing is corrected in the Phase 77 design doc's Feature Scope (`docs/roadmap/77-pre-1-0-cleanup.md` line 48 — "wire-and-verify, not a C port"). The "missing syscall surface filled as the resolver demands it" framing is now concrete: the demanded surface was `recvmsg` on INET sockets.

### D.2 — TCP retransmission timer + multi-connection slot lift

**File:** `kernel/src/net/tcp.rs`
**Symbol:** `MAX_TCP_CONNECTIONS` (line 291, **currently `8`** — the roadmap's "4-slot" claim is stale), `TcpConnections` (line 293), `TcpConnection` (line 66), `TCP_CONNS` (line 309)
**Why it matters:** The fixed 8-element `[Option<TcpConnection>; 8]` array caps concurrent connections far below a realistic 1.0 workload, and there is **no retransmission logic** today (only a comment about "retransmitting SYNs" at line 216 — no RTO timer, no resend queue). On QEMU's perfect LAN this is invisible; the first dropped packet on the real internet hangs the connection.

**Acceptance:**
- [x] `MAX_TCP_CONNECTIONS` raised 8 → 64; the backing `[Option<TcpConnection>; 64]` now uses inline-const init (`[const { None }; 64]`) instead of a hand-written list of `None`s (`TcpConnection` is non-`Copy` — it owns `VecDeque`s + the estimator — so this scales to any cap). A `BoundedVec` would add a second length source of truth; the fixed array with `flatten()` iteration is the simpler equivalent bounded structure.
- [x] A per-connection one-shot RTO timer (`rto_deadline` + `RttEstimator`) is armed on send and disarmed/recomputed on the ACK that covers the segment, computing RTO per **RFC 6298** (SRTT/RTTVAR integer smoothing, 1 s min / 60 s max clamps). The estimator is `kernel_core::net::tcp::RttEstimator` with **6 host tests** pinning the formula (initial, LAN-clamp, large-RTT, smoothing, backoff cap, post-backoff re-clamp).
- [x] On RTO expiry (`tcp_tick`, driven every ~200 ms from the net task's deadline-wake) the oldest unacked segment is retransmitted with its **original sequence number** and the RTO doubles (`RttEstimator::on_timeout`), capped at 60 s; after `MAX_RETRANSMITS` (8) the connection is reset. Karn's algorithm: no RTT sample on retransmitted segments.
- [x] The connection-state machine is otherwise unchanged — the retransmit hooks are additive (`arm_retransmit` on SYN/data send, `on_ack` in the existing ACK arms); `cargo xtask check` (clippy + fmt + host tests) and the smoke/regression suites still pass.
- [~] `tcp-loss-smoke` 100 MB / 5 % drop: **infeasible in the QEMU SLIRP harness** (SLIRP does not drop packets and there is no tap+netem rig). Per the implementation note in this file, the loss-recovery *logic* (RFC 6298 RTO timing + exponential backoff + the wrapping ACK-coverage check) is **host-tested** in `kernel-core`; the live retransmit integration rides the real TCP path exercised by the smoke/regression boots. Documented limitation.
- [~] 64 concurrent connections: the cap is satisfied by construction (the 64-slot table) and the allocation path (`create` scans for a free slot up to 64). A runtime 64-client open requires an in-guest TCP listener + 64-client driver the harness does not have; documented alongside the netem limitation.

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
- [x] A vendor-supplied AMD microcode blob (linux-firmware `microcode_amd_fam19h.bin`, matching the dev machine's `AuthenticAMD` CPU) is embedded at `kernel/initrd/lib/firmware/amd-ucode.bin` via `include_bytes!` (available in the BSP/AP bring-up window before any fs mount).
- [x] The container is parsed + validated by the host-tested `kernel_core::microcode` module (magic, equivalence table → `equiv_id`, patch `processor_rev_id` match, `patch_id` revision strictly newer than the running level) **before** any MSR write — 5 host tests cover the parse + the strictly-newer gate + truncated-blob safety.
- [x] On a match the patch VA is written to `MSR_AMD64_PATCH_LOADER` (`0xC0010020`) — applied on the **BSP first** (`lib.rs`, after the SMEP/SMAP setup) then on every **AP** during bring-up (`smp/boot.rs::ap_entry`). (The AMD path is used because the dev machine is AMD; the Intel `0x79` path is not implemented since no Intel blob is embedded.)
- [x] The loaded patch level (read back from MSR `0x8B`) is logged on **every CPU** at boot — verified: `[ucode] CPU0..3 sig=0x60fb1 ... current level 0x1000065`.
- [x] On signature mismatch the load is skipped and boot continues unchanged — verified in QEMU: all 4 CPUs log `no newer microcode in blob ... skipped` (no MSR write, since QEMU's `AuthenticAMD` sig `0x60fb1` is not in the fam19h equivalence table) and `cargo xtask smoke-test` PASSES. (Per the user, the QEMU no-op/skip is the expected behaviour; the apply path activates only on matching real AMD hardware.)

---

## Track F — `epoll_*` Verify (Implement Only If Missing)

### F.1 — Confirm `epoll_*` syscalls and add a smoke gate

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/epoll.rs`
- `docs/appendix/audit-status/74a-pre-1.0-audit.md`

**Symbol:** `sys_epoll_create1` (`syscall/mod.rs:18453`), `sys_epoll_ctl` (line 18496), `sys_epoll_wait` (line 18593); dispatch arms at lines 1902/1906/1913; supporting types `EpollInstance`/`EpollInterest`/`EPOLL_TABLE` and the **entire implementation** live in `syscall/mod.rs:18355-18720`. `kernel/src/epoll.rs` is only a 21-line teardown shim (`epoll_free_pub`), **not** the implementation — do not point the verification at it
**Why it matters:** Audit §2 flagged `epoll_*` as PARTIAL/possibly-absent. Source verification shows all three handlers **exist and are fully implemented** (FD-table integration, interest lists, wait-queue-backed blocking, close-on-exec cleanup). So this track is verification + a regression gate + an audit correction — **not** new implementation.

**Acceptance:**
- [x] A new `userspace/epoll-smoke` binary registers a pipe read end with `epoll_ctl(EPOLL_CTL_ADD)`, writes to it, and asserts `epoll_wait` reports the fd ready with the correct event mask (`EPOLLIN`) and `data` token; it also exercises `EPOLL_CTL_MOD` (token change verified), `EPOLL_CTL_DEL` (no event after delete), and the timeout path (returns 0)
- [x] The binary prints `EPOLL_SMOKE:PASS` / `EPOLL_SMOKE:FAIL <detail>` and is wired into the `smoke-runner` gate (`SMOKE:epoll-smoke:PASS` observed in `cargo xtask smoke-test`)
- [x] `docs/appendix/audit-status/74a-pre-1.0-audit.md` §1 row 17 + §2 entry updated from PARTIAL to "wired and verified," citing the three handler line numbers (`18453`/`18551`/`18593`) and the new smoke gate
- [x] No new syscall handler was added — the smoke test surfaced no gap; all three handlers were already fully implemented

---

## Track G — Open-Handoff Resolution + Doc-Drift PR

### G.1 — Doc-drift PR: strike shipped items from Phase-56-and-earlier deferral lists

**Files:** `docs/roadmap/50-*.md` through `docs/roadmap/56-*.md` "Deferred Until Later" sections (notably Phases 50, 51, 52a, 52b, 52c, 53a, 54, 55a, 55b, 56)
**Symbol:** N/A (doc bookkeeping)
**Why it matters:** Phase 74a §6 found substantial drift: deferral lists in phases 50-56 still claim items as deferred that later phases actually shipped. This matters for trust before the Phase 83 release gate.

**Acceptance:**
- [x] The clearly-shipped drifts in the Phase 50-56 deferral lists are annotated with their shipping phase: `53-headless-hardening.md` — "DNS resolution" → Phase 77 D.1, "Dynamic linking / shared libraries" → Phase 76; `55c-ring-3-driver-correctness-closure-learning.md` — `ipc_recv_timeout` → Phase 74 (`SYS_IPC_RECV_TIMEOUT`). (Prior phases already de-drifted the Phase 6/50/55c lists with explicit Phase 74 references.)
- [x] No deferral entry contradicts a shipped capability — the line-by-line sweep of all 16 Phase 50-56 deferral lists is **complete** (2026-05-28; verified each "now-shipped" claim against in-tree artifacts before striking, discarding several cross-contaminated/already-annotated false positives). Additional drifts annotated this pass: `52` (storage/namespace/networking extraction → Phase 54; fully graphical display ownership → Phase 56), `52a` (`syscall_user_rsp` task-owned state + typed `UserBuffer` wrappers → 52b/52d), `52b` (VMA tree, per-core scheduler/work-stealing, ISR-direct wakeup → 52c), `52c` (preemptive scheduling from interrupt context → Phase 57d), `53` (GUI/compositor + mouse/audio → Phase 56/57; DNS → Phase 77 D.1; dynamic linking → Phase 76), `55` (IOMMU → 55a, ring-3 NVMe/e1000 → 55b), `56` (native bar/launcher/notifyd/lockscreen + animation engine → Phase 73, correcting the stale "Phase 57b/57c territory" pointers). Genuinely-still-deferred entries (HTTPS/TLS clients, `git`/`gh`, package feeds, Wi-Fi/GPU/USB, growable notif pool, Wayland, IME/keymaps) are left intact.
- [x] Annotations cite the delivering phase inline, so the change is auditable from the doc alone.

### G.2 — Confirm the 2026-05-22 multi-term OOM reproducer is fixed

**File:** `docs/handoffs/2026-05-22-compositor-shm-leak-multi-term-oom.md`
**Symbol:** N/A (verification + evidence capture)
**Why it matters:** The handoff documents an OOM when launching 4× terminals at 4K; it must be confirmed fixed (or re-opened) before 1.0.

**Acceptance:**
- [x] Multi-term spawn pressure is exercised via the purpose-built `cargo xtask compositor-stress --cycles 3 --spawns-per-cycle 4` (12 terminals across 3 workspaces via SUPER+RETURN / SUPER+1/2/3) — a more deterministic harness than a manual `run-gui` and the tool built for this exact reproducer.
- [x] No OOM / kernel panic: the run reports `compositor-stress: PASSED (no kernel panic)`; serial captured at `/tmp/g2-oom/serial.log`.
- [x] The handoff (`2026-05-22-compositor-shm-leak-multi-term-oom.md`) is marked `resolved` with the compositor-stress evidence cited.

### G.3 — Resolve the 2026-05-04 virtio-input migration "status unclear" handoff

**File:** `docs/handoffs/2026-05-04-virtio-input-migration.md`
**Symbol:** N/A
**Why it matters:** An unresolved "status unclear" handoff is a trust gap before the release gate.

**Acceptance:**
- [x] Closed as superseded by Phase 56: the shipped input stack is the PS/2-based `kbd_server`/`mouse_server` → `display_server` dispatcher, which covers the graphical-session input needs; virtio-input is not a 1.0 requirement and is left un-scheduled (re-openable as a fresh phase if QEMU virtio-input support is later wanted).
- [x] The handoff doc (`2026-05-04-virtio-input-migration.md`) reflects the CLOSED decision.

### G.4 — Verify the 2026-04-28 graphical-stack-startup handoff on current `main`

**File:** `docs/handoffs/2026-04-28-graphical-stack-startup.md`
**Symbol:** N/A (verification; root-cause only if reproducible)
**Why it matters:** The handoff reports the cursor pinned at (0,0) when `display_server` lands on an AP. It is likely closed by the Phase 57a/57b/57e SMP discipline hardening (`pi_lock`, `with_block_state`, `wake_task_v2` precondition closure), but must be confirmed.

**Acceptance:**
- [x] The graphical stack was brought up repeatedly this session (the compositor-stress + htop-render-probe headless boots, plus the session-smoke gates land `display_server` on an AP); the cursor-at-(0,0) symptom did not recur.
- [x] Marked resolved citing the Phase 57a/b/e SMP scheduler hardening (`pi_lock` discipline, `with_block_state_locked_scheduler`, `wake_task_v2` precondition closure) plus the Phase 77 Track A wake/defer + futex fixes — see `2026-04-28-graphical-stack-startup.md` front-matter.
- [x] Not reproducible, so no further root-cause was needed.

### G.5 — Capture-log path for the 2026-05-13 mouse-reset-top-left handoff

**File:** `docs/handoffs/2026-05-13-mouse-reset-top-left-intermittent.md`
**Symbol:** N/A
**Why it matters:** The PS/2 cursor intermittently enters a sticky bad state and resets to (0,0) on tiny motion. It is too rare to reproduce on demand, so an after-the-fact capture path is needed.

**Acceptance:**
- [x] Capture path confirmed (existing): `M3OS_SMOKE_SERIAL_DUMP=<path>` retains the full serial transcript per run, and the AGENTS.md QMP `screendump` path captures the framebuffer state — together enough to analyse the sticky bad state after the fact.
- [x] No on-demand instance captured (too rare); the handoff is **explicitly downgraded** to "known intermittent issue, post-1.0 follow-up" (`status: downgraded-post-1.0-followup` in `2026-05-13-mouse-reset-top-left-intermittent.md`).
- [x] No root cause emerged from a capture this session; the downgrade decision is documented in the handoff.

---

## Track H — `/proc` Compatibility for `htop` / `ps` / `top`

### H.1 — Root-cause and fix the 2026-05-20 htop-zero-processes regression

**Files:**
- `kernel/src/fs/procfs.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_linux_getdents64`, lines 13395-13450; dispatch at line 1755)

**Symbol:** `list_dir` (`procfs.rs:160`), `render_pid_stat` (line 701), `render_status` (line 550), `render_comm_bytes` (line 661), `sys_linux_getdents64`
**Why it matters:** `htop` shows zero processes. The 2026-05-20 handoff lists five suspected causes, ranked: (1) `getdents64` semantics (`d_off`/`d_reclen` alignment), (2) `/proc/<pid>/stat` field count or `(comm)` paren escaping, (3) `/proc/<pid>/status` missing fields (`Tgid`, `VmRSS`, `VmData`, `VmStk`), (4) `openat(dirfd, "/proc/<pid>")` across a dir fd, (5) `/proc/cpuinfo` or `/proc/stat` cpu-line parser failure. `render_status` today emits only Name/State/Pid/PPid/Uid/Gid/Threads/VmSize/Cwd — the missing fields are a confirmed candidate.

**Acceptance:**
- [x] Root cause **fully identified and fixed** (final story below). The journey: Phase 72b's all-PIDs `list_dir` fix (every user sees every PID) made `/proc` enumeration + `ps -e` work but did **not** fix htop — a headless screendump still showed `Tasks: 0`. The true blocker (pinned via a PID-scoped `/proc` trace) was that htop's `scanMainThread`/`readStatFile` reads the main thread's stat via **`/proc/<pid>/task/<pid>/stat`**, and m3OS had **no `/proc/<pid>/task/` subtree** → ENOENT → every process discarded. Fixed by implementing the `/proc/<pid>/task/<tid>/…` subtree in `kernel/src/fs/procfs.rs` (commits `d3cc752`/`a012ab8`). See the final acceptance item below and `docs/handoffs/2026-05-20-htop-zero-processes.md` (resolved).
- [x] `/proc/<pid>/status` now emits `Tgid`, `VmRSS`, `VmData`, and `VmStk` (added to `render_status`) alongside the pre-existing Name/State/Pid/PPid/Uid/Gid/Threads/VmSize/Cwd. VmRSS approximates the mapped footprint, VmData sums writable mappings + heap, VmStk is the fixed 64 KiB user stack, Tgid == Pid (procfs lists the thread-group leader).
- [x] `getdents64` over `/proc` is exercised by every htop launch in `tui-app-smoke` (htop enumerates `/proc` via getdents to find PIDs and renders its header); per-pid dirs (`status`/`stat`/`comm`/...) are openable and read by htop.
- [x] `/proc/<pid>/stat` field order is confirmed correct by `render_pid_stat` (the `(comm)` field is paren-wrapped; m3OS process names cannot contain `)` since `comm` is set from PR_SET_NAME / argv basenames).
- [x] `htop` launched from `term` shows a non-empty process list — **MET (2026-05-28).** `cargo xtask htop-render-probe` now PASSES (≈480 changed process-table band scanlines vs. the 20 threshold; 0 before). Root cause was finally pinned via a PID-scoped `/proc` trace: htop's `readStatFile` reads the main thread's stat via `/proc/<pid>/task/<pid>/stat` (its `scanMainThread` path), and m3OS had **no `/proc/<pid>/task/` subtree** → ENOENT → every process discarded. (The earlier "never opens `/proc/<pid>/stat`" note was correct but mis-attributed — htop opens `task/<pid>/stat`, not `stat`, in its default config.) Fixed by implementing the `/proc/<pid>/task/<tid>/…` subtree in `kernel/src/fs/procfs.rs`. Same pass also gave htop real per-process CPU% (real `utime`/`stime` from `task_times_for_pid` + corrected 52-field stat layout), a real `/proc/stat` busy/idle split (was hard-coded 10 %), and a real `/proc/loadavg` + state column from the scheduler's `TaskState` (was the stale `Process.state`, always `Ready`). Regression gate: `M3OS_HTOP_REGRESSION=1`. See `docs/handoffs/2026-05-20-htop-zero-processes.md` (resolved).

### H.2 — `htop-smoke` gate folded into `tui-app-smoke`

**File:** `xtask/src/main.rs`
**Symbol:** `tui_app_smoke_steps` (line 9831) — htop is already a step here; this task adds the process-row assertion
**Why it matters:** Without an automated gate this regression silently returns. The htop launch already exists in `tui-app-smoke`; what is missing is an assertion on the rendered process count.

**Acceptance:**
- [x] The cell-grid capture mechanism is built and **WORKS**: `cargo xtask htop-render-probe` boots the graphical stack **headless** (QMP + VNC, per the new AGENTS.md "Headless framebuffer screenshots" section), launches htop via QMP `send-key`, and screendumps the framebuffer. Correction: an earlier note here wrongly claimed the graphical-term render didn't reach the screendump — it does. The screenshot clearly shows htop's full UI (meters, column header, F-keys). The keystrokes DO reach the term and htop DOES render.
- [x] Fails loudly when the process table is empty (`changed_rows_in_band` in `xtask/src/main.rs` diffs the process-table band vs the prompt baseline; `MIN_CHANGED_BAND_SCANLINES`). With H.1 fixed it now **PASSES** correctly — ≈480 changed process-table band scanlines vs. the 20 threshold (0 before the fix), so the assertion both catches a regression to zero processes and confirms a populated list.
- [x] Wired into the pre-push gate under its own opt-in env var `M3OS_HTOP_REGRESSION=1` (`.githooks/pre-push` → `cargo xtask htop-render-probe --timeout 300`; listed in the AGENTS.md hooks table). A dedicated gate — rather than folding into `M3OS_TUI_APP_REGRESSION` — matches the other heavyweight opt-in gates (the probe cross-compiles ncurses + drives headless QMP/VNC), keeping the default `tui-app-smoke` fast. With H.1 fixed the gate passes; `ps -e` regression coverage in `smoke-test` remains as the always-on procfs check.

### H.3 — Verify `ps aux` and `top` show non-empty process lists

**Files:** `userspace/` (BusyBox/coreutils `ps` and `top` equivalents), consuming `kernel/src/fs/procfs.rs`
**Symbol:** N/A (consumes the same `/proc` files fixed in H.1)
**Why it matters:** `ps` and `top` read the same `/proc` files as htop and should work as a side effect of H.1 — confirming this closes the compatibility story.

**Acceptance:**
- [x] `ps -e` (the m3OS `coreutils-rs` ps) shows a non-empty process list matching the live set — it enumerates `/proc` via `getdents64` and reads `/proc/<pid>/status` (the same path htop uses), and the existing `smoke-test` gate asserts both the `PID` header and the live `ion` process row. This is the regression-protected functional proof that procfs process listing works (closing the htop-zero-processes story on the kernel side).
- [~] `top`: there is **no `top` binary in m3OS** (no in-tree applet or port; `grep` for a `top` ramdisk entry is empty). The process-listing compatibility story is carried by `ps` + htop, which read the same `/proc` files. Documented; adding a `top` is out of scope for this track.
- [x] No residual zero-list `/proc` gap: `ps` shows the live set, confirming `getdents64` + `/proc/<pid>/status` (with the H.1 Tgid/VmRSS/VmData/VmStk additions) are correct.

---

## Track I — Documentation and Release

### I.1 — Create the Phase 77 learning doc

**File:** `docs/77-pre-1-0-cleanup.md`
**Symbol:** N/A
**Why it matters:** A learner-friendly doc scoped to Phase 77 consolidates the cleanup story — SSH/nanosleep correctness, SMEP/SMAP, `PT_TLS`, DNS, TCP retransmission, microcode, epoll verification, and the `/proc` compatibility fix — so readers do not have to reconstruct the bundle from eight separate tracks. Follows the "aligned legacy learning doc" template in `docs/appendix/doc-templates.md`.

**Acceptance:**
- [x] File exists at `docs/77-pre-1-0-cleanup.md`
- [x] All required template fields populated: `**Aligned Roadmap Phase:** Phase 77`, `**Status:** Complete`, `**Source Ref:** phase-77`, `**Supersedes Legacy Doc:** new`
- [x] Overview explains, learner-first, why a release-gate phase is the wrong place to find small correctness bugs and contrasts cheap CR4 flips (SMEP/SMAP) with expensive page-table reshapes (KPTI)
- [x] Key Files table cites the real files touched (the actual fix sites — `cpuid.rs`/`lib.rs`/`boot.rs` for SMEP/SMAP rather than the planning-doc's `user_mem.rs`, plus `process/mod.rs`, `scheduler.rs`, `net/tcp.rs`, `net/udp.rs`, `net_server`, `microcode.rs`, `procfs.rs`)
- [x] "How This Phase Differs From Later Memory/Security Work" notes KPTI/retpoline/IBRS are Phase 84 and congestion control beyond Reno-style retransmit is post-1.0
- [x] Related Roadmap Docs links the design doc + this task list

### I.2 — Bump kernel version to `0.77.0`

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]` (currently `0.76.3` at line 3)
**Why it matters:** Project convention is one minor-version bump per shipped phase; disciplined version tracking signals a complete, shippable phase.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.77.0"`
- [x] `Cargo.lock` regenerated (via `cargo xtask check`)
- [x] `AGENTS.md` kernel version updated to `v0.77.0` and a "CPU hardening" capability bullet added (per the file's keep-it-small maintenance policy — the detailed per-phase record lives in `docs/roadmap/`)
- [x] `docs/roadmap/README.md` Phase 77 row Status updated to "Complete" (Tasks cell already points at this list); the design doc + task-doc Status headers set to Complete
- [x] `cargo xtask check` passes
- [ ] Git tag `v0.77.0` — recommended at phase merge (left to the merge step)

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
