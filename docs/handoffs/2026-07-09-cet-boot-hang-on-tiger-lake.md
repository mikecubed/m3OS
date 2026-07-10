# Handoff — Phase 110 B.3 CET boot hang on real silicon (Dell/Tiger Lake)

**Session:** 2026-07-09 Dell Precision 5560 (Intel Tiger Lake, has `CET_SS`).
**Runbook this executes:** [2026-07-09 Dell validation session](./2026-07-09-dell-validation-session.md).
**Branch:** `feat/phase-110-cet-shstk` (this session's fixes committed).
**Status:** ✅ **VALIDATED ON REAL SILICON — Phase 110 B.3 CET COMPLETE. → next
steps in §0.8.**
The Dell boots clean under CET to the greeter → login → compositor → terminal →
fork/exec, and the security property is **proven**: `m3ctl mitigations status`
reports `CET: enabled (user shadow stacks)`, `rop-cet-poc`'s return-address
overwrite is `#CP`-killed (**no `PWNED`**; `dmesg` shows the `#CP` kill). Also
validated this session: **userspace Spectre-v2 via eIBRS** (`Spectre-v2 (IBRS):
eIBRS enhanced … covers ring 3`, no `UNCOVERED` warning — the correct posture
after fix #4 dropped userspace retpolines) and **Meltdown correctly not-applicable**
(Tiger Lake `rdcl_no=true` → immune → KPTI off; `meltdown-poc`'s uniform-`0xdb`×16
"leak" was a stuck-slot cache artifact, now hardened to report `NO-LEAK`). Five
real-silicon CET bugs — none exercisable by QEMU (TCG models no CET) — were found
and fixed; **top next step: open the PR to `main` (§0.8).**

The Dell now boots to the **greeter, logs in, runs the compositor, the terminal,
and fork/exec (shells, pipelines) — all under active CET**. Fix #4 (§0.5) — **userspace retpolines are incompatible with
CET shadow stacks** (dropped `-Zretpoline`, eIBRS instead) — was the dominant
crash cause. Fix #5 (§0.6) — **fork must eagerly copy shadow-stack pages**, not
share them (the `ion _Fork` `#CP`). The earlier three: **(1)** the AP
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

### 0.7 ✅ VALIDATION RESULT (2026-07-09, Dell/Tiger Lake) — CET B.3 CLOSED

After all five fixes, `A-default.img` on the Dell:
- boots clean under CET → greeter → **login** → compositor → **terminal** →
  fork/exec (shells, pipelines) — no `#CP`/`#PF`/`#GP` crashes;
- `m3ctl mitigations status` → **`CET: enabled (user shadow stacks)`**;
- `rop-cet-poc` → the return-address overwrite is **`#CP`-killed** — segfaults
  with **no `ROP_CET_POC:PWNED`**, and `dmesg` shows `[int] userspace #CP (CET
  control-protection): … process killed`.

That is the full positive proof: CET user shadow stacks are live AND actively
reject a ROP-style return-address overwrite on real silicon — the exact property
QEMU (no CET model) can never demonstrate. **Phase 110 B.3 is done.**

**Spectre-v2 / eIBRS (validated).** Dropping userspace `-Zretpoline` (fix #4)
moved userspace Spectre-v2 onto **eIBRS**. Confirmed on the Dell: `m3ctl
mitigations status` → `Spectre-v2 (IBRS): eIBRS enhanced, set-once at boot —
covers ring 3 (userspace)`, and **no** `UNCOVERED` warning. So userspace indirect
branches are protected by hardware eIBRS + IBPB (the standard eIBRS-silicon
posture, matching Linux). The reporter was fixed this session to surface the IBRS
mode explicitly + warn if eIBRS is absent (fix in `m3ctl`/`spectre` reporting).

**Meltdown / A.6 (immune silicon — validated, PoC hardened).** Tiger Lake is
`rdcl_no=true`, Meltdown-immune in hardware, so `mitigations=auto` correctly
leaves **KPTI OFF** (`m3ctl` → `Meltdown: Not affected`). `meltdown-poc` initially
false-flagged `LEAK bytes=16/16` — but recovered `0xdb`×16, a **uniform** byte =
a stuck-hot cache-channel slot (untuned `CACHE_HIT_THRESHOLD` on real silicon),
NOT memory. Fixed the PoC to require a **non-uniform** recovery; it now correctly
prints `NO-LEAK (… stuck-slot cache artifact …)`. **No real Meltdown exposure.**
NB: the "leak on B / no-leak on A" A/B *cannot* be shown on this CPU — both arms
are no-leak because the silicon is immune; demonstrating a real leak needs
Meltdown-**susceptible** silicon (pre-`rdcl_no` Intel).

All session fixes are committed on `feat/phase-110-cet-shstk`; QEMU gates
(`mitigations-status-smoke`, `meltdown-poc-smoke`, `rop-cet-poc-smoke`,
`smp-smoke`, `smoke-test`) all PASS.

### 0.8 NEXT STEPS (pick-up for the next session)

1. **Open the PR** — `feat/phase-110-cet-shstk` → `main`. It's complete and
   real-silicon-validated; ~11 commits (2f5a58bc … the reporter/gate fixes).
   Nothing else blocks it.
2. **Mark Phase 110 B.3 done in the roadmap/inventory** — the AGENTS.md CET
   bullet says "dormant on QEMU / live on CET silicon"; add "validated on Tiger
   Lake (rop-cet-poc `#CP`-kill)". Update `docs/roadmap/` Phase 110 status +
   record the Dell run in `docs/handoffs/2026-07-09-dell-validation-session.md`
   (the runbook this executed).
3. **Decide the userspace-retpoline tradeoff (design call).** Fix #4 drops
   `-Zretpoline` from userspace **unconditionally** — correct on eIBRS silicon
   (Tiger Lake+), but a **non-eIBRS** CPU then has NO userspace Spectre-v2 cover
   (the `m3ctl` `UNCOVERED` warning now surfaces this; QEMU/TCG shows it too, and
   is not a security target). Options: accept it (CET deployment is eIBRS-class),
   or build two userspace variants (retpoline for non-CET, eIBRS for CET). Not
   blocking; document the decision.
4. **(Optional) Meltdown leak demo** — if a *positive* Meltdown demonstration is
   wanted, run `meltdown-poc` on Meltdown-susceptible silicon (or a QEMU/KVM CPU
   model without `rdcl_no`) and, on that box, tune `CACHE_HIT_THRESHOLD`/`TRIES`
   per the PoC header before trusting the kernel arm.
5. **CET follow-ups already noted in code** (lower priority): nested-signal
   shadow-stack handling still uses the single `Task::cet_signal_ssp` field
   (§0.3 note) — single-level delivery is correct; deep nesting would want a
   per-frame SSP or to rely purely on the WRUSS seeds.

### 0.6 fork must eagerly copy shadow-stack pages (FIXED)

After the retpoline fix the Dell **reached the greeter, logged in, and ran the
compositor** — but launching the terminal crashed. The freeze caught it:
```
*** BRINGUP_DIAG HALT: userspace #CP fault ***
pid=38 comm=ion rip=0x66a0d9 rsp=0x7fffffef9f688 err=0x1
```
`0x66a0d9` is the `ret` at the end of **`_Fork`** (musl's fork) in the `ion`
shell — `err=1` near-RET mismatch, no retpoline involved.

**Root cause — fork *shared* the shadow-stack page instead of copying it.**
`cow_clone_user_pages` CoW-marks *writable* pages (clear WRITABLE + BIT_9) and
shares *non-writable* pages verbatim. A CET shadow-stack page is deliberately
**non-writable** (WRITABLE=0, DIRTY=1) yet it *does* change (a `CALL` pushes a
return address), so it fell into the "share verbatim" path → parent and child
aliased one shadow-stack frame. CoW can't fix this either: a shadow-stack push
does **not** trap on WRITABLE=0. So the parent's post-fork `CALL`/`RET` chain
overwrote the shared frame, and when the child (or the returning parent) hit
`_Fork`'s `ret`, the shadow-stack slot no longer matched the data-stack return →
`#CP`. (The code even flagged it: *"The CoW-of-shadow-stack push interaction is a
Dell-validation item."*)

**Fix #5 (landed):** in `cow_clone_user_pages`, detect a shadow-stack leaf
(`kernel_core::cet::is_shadow_stack_pte` && `!BIT_9`) and **eagerly copy** it —
allocate a fresh frame, `copy_nonoverlapping` the parent's return-address chain,
map the child to the new frame with the same shadow-stack encoding, leave the
parent PTE untouched and unshared. Parent and child now have independent shadow
stacks. Inert on QEMU (no page carries the encoding when CET is off). `cargo
xtask check` + boot smoke-test (heavy fork/exec) PASS; images rebuilt.

> Re-flash **`A-default.img`** → the terminal (and any fork+exec: shells, `ls`,
> pipelines) should now run under CET. If a further process `#CP`s, the freeze
> names it.

### 0.5 THE userspace root cause — retpolines are incompatible with CET (FIXED)

The `I-cet-diag.img` on-screen freeze (§0.3) caught the real bug in one shot:
```
*** BRINGUP_DIAG HALT: userspace #CP fault ***
pid=4 comm=xhci rip=0x214bc4 rsp=0x7fffffefcff00 err=0x1
```
`err=0x1` = **near-RET** shadow-stack mismatch, and `0x214bc4` maps (fixed
non-PIE base `0x200000`) to the `ret` inside **`__llvm_retpoline_r11`** — the
Spectre-v2 **retpoline thunk**.

**Retpolines and CET shadow stacks are mutually exclusive.** A retpoline thunk is
`call <n>; <n>: mov %r11,(%rsp); ret` — it OVERWRITES the return address the
`call` pushed with the real indirect target (r11), then `ret`s. But the CET
**shadow stack** still holds the `call`-pushed address, so that `ret` is a
near-RET mismatch → `#CP`. Once user shadow stacks are live, **every indirect
call through a retpoline `#CP`s** — so xhci_driver, display_server, and every
other execve'd daemon that dispatches through a function pointer dies with signal
11 (init survived because its early path took no such indirect call before the
symptom). This also explains the empty boot.log (§0.4): xhci dies → no USB.

Userspace was built with `-Zretpoline` (`.cargo/config.toml [target.x86_64-m3os]`).
CET-capable silicon (Tiger Lake+) uses **eIBRS** as the hardware Spectre-v2
mitigation instead, which the kernel enables at boot — retpolines are neither
needed nor allowed there.

**Fix #4 (landed):** drop `-Zretpoline` from the userspace target (kept the
`-Zstack-protector=strong` canary); also removed it from the `rop-cet-poc`
per-crate RUSTFLAGS override (a lone retpolined binary would itself `#CP`). The
**kernel keeps** `-Zretpoline` — it has no *supervisor* shadow stack
(`IA32_S_CET.SH_STK_EN=0`), so ring-0 retpoline `ret`s are never shadow-checked;
the retpoline gate (kernel-only) still passes (2249 thunks). Verified:
`xhci_driver` now has **0** retpoline thunks + 53 plain (CET-compatible) indirect
calls; `cargo xtask check` + boot smoke-test PASS. All images rebuilt.

> This was the dominant userspace blocker. Re-flash **`A-default.img`** → expect
> display_server + the greeter to come up (indirect calls no longer `#CP`).
> Follow-up to confirm on the Dell: eIBRS is actually active (`m3ctl mitigations
> status` / the `[sec] … ibrs=` boot line) so userspace Spectre-v2 is covered.

### 0.4 BEST capture path — `boot.log` on a USB log partition (validated)

The cleanest way to read the fault on the serial-less Dell: the resident
`usb-logsink` daemon snapshots the kernel dmesg ring (`/proc/kmsg`, which
**includes every `_panic_print` fault line + RIP**) to a USB ext2 log partition
every 3 s. Pull the stick, mount it on the host, read `boot.log`.

**Recipe (validated end-to-end in QEMU — `boot.log` came out a real 28 KB file):**
```
# 1. Plain (NON-diag) graphical image — NOT the freeze build (the freeze halts
#    usb-logsink before it can snapshot). Already staged as A-default.img.
cargo xtask image
cp target/x86_64-unknown-none/release/boot-uefi-m3os.img target/dell-images/A-default.img
# 2. Splice a 128 MiB ext2 "log" partition after the ESP:
scripts/build-usb-log-image.sh --boot target/dell-images/A-default.img \
    --out target/dell-images/A-default-usb-log.img --logs-mb 128
# 3. Flash the OUTPUT (…-usb-log.img, not A-default.img) and boot the Dell.
#    Let it sit ~20-30 s in the greeter-fail loop (usb-logsink snapshots every 3 s).
# 4. Power off, pull the stick, on the host read the 2nd (ext2) partition:
sudo mount -o ro $(sudo losetup -Pf --show <stick-or-img>)p2 /mnt && cat /mnt/boot.log
#    (or, no root: dd the p2 region out and `debugfs -R "cat /boot.log" p2.ext2`)
```

**Bug found + fixed en route (`userspace/usb-logsink`):** the `[ESP]+[ext2]`
stick's blank log partition is now adopted by init as **root** (Phase 106
last-resort USB-root adoption, added *after* the Phase 96 diskless design), so
usb-logsink mounting `usb0` *again* at `/mnt/usb0` created a **dual mount of one
device → two incoherent ext2 caches → a torn inode/dirent that never committed**
(pulled stick showed a 0-byte `boot.log`). Fix: usb-logsink now probes whether
root is the writable USB volume and, if so, writes `/boot.log` on that single
existing mount; it falls back to the `/mnt/usb0` mount only on a read-only root
(the original separate-log-partition case). Re-verified: `boot.log` commits.

> Use **this** (`A-default-usb-log.img`) to capture the display_server fault as a
> file. The on-screen freeze (§0.3, `I-cet-diag.img`) remains the fallback if the
> USB storage stack itself won't come up on the Dell.

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
