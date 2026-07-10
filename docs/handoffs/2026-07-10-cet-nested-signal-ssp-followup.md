# Handoff — Phase 110 B.3 CET nested-signal shadow-stack `#CP` (Dell/Tiger Lake)

**Session:** 2026-07-10 Dell Precision 5560 (Intel Tiger Lake, `CET_SS`).
**Runbook:** [2026-07-09 Dell validation session](./2026-07-09-dell-validation-session.md), Block 4b.
**Depends on:** [2026-07-09 CET bring-up — RESOLVED](./2026-07-09-cet-boot-hang-on-tiger-lake.md)
(Fix #3 seeds the signal-delivery shadow stack; §0.3 "Known follow-up" + §0.8
step 5 flagged *this exact* nesting limitation as deferred).
**Status:** 🟢 **FIX VALIDATED ON REAL SILICON (2026-07-10, run 2).** After a
clean re-flash, `/bin/nested-sig-cet-poc` **PASSES** on the Dell (image C, CET
active) — no `#CP`. The RDSSP-based fix on `fix/cet-nested-signal-ssp` → **PR
#328** (source the user SSP from the live `RDSSP` register, MSR fallback, at the
seed + `sigreturn`; drop the single-slot `Task::cet_signal_ssp`) is correct on HW.
**The earlier "identical failures" (run 1/1b) were a stale image booting** — the
flash wasn't landing on the USB the Dell actually booted (the `#CP` line lacked
the `[CET-DIAG]` marker, proving an un-instrumented old kernel was running). A
clean re-flash fixed that and the PoC passed. **Remaining close-out:** (a) HW
regression sweep — `fork-cet-poc` PASS + `rop-cet-poc` `#CP`-kill on the same
image; (b) revert the TEMP `[CET-DIAG]` instrumentation (commit `12338eb2`) and
rebuild the clean image; (c) the independent perf A/B.
PoC: `/bin/nested-sig-cet-poc` (`userspace/nested-sig-cet-poc`).

---

## TL;DR

The single-slot `Task::cet_signal_ssp` — correct for one level of signal
delivery — does not survive a **nested** signal. On the Dell,
`/bin/nested-sig-cet-poc` (an outer `SIGUSR1` handler that self-raises `SIGUSR2`,
both `sigreturn`) is `#CP`-killed on the **nested handler's own `ret`**. This is
the one CET risk the bring-up session explicitly deferred; it is now confirmed
with a captured fault and needs the per-frame-SSP / `RSTORSSP`-token redesign.
QEMU can never show it (TCG models no CET).

## The captured fault (run 2, 2026-07-10, Dell/Tiger Lake)

```
[int] userspace #CP (CET control-protection): pid=45 rip=0x2014b3 rsp=0x7ffffefc0438 err=0x1 (shadow-stack/CFI violation — return-address overwrite) — process killed
```

- `err=0x1` = **near-`RET`** shadow-stack mismatch (data-stack return address ≠
  shadow-stack top). The "return-address overwrite" wording in the message is the
  handler's generic `#CP` text — here it is a legitimate handler→restorer `ret`,
  not an attack.
- `rip=0x2014b3` maps — static non-PIE base `0x200000`, confirmed with
  `readelf -l` (`R E` LOAD at vaddr `0x201410`) — to the **final `ret` of
  `inner_handler`**, the *nested* `SIGUSR2` handler:

```
0000000000201480 <nested_sig_cet_poc::inner_handler>:
  201480: mov $0x1,%eax ; … ; syscall        # write_str("inner-entered")
  201498: lock orl $0x2,STAGE(%rip)           # STAGE |= inner-entered
  2014a0: mov $0x1,%eax ; … ; syscall         # write_str("inner-returning")
  2014b3: c3   ret                            # ← #CP HERE (returns to the restorer)
```

So the fault is the **nested handler's `ret` to `__syscall_lib_sigrestorer`**
(`0x201410` → `rt_sigreturn`), *not* the outer handler's return. That localizes
the defect to the **second (nested) delivery's** shadow-stack setup, not the
outer unwind. (The PoC even printed `outer-entered → inner-entered →
inner-returning` before the kill, confirming true nesting.)

## Reproduce

1. Boot **image C** (`M3OS_MITIGATIONS=full`) on Tiger Lake (CET active).
2. Run `/bin/nested-sig-cet-poc`. Broken behavior: prints
   `NESTED_SIG_POC:outer-entered`, `:inner-entered`, `:inner-returning`, then the
   kernel `#CP` kill above — no `NESTED_SIG_POC:after`, no `:PASS`.
3. Not reproducible under QEMU (no CET model).

## Root cause

Signal delivery seeds the user shadow stack with the handler's restorer via
`WRUSS` and stashes the interrupted SSP in the **single** `Task::cet_signal_ssp`
slot (bring-up Fix #3): `kernel/src/arch/x86_64/cet.rs::seed_signal_shadow_stack`
+ `deliver_user_signal` in `kernel/src/arch/x86_64/syscall/mod.rs`. A **second**
signal taken *inside* a running handler reuses that one slot and re-seeds
relative to the current (outer-handler) SSP; the nested handler's `ret` then
finds a shadow-stack top that does not match its restorer → `#CP`. The design is
correct for exactly one level of delivery, as the bring-up handoff called out
(§0.3 note, §0.8 step 5).

## Fix (design)

Replace the single-slot save/restore with **per-delivery shadow-stack restore
tokens**, so each (possibly nested) signal frame carries its own SSP:

- On delivery, push an `RSTORSSP` restore token onto the shadow stack per signal
  instead of stashing one SSP in `cet_signal_ssp`. `WR_SHSTK_EN` is already on,
  so `WRUSS`/`WRSS` can seed the token; the codec is modeled in
  `kernel_core::cet::shadow_stack_restore_token`.
- On `sigreturn`, `RSTORSSP` the token for the frame being unwound rather than
  reloading the single slot.
- **Touch points:** `kernel/src/arch/x86_64/cet.rs` (seed + token), signal
  delivery + `sys_rt_sigreturn` in `kernel/src/arch/x86_64/syscall/mod.rs`, and
  `Task::cet_signal_ssp` (→ per-frame token, or drop it in favor of a pure
  token-on-shadow-stack scheme).

Keep the whole path `cet_active`-gated so it stays inert on QEMU.

## Dell re-validation — run 1 (2026-07-10): STILL FAILS, diagnosing

The RDSSP-based fix (below) did **not** resolve it: `/bin/nested-sig-cet-poc`
still dies at the inner handler's `ret`, an **identical** signature to pre-fix —
`outer-entered → inner-entered → inner-returning`, then a SIGSEGV/`#CP` kill
(`ion: process (44) ended by signal SIGSEGV`), no `outer-resumed`/`after`. Two
possibilities: (a) the pre-fix image was booted (the fix never ran), or (b) the
root-cause model is wrong. First-principles analysis kept concluding the inner
`ret` should *match* once seeded relative to the live SSP, so the observed
inner-`ret` fault points at something not yet understood.

**Action: instrumented for one-shot ground truth** (commit `12338eb2`, TEMP,
`M3OS_MITIGATIONS=full` → staged `target/dell-images/C-mitigations-full-nestdiag.img`,
sha256 `bc141840…`). `[CET-DIAG]` kernel logs, retrievable via `dmesg`:
- per signal delivery: `deliver sig=… pid=… handler=… restorer=… frame_rsp=…`
  then `seed rdssp=… msr=… live=… ret=…` and `seed new_ssp=…` (or `seed SKIPPED`).
- the `#CP` handler line now carries `[CET-DIAG faulting_ssp(msr)=… rdssp=…]` —
  the SSP at the mismatching `RET` (the IDT entry saved it into `IA32_PL3_SSP`).

From one run this shows whether the fix code is live (`[CET-DIAG]` present),
whether `RDSSP` returns a sane SSP or `0` (always-fallback), whether the seed
skipped, and whether the inner seed's `new_ssp` matches the faulting SSP.
**Next: flash the `nestdiag` image, run the PoC, `dmesg | grep CET-DIAG`, report.**
The TEMP instrumentation is reverted once the true cause is found.

## Dell re-validation — runs 1/1b → run 2 (2026-07-10): stale flash → clean re-flash PASSES

Runs 1 and 1b `#CP`-killed at the inner `ret` with a signature **identical to
pre-fix** — and the run-1b `#CP` line (`pid=62 rip=0x2014b3 … err=0x1`) carried
**no `[CET-DIAG faulting_ssp(msr)=…]` marker**. Cross-checked on the build host:
the diagnostic kernel + `C-mitigations-full-nestdiag.img` contain the `CET-DIAG`
strings (3 occurrences); pre-fix `C-mitigations-full.img` and `-nestfix.img`
contain **0**. So an **un-instrumented old kernel was booting** — the flash wasn't
landing on the USB the Dell actually booted (not the internal NVMe, which the
operator ruled out; flashing was done from this build host). **A clean re-flash
resolved it: run 2 → `NESTED_SIG_POC:PASS`, no `#CP`.**

**Lesson for next time — verify the running kernel before trusting a result.**
`nested-sig-cet-poc; dmesg | grep CET-DIAG` on the diagnostic image must print
`[CET-DIAG]` lines; an **empty grep ⇒ a stale image is booting** (re-flash /
re-check the boot device) — don't read the fault until `[CET-DIAG]` shows.

## Fix landed (2026-07-10, PR #328)

The implemented fix is **simpler and more fundamental than the RSTORSSP-token
design above** — it removes the need for any per-frame kernel state at all — and
it matches the bring-up handoff's own recommendation
([§0.2](./2026-07-09-cet-boot-hang-on-tiger-lake.md): *"a robust nested design
would drop the explicit SSP restore and rely purely on the seeds"*).

**The deeper root cause — a stale MSR on `SYSCALL`.** Signal delivery read the
live SSP from the `IA32_PL3_SSP` **MSR**. But `SYSCALL` (unlike an IDT
interrupt/exception entry) does **not** save the live user SSP into that MSR:
with no supervisor shadow stack to switch to, the `SSP` register is left holding
the live user value and the MSR stays stale. A nested signal is delivered on the
*outer* handler's own `kill()` **syscall**, so the seed read a stale MSR (the
value the outer *delivery* left) — mis-seeding the nested restorer — and the
single `cet_signal_ssp` slot compounded it by clobbering the outer frame's saved
SSP. (The original "single-slot clobber" framing above is one half; the stale
`SYSCALL` MSR is the other, and is why the nested handler's own `ret` faults.)

**What landed:**
- **`kernel_core::cet::select_live_ssp(rdssp, msr)`** — host-tested source policy:
  `RDSSP` is authoritative when non-zero (the `SYSCALL` path, register live);
  zero `RDSSP` means an IDT entry zeroed the register and saved the live SSP to
  the MSR, so the MSR wins. Two new host tests.
- **`kernel/src/arch/x86_64/cet.rs`** — `read_ssp_reg` (`rdsspq` as raw bytes,
  `F3 48 0F 1E C8`), `live_user_ssp` (register-first, MSR fallback),
  `restore_signal_ssp` (re-sync the MSR from the live SSP at `sigreturn`).
  `seed_signal_shadow_stack` now reads `live_user_ssp` instead of the raw MSR.
- **`kernel/src/arch/x86_64/syscall/mod.rs`** — dropped the save-at-delivery call;
  `sigreturn` now calls `cet::restore_signal_ssp()`.
- **Removed** `Task::cet_signal_ssp` and the two scheduler helpers
  (`save_/restore_current_task_signal_ssp`).
- **QEMU gate** `nested-sig-cet-poc-smoke` (xtask + pre-push `M3OS_SEC_POC_REGRESSION`).

**Why it is correct for arbitrary nesting:** the handler's own final `RET` pops
the `WRUSS`-seeded restorer slot, leaving the live `SSP` register at exactly the
interrupted context's value; `restore_signal_ssp` copies that live value into the
MSR that `IRETQ` reloads from. Every (possibly nested) frame recovers its SSP
straight from hardware — there is no clobberable per-task slot. `RSTORSSP` tokens
(the codec still lives in `kernel_core::cet::shadow_stack_restore_token`) were not
needed.

**Scope note (separate latent item, not this bug).** The scheduler's
`save_task_ssp`/`restore_task_ssp` still read/write `IA32_PL3_SSP` via the MSR
(`read_task_ssp_live`). A *timer* preemption freshens the MSR through the IDT save
so a context switch inside a handler is fine; but a **voluntary** (blocking-
syscall) switch *inside a signal handler* would save a stale MSR. This is
pre-existing, unchanged by this fix, rare (a handler doing a blocking syscall),
and not exercised by any PoC. If ever chased, `save_task_ssp` should adopt the
same `live_user_ssp()` (`RDSSP`-first) source — deferred as out of scope here.

## Acceptance

- On the Dell (image C): `/bin/nested-sig-cet-poc` → `NESTED_SIG_POC:PASS` (both
  handlers enter and return, no `#CP`). **← the one remaining step (bench-only).**
- ✅ QEMU **run-to-completion** gate for the delivery/`sigreturn` logic
  (`nested-sig-cet-poc-smoke`) — the nesting + `sigreturn` control flow works
  even without CET, so the non-CET half is regression-covered in CI; the
  `#CP`-reject half stays bench-only. **Done** (PR #328).

---

## Next session — start here

**Branch / PR.** The fix is on `fix/cet-nested-signal-ssp` (pushed) → **PR #328**
(open, base `main`), branched off `feat/dell-cet-stress-pocs` (PR #327 — the three
PoC binaries + run-2 docs). Both are open; merge #327 then #328 (or #328 direct to
`main` — it carries the kernel fix + the new gate). See *Fix landed* above for the
full change list.

**Only remaining task — re-validate on the Dell (bench-only; QEMU can't).**

**Fixed image C is BUILT + STAGED** (2026-07-10, off `fix/cet-nested-signal-ssp`
@ `fe31d1be`): `target/dell-images/C-mitigations-full-nestfix.img`
(sha256 `6fdc5787e4154fe13d192c0446df7b51c0a03d7fa94a24599975b4639192dbc5`;
distinct from the pre-fix `C-mitigations-full.img` @ `5c2340ef…`). No rebuild
needed — flash this file.

1. **Flash** (USB = `/dev/sda` on the Dell; the NVMe system disk is `nvme0n1` —
   **never** flash that). The script refuses partitions / the root disk / non-
   removable disks and asks to confirm:
   ```
   scripts/phase-100-write-usb.sh --image target/dell-images/C-mitigations-full-nestfix.img /dev/sda
   ```
   (Direct fallback: `sudo dd if=target/dell-images/C-mitigations-full-nestfix.img of=/dev/sda bs=4M conv=fsync status=progress && sync`.)
2. Boot the Dell, log in, run `/bin/nested-sig-cet-poc` → **expect
   `NESTED_SIG_POC:PASS`** (no `#CP`). The pre-fix build `#CP`-killed the nested
   handler's `ret` (`pid=45 rip=0x2014b3`); the fix should make it a clean PASS.
   Read the line ordering (`outer-entered → inner-entered → inner-returning →
   outer-resumed → after → PASS`) to confirm true nesting.
3. Regression-check the other CET PoCs on the same image: `/bin/fork-cet-poc` must
   stay `FORK_CET_POC:PASS`, `rop-cet-poc` must still `#CP`-kill (no `PWNED`), and
   a normal login/compositor/terminal session must still work (single-level signal
   delivery unchanged). If any faults, the freeze dumper (`M3OS_BRINGUP_DIAG=1`)
   names the pid/RIP.
4. **Record the result here** (promote status 🟡 → ✅ on PASS, or reopen with the
   captured fault on `#CP`).

**Independent open item — Block 3 perf A/B.** Needs the `off` baseline: build
image B (`M3OS_MITIGATIONS=off cargo xtask image`), run `/bin/perf-bench` on it,
compare `ns_off` to the captured image-C `ns_per_syscall=6128` against the ≤30 %
bound. Tracked in `next-dell-session.md` (A.5 perf box). Not affected by this fix.

**Everything else Phase 110 is validated** (run 2, checked off in
`next-dell-session.md`): A.5 PCID live, B.3 CET live, B.3 ROP `#CP`-kill, A.6
immune-silicon, and 4a fork-CoW.

### Orientation for a fresh agent
Phase 110 Dell/Tiger Lake validation is essentially complete. Run 2 confirmed
KPTI + PCID + CET all live and the ROP / fork-CoW defenses working; the one
remaining correctness gap — **this** nested-signal shadow-stack bug — now has a
**landed fix (PR #328)**. Root cause was two-fold: the single-slot
`cet_signal_ssp` clobber *and* a stale `IA32_PL3_SSP` MSR on `SYSCALL`-triggered
(nested) delivery; the fix reads the live SSP register (`RDSSP`) instead, at both
the seed and `sigreturn`, and drops the slot. It is host-tested + QEMU-gated, but
the `#CP`-reject property is bare-metal-only — so the **only** open work is the
Dell re-flash confirmation above. The bench PoCs live in `userspace/` (PR #327);
the perf A/B is a quick independent close-out.
