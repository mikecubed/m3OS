# Handoff — Phase 110 B.3 CET boot hang on real silicon (Dell/Tiger Lake)

**Session:** 2026-07-09 Dell Precision 5560 (Intel Tiger Lake, has `CET_SS`).
**Runbook this executes:** [2026-07-09 Dell validation session](./2026-07-09-dell-validation-session.md).
**Branch:** `feat/phase-110-cet-shstk` (all changes below are **uncommitted** — see §6).
**Status:** Block 0 pre-flight **complete + verified**. Bench hit a **CET-enable boot
hang on real silicon**, bisected to the CET path; a first fix (`CR0.WP`) was
necessary-but-insufficient. **Blocked on serial capture** to read the exact fault.

---

## 1. TL;DR

Everything the build host can do is done and green (both PoCs authored + wired +
QEMU run-to-completion smoke gates PASS; 8 posture/bisect images staged). On the
Dell, the **default security image (KPTI+PCID+CET) black-screens right after the
bootloader's "jumping to kernel"**. A clean A/B/bisect sweep **pins the cause to
the CET user-shadow-stack enable** (`enable_user_cet_if_supported`,
`kernel/src/arch/x86_64/cpuid.rs`), which runs for the first time only on real
`CET_SS` silicon (QEMU TCG models no CET). A first fix — set `CR0.WP=1` before
`CR4.CET` (Intel SDM requires it; QEMU never enforced it) — **did not resolve the
hang**. Next session **must wire serial** (AMT SOL / USB-serial COM1) to read the
fault (`#GP` vs `#PF` vs triple-fault + RIP); the fix is then likely small.

---

## 2. The bisection — CET is the culprit (decisive)

Every image is default-`auto` posture unless noted; all are staged under
`target/dell-images/` (19 MiB each, 8 distinct kernels). Booted from USB
(`/dev/sda`) on the Dell, judged by **reaches serial login vs black screen**
(no serial wired, so this is the only signal):

| Image | Posture | Result |
|---|---|---|
| `A-default` | KPTI + PCID + CET | **black screen** |
| `B-mitigations-off` | all off (`M3OS_MITIGATIONS=off`) | **boots to login** (root works) |
| `F-cet-masked` | KPTI + PCID, **CET off** (`M3OS_MASK_CET=1`) | **boots** |
| `G-pcid-masked` | KPTI + CET, **PCID off** (`M3OS_MASK_PCID=1`) | **black screen** |
| `H-cetfix-default` | KPTI + PCID + CET **+ `CR0.WP` fix** | **black screen** |

**Reading:** B proves the base boots. F (CET removed) boots → removing CET fixes
it. G (PCID removed, CET kept) still hangs → PCID is *not* it. So **PCID and KPTI
both work on this silicon; CET is the sole culprit.** H shows the first CET fix
attempt was insufficient (see §3).

This is a genuine Objective-1 finding, not a session failure: QEMU TCG can prove
*none* of the CET/PCID asm/MSR paths, so a real-silicon bring-up fault here is
exactly what the bench exists to catch.

---

## 3. Root-cause analysis so far

**Where it dies (boot order, `kernel/src/lib.rs`):**
- `post_marker(6)` (`lib.rs:340`) = framebuffer console init — **this clears the
  screen**. Everything after paints text on a freshly-cleared (black) screen, so
  a hang/fault past this point *looks* like a black screen.
- `crate::mitigations::init_bsp()` at **`lib.rs:536`** — runs **between
  `post_marker(12)` and `post_marker(13)`**. This is where `enable_pcid_*` and
  **`enable_user_cet_if_supported`** fire (BSP; APs re-run it in
  `mitigations::init_ap`, `mitigations.rs:225`; also on S3 resume).

**The CET enable (`kernel/src/arch/x86_64/cpuid.rs::enable_user_cet_if_supported`):**
```
1. (NEW fix) ensure CR0.WP = 1
2. CR4 |= CR4_CET ; mov cr4       ← the privileged write real CET silicon gates
3. wrmsr IA32_U_CET = SH_STK_EN | WR_SHSTK_EN  (= 0x3, valid — not the fault)
```

**Fix #1 (applied, insufficient): `CR0.WP` precondition.** Intel SDM Vol 3A:
setting `CR4.CET` while `CR0.WP=0` raises `#GP(0)`. **Nothing in BSP boot sets
`CR0.WP`** (only the kgdb/ptrace debug path in `arch/x86_64/debug.rs` touches it);
the enable routine's own comment *assumed* "m3OS always has it." QEMU models no
CET and never enforced the precondition — invisible until now. The fix ensures
`CR0.WP=1` per-core before `CR4.CET`. **H still black-screens**, so either:

- **(a)** the `CR4.CET` write still faults for another reason (WP not actually 1
  at that point? another precondition?), **or**
- **(b)** WP=1 now succeeds for CET but **exposed a *different* `#PF`**: some
  ring-0 code writes a read-only page later in boot, which was silently allowed
  under WP=0 and now faults (WP=1 is CET-mandatory, so this would need fixing
  regardless), **or**
- **(c)** `CR4.CET` sets OK but the **first shadow-stack operation** (or the
  `IA32_U_CET` write / a later CET-gated path) faults, **or**
- **(d)** the fault triple-faults (no clean handler at that boot stage) →
  **reboot loop** rather than a true hang.

We cannot distinguish (a)–(d) without seeing the fault. That is the blocker.

**Open question that changes the approach:** is H a **hard hang** or a **reboot
loop**? Watch whether "jumping to kernel" reappears/cycles. A loop ⇒ triple-fault
⇒ POST squares get wiped on reset (serial needed); a hang ⇒ POST squares persist
(the non-serial diagnostic in §4.2 works).

---

## 4. Next-session plan (ordered)

### 4.1 Wire serial FIRST — this unblocks everything
The display cannot show a fault RIP; on this port-less laptop that means **AMT
Serial-over-LAN** (Intel ME redirects COM1/`0x3F8` over Ethernet; capture with
`amtterm` from a 2nd machine — runbook `scripts/ure-vfio-validate.md`) or a
**USB-serial adapter**. Boot `H-cetfix-default.img` (or `A-default.img`) and read
the boot log around `mitigations`: expect a `#GP`/`#PF`/`#DF` line with a RIP.
Map RIP back with `nm target/x86_64-unknown-none/release/kernel` **+ the 1 TiB PIE
base `0x10000000000`** (same idiom as the kgdb arm). That RIP names the faulting
instruction (the `mov cr4`, the `wrmsr`, or a later RO write) and the fix follows
directly.

### 4.2 If serial is impossible this session — POST-square bisection (hang-only)
Serial-free diagnostic already in-tree: set `BRINGUP_DIAG = true`
(`kernel/src/lib.rs:96`) to paint numbered POST squares straight to the
framebuffer (survives even before the fb console, and past a hang — **but not a
reset**). Then add **fine-grained `post_marker(...)` calls *inside*
`enable_user_cet_if_supported`**: one before the WP set, one before `mov cr4`, one
before the `IA32_U_CET` wrmsr, one after. Rebuild + boot: the **last square
painted localizes the faulting instruction** with no serial. (Only works if H is
a hang, not a reboot loop — see §3.)

### 4.3 Source-level bisection within the CET enable (build-time, no serial)
If POST squares are inconclusive, split the enable into staged test images
(env-gated like the existing mask knobs) and boot each:
- **WP-only**: set `CR0.WP=1` but *skip* `CR4.CET` + the MSR. Boots? ⇒ WP=1 is
  fine on its own; the fault is the `CR4.CET`/MSR.
- **WP + CR4.CET, no MSR**: isolates the `mov cr4` from the `wrmsr`.
- This narrows (a) vs (c) without serial.

### 4.4 Candidate fixes to evaluate once the fault is known
- If the `mov cr4` still `#GP`s with WP=1: re-check WP is actually 1 at that
  instant (print/marker CR0 just before), and confirm no reserved CR4 bit; some
  parts also want `CR4.CET` set only after paging/`IA32_S_CET` is coherent
  (`IA32_S_CET.SH_STK_EN` must stay 0 — verify it isn't inadvertently set).
- If WP=1 exposed a ring-0 RO-write `#PF` (hypothesis b): that RIP names the
  offending write; fix that site (map RW, or use `WRUSS`/an explicit WP window).
  Consider whether m3OS should run **`CR0.WP=1` globally from early boot** (the
  correct hardened baseline the code already assumed) rather than only at CET
  enable — but that must be validated for latent RO-writes across the whole boot.
- If a later CET-gated op faults (shadow-stack setup / first ring-3 `CALL`): that
  is the B.3 per-task-SSP / PTE-encoding path — see the CET handoff risks.

### 4.5 After CET boots
Resume the runbook: confirm `[sec] … cet(active=true supported=true)` +
`CR4.CET enabled`, `m3ctl mitigations status` → `CET: enabled`, then **Block 2b**
(`rop-cet-poc` must now `#CP`-kill, no `PWNED`) and **Block 2a** (`meltdown-poc`
leak on B / no-leak on A). PCID (A.5) and KPTI already validated as booting via F.

---

## 5. Artifacts (all on the build host)

**Staged images — `target/dell-images/`:**
`A-default`, `B-mitigations-off`, `C-mitigations-full`, `D-kgdb`, `E-ptrace`
(Block 0 A–E), plus bisect images `F-cet-masked`, `G-pcid-masked`, and
`H-cetfix-default`. Flash: `scripts/phase-100-write-usb.sh --image
target/dell-images/<X>.img /dev/sda` (USB=`/dev/sda`; internal NVMe=`nvme0n1` —
never flash that). Rebuild any with the commands in Block 0 / the mask knobs.

**Build-time bisect knobs (added this session, default-off, no production effect),
`kernel/src/arch/x86_64/cpuid.rs`:**
- `M3OS_MASK_CET=1` → `probe_cet` returns "no CET" (KPTI+PCID stay on).
- `M3OS_MASK_PCID=1` → `probe_pcid` returns false (KPTI+CET stay on).
Each masks the CPUID probe, which gates the corresponding `CR4` enable.

**PoCs (Block 0, verified, shipping in every image):**
- `userspace/rop-cet-poc` — asm-verified naked return-address overwrite
  (`mov [rsp],rdi; ret`), canary-off build. `#CP` on CET / `PWNED` without.
- `userspace/meltdown-poc` — flush+reload + mispredicted-branch speculative
  kernel read + calibration control. Uses `rdtsc` (**not** `rdtscp` — the default
  QEMU CPU faulted `rdtscp` in ring 3 into a retry loop) and a `--smoke` fast
  mode. Tunable `const`s at the top of `main.rs` for the bench.

**Smoke gates (QEMU run-to-completion, both PASS):**
`cargo xtask rop-cet-poc-smoke` / `meltdown-poc-smoke`; behind
`M3OS_SEC_POC_REGRESSION=1` in pre-push. Docs: AGENTS.md gate table +
`docs/appendix/regression-gates.md`.

---

## 6. Uncommitted state — commit before/after next session

Branch `feat/phase-110-cet-shstk`, all clean under `cargo xtask check`. Nothing
is committed yet. Changes:

```
 M .githooks/pre-push                     # M3OS_SEC_POC_REGRESSION gate
 M AGENTS.md                              # gate table row
 M Cargo.toml / Cargo.lock                # 2 new workspace members
 M docs/appendix/regression-gates.md      # gate section
 M docs/handoffs/2026-07-09-dell-validation-session.md   # Block 0 artifacts note
 M docs/handoffs/next-dell-session.md     # A.6 / B.3 point at the shipped binaries
 M kernel/src/arch/x86_64/cpuid.rs        # CR0.WP-before-CR4.CET fix + mask knobs
 M kernel/src/fs/ramdisk.rs               # embed the two PoCs
 M xtask/src/main.rs                      # bins + 2 smoke gates + rop rustflags
?? userspace/meltdown-poc/                # new crate
?? userspace/rop-cet-poc/                 # new crate
```

The `CR0.WP` fix is correct and worth keeping even though it didn't fully resolve
the hang (it's an architectural requirement for CET). The mask knobs are reusable
bench tooling. Recommend committing all of this (the Block 0 work is done; the
CET fix is a partial-but-correct step) so the next session starts from a clean
tree and iterates only on the remaining CET-enable fault.

---

## 7. One-paragraph orientation for the next agent

Block 0 is done. The bench proved (via the F/G/H image bisect in §2) that the
**CET user-shadow-stack enable** hangs the Dell at boot — the one Phase 110 path
QEMU can't exercise. A `CR0.WP`-before-`CR4.CET` fix was correct but insufficient.
You are **blind without serial**: wire AMT SOL / USB-serial (§4.1), boot
`H-cetfix-default.img`, read the fault RIP around `mitigations::init_bsp`
(`lib.rs:536`, between POST markers 12–13), and the fix follows. If serial is
truly unavailable, use the `BRINGUP_DIAG` POST-square bisection (§4.2) — but first
determine whether it's a **hard hang or a reboot loop** (does "jumping to kernel"
recur?), because a triple-fault reset wipes the squares. All images, knobs, and
the exact code sites are in §3 / §5.
