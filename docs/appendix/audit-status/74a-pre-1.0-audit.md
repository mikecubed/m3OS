# Phase 74a — Pre-1.0 Audit and Release Blocker Inventory

**Status:** Planned (audit artifact — feeds Phase 83 Release 1.0 Gate)
**Source Ref:** phase-74a
**Depends on:** Phase 74 ✅, Phase 75 ✅, Phases 60–73 ✅
**Builds on:** The audit reconciliation captured in `docs/appendix/audit-status/` and the per-phase "Deferred Until Later" sections of every roadmap doc 01–75.
**Primary Components:** none (documentation-only)

## Milestone Goal

Produce one auditable, source-verified list of every item still standing between m3OS as of phase 74 and a credible 1.0 release. The list answers four questions:

1. Does the OS work on real hardware? (No — see §3.)
2. What drivers are missing? (USB, modern NICs, Wi-Fi, HDA, AHCI — see §3.)
3. Can it compile and run code built by tcc on-target? (Yes — see §4.)
4. What follow-ups from earlier phases are still genuinely open? (See §5.)

This doc is the planning input for Phases 74b…77. Items here are graded by 1.0 impact, not by aesthetic priority.

## Why This Phase Exists

Phases 60–73 were audit-driven cleanup of Phases 1–58. Phase 73 closed the visible desktop story (compositor + bar + launcher + notifyd + lockscreen). Phase 74 closed the last IPC-correctness gap (capability grants + bulk transfers). Phase 75 closed the W^X security gap. What remains is the gap between *runs in QEMU* and *runs on the metal in front of the user*, plus a small tail of correctness items from earlier phases that quiet doc drift has been hiding.

Without this inventory, Phase 83 (Release 1.0 Gate) cannot be scoped — there is no single place that lists what 1.0 must include and what it must defer.

## Audit Methodology

Five parallel agents catalogued and source-verified every item in:

- Every phase doc 01–75 ("Deferred Until Later", "How Real OS Implementations Differ", embedded TODOs, unmet acceptance criteria)
- Every doc under `docs/handoffs/`, `docs/post-mortems/`, `docs/research/`, `docs/evaluation/`, `docs/debug/`
- Driver inventory under `kernel/src/`, `userspace/drivers/`, `kernel-core/src/iommu/`
- The tcc port (`xtask/src/main.rs` ports build) + ELF loader (`kernel/src/mm/elf.rs`) + syscall table + libc/CRT staging

Findings were then re-verified with targeted `grep` against current source. Items the docs called "deferred" but a later phase quietly shipped are graded SECRETLY DONE. Items the docs call "complete" but source disagrees with are graded as drift in §6.

Hardware target for §3: the user's actual laptop (HP OmniBook, AMD Ryzen AI 9 365 / Strix Halo) plus a generic modern x86_64 desktop and laptop.

---

## 1. 1.0 Blocker Summary (one screen)

| # | Blocker | Class | Owner phase | Severity |
|---|---|---|---|---|
| 1 | No USB stack at all — `kernel/`/`userspace/` contain zero xHCI/EHCI/UHCI/OHCI bytes | Hardware driver | Phase 78 | **BLOCKER** |
| 2 | No Wi-Fi driver of any kind (laptop target has zero ethernet) | Hardware driver | Phase 81 | **BLOCKER for laptops** |
| 3 | Only e1000-82540EM supported — no e1000e/igb/igc/Realtek/Broadcom | Hardware driver | Phase 79 | **BLOCKER for desktops** |
| 4 | No SATA/AHCI driver — NVMe-only storage | Hardware driver | Phase 82 (optional) | HIGH (NVMe-only systems work; older boxes fail) |
| 5 | Only AC'97 audio — no HDA, no HDMI audio | Hardware driver | Phase 80 | HIGH (modern boxes have no AC'97) |
| 6 | Phase 65 fat_server still stubs every op to `ENOSYS` | Code | Phase 65 (planned) | HIGH (declared release-gating in roadmap) |
| 7 | ~~W^X not enforced — userspace text pages are RWX~~ | Security | Phase 75 ✅ (merged 2026-05-26) | CLOSED |
| 8 | Dynamic linker absent — no `PT_INTERP`, no `dlopen`, no `.so` support | Code | Phase 76 (planned) | HIGH (toolkit GUI apps and Node.js blocked) |
| 9 | No Spectre/SMEP/SMAP/KPTI mitigations on real silicon | Security | Phase 77 (SMEP+SMAP) + Phase 84 (KPTI/retpoline/IBRS, post-1.0) | HIGH for SMEP+SMAP; deferrable for KPTI |
| 10 | SSH client disconnect hangs forever — PR #118 residual | Correctness | Phase 77 (Track A.1) | MEDIUM |
| 11 | `sys_nanosleep` busy-yield can starve PID 1 — PR #118 residual | Correctness | Phase 77 (Track A.2) | MEDIUM (flaky `serverization-fallback` regression) |
| 12 | TCP retransmit + 4-connection-slot limit | Code | Phase 77 (Track D.2) | MEDIUM (real internet is broken without it) |
| 13 | `PT_TLS` segment not parsed — musl works around it via reserved stack space; multi-threaded TLS is fragile | Code | Phase 77 (Track C) | MEDIUM |
| 14 | Phase 10 (Secure Boot) never validated on real hardware | Validation | Phase 59 | MEDIUM |
| 15 | DNS resolver absent (user must type IPs); IPv6 / DHCPv6 absent | Code | Phase 77 (Track D.1, DNS) + Phase 91 (IPv6 post-1.0) | MEDIUM |
| 16 | No CPU microcode loading on real hardware | Correctness/Security | Phase 77 (Track E) | MEDIUM |
| 17 | `epoll_*` RESOLVED — `epoll_create1`/`epoll_ctl`/`epoll_wait` all fully implemented (`syscall/mod.rs:18453/18551/18593`); verified by the `epoll-smoke` gate (Phase 77 Track F). The PARTIAL flag was a source-search miss. | Code | Phase 77 (Track F) ✅ | RESOLVED |
| 18 | virtio-input migration status unclear from 2026-05-04 handoff | Handoff | Phase 77 (Track G.3) or Phase 59 | LOW |
| 19 | `htop` shows zero processes even as root — `/proc` compatibility gap with Linux's `getdents64` / `/proc/<pid>/stat` / `openat`-on-dir-fd semantics (2026-05-20 handoff, open) | Code | Phase 77 (Track H) | HIGH (breaks a Phase 69d-shipped TUI app) |
| 20 | Cursor pinned at (0,0) on display_server start under some scheduler conditions (2026-04-28 handoff, open) | Correctness | Phase 77 (Track G.4) | MEDIUM (likely closed by 57a/57b/57e SMP hardening; needs verification on current main) |
| 21 | PS/2 mouse intermittently resets cursor to (0,0) after sticky bad state (2026-05-13 handoff, open) | Correctness | Phase 77 (Track G.5) | LOW (reboot-recoverable, intermittent, no log captured yet) |

Items 1–9 are the gate for 1.0. Items 10–21 are degradations a 1.0 user will hit within a session and are bundled into Phase 77.

The new-phase sequence to clear 1–9 before Phase 83 is in §7. Phase 75 closed item 7. Phase 74 closed the items previously on this list (IPC timeouts, capability grants, page-grant bulk transfers).

---

## 2. Audit by Source Document

Full per-doc inventory lives in `docs/appendix/audit-status/74a-detail.md` (to be written as a companion). The summary above is the actionable subset. Where the per-phase scan turned up an item that *was* shipped in a later phase, this audit flags it as doc drift (§6) rather than carrying it as a blocker.

### Cross-cutting "still genuinely deferred" items (from phases 6, 16, 23, 33, 48, 54, 55, 55a, 55b, 56, 57)

These are the items where source grep confirmed the deferral is still real:

- **IPC timeouts and cancellation** (Phase 6 deferred list) — closed by Phase 74 ✅ (merged 2026-05-26).
- **W^X enforcement** (Phase 11 / Phase 36 deferred lists) — closed by Phase 75 ✅ (merged 2026-05-26).
- **TCP retransmission timer + multi-connection slots** (Phase 16 deferred list) — kernel TCP has fixed 4-slot array and no retransmit on loss; networking outside an idle LAN will hang. → Phase 77 Track D.2.
- **IPv6 / DHCP / DNS resolver** (Phase 16 / 23 deferred list) — IPv4 only; no in-OS resolver. → DNS resolver in Phase 77 Track D.1; IPv6 / DHCPv6 in Phase 91 (post-1.0).
- **`epoll` proper** (Phase 23 deferred list) — **wired and verified** (Phase 77 Track F). All three handlers are fully implemented in `kernel/src/arch/x86_64/syscall/mod.rs`: `sys_epoll_create1` (line 18453), `sys_epoll_ctl` (line 18551), `sys_epoll_wait` (line 18593), with the `EpollInstance`/`EpollInterest`/`EPOLL_TABLE` machinery (FD-table integration, interest lists, `WaitQueue`-backed blocking, close-on-exec cleanup) at 18355-18720. The earlier PARTIAL flag was a source-search miss, not an absence. The new `userspace/epoll-smoke` regression (gate `SMOKE:epoll-smoke:PASS` in `cargo xtask smoke-test`) exercises `EPOLL_CTL_ADD`/`MOD`/`DEL`, `epoll_wait` readiness + event-mask + `data`-token reporting, and the timeout path end to end against a pipe.
- **Setuid bit on executables + supplementary groups + privilege separation in sshd** (Phase 48 deferred) — `sshd` still runs as root for the entire session.
- **Argon2id password hashing** (Phase 48 deferred) — iterated SHA-256 only. Acceptable for local-only systems; below modern standards.
- **Driver-side seccomp / syscall filtering** (Phase 55b deferred) — ring-3 drivers have full syscall surface. Phase 74's capability-grants work narrows this but does not close it.
- **MSI-X per-queue steering / multiqueue NVMe** (Phase 55 / 55b deferred) — single completion queue caps NVMe throughput at ~1 GB/s on high-end drives.
- **Interrupt remapping** (Phase 55a deferred) — VT-d / AMD-Vi IRTE not used; IDT integration deferred.
- **International keymaps** (Phase 56 deferred) — US QWERTY hardcoded; non-US users cannot type their language.
- **Multi-output / multi-seat compositor** (Phase 56 deferred) — single framebuffer, single seat. Phase 72/73 did not change this.
- **Hardware-accelerated composition (KMS/DRM/GL)** (Phase 56 deferred) — software composition only, single buffer (tearing).
- **Multi-client audio mixing** (Phase 57 deferred) — single client; second client gets `EBUSY`. Phase 63a layers a userspace mixer on top but the kernel-side single-client policy stands.
- **Multiple graphical sessions / fast user switching** (Phase 57 deferred) — one session at boot.
- **Slab adoption** (Phase 33 deferred) — Phase 60 migrated the two hot families (Task, XSaveArea); others still use the global allocator. Acceptable.

### Outstanding handoffs and post-mortems

Source: `docs/handoffs/`, `docs/post-mortems/`.

- **2026-04-25 PR #118 residuals** — SSH disconnect hang (`userspace/sshd/src/session.rs:1474`) and `sys_nanosleep` busy-yield (`kernel/src/arch/x86_64/syscall/mod.rs:3174-3191`). Both still open; the second causes `cargo xtask regression --test serverization-fallback` to be flaky. → Phase 77 Tracks A.1 + A.2.
- **2026-04-28 graphical-stack-startup** (STATUS: open) — display_server lands on an AP and its `MouseInputSource::poll_pointer` calls `ipc_call(mouse_handle, MOUSE_EVENT_PULL, 0)` and blocks forever; cursor pinned at (0,0). Suspected to be the same lost-wake root cause as 2026-04-25-scheduler-design-comparison.md. Largely superseded by Phase 57a / 57b / 57e SMP discipline hardening; needs a verification run on current `main` to confirm closure. → Phase 77 Track G.4 (verify-and-close on current main; if still reproducible, root-cause and fix).
- **2026-05-04 virtio-input migration** (STATUS: open) — status unclear from doc; needs picker. → Phase 77 Track G.3.
- **2026-05-11 audio IRQ/wake race** — empirically mitigated to ~1 error per boot via retry; full fix requires `wake_task_v2` precondition-closure refactor. Acceptable for 1.0.
- **2026-05-13 mouse-reset-top-left-intermittent** (STATUS: open) — PS/2 mouse cursor enters a sticky bad state after boot where tiny motions reset it to (0,0). Reboot-recoverable. No log captured during failure window. May or may not share root cause with the 2026-04-28 handoff. → Phase 77 Track G.5 (capture log on reproduction; fix or document as known issue).
- **2026-05-17 less render disappearance** — animation polish, low priority. Acceptable for 1.0.
- **2026-05-20 htop-zero-processes** (STATUS: open) — `htop` (a Phase 69d TUI deliverable) shows zero processes even as root. Suspected root causes: (1) `getdents64` semantic mismatch (m3OS musl path may differ from glibc), (2) `/proc/<pid>/stat` field-format mismatch with htop's `sscanf` template, (3) `/proc/<pid>/status` missing a field htop relies on, (4) `openat`-on-`/proc`-dir-fd semantics, (5) `/proc/cpuinfo` or `/proc/stat` row-format mismatch. Touches `kernel/src/fs/procfs.rs` plus the userspace `getdents64`/`readdir` path. → **Phase 77 Track H** (own track, see §7).
- **2026-05-22 compositor SHM leak / multi-term OOM** — five separate fixes landed; the *original* reproducer (multi-terminal 4K launch) has not been re-confirmed on-target. Run `cargo xtask run-gui --kvm --fresh` to close. → Phase 77 Track G.2.
- **2026-05-24 4 GiB + SMP silent hang** — closed via NMI-based TLB shootdown (commit 646cb60). Framebuffer MMIO cacheability is a known latent hygiene gap; not blocking.
- **57e Bug #9 (FS-mutex fairness)** and **57e Bug #10 (sporadic DOOM GPF)** — both low priority; Bug #10 was observed once, never reproduced.

### Roadmap-level open phases (per `docs/roadmap/README.md`)

| Phase | Status | 1.0 role |
|---|---|---|
| 54a | Planned | Post-serverization hygiene (CLOEXEC + arch-syscall relocation). Small. |
| 59 | Planned | Validation backlog — manual QEMU tests from earlier phases never run. Hard 1.0 gate. |
| 65 | Planned | fat_server real implementation. Hard 1.0 gate. |
| 74 | Complete (merged 2026-05-26) | IPC capability grants + bulk transfers. Closes Phase 6/50 deferrals. |
| 75 | Complete (merged 2026-05-26) | W^X enforcement. Closes audit § E1. |
| 76 | Planned | Dynamic linker. Optional pre-1.0; required for Node.js (Phase 89). |
| 77 | Planned | Pre-1.0 Correctness, Cheap Security, and Network Polish (bundle phase). Hard 1.0 gate. |
| 78 | Planned | USB Host Foundation. Hard 1.0 gate (single biggest unblocker). |
| 79 | Planned | Modern Intel/Realtek NIC. Hard 1.0 gate. |
| 80 | Planned | Intel HDA Audio. Hard 1.0 gate. |
| 81 | Planned | Wi-Fi Reference (MT7925). Hard 1.0 gate for laptop targets. |
| 82 | Planned (optional) | AHCI/SATA. Optional pre-1.0; deferrable to post-1.0. |
| 83 | Planned | Release 1.0 Gate. |
| 84 | Planned (post-1.0) | KPTI / retpoline / IBRS — the expensive Spectre mitigations. |
| 89 | Planned (post-1.0) | IPv6 / DHCPv6. |
| 57e | Deferred 2026-05-07 | Full kernel preemption; voluntary mode is the 1.0 baseline. Not a blocker. |

---

## 3. Real-Hardware Gap Analysis

### Reference dev hardware (this laptop)

```
HP OmniBook — AMD Ryzen AI 9 365 (Strix Halo, 10C/20T, 2024)
  IOMMU            AMD-Vi  [1022:1508]    — supported (Phase 67)
  Storage          Micron 3500 NVMe SSD [1344:5415]
                   — probes via NVMe class match (kernel/userspace/drivers/nvme/src/main.rs:103)
                   — should work
  Network          MediaTek MT7925 802.11be Wi-Fi [14c3:7925]
                   — NO DRIVER. No Wi-Fi support of any kind in tree.
                   — laptop has zero ethernet. Without MT7925: zero network.
  GPU              AMD Radeon 880M (Strix iGPU) [1002:150e]
                   — NO DRIVER. Falls back to UEFI GOP framebuffer (works for static display)
  Audio (HDA)      AMD Ryzen HD Audio [1022:15e3]
                   AMD/ATI Radeon HDMI Audio [1002:1640]
                   — NO DRIVER. AC'97 backend will not match this hardware.
  USB              Six AMD xHCI controllers (1022:151e / 151f / 151a / 151b / 151c / 151d)
                   — NO DRIVER. Zero USB code in tree.
                   — Modern laptops have no PS/2. No USB-HID = no keyboard, no mouse.
```

On this laptop, m3OS as of phase 74 would boot, paint a framebuffer, find the NVMe disk, and then **sit at a black screen with no working input** (no PS/2, no USB-HID). Even if input worked, there would be no network.

### Generic desktop / laptop coverage

| Subsystem | Coverage | Real-HW gap |
|---|---|---|
| **Boot / UEFI / Secure Boot** | UEFI via OVMF; signing toolchain present | Secure Boot never validated on real metal |
| **CPU / SMP** | MADT + xAPIC; APs via SIPI; up to 16 cores | x2APIC absent (255+ core boxes break); microcode loading absent |
| **MMU** | 4-level paging only | 5-level code exists (Phase 67) but not on boot path; MTRR absent (GPU WC mapping suffers); no NUMA |
| **Interrupts** | PIC + I/O APIC + MSI/MSI-X | Per-core MSI-X steering absent; interrupt remapping disabled |
| **Storage** | NVMe (class-match probe → works on real drives), VirtIO-blk | Single I/O queue (no multiqueue); no SATA/AHCI; no abort/format/namespace mgmt |
| **Network — wired** | Intel 82540EM e1000 (device-ID-gated to `0x8086:0x100E`) | Real Intel NICs are e1000e/igb/igc — none supported. No Realtek (RTL8169/8125), no Broadcom |
| **Network — Wi-Fi** | none | Mandatory on every modern laptop |
| **TCP/IP** | IPv4 + TCP + UDP + DHCP (kernel-side) | No retransmission timer (hangs on loss); 4-conn cap; no IPv6/DHCPv6; no DNS resolver |
| **USB** | none | **No keyboard/mouse on modern hardware (no PS/2 ports)** |
| **Input** | PS/2 keyboard + mouse | USB-HID absent; touchpads absent; touchscreen absent |
| **Display** | UEFI GOP framebuffer (firmware mode) | No mode-setting; no GPU drivers; no multi-monitor; no HiDPI negotiation |
| **Audio** | Intel 82801AA AC'97 only `0x8086:0x2415` | HDA absent — every modern Intel/AMD box since ~2008 ships HDA |
| **Power** | none | No suspend/resume, lid, thermal, battery |
| **Time** | TSC + APIC timer | RTC reads exist (Phase 34) but no HPET; no NTP; no TSC calibration on freq-scaled CPUs |
| **Crypto / RNG** | RDRAND + TSC mix | RDSEED unused; entropy may be thin on first boot |
| **IOMMU** | VT-d + AMD-Vi per-device domains, AMD-Vi fault ISR, VT-d queued invalidation | Complete (Phase 67) — no gap here |

### Real-hardware verdict

m3OS **cannot boot to an interactive login** on:

- Any modern laptop (no USB-HID = no input; no Wi-Fi = no network)
- Any modern desktop without a PS/2 port (overwhelmingly common since ~2015)
- Any system whose primary NIC is not the QEMU-emulated Intel 82540EM
- Any system whose primary audio is HDA (which is all of them since ~2008)

It can boot on:

- QEMU with PS/2, e1000-82540EM, AC'97, NVMe or VirtIO-blk, VirtIO-net (fully working)
- A bare-metal box that happens to expose all of: PS/2 keyboard/mouse, real Intel 82540EM NIC, AC'97 audio, NVMe — this configuration is essentially absent from 2024-era hardware

---

## 4. tcc On-Target Verdict

**tcc-built code runs end-to-end.** The toolchain stack from `xtask/src/main.rs:2039–2247` cross-compiles tcc 0.9.28rc with `--triplet=x86_64-linux-musl --config-musl`, stages it at `/usr/bin/tcc` with the full musl tree at `/usr/lib/` and `/usr/include/`, including `crt1.o` / `crti.o` / `crtn.o` / `libtcc1.a`. The ELF loader at `kernel/src/mm/elf.rs` handles both `ET_EXEC` and `ET_DYN` (static-PIE), applies `R_X86_64_RELATIVE` relocations, sets a SysV-ABI-compliant stack with full `AT_*` aux vector (`AT_PHDR/PHENT/PHNUM/PAGESZ/RANDOM/NULL`), and invokes the binary at the right RSP alignment. The syscall table (`kernel/src/arch/x86_64/syscall/mod.rs`, ~139 numbers defined) covers everything musl's runtime needs: `read/write/openat/close/lseek/mmap/munmap/mprotect/brk/fork/execve/wait4/exit_group/stat/fstat/getpid/pipe2/dup2/socket/poll/select/fcntl/ioctl/sigaction/clock_gettime` (228, at `mod.rs:1393`) `/nanosleep/prctl/getrandom`, plus `unlink/rmdir/mkdir/rename/symlink/link/readlink/getcwd/chdir`.

The on-target build path is exercised by:

- `userspace/smoke-runner/src/main.rs:46-87` — `tcc-version` + `tcc-compile /usr/src/hello.c → /tmp/h` smoke tests
- `userspace/demo-project/build.sh` + `Makefile` — multi-file C build via on-target `make` (pdpmake, Phase 32)

**Concrete prerequisites that already work:**

- ELF interpreter is not needed (tcc emits statically linked PIE; no `PT_INTERP` resolution required until Phase 76)
- Stack alignment is correct at `_start` (8 mod 16 per SysV AMD64)
- `_start` from musl's `crt1.o` is wired and walks `argc/argv/envp` from RSP correctly
- File timestamps survive `open(O_TRUNC)` so `make`'s dependency tracking works (Phase 31 fix)
- `sys_execve` reads ELF binaries from the on-target ext2 partition (not just ramdisk)

**Caveats that are not blockers:**

- **No `PT_TLS` parsing in `elf.rs`** — musl uses errno; single-threaded code works because musl's TCB allocation lives above the initial RSP (reserved stack space; see `kernel/src/mm/elf.rs:44, 394`). Multi-threaded TLS is fragile; a real `PT_TLS` parse is required when Phase 40's threading is exercised by tcc-built code with threads.
- **No dynamic linking** — fine for tcc (its default is `-static`); blocks porting any code with `.so` deps until Phase 76.
- **TCC is pulled from the `mob` branch of `repo.or.cz/tinycc.git`** (`xtask/src/main.rs:2039`) — moving target; pin to a tag before 1.0 for reproducibility.
- **No `nm`/`objdump`/`readelf`/`ld`** on-target — tcc does its own linking, but anyone debugging needs the BFD tools.

**Verdict for 1.0: tcc is shippable.** No code or flag changes are required to make tcc-built C programs work today. The 1.0 readiness for tcc is independent of the hardware blockers.

---

## 5. Cross-Cutting Code Blockers (not hardware)

In dependency order, the code-only items that should block 1.0:

1. **Phase 65 — fat_server implementation** (already on roadmap). Today every FAT op routes through `vfs_server` to `fat_server` and returns `ENOSYS`. Either implement it or drop FAT32 from the supported matrix and document ext2-only.
2. ~~**Phase 75 — W^X enforcement**~~ — **CLOSED 2026-05-26** by Phase 75. ELF loader rejects `PF_W|PF_X`; `mprotect` rejects `PROT_WRITE|PROT_EXEC`; stack / brk / mmap NX-audited; regression in `wx-violation` smoke binary.
3. **Phase 77 Track A — PR #118 residuals** — SSH disconnect hang (HIGH user-visible bug) and `sys_nanosleep` busy-yield (causes flaky regression test). Both bundled into Phase 77.
4. **Phase 77 Track D.2 — TCP retransmission + connection-count cap** — IPv4 networking will appear to "work" on a perfect LAN and hang on the real internet. The single biggest code-only correctness gap; bundled into Phase 77.
5. **Phase 77 Track C — `PT_TLS` parsing in ELF loader** — required before any multi-threaded tcc-built or musl-built program is reliable. Phase 40 threading exists but TLS is fragile without it; bundled into Phase 77.
6. **Spectre / SMEP / SMAP / KPTI** — split: Phase 77 Track B lands SMEP + SMAP (CR4 bit flips, ~200 LOC, pre-1.0). Phase 84 (post-1.0) lands KPTI + retpoline + IBRS.
7. **Phase 77 Track D.1 — `/etc/resolv.conf` DNS resolver** — no on-target resolver; user must type IPs. Trivial port of musl's stub resolver against a Phase 23 socket; bundled into Phase 77.
8. **Phase 77 Track E — microcode loading** — ~300 LOC, real correctness impact on the dev laptop's Strix Halo silicon (multiple known errata patched only via microcode updates).
9. **Phase 77 Track F — `epoll_*` verify-and-implement-if-missing** — `sys_poll` exists; audit could not confirm `epoll_*` syscall handlers. If absent, implement against the existing `WaitQueue` infrastructure.
10. **Phase 91 — IPv6 / DHCPv6** — explicitly deferred to post-1.0. Phase 83 Release Gate documents the IPv4-only-for-1.0 promise.

(Phase 74's IPC capability grants and bulk transfers merged 2026-05-26 — that closes the Phase 6 timeout/cancellation deferrals and the Phase 50 page-grant gap that were previously on this list. Phase 75's W^X enforcement merged 2026-05-26 — that closes the Phase 11 / Phase 36 deferred-W^X notes.)

---

## 6. Doc Drift to Reconcile

Items the per-phase audit pulled forward as "deferred" but a later phase quietly shipped:

- **Phase 56 "Deferred Until Later" lists tiling layouts** — shipped in Phase 72.
- **Phase 56 lists native bar/launcher/notifyd/lockscreen** — shipped in Phase 73.
- **Phase 57 lists graphical login manager** — shipped in Phase 71.
- **Phase 56 lists animation engine** — shipped in Phase 73 (`userspace/display_server/src/animation.rs`).
- **Phase 56 lists rounded corners + drop shadows** — shipped in Phase 73 (`userspace/display_server/src/decoration.rs`).
- **Phase 15 lists "AP startup deferred to Phase 17"** — actually shipped in Phase 25.
- **Phase 11 lists "CoW fork deferred beyond Phase 17"** — shipped in Phase 17 itself.
- **Phase 13 lists per-process FD tables as deferred** — shipped in Phase 14.
- **Phase 21 dependency on Phase 22 termios** — Phase 22 shipped.

Recommended action: a doc-only PR (Phase 77 Track G.1) that walks each Phase-56-and-earlier "Deferred Until Later" list and strikes items shipped in later phases, citing the shipping phase. This is bookkeeping but matters for trust in the audit before Phase 83.

Phase 56 also has open deferrals that genuinely have NOT shipped (kept in §1/§2):

- Multi-output / multi-seat
- Hardware-accelerated composition (GL/EGL/KMS/DRM)
- International keymaps
- USB-HID breadth
- Back-buffer / double-buffer (tearing)

---

## 7. Recommended Phase Sequencing to Reach Phase 83

Strict dependency order; items in the same group can run in parallel.

### Group A — existing roadmap phases that still need to ship before Phase 83

- **Phase 54a** — Post-serverization hygiene (already small)
- **Phase 59** — Validation backlog (mechanical: run the manual QEMU tests; also resolves the 2026-05-04 virtio-input handoff and the Phase 10 Secure Boot real-hardware validation)
- **Phase 65** — fat_server implementation (or drop FAT32 from matrix)

(Phase 74 shipped 2026-05-26 — IPC capability grants + bulk transfers. Phase 75 shipped 2026-05-26 — W^X enforcement.)

### Group B — Phase 77 (bundle phase, ~1–2 sprints across parallel tracks)

Single phase, multiple parallel tracks. The full track list lives in [`docs/roadmap/77-pre-1-0-cleanup.md`](../../roadmap/77-pre-1-0-cleanup.md):

- **Track A** — PR #118 residuals (SSH hang + nanosleep busy-yield)
- **Track B** — SMEP + SMAP CR4 enable (cheap security)
- **Track C** — `PT_TLS` parsing in ELF loader
- **Track D.1** — DNS resolver stub
- **Track D.2** — TCP retransmission + multi-connection slot lift
- **Track E** — Microcode loading
- **Track F** — `epoll_*` verify-and-implement-if-missing
- **Track G** — Open-handoff resolution (§6 doc-drift, multi-term OOM verify, virtio-input picker, graphical-stack-startup verify, mouse-reset capture)
- **Track H** — `/proc` compatibility for `htop` / `ps` / `top` (root-cause `getdents64` / `/proc/<pid>/stat` / `openat`-on-dir-fd mismatch from 2026-05-20 handoff; touches `kernel/src/fs/procfs.rs`)

### Group C — new hardware-driver phases (the actual 1.0 work)

Concrete LOC estimates are rough; refer to each phase doc for full scope.

- **[Phase 78 — USB Host Foundation (xHCI + Hub + HID)](../../roadmap/78-usb-host-foundation.md)** — ~5500 LOC. **Single biggest 1.0 unblocker.** Without this, no interactive use on any modern hardware.
- **[Phase 79 — Modern Intel/Realtek NIC](../../roadmap/79-modern-nic.md)** — ~3000–5000 LOC. e1000e + igb + igc + RTL8169 + RTL8125. Phase 78 + Phase 79 together are the "boot-on-real-hardware bundle."
- **[Phase 80 — Intel HDA Audio](../../roadmap/80-intel-hda-audio.md)** — ~2000–5000 LOC. HDA controller + Realtek ALC888/892/1220 codec family.
- **[Phase 81 — Wi-Fi Reference (MediaTek MT7925)](../../roadmap/81-wifi-reference.md)** — ~8000–15000 LOC. One chipset only for 1.0; explicit deferral of the rest.
- **[Phase 82 — AHCI/SATA](../../roadmap/82-ahci-sata.md)** — ~2000 LOC. **Optional pre-1.0.** Defer to post-1.0 if anything else slips.

### Group D — release gate

- **[Phase 83 — Release 1.0 Gate](../../roadmap/83-release-1-0-gate.md)**. Defines the supported hardware matrix (baseline: NVMe + e1000e *or* Realtek + xHCI USB-HID + HDA audio + UEFI GOP framebuffer, with MT7925 Wi-Fi on the laptop target), the runs-on-real-hardware test plan, and the version/branding cut.

### Group E — post-1.0 (explicit deferrals)

- **[Phase 84 — KPTI + retpoline + IBRS](../../roadmap/84-spectre-mitigations.md)** — the expensive Spectre mitigations. Phase 77 covered SMEP + SMAP.
- **[Phase 85 — Cross-Compiled Toolchains](../../roadmap/85-cross-compiled-toolchains.md)** — git, Python, Clang. tcc covers 1.0.
- **[Phase 86 — Networking and GitHub](../../roadmap/86-networking-and-github.md)**
- **[Phase 89 — Node.js](../../roadmap/89-nodejs.md)** — depends on Phase 76 (dynamic linker).
- **[Phase 90 — Claude Code](../../roadmap/90b-claude-code.md)**
- **[Phase 91 — IPv6 / DHCPv6](../../roadmap/91-ipv6-dhcpv6.md)**

Optional pre-1.0 (defer if the rest slips):

- **Phase 76** — Dynamic linker. Required for Phase 89 Node.js but not for 1.0 itself.
- **Phase 67/55b follow-ups** — multiqueue NVMe, MSI-X per-core steering, interrupt remapping.

---

## 8. What 1.0 Should Explicitly Defer

To keep 1.0 honest and shippable, the Release Gate should commit in writing to:

- **No GPU acceleration** — UEFI GOP framebuffer only. No KMS/DRM, no GL, no Vulkan.
- **No power management** — no suspend/resume/lid/thermal/battery. Server-style "always on" only.
- **No multi-output / multi-seat compositor.**
- **No international keymaps beyond US QWERTY.**
- **No setuid programs / no supplementary groups / no sshd privilege separation.**
- **No multi-client kernel audio mixing** (userspace mixer in `audio_mixer` is the answer for 1.0).
- **No IPv6 / DHCPv6** — Phase 91, post-1.0.
- **No KPTI / retpoline / IBRS** — Phase 84, post-1.0. SMEP + SMAP land in Phase 77.
- **No SR-IOV, no hot-plug, no live driver update.**
- **No clang/llvm/gcc on-target** — tcc only. Cross-compiled toolchains land in Phase 85.
- **No dynamic libraries / `.so` on-target** unless Phase 76 lands.
- **No Wi-Fi chipsets beyond MT7925** — Phase 81 ships exactly one.
- **No NIC silicon beyond e1000 / e1000e / igb / igc / RTL8169 / RTL8125.**
- **No HDMI / DisplayPort audio** — Phase 80 ships analog HDA only.
- **No x2APIC** (255+ core boxes break; rare on consumer hardware).
- **No MTRR** (relevant only with GPU acceleration, which is also deferred).
- **No HPET** — TSC + APIC timer are the 1.0 time sources.
- **No on-target package management or online updates** — disk image rebuild is the supported update path.

All of these are documented in the source-of-truth phase docs already; the Release Gate doc should pull them into one user-facing "what's supported and what isn't" page.

## Acceptance Criteria

- [x] This audit doc lists every blocker source-verified against current code, with citations.
- [x] The proposed Phase 77–84 + 89 phase sequence has stub design docs under `docs/roadmap/` reflecting the audit's recommended scope.
- [ ] The doc-drift PR (Phase 77 Track G.1) removes shipped items from Phase 56's deferred list with citations to the shipping phase.
- [ ] A `docs/appendix/audit-status/74a-detail.md` companion captures the per-doc inventory the parallel audit produced (raw data, kept for traceability).
- [x] The roadmap README links to this doc and to the proposed Phase 77–84 + 89 sequence.
- [ ] Phase 83 (Release 1.0 Gate) treats §1 as its blocker list.

## Companion Task List

Each of Phases 77–84 + 89 gets its own task doc under `docs/roadmap/tasks/` when its implementation planning begins. Phase 77 should be authored first as it is the most concrete (and the audit has already named the eight tracks).

## How Real OS Implementations Differ

A real OS distribution at "1.0" typically ships:

- A USB stack and HID class driver before any release (PS/2-only would be a non-starter)
- At least one modern Intel + one Realtek wired NIC driver
- At least one Wi-Fi chipset family
- HDA audio, not AC'97
- A dynamic linker and at least libc/libm/libdl as `.so`
- Spectre / Meltdown mitigations turned on by default
- IPv6 and DHCPv6

m3OS at 1.0 is justified in deferring GPU/Wi-Fi-breadth/multi-seat/Spectre-retpoline because the project's stated goal is a learning microkernel, not a daily driver. But USB, modern NIC, and HDA are the floor for "actually usable on a real machine."

## Deferred Until Later (post-1.0)

- Phase 76 dynamic linker (unless required for 1.0 polish)
- Phase 84 Spectre / KPTI / retpoline / IBRS
- Phase 85 cross-compiled toolchains (git, Python, Clang)
- Phase 86 networking + GitHub
- Phase 89 Node.js
- Phase 90 Claude Code
- Phase 91 IPv6 / DHCPv6
- Multi-output / multi-seat compositor
- Hardware-accelerated composition (KMS/DRM/GL)
- Wi-Fi breadth beyond the reference chipset (MT7925)
- AML interpreter / dynamic ACPI events
- Suspend / resume / power management
- Hot-plug / SR-IOV / live driver update
