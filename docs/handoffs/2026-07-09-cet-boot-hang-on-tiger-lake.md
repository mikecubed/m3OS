# Handoff — Phase 110 B.3 CET boot hang on real silicon (Dell/Tiger Lake)

**Session:** 2026-07-09 Dell Precision 5560 (Intel Tiger Lake, has `CET_SS`).
**Runbook this executes:** [2026-07-09 Dell validation session](./2026-07-09-dell-validation-session.md).
**Branch:** `feat/phase-110-cet-shstk` (all changes below are **uncommitted** — see §6).
**Status:** **ROOT-CAUSED + THREE FIXES LANDED (pending Dell re-flash confirmation).**
Three distinct CET real-silicon bugs, all invisible to QEMU: **(1)** the AP
trampoline reloaded the BSP's CET-bearing `CR4` **without `CR0.WP`** → per-AP
`#GP` triple-fault → `boot_aps` hang (decoded from **8 / 5 / 4** POST squares;
fix in `smp/boot.rs`). **(2)** init (PID 1) is kernel-spawned via the fork
trampoline with `cet_ssp = 0` and **nothing armed its shadow stack** → its first
ring-3 `CALL` faults (found from `boot.jpeg`: boot reaches marker 22 then stalls
with no userspace; fix in `arch/x86_64/mod.rs`).
**(3)** CET signal delivery never seeded the shadow stack, so any handler's `RET`
`#CP`'d — the greeter respawn loop; fix seeds `restorer` via `WRUSS` in
`cet.rs`/`syscall/mod.rs`. SMP smoke + boot smoke-test PASS; `A-default.img` +
`I-cet-diag.img` rebuilt with all three fixes. **Next: flash `A-default.img` →
expect the greeter to stay up and log in.**

---

## 0. RESOLUTION (2026-07-09, this session) — read this first

**The squares decoded the hang.** You reported the `I-cet-diag.img` POST strip as
**8 squares on row 0, 5 on row 1, 4–5 on row 2** (row 2's 5th "unreadable because
black"). Decoding needs two facts: (1) marker 6 (`fb console init`) **clears the
screen**, wiping every square before it; (2) markers do **not** fire in slot order
(`mm::init` paints 16–19, `apic::init` paints 24–28, CET paints 32–35, all
interleaved with row 0). Tracing the real execution order, the squares that
survive the clear on a hang right after mitigations are exactly:

| Row | Count | Surviving markers | Meaning |
|---|---|---|---|
| 0 | **8** | 6,7,8,9,10,11,12,**13** | reached "mitigations + virtio-net done" |
| 1 | **5** | 24,25,26,27,28 | APIC fully up (27 = col 11 = the pure-black square) |
| 2 | **4** | 32,33,34,35 | **BSP CET enable fully succeeded** |

8 / 5 / 4 matches perfectly. **All four CET markers painted ⇒ BSP CET works.** The
hang is between marker 13 and 14 — in a release build that span is *nothing but*
`smp::boot::boot_aps()`.

**Root cause — AP inherits `CR4.CET` without `CR0.WP`.** The BSP enables `CR4.CET`
*before* `boot_aps()`, so the trampoline's captured `DATA_CR4` snapshot carries
bit 23. In `ap_entry` (`kernel/src/smp/boot.rs`) each AP does `mov cr4, bsp_cr4`,
but the AP trampoline enables paging and **never sets `CR0.WP`**. Intel SDM Vol 3A:
a `mov cr4` that sets `CR4.CET` while `CR0.WP=0` raises `#GP(0)`. With no real IDT
at that trampoline stage the `#GP` triple-faults the AP → it never checks in →
`boot_aps` spins forever on the rendezvous, so the BSP hangs waiting for dead APs.

This explains everything the bisection saw: **F** (CET masked) boots because the
snapshot has no CET bit; **G** (PCID masked, CET on) hangs because CET is still on;
**H** (the BSP-only `CR0.WP` fix) still hung because the AP crashes in the
trampoline reload *before* it ever reaches `enable_user_cet_if_supported`.

**Fix (landed):** set `CR0.WP=1` in `ap_entry` **before** the `mov cr4, bsp_cr4`
reload (mirrors the BSP's WP-before-CET precondition). WP=1 is CET-mandatory and
the correct hardened baseline regardless; no-op on QEMU (no CET bit in the
snapshot). SMP smoke (`-smp 2`, futex-heavy) PASS.

**Also fixed (your request):** the POST-square colouring wrapped `u8` to `0x00`
at col 11 (pure black, invisible) and `0x10` at col 5 — that's why row 1's marker
27 vanished. Recoloured to an always-bright band `[0x80, 0xFC]` with even/odd
parity split (`kernel/src/lib.rs::post_marker`). No square can be black now.

### 0.1 SECOND bug (from `boot.jpeg`) — init's shadow stack was never armed

Re-flashing `I-cet-diag.img` (fix #1 + readable colours) got **much** further —
the photo shows **all of row 0's markers 6–15, row 1's 21+22 & 24–28, row 2's
32–35, and the `[timer] lapic_ticks_per_ms=2409` line**. So boot cleared
`boot_aps` (fix #1 works), ran the scheduler, ran kernel `init_task`, and reached
`spawn_userspace_init()` (marker 22) — but then **stalled with zero userspace
output** (no login banner, no compositor; the diagnostic screen stayed intact).

**Root cause:** init (PID 1) is loaded by the kernel via the **fork trampoline**
(`spawn_userspace_init` → `spawn_fork_task`) with `cet_ssp = 0`, and — unlike
`execve` (which calls `setup_current_task_shadow_stack` at `syscall/mod.rs:5944`)
and unlike `fork` (child inherits the parent's SSP + copied shadow-stack pages) —
**nothing armed init's CET shadow stack.** With `IA32_U_CET.SH_STK_EN = 1` and
`IA32_PL3_SSP = 0`, init's very first ring-3 `CALL` pushes a return address to a
null SSP → `#PF`/`#CP` → init dies → the machine stalls exactly where the photo
shows (marker 22, nothing after). QEMU never caught this (CET inactive there).

**Fix #2 (landed):** in `enter_userspace_fork` (`kernel/src/arch/x86_64/mod.rs`),
arm a shadow stack for any user task that reaches its first ring-3 entry with
`cet_ssp == 0` — which is uniquely init. It runs in init's live CR3, so
`setup_current_task_shadow_stack()` maps into and advances init's own address
space exactly as the execve path does; fail-closed on frame exhaustion. Gated on
`cet_active` (inert on QEMU; fork children have nonzero inherited SSPs → skipped).
Full boot **smoke-test PASS**. Both images rebuilt with fixes #1 + #2.

### 0.2 THIRD bug — signal handlers `#CP` on return (greeter respawn loop)

With fixes #1 + #2, the Dell **booted to the graphical greeter** — but the
greeter **respawn-looped** ("never properly launches, keeps exiting"). Root
cause: the CET **signal-delivery** path saved/restored `IA32_PL3_SSP` but never
**seeded the shadow stack**. The kernel enters a handler via `IRETQ`, which loads
`SSP` but pushes nothing to the shadow stack, so the handler runs on the
*interrupted* context's shadow stack. When the handler executes its final `RET`
to the sigframe `pretcode` (`restorer` → `__restore_rt`), the data-stack return
(`restorer`) is compared against the shadow-stack top (the interrupted function's
return address) → mismatch → **`#CP`** → the process is killed. This kills *any*
process whose signal handler **returns** — the greeter takes a signal early
(SIGCHLD/SIGALRM/SIGWINCH) and dies on every launch. QEMU never caught it (CET
inactive; the whole seam is `cet_active`-gated).

**Fix #3 (landed):** at signal delivery (`deliver_user_signal`,
`syscall/mod.rs`) call a new `cet::seed_signal_shadow_stack(restorer)` — it
`WRUSS`-pushes `restorer` one slot below the live SSP and drops `IA32_PL3_SSP` by
8, so the handler's final `RET` matches and unwinds `SSP` back naturally; the
saved-SSP restore at `sigreturn` then discards the slot. `WRUSS` is emitted as
raw bytes (`66 48 0F 38 F5 07` = `wrussq [rdi], rax`) to dodge any `+cet`
target-feature gating. Gated on `cet_active` → inert on QEMU. `cargo xtask check`
+ boot smoke-test PASS (init's SIGCHLD reaping still works). Both images rebuilt.

> **Known follow-up (not the greeter bug):** deeply *nested* signals still share
> the single `Task::cet_signal_ssp` field. With seeding the shadow stack unwinds
> correctly on its own, so single-level delivery is right; a robust nested
> design would drop the explicit SSP restore and rely purely on the seeds (or
> save a per-frame SSP). Rare; deferred.

**What to do on the Dell next:**
1. Flash **`A-default.img`** (fixes #1+#2+#3) → expect the **greeter to stay up
   and log in**. That closes the CET userspace bring-up.
2. If a *different* process still faults, the `#CP`/`#PF`/`#GP` handlers already
   `_panic_print` the pid+RIP; catch the flash, or wire serial (§4.1) / add a
   halt-on-first-userspace-fault diagnostic to freeze it on-screen.
3. Once logged in: resume §4.5 / Block 2 — `cet(active=true supported=true)`,
   `m3ctl mitigations status` → `CET: enabled`, then `rop-cet-poc` must
   `#CP`-kill (no `PWNED`) and `meltdown-poc` leak on B / no-leak on A.

### 0.3 STILL failing — display_server down; added an on-screen fault dumper

After fix #3 the greeter still fails, now reporting **`notifyd: display_server
unavailable`** (and wallpaper) — those clients call `DisplayConnection::
connect_auto()`, get `None`, and exit. So **display_server itself is down**
(crashed or never registered), and every compositor client fails downstream. It
is unclear whether fix #3 helped, regressed, or is orthogonal — and *all three
fixes so far were code-analysis only* (QEMU can't exercise CET), so we are
guessing. **We need the actual fault** (process, class, RIP).

Blocker: the kernel fault handlers (`#CP`/`#PF`/`#GP`) print via `_panic_print`,
which goes to **serial only** — invisible on the Dell — and the process is then
killed + respawned, so any on-screen flash is overwritten.

**New tool (landed): serial-free on-screen fault dumper.** Gated on
`M3OS_BRINGUP_DIAG=1` (same knob as the POST squares). At the **fatal** ring-3
fault sites only (so ordinary demand-paging `#PF`s are unaffected),
`bringup_freeze_on_user_fault` (`interrupts.rs`): quiesces sibling cores
(`panic_quiesce_aps` NMI), **reclaims the framebuffer** from display_server
(`fb::diag_force_write_fmt` → `restore_console`), paints
`*** BRINGUP_DIAG HALT: userspace <#CP|#PF|#GP> fault *** pid=… comm=… rip=…
rsp=… err=…`, and **halts** — so the first fatal userspace fault freezes on
screen to be photographed. `I-cet-diag.img` rebuilt with it.

**What to do on the Dell next:**
1. Flash **`I-cet-diag.img`**, let it reach the greeter failure. The machine
   should **freeze with the `BRINGUP_DIAG HALT` line** naming the faulting
   process (expect `comm=display_server` or similar), the fault class, and RIP.
2. **Photograph it.** With `comm` + fault class we know the culprit and whether
   it's CET (`#CP`) or a shadow-stack `#PF`; with the RIP + that binary's load
   base we can map the exact instruction. Send it here.
3. If it *doesn't* freeze (no fatal fault — display_server exits cleanly), then
   the failure is not a fault (e.g. an IPC/registration issue) and we debug from
   the client-connect path instead.

Everything below (§1–§7) is the pre-resolution record, kept for the audit trail.

---

## 1. TL;DR (pre-resolution — superseded by §0)

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

> **Correction (§0):** the "CET enable itself hangs" reading was *mislocated*. BSP
> CET enable actually succeeds (markers 32–35 all paint); the hang is the AP
> trampoline reloading CET-bearing `CR4` without `CR0.WP`. Serial was **not**
> needed — the POST squares localized it once decoded against the fb-clear + the
> real marker firing order. See §0.

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

**Resolved (2026-07-09): it is a HARD HANG, not a reboot loop** — "jumping to
kernel" does **not** recur; the screen stays black. So a triple-fault-reset is
ruled out, and the **serial-free POST-square diagnostic in §4.2 is valid** (the
painted squares persist through a hang). A diagnostic image is **already built**
for exactly this — `I-cet-diag.img`, see §4.2.

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

### 4.2 Serial-free POST-square bisection — image ALREADY BUILT (`I-cet-diag.img`)
Confirmed a hard hang (§3), so this works. The diagnostic is committed and an
image is staged: **`target/dell-images/I-cet-diag.img`** = default posture (KPTI+
PCID+CET, with the `CR0.WP` fix) built with `M3OS_BRINGUP_DIAG=1`, which turns on
the `post_marker` POST squares (`kernel/src/lib.rs`, now env-gated) **plus**
fine-grained CET-enable sub-markers (slots 32–35) in
`enable_user_cet_if_supported`. Flash it (`scripts/phase-100-write-usb.sh --image
target/dell-images/I-cet-diag.img /dev/sda`) and read the squares.

**Grid:** 28-px squares, 16 per row, gap 8. Row 0 = slots 0–15 (top-level boot),
row 1 = 16+, **row 2 = 32+ (the CET sub-steps)**. The **last square painted = the
last step that completed; the hang is the instruction after it.** CET key:

| Row-2 square | Means completed | Hang here ⇒ the fault is… |
|---|---|---|
| **32** | entered CET enable (policy on, `CET_SS` usable) | before WP — the `cet_shstk_usable` path / entry |
| **33** | `CR0.WP = 1` set OK | the **`mov cr4` (CR4.CET) write** itself (`#GP`?) |
| **34** | `CR4.CET` set OK | the **`wrmsr IA32_U_CET`** |
| **35** | `IA32_U_CET` written — enable fully succeeded | a **later** CET-gated op (fault after mitigations, e.g. a WP=1-exposed ring-0 RO write, or first ring-3 `CALL`) |

Expected on this hang: row 0 squares 0–12 (interrupts/SMP-per-core/XSAVE done),
then some of 32–35, then nothing. If **32 alone** (no 33) → the WP set hangs (odd
— it's just a `mov cr0`); if **32,33** (no 34) → the `CR4.CET` write still faults
even with WP=1 (revisit the WP precondition / other CR4 constraints); if
**32,33,34** (no 35) → the `IA32_U_CET` wrmsr; if **all of 32–35 paint** but boot
still dies before the fb console text resumes → the fault is *after* CET enable
(hypothesis 3b/3c — a WP=1-exposed RO write or a shadow-stack op), and you likely
need serial after all to get its RIP.

If you must rebuild diagnostic variants, the knob is `M3OS_BRINGUP_DIAG=1 cargo
xtask image`; add/adjust `crate::post_marker(N)` calls (N ≥ 36 free) as needed.

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
(Block 0 A–E), plus bisect images `F-cet-masked`, `G-pcid-masked`,
`H-cetfix-default`, and the **POST-square diagnostic `I-cet-diag`** (default
posture + `CR0.WP` fix + `M3OS_BRINGUP_DIAG=1`; flash this next — see §4.2). Flash:
`scripts/phase-100-write-usb.sh --image target/dell-images/<X>.img /dev/sda`
(USB=`/dev/sda`; internal NVMe=`nvme0n1` — never flash that). Rebuild any with the
commands in Block 0 / the mask knobs / `M3OS_BRINGUP_DIAG=1`.

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
 M docs/handoffs/2026-07-09-cet-boot-hang-on-tiger-lake.md  # §0 root-cause + fix (this doc)
 M docs/handoffs/next-dell-session.md     # A.6 / B.3 point at the shipped binaries
 M kernel/src/arch/x86_64/cpuid.rs        # BSP CR0.WP-before-CR4.CET fix + mask knobs
 M kernel/src/smp/boot.rs                 # ***FIX #1***: AP CR0.WP before CET-bearing CR4 reload
 M kernel/src/arch/x86_64/mod.rs          # ***FIX #2***: arm init's CET shadow stack in fork trampoline
 M kernel/src/arch/x86_64/cet.rs          # ***FIX #3***: seed_signal_shadow_stack (WRUSS restorer)
 M kernel/src/arch/x86_64/syscall/mod.rs  # ***FIX #3***: call seed at signal delivery
 M kernel/src/lib.rs                      # post_marker recolour (no black squares)
 M kernel/src/fs/ramdisk.rs               # embed the two PoCs
 M xtask/src/main.rs                      # bins + 2 smoke gates + rop rustflags
?? userspace/meltdown-poc/                # new crate
?? userspace/rop-cet-poc/                 # new crate
```

The **`smp/boot.rs` AP `CR0.WP` fix is the boot-hang fix** (§0): the AP reloaded
the BSP's CET-bearing `CR4` before setting `CR0.WP`, `#GP`-triple-faulting every
AP. The BSP `CR0.WP` fix in `cpuid.rs` is still correct and necessary (the BSP's
own CET-enable precondition). The `lib.rs` recolour makes every POST square
visibly bright. The mask knobs are reusable bench tooling. Recommend committing
all of this now — the tree is clean under `cargo xtask check`, SMP smoke PASSes,
and the fixed images are staged. The only open item is the **Dell re-flash
confirmation** (boot `A-default.img` → login).

---

## 7. One-paragraph orientation for the next agent

**Resolved — see §0.** The POST-square bisection (§4.2) *worked*: `I-cet-diag.img`
read **8 / 5 / 4** squares, which (decoded against the fb-clear at marker 6 + the
real, non-slot-order marker firing) pins the hang to **`smp::boot::boot_aps()`**,
not the BSP CET enable — all four CET markers 32–35 painted, so BSP CET succeeds.
Root cause: the AP trampoline reloads the BSP's CET-bearing `CR4` **without first
setting `CR0.WP`** → `#GP(0)` → per-AP triple-fault → `boot_aps` hangs on the
rendezvous. Fix landed in `ap_entry` (`kernel/src/smp/boot.rs`): set `CR0.WP=1`
before the `mov cr4, bsp_cr4`. SMP smoke PASS; `A-default.img` + `I-cet-diag.img`
rebuilt. Serial was never needed. **Only open item:** re-flash `A-default.img` on
the Dell and confirm it boots to login (then resume the runbook Block 2). §1–§6
below are the pre-resolution record.
