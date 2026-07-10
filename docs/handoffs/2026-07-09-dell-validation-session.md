# Dell Precision 5560 validation session — runbook

**Date:** 2026-07-09 (planned bench session)
**Machine:** Dell Precision 5560 — Intel **Tiger Lake** (11th-gen). Has PCID +
INVPCID, is Meltdown-susceptible with KPTI off, has **CET (`CET_SS`)**, an
AX201/CNVi radio, an Elan I2C-HID touchpad, and is Secure-Boot capable. No DE-9
serial port (use AMT SOL or a USB-serial adapter).
**Protocol:** [bare-metal validation strategy](../appendix/bare-metal-validation.md).
Bench mechanics (flash, serial capture idiom): [phase-100 HW handoff §6](./2026-06-30-phase-100-bare-metal-gui-hw-validation.md).
**Standing checklist this session works down:** [`next-dell-session.md`](./next-dell-session.md).

> **This session is a runbook, not a checklist.** It sequences the batched Dell
> arms by value + dependency, gives each test an exact **objective**, the exact
> **expected serial line(s)** (copied from the shipping code, so a mismatch is a
> real regression), a **pass/fail** bar, and a **failure → likely cause** map.
> Work top-down; each block gates the next. Capture *everything* per the
> evidence convention and record `run N, YYYY-MM-DD, Dell/Tiger Lake`.

---

## Objectives (what "done" looks like for this session)

1. **Prove the Phase 110 security substrate is live and correct on real
   silicon** — the single highest-value goal, because QEMU can prove *none* of
   it. One clean boot of the default image simultaneously exercises: KPTI
   (active on Meltdown-susceptible silicon), the **A.5 PCID asm/CR3 path**
   (tagged CR3 + no-flush trampolines + both-PCID `INVPCID`, which never ran
   under QEMU's full-flush fallback), and the **B.3 CET path** (CR4.CET +
   `IA32_U_CET` + the shadow-stack PTE encoding + per-task SSP save/restore,
   entirely dormant under QEMU). **A clean boot-to-login is itself the proof
   these asm/MSR/PTE paths are correct** — a wrong PCID order `#GP`s, a wrong
   shadow-stack encoding or stale SSP `#CP`/`#PF`s on the first ring-3 `CALL`.
2. **Functionally prove the two defenses actually defend** — the Meltdown PoC
   (A.6) leaks with KPTI off and fails with it on; a ROP/return-overwrite PoC
   (B.3) faults `#CP` with CET on and succeeds with it off.
3. **Meet the A.5 perf bound** — smoke suite ≤ 30 % slower `full` vs `off`.
4. **Flush out the two flagged CET risks** — fork CoW-of-shadow-stack and
   nested-signal SSP handling — with targeted stress.
5. **Batch the older bare-metal arms** if bench time remains — **Phase 103
   power** (battery/backlight/HWP/S3; entirely bare-metal — QEMU has none of the
   devices) and **Phase 106 M3** (install to the internal NVMe), plus 111
   kgdb/ptrace, 101 ACPI capture, 100 GUI/USB. Two Phase 103 arms piggyback on
   the Block 1 boot + the Phase 101 capture, so they cost almost nothing.

**Definition of success:** objectives 1–3 recorded `Validated-on-HW`; objective
4 either clean or with a captured, root-caused failure that becomes a follow-up.

---

## Block 0 — pre-flight (do BEFORE the bench, on the build host)

Build all images we might need up front so bench time is boot-and-capture, not
compile. `M3OS_MITIGATIONS` and `M3OS_KERNEL_FEATURES` are **build-time**
(`option_env!`), so each posture is a separate image.

| # | Image | Build command | Purpose |
|---|---|---|---|
| A | **default (auto)** | `cargo xtask image` | ⚠️ **CORRECTED (run 2):** this silicon is `rdcl_no=true` (Meltdown-**immune**), so `auto` leaves **KPTI OFF** → PCID also off (gated on KPTI) → **only CET is active**. Use image **C** to exercise KPTI/PCID. |
| B | **mitigations=off** | `M3OS_MITIGATIONS=off cargo xtask image` | the A/B baseline: KPTI/PCID/CET all off (ROP returns, perf floor) |
| C | **mitigations=full** | `M3OS_MITIGATIONS=full cargo xtask image` | ⚠️ **CORRECTED (run 2):** on `rdcl_no=true` silicon this is **NOT** the same as A — `full` **forces KPTI+PCID on** (auto would leave them off). **This is the security-posture image on the Dell** (KPTI+PCID+CET all active) and the perf-run image. |
| D | **kgdb** | `M3OS_KERNEL_FEATURES=kgdb cargo xtask image` | Phase 111 kgdb arm |
| E | **ptrace** | `M3OS_KERNEL_FEATURES=ptrace cargo xtask image` | Phase 111 native gdbserver arm |

Each image lands at `target/x86_64-unknown-none/release/boot-uefi-m3os.img`
(rename per posture before the next build). **All five (A–E) are pre-built and
staged** at `target/dell-images/{A-default,B-mitigations-off,C-mitigations-full,
D-kgdb,E-ptrace}.img` (19 MiB each, distinct kernels confirmed) — flash directly
from there, no rebuild needed. Flash with `scripts/phase-100-write-usb.sh
/dev/sdX` (on the Dell, USB = `/dev/sda`; the NVMe system disk is `nvme0n1` —
**do not** flash that). Direct fallback:
`sudo dd if=<img> of=/dev/sda bs=4M conv=fsync status=progress && sync`.

> **CI de-risk done on the build host:** both PoCs ship in every image and have
> QEMU run-to-completion gates (`cargo xtask meltdown-poc-smoke` /
> `rop-cet-poc-smoke`, both PASS; behind `M3OS_SEC_POC_REGRESSION=1` in pre-push).
> `rop-cet-poc` is asm-verified (`mov [rsp],rdi; ret`); `meltdown-poc` uses
> `rdtsc` (not `rdtscp` — the default QEMU CPU faulted `rdtscp` in ring 3 into a
> retry loop) and a `--smoke` fast mode for CI. The security arms (leak-reject,
> `#CP`-kill) remain HW-only — that is what the bench is for.

**Serial capture (pick one, wire it first):**
- **AMT Serial-over-LAN** — the port-less path: Intel ME redirects COM1
  (`0x3F8`, where m3OS logs) over Ethernet; capture from a 2nd machine with
  `amtterm`. Gets **pre-network / pre-login** boot lines (the `[sec]` line).
  Runbook: `scripts/ure-vfio-validate.md`.
- **`usb-logsink` boot.log** — the resident daemon writes dmesg to
  `/mnt/usb0/boot.log` on a writable USB ext2 partition; survives with no
  network, readable after the boot.
- **Post-login SSH** — `echo '<cmd>' | ssh root@<ip> > out.log`. **FILTER ON THE
  M3OS SIDE with a single fixed-string pattern** — a full `dmesg | ssh` truncates
  (we lost two captures to this). m3OS `grep` is single-pattern fixed-string:
  `echo 'dmesg | grep sec' | ssh root@<ip> > sec.log`.

**Artifacts (scaffolded — now ship in every image):** `meltdown-poc` and
`rop-cet-poc` are wired ring-3 binaries (`userspace/{meltdown-poc,rop-cet-poc}`,
workspace member + `bins` + ramdisk entry) staged at `/bin/meltdown-poc` and
`/bin/rop-cet-poc`. Run them by bare name from the shell. Notes for the bench:
- `rop-cet-poc` ships **without** the stack canary (xtask builds it with
  `-Zstack-protector=none`) so the overwrite reaches `ret` and trips CET, not
  `__stack_chk_fail`. Its `vulnerable()` is a naked `mov [rsp],rdi; ret` — a
  precise, deterministic return-address overwrite (verified at the asm level;
  QEMU can't exercise CET, so determinism is the point). Sentinels:
  `ROP_CET_POC:before`, then either the kernel `#CP` kill line (CET on) or
  `ROP_CET_POC:PWNED` (CET off). `ROP_CET_POC:after-NOT-OVERWRITTEN` = regression.
- `meltdown-poc` uses a flush+reload channel + a **mispredicted-branch**
  speculative kernel read (m3OS has no catchable `SIGSEGV` and Tiger Lake has TSX
  off, so the illegal read is squashed speculatively — never architecturally
  faults, so the same binary runs safely under KPTI on **and** off). It first
  runs a positive **control** (recover a known user byte — `channel=CALIBRATED`
  is the on-CPU self-check that timing/thresholds are right), then leaks
  `LEAK_LEN` bytes from the kernel base and prints `MELTDOWN_POC:LEAK` (KPTI off)
  or `MELTDOWN_POC:NO-LEAK` (KPTI on). Expect to tune `TRIES` /
  `CACHE_HIT_THRESHOLD` / `CONFIDENCE` / `KERNEL_TARGET_VA` at the bench (all
  `const`s at the top of `main.rs`). Cannot be validated under QEMU (TCG models
  no caches/speculation).

---

## Block 1 — the security posture boot (do this FIRST; it validates the most)

> **✅ RUN 2 (2026-07-09) RESULT — VALIDATED.** KPTI + PCID + CET all boot clean
> on the Dell under image **C** (`mitigations=full`) → login → compositor →
> fork/exec. `m3ctl mitigations status` reads `KPTI PCID: active (kernel/user
> PCID, no-flush)` and `CET: enabled (user shadow stacks)`; the Meltdown line
> reads `Not affected` (this silicon is `rdcl_no=true`, so KPTI is not *needed* —
> but `full` runs it anyway, which is exactly what this posture boot validates).
> RUN 1's CET black-screen was root-caused and fixed (five CET bugs); full
> analysis:
> [2026-07-09 CET boot-hang handoff — RESOLVED](./2026-07-09-cet-boot-hang-on-tiger-lake.md).
> **Use image C, not A** — on `rdcl_no=true` silicon `auto` (image A) leaves
> KPTI+PCID OFF (see the corrected Block 0 table).

**Objective.** Prove KPTI, PCID, and CET all activate and the machine boots
clean to a login on real Tiger Lake silicon — validating in one shot every
asm/CR3/MSR/PTE path QEMU could not run.

**Steps.**
1. Boot **image C** (`mitigations=full` — on `rdcl_no=true` silicon this is the
   image that activates KPTI+PCID; image A/`auto` leaves them off). Capture the
   boot log over AMT SOL / boot.log.
2. Grep the `[sec]` policy line and the per-core enable lines.
3. Log in; run `m3ctl mitigations status`.

**Expected serial (exact — from the shipping code; `mitigations=Full` under image C):**
```
[sec] mitigations=Full … kpti(policy=true active=true) pcid(active=true supported=true) cet(active=true supported=true) global_kernel_ptes=0
[sec] CR4.PCIDE enabled (KPTI PCID TLB-cost recovery active)
[sec] CR4.CET enabled (CET user shadow stacks active)
[sec] AP CR4.SMEP enabled CR4.SMAP enabled CR4.PKE enabled CR4.PCIDE enabled     ← one per AP
```
(Note: the AP line reports PCIDE but **not** CET today — CET on APs is proven
by the boot line's `cet(active=true)` + clean multi-core run. Adding a CR4.CET
field to the AP line is a nice 1-line follow-up if we want per-AP CET evidence.)

**Expected `m3ctl mitigations status`** (⚠️ corrected for `rdcl_no=true` silicon):
```
Meltdown: Not affected          ← rdcl_no=true → Meltdown-immune, so the vuln
                                   line reads "Not affected" EVEN under `full`
                                   where KPTI is actively enforcing. NOT the line
                                   that confirms KPTI.
… KPTI PCID: active (kernel/user PCID, no-flush)   ← THIS confirms KPTI is
                                                      enforcing (line is absent
                                                      when KPTI is off)
… CET: enabled (user shadow stacks)
```
(Contrast QEMU: `pcid(active=false …)` / `cet(active=false …)` /
`KPTI PCID: fallback …` / `CET: not-supported`, and — because TCG is
`rdcl_no=false` — `Meltdown: Mitigation: PTI`. The Meltdown *line* legitimately
differs between QEMU and this Dell for that reason; the KPTI-PCID / CET lines are
the real cross-environment proof.)

**Pass:** all three `active=true`, both `CR4.*` enable lines present, `m3ctl`
shows the three active postures, and the machine reaches a **usable login shell
that runs commands** (fork/exec/signals all work — that alone exercises the CET
per-task-SSP path across every process spawn).

**Fail → likely cause:**
- **Triple-fault / `#GP` at first ring-3 entry** → a PCID ordering bug (CR4.PCIDE
  set after a tagged CR3 load) *or* a wrong CET shadow-stack PTE encoding.
  Capture the fault RIP/CR2. Fall back to image B (`off`) to confirm the base
  boots, then bisect: build with CET masked (comment the `enable_user_cet_*`
  call or force `cet_active=false`) to isolate PCID vs CET.
- **Boot stops right after "init registered as pid 1", no fault** → the classic
  silent user-CR3 `#PF` loop (a KPTI user-slot miss) — but that's covered by
  QEMU; if it appears *only* here it points at PCID tagging. Grab the boot.log.
- **`#CP … process killed` on the first shell command** → a stale/wrong SSP:
  the shadow-stack setup or the context-switch save/restore is wrong. This is
  exactly the CET path QEMU can't test — capture the pid/rip and see Block 4.

**Record:** `next-dell-session.md` A.5 + B.3 "live on real silicon" boxes →
`Validated-on-HW (run 2, 2026-07-09)` — **done** (checked off); quote the `[sec]`
line as evidence.

---

## Block 2 — the functional PoCs (the whole point of Track A/B)

### 2a — Meltdown reject (A.6)

> **⚠️ CORRECTED (run 2) — NOT demonstrable on this silicon.** The Dell is
> `rdcl_no=true` (Meltdown-immune in hardware), so there is **no leak to reject**:
> `meltdown-poc` runs its `channel=CALIBRATED` control then reports
> `MELTDOWN_POC:NO-LEAK` on **both** image B (KPTI off) and image C (KPTI on) —
> the "leak on B / no-leak on A" A/B below **cannot** be shown here. A positive
> leak demo needs Meltdown-**susceptible** (pre-`rdcl_no`) silicon or a KVM CPU
> model without `rdcl_no`. Recorded result: **immune-silicon, no exposure**
> (handoff §0.7). The susceptible-silicon steps below are retained for that case.

**Objective.** A ported public Meltdown PoC **leaks** kernel memory with KPTI
**off** and **fails** with it **on** — the proof QEMU can never give.

**Artifact.** `meltdown-poc` (ring-3): flush+reload cache side-channel that
speculatively reads a known kernel address and times the covert channel to
recover a byte. (Standard public PoC, ported to the m3OS syscall/timing ABI.)

**Steps.**
1. Boot **image B** (`mitigations=off`, KPTI off). Run `meltdown-poc`.
   **Expected:** it recovers ≥1 known kernel byte (a non-zero leak rate). This
   proves the PoC works and the CPU is susceptible.
2. Boot **image C** (KPTI on — *not* image A on this silicon). Run the same PoC.
   **Expected:** it recovers **nothing** (leak rate at noise floor) — the user
   CR3 has no kernel mapping to speculate against.

**Pass:** leak with KPTI off, no leak with KPTI on. **Fail (leaks with KPTI
on):** a global kernel PTE survived the CR3 switch (the `global_kernel_ptes=0`
guard should have caught it at boot — re-check that line) or a kernel mapping
leaked into the user half (re-run the QEMU `kpti-selftest-smoke` invariant).

**Record:** A.6 box → `Validated-on-HW (run 2, 2026-07-09) — immune silicon,
NO-LEAK`; commit the PoC output. (Done — this silicon cannot show a positive
leak; recorded as immune.)

### 2b — CET catches a ROP/return-overwrite (B.3)

> **✅ VALIDATED (run 2, 2026-07-09).** `rop-cet-poc` on the Dell (CET on) was
> `#CP`-killed — no `ROP_CET_POC:PWNED`; `dmesg` shows the `#CP … process killed`
> line. B.3 ROP box checked off in `next-dell-session.md`. Steps retained for the
> CET-off control arm (image B) and re-runs.

**Objective.** A return-address overwrite faults `#CP` with CET **on** and
returns into the planted address with CET **off** — the CFI analogue of 2a.

**Artifact.** `rop-cet-poc` (ring-3): a function that deliberately overflows a
local buffer to overwrite its own return address with the address of a
`pwned()` marker fn, then returns. (Build **without** the stack canary for this
one — `-Z stack-protector=none` on that crate — so the canary doesn't catch it
first; CET is the layer under test.)

**Steps.**
1. Boot **image C** (CET on; image A also has CET but not KPTI). Run `rop-cet-poc`.
   **Expected:** the `RET` faults `#CP` and the kernel kills it:
   ```
   [int] userspace #CP (CET control-protection): pid=… rip=… rsp=… err=… (shadow-stack/CFI violation — return-address overwrite) — process killed
   ```
   `pwned()` must **not** run (no "PWNED" print).
2. Boot **image B** (`off`, CET off) — or a CET-masked build (force
   `cet_active=false` / mask `CET_SS` in `probe_cet`). Run the same PoC.
   **Expected:** the overwrite succeeds → `pwned()` runs → "PWNED" prints, **no**
   `#CP`.

**Pass:** `#CP` kill (no PWNED) with CET on; PWNED with CET off. **Fail (PWNED
with CET on):** the shadow stack isn't actually enforcing — check the boot
`cet(active=true)`, that the PoC's shadow-stack pages carry the RO+Dirty
encoding, and that `IA32_U_CET.SH_STK_EN` is set (`rdmsr 0x6A0`).

**Record:** B.3 ROP box → `Validated-on-HW (run 2, 2026-07-09)` — **done**; quote the `#CP` line.

---

## Block 3 — the A.5 PCID perf bound

**Objective.** With PCID active, the smoke suite is **≤ 30 %** slower than
`mitigations=off` — the Phase 84 bound the naive full-flush KPTI cannot meet, so
this proves the no-flush tags buy back the cost.

**Tool (shipped this session):** **`/bin/perf-bench`** — a 3M-iteration
`getpid()` round-trip timer (pure ring3→ring0→ring3, so it isolates the
KPTI/PCID trampoline + CR3-switch cost). Prints `ns_per_syscall`. `ITERS` is a
tunable `const` at the top of `userspace/perf-bench/src/main.rs`.

**Steps.**
1. On **image C** (`full`, PCID active): run `/bin/perf-bench`; record
   `ns_per_syscall` as `ns_full`.
2. On **image B** (`off`): run `/bin/perf-bench`; record `ns_off`.
3. Compute `(ns_full − ns_off) / ns_off`.

**Pass:** ≤ 30 %. **Fail (> 30 %):** the same-address-space re-dispatch no-flush
skip already landed (PR #325, 23/n) — confirm it's active; if still over,
the per-CPU last-CR3 cache is the next lever. **A/B sanity:** temporarily mask
PCID in `probe_pcid` (forces the full-flush fallback on the same silicon) to
measure exactly what the tags recover.

**Record:** A.5 perf box → **`Validated-on-HW (2026-07-10)`** — image C (`full`)
`ns_full=6128`, image B (`off`) `ns_off=5967` → overhead `2.7 %` (≤ 30 % ⇒ **PASS**).
PCID hides nearly all the KPTI CR3-switch cost; the full-flush fallback could not
meet this. **Done.**

---

## Block 4 — CET stress: flush out the two flagged risks

These are documented Dell-validation risks in the CET handoff; Block 1's clean
boot doesn't exercise them hard enough. Run **on image C** (KPTI+CET active on
this `rdcl_no=true` silicon; image A leaves KPTI off). Two dedicated PoCs ship
this session: **`/bin/fork-cet-poc`** (4a) and **`/bin/nested-sig-cet-poc`** (4b).

> **RUN 2 (2026-07-10) RESULTS.** 4a **✅ PASS** (`FORK_CET_POC:PASS` — Fix #5
> holds on HW). 4b **🔴 FAIL — confirmed bug:** `nested-sig-cet-poc` `#CP`-killed
> on the *nested* handler's `ret` (`pid=45 rip=0x2014b3 err=0x1`). Follow-up +
> per-frame-SSP fix plan:
> [2026-07-10 nested-signal SSP `#CP`](./2026-07-10-cet-nested-signal-ssp-followup.md).

### 4a — fork CoW-of-shadow-stack (regression-confirm; **already fixed**)
**Status.** **Fixed** by Fix #5 of the CET bring-up (handoff §0.6): fork now
eagerly copies shadow-stack pages instead of sharing them. This arm is a
**regression confirmation**, not an open bug hunt.
**Risk (historical).** The generic CoW shared non-writable pages verbatim, so a
fork parent and child aliased one RO+Dirty shadow-stack frame → a post-fork `RET`
`#CP` (the original `ion _Fork` kill).
**Test.** **Run `/bin/fork-cet-poc`** — forks 8 children; each (and the parent
between spawns) recurses so both actively push/pop their shadow stacks. A shared
page would corrupt across them.
**Pass:** `FORK_CET_POC:PASS` (all children survived → shadow stacks are
independent, Fix #5 holds). **Fail:** `FORK_CET_POC:FAIL` + a child `#CP … process
killed` line → Fix #5 regressed; capture the child pid/rip.

### 4b — nested-signal SSP (the one genuinely OPEN CET risk)
**Risk.** `Task.cet_signal_ssp` is a single slot — correct for non-nested
signals, unproven for a signal interrupting a handler (handoff §0.3/§0.8 defer
this).
**Test.** **Run `/bin/nested-sig-cet-poc`** — installs handlers for two signals,
raises `SIGUSR1` (`kill` self), and from inside that handler raises `SIGUSR2`
(`kill` self) to force a nested delivery, then both `sigreturn`. (m3OS has no
`SIGALRM`/`sigprocmask`, so self-`kill` of a *different* signal is how we nest.)
Read the printed line ordering to tell true nesting (`outer-entered →
inner-entered → inner-returning → outer-resumed`) from deferred delivery.
**Pass:** `NESTED_SIG_POC:PASS` — both handlers entered and returned, no `#CP`
(the single slot survives one nesting level). **Fail:** a `#CP` kill on the outer
handler's return → confirms the `RSTORSSP`-token path is needed
(`kernel_core::cet::shadow_stack_restore_token` is modeled; `WR_SHSTK_EN` is on
so `WRUSS` can seed the token) — push a restore token per delivery instead of the
single slot. **PARTIAL** (`NESTED_SIG_POC:PARTIAL`): handlers ran, no `#CP`, but
delivery was sequential not nested — record the platform behavior.

**Record:** note both outcomes in the CET handoff's "known risks" section
(resolved, or promoted to a tracked follow-up with the captured failure).

---

## Block 5 — batch the older bare-metal arms (if bench time remains)

Lower priority than the security work, but the machine is already booted — and
several of these arms exist **only** on metal (a real battery, charger, panel,
and internal NVMe have no QEMU device). Pull exact steps from
`next-dell-session.md`. Quick index, highest-value first:

- **Phase 103 power** (image A — no separate build) — the daily-driver headline,
  entirely bare-metal. On the default boot: `cpufreq: HWP enabled, perf range
  <lo>..<hi>` (a real Tiger Lake range, not the QEMU no-HWP no-op),
  `POWERD:ready battery=<path> ac=<path> zones=<N> mech=hwp` (vs the VM
  `battery=none ac=assumed-online zones=0 mech=none`), and `m3ctl power status`
  showing a real `battery: <pct>%` / `ac: online` / `thermal: <temp>`. **Track
  G.3 headline arm:** unplug the charger → `Notify(ADP,0x80)` + `_BST` re-read,
  AC online→offline, pct decreasing. Then `m3ctl backlight <pct>` visibly dims
  the panel (photo; `_BCM`/`_BQC`). Stretch: an S3 round trip (`POWERD:resume` +
  `_WAK(3)` → live shell/disk/brightness) **or** a clean fail-closed. **Near-zero-
  cost overlaps:** the ACPI capture below also feeds 103's `_BST`/`_BIF` fixture
  swap, powerd's battery posture flips from the Block 1 boot for free, and the
  lid-switch SCI is the Phase 101 lid arm — do these in the same pass.
- **Phase 106 NVMe install (M3)** (combined USB image) — the one arm that
  deliberately writes the internal `nvme0n1` (**not** the USB `/dev/sda`). Boot
  the combined USB image (M1: writable ext2 root), run `/sbin/installer --part`
  targeting the internal NVMe, watch `INSTALLER:mode/layout/target/gpt-written/
  esp-copied/format/populate` (no `INSTALLER:error part-*`) + the first-user
  prompt, then reboot **with the USB removed** → the NVMe boots alone to a login
  as the created first user.
- **Phase 111 kgdb** (image D) — freezes at `KGDB:waiting`; attach a raw-RSP
  client (or real `gdb`) over COM2 (USB-serial adapter at `0x2F8`), breakpoint at
  `nm` addr + `0x10000000000`, confirm hit + register/memory read-back; then the
  async-break (Ctrl-C `0x03`) and the **panic hook** (`KGDB:panic`) — the
  highest-value bare-metal use.
- **Phase 111 ptrace / m3gdbserver** (image E) — `m3gdbserver <port> <prog>`;
  attach a host RSP client over Wi-Fi / a USB-Ethernet dongle; breakpoint + step
  + continue-to-exit.
- **Phase 101 ACPI** — capture the Dell's DSDT/SSDTs (`usb-logsink` boot.log or
  boot Linux + `acpidump`); land under `kernel-core/tests/fixtures/acpi/` and
  re-point the host tests; confirm `ACPI_SMOKE:namespace-built` + `sci-armed` +
  the FADT boot line from real firmware; lid-switch SCI.
- **Phase 100 GUI/USB** — usbhub tier-2 capture, USB keyboard types, greeter
  photo, WC blit-latency (see `next-dell-session.md` Phase 100 + the phase-100
  runbook).

---

## Capture + record discipline (every block)

1. Save each captured log under the phase's `scripts/*-validate.md` results
   appendix (generalize `scripts/ure-vfio-validate.md`); reference from the task
   doc checkbox.
2. Set the task-doc / README Status to `Validated-on-HW (run N, 2026-07-09) —
   Dell Precision 5560 / Tiger Lake; evidence: <path>` — never a bare "Complete".
3. Check the corresponding box in `next-dell-session.md` with the capture path.
4. A failure is **also** a recorded result: capture the fault (RIP/CR2/error
   code + boot.log) and open a follow-up rather than leaving the box blank.

## Risk watchlist (things that will bite, ordered by likelihood)

1. **CET fork CoW-of-shadow-stack** (Block 4a) — the most likely real bug; the
   generic CoW path is untested against RO+Dirty pages.
2. **PCID `#GP` at first ring-3 entry** (Block 1) — a CR4.PCIDE-vs-tagged-CR3
   ordering hazard; the enable-before-`boot_aps` + idempotent per-AP re-assert
   were written to prevent it, but it's never run.
3. **Stale SSP `#CP`** on a heavy context-switch / signal workload (Block 1/4) —
   the save/restore co-location with FPU state is the correctness argument, but
   unproven on hardware.
4. **Nested-signal `#CP`** (Block 4b) — known single-slot limitation.
5. **Perf > 30 %** (Block 3) — the re-dispatch skip should keep it under; the
   last-CR3 cache is the fallback lever.
