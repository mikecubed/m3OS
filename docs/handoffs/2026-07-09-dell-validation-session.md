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
5. **Batch the older HW arms** if bench time remains (111 kgdb/ptrace, 101
   ACPI capture, 100 GUI/USB).

**Definition of success:** objectives 1–3 recorded `Validated-on-HW`; objective
4 either clean or with a captured, root-caused failure that becomes a follow-up.

---

## Block 0 — pre-flight (do BEFORE the bench, on the build host)

Build all images we might need up front so bench time is boot-and-capture, not
compile. `M3OS_MITIGATIONS` and `M3OS_KERNEL_FEATURES` are **build-time**
(`option_env!`), so each posture is a separate image.

| # | Image | Build command | Purpose |
|---|---|---|---|
| A | **default (auto)** | `cargo xtask image` | KPTI+PCID+CET all active — the main security boot |
| B | **mitigations=off** | `M3OS_MITIGATIONS=off cargo xtask image` | the A/B baseline: KPTI/PCID/CET all off (Meltdown leaks, ROP returns, perf floor) |
| C | **mitigations=full** | `M3OS_MITIGATIONS=full cargo xtask image` | same as A on this silicon (Tiger Lake `rdcl_no=false`); use for the perf run to be explicit |
| D | **kgdb** | `M3OS_KERNEL_FEATURES=kgdb cargo xtask image` | Phase 111 kgdb arm |
| E | **ptrace** | `M3OS_KERNEL_FEATURES=ptrace cargo xtask image` | Phase 111 native gdbserver arm |

Each image lands at `target/x86_64-unknown-none/release/boot-uefi-m3os.img`
(rename per posture before the next build). Flash with
`scripts/phase-100-write-usb.sh /dev/sdX` (on the Dell, USB = `/dev/sda`; the
NVMe system disk is `nvme0n1` — **do not** flash that). Direct fallback:
`sudo dd if=<img> of=/dev/sda bs=4M conv=fsync status=progress && sync`.

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

**Artifacts to create before the bench (small userspace PoCs — see Block 2):**
`meltdown-poc` and `rop-cet-poc`. Both are un-writable today; scaffold them as
ring-3 binaries (workspace member + `bins` + ramdisk entry) so they ship in the
image. If not ready, Block 1 (the posture boot) still stands alone.

---

## Block 1 — the security posture boot (do this FIRST; it validates the most)

**Objective.** Prove KPTI, PCID, and CET all activate and the machine boots
clean to a login on real Tiger Lake silicon — validating in one shot every
asm/CR3/MSR/PTE path QEMU could not run.

**Steps.**
1. Boot **image A** (default). Capture the boot log over AMT SOL / boot.log.
2. Grep the `[sec]` policy line and the per-core enable lines.
3. Log in; run `m3ctl mitigations status`.

**Expected serial (exact — from the shipping code):**
```
[sec] mitigations=Auto … kpti(policy=true active=true) pcid(active=true supported=true) cet(active=true supported=true) global_kernel_ptes=0
[sec] CR4.PCIDE enabled (KPTI PCID TLB-cost recovery active)
[sec] CR4.CET enabled (CET user shadow stacks active)
[sec] AP CR4.SMEP enabled CR4.SMAP enabled CR4.PKE enabled CR4.PCIDE enabled     ← one per AP
```
(Note: the AP line reports PCIDE but **not** CET today — CET on APs is proven
by the boot line's `cet(active=true)` + clean multi-core run. Adding a CR4.CET
field to the AP line is a nice 1-line follow-up if we want per-AP CET evidence.)

**Expected `m3ctl mitigations status`:**
```
Meltdown: Mitigation: PTI
… KPTI PCID: active (kernel/user PCID, no-flush)
… CET: enabled (user shadow stacks)
```
(Contrast QEMU, which prints `pcid(active=false …)` / `cet(active=false …)` /
`KPTI PCID: fallback …` / `CET: not-supported`.)

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
`Validated-on-HW (run 1, 2026-07-09)`; quote the `[sec]` line as evidence.

---

## Block 2 — the functional PoCs (the whole point of Track A/B)

### 2a — Meltdown reject (A.6)

**Objective.** A ported public Meltdown PoC **leaks** kernel memory with KPTI
**off** and **fails** with it **on** — the proof QEMU can never give.

**Artifact.** `meltdown-poc` (ring-3): flush+reload cache side-channel that
speculatively reads a known kernel address and times the covert channel to
recover a byte. (Standard public PoC, ported to the m3OS syscall/timing ABI.)

**Steps.**
1. Boot **image B** (`mitigations=off`, KPTI off). Run `meltdown-poc`.
   **Expected:** it recovers ≥1 known kernel byte (a non-zero leak rate). This
   proves the PoC works and the CPU is susceptible.
2. Boot **image A** (KPTI on). Run the same PoC.
   **Expected:** it recovers **nothing** (leak rate at noise floor) — the user
   CR3 has no kernel mapping to speculate against.

**Pass:** leak with KPTI off, no leak with KPTI on. **Fail (leaks with KPTI
on):** a global kernel PTE survived the CR3 switch (the `global_kernel_ptes=0`
guard should have caught it at boot — re-check that line) or a kernel mapping
leaked into the user half (re-run the QEMU `kpti-selftest-smoke` invariant).

**Record:** A.6 box → `Validated-on-HW (run 1, date)`; commit the PoC output.

### 2b — CET catches a ROP/return-overwrite (B.3)

**Objective.** A return-address overwrite faults `#CP` with CET **on** and
returns into the planted address with CET **off** — the CFI analogue of 2a.

**Artifact.** `rop-cet-poc` (ring-3): a function that deliberately overflows a
local buffer to overwrite its own return address with the address of a
`pwned()` marker fn, then returns. (Build **without** the stack canary for this
one — `-Z stack-protector=none` on that crate — so the canary doesn't catch it
first; CET is the layer under test.)

**Steps.**
1. Boot **image A** (CET on). Run `rop-cet-poc`.
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

**Record:** B.3 ROP box → `Validated-on-HW (run 1, date)`; quote the `#CP` line.

---

## Block 3 — the A.5 PCID perf bound

**Objective.** With PCID active, the smoke suite is **≤ 30 %** slower than
`mitigations=off` — the Phase 84 bound the naive full-flush KPTI cannot meet, so
this proves the no-flush tags buy back the cost.

**Steps.**
1. On **image C** (`full`, PCID active): run the smoke suite / a syscall-heavy
   workload on the Dell; record wall time `T_full`.
2. On **image B** (`off`): same workload; record `T_off`.
3. Compute `(T_full − T_off) / T_off`.

**Pass:** ≤ 30 %. **Fail (> 30 %):** the same-address-space re-dispatch no-flush
skip already landed (PR #325, 23/n) — confirm it's active; if still over,
the per-CPU last-CR3 cache is the next lever. **A/B sanity:** temporarily mask
PCID in `probe_pcid` (forces the full-flush fallback on the same silicon) to
measure exactly what the tags recover.

**Record:** A.5 perf box → `Validated-on-HW (run 1, date)` with both wall times.

---

## Block 4 — CET stress: flush out the two flagged risks

These are documented Dell-validation risks in the CET handoff; Block 1's clean
boot doesn't exercise them hard enough. Run **on image A**.

### 4a — fork CoW-of-shadow-stack
**Risk.** A fork child inherits the parent's SSP and a CoW copy of the parent's
RO+Dirty shadow-stack pages. The child's first shadow-stack push must
CoW-duplicate that page; m3OS's *generic* CoW may not correctly duplicate a
shadow-stack page (Linux copies it explicitly).
**Test.** A fork-heavy workload where children make deep-ish calls after fork
(any coreutils pipeline, `sh` running a script with subshells, or a small
fork+recurse+return PoC). **Watch for** a `#CP`/`#PF` in a *forked child's*
first `RET`/`CALL`.
**Pass:** fork-heavy workloads run clean. **Fail:** capture the child pid/rip →
this confirms the CoW arm is needed (a shadow-stack-aware CoW that copies rather
than shares, or marks the page for eager-copy). Becomes the top CET follow-up.

### 4b — nested-signal SSP
**Risk.** `Task.cet_signal_ssp` is a single slot — correct for non-nested
signals, wrong for a signal interrupting a handler.
**Test.** A program that takes a signal whose handler itself takes a second
signal (e.g. `SIGALRM` handler that triggers/receives another), then both
`sigreturn`. **Watch for** a `#CP` on return from the outer handler.
**Pass:** nested signals resume clean. **Fail:** confirms the `RSTORSSP`-token
path is needed (`kernel_core::cet::shadow_stack_restore_token` is modeled;
`WR_SHSTK_EN` is on so `WRUSS` can seed the token) — the fix is to push a
restore token per delivery instead of the single slot.

**Record:** note both outcomes in the CET handoff's "known risks" section
(resolved, or promoted to a tracked follow-up with the captured failure).

---

## Block 5 — batch the older HW arms (if bench time remains)

Lower priority than the security work; pull exact steps from
`next-dell-session.md`. Quick index:

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
