# Handoff — Phase 110 B.3 CET nested-signal shadow-stack `#CP` (Dell/Tiger Lake)

**Session:** 2026-07-10 Dell Precision 5560 (Intel Tiger Lake, `CET_SS`).
**Runbook:** [2026-07-09 Dell validation session](./2026-07-09-dell-validation-session.md), Block 4b.
**Depends on:** [2026-07-09 CET bring-up — RESOLVED](./2026-07-09-cet-boot-hang-on-tiger-lake.md)
(Fix #3 seeds the signal-delivery shadow stack; §0.3 "Known follow-up" + §0.8
step 5 flagged *this exact* nesting limitation as deferred).
**Status:** 🔴 **CONFIRMED ON REAL SILICON — open bug.** A nested signal under
CET `#CP`-kills the process. Fix = per-frame / token-based shadow-stack restore
for signal delivery. PoC: `/bin/nested-sig-cet-poc` (`userspace/nested-sig-cet-poc`).
**Next-session pick-up:** see the final section, *Next session — start here*.

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

## Acceptance

- On the Dell (image C): `/bin/nested-sig-cet-poc` → `NESTED_SIG_POC:PASS` (both
  handlers enter and return, no `#CP`).
- Add a QEMU **run-to-completion** gate for the delivery/`sigreturn` logic (the
  nesting + `sigreturn` control flow works even without CET), so the non-CET half
  is regression-covered in CI; the `#CP`-reject half stays bench-only.

---

## Next session — start here

**Branch / PR.** All of run 2 is on `feat/dell-cet-stress-pocs` (pushed) →
**PR #327** (open, base `main`): the three PoC binaries (`fork-cet-poc`,
`nested-sig-cet-poc`, `perf-bench`) + the run-2 validation doc sync, and this
follow-up. Merge #327 whenever; the fix below can branch off it, or off `main`
after merge.

**Primary task — fix the nested-signal `#CP` (this doc's bug).** Implement the
per-frame / `RSTORSSP`-token shadow-stack restore (see **Fix (design)** above).
Order of work:
1. `kernel/src/arch/x86_64/cet.rs` — add the per-delivery restore-token seed
   (codec in `kernel_core::cet::shadow_stack_restore_token`); host-test the codec.
2. `deliver_user_signal` + `sys_rt_sigreturn` in
   `kernel/src/arch/x86_64/syscall/mod.rs` — push a token per delivery and
   `RSTORSSP` the right token on `sigreturn`; retire the single
   `Task::cet_signal_ssp` slot (or make it per-frame). Keep it all
   `cet_active`-gated (inert on QEMU).
3. `cargo xtask check` + `mitigations-status-smoke` + `smp-smoke` + `smoke-test`
   green; add a QEMU run-to-completion gate for `nested-sig-cet-poc` (nesting +
   `sigreturn` work without CET, so CI can cover the control-flow half).
4. **Re-validate on the Dell:** rebuild image C (`M3OS_MITIGATIONS=full cargo
   xtask image`), flash `/dev/sda`, run `/bin/nested-sig-cet-poc` → expect
   `NESTED_SIG_POC:PASS` (no `#CP`); re-run `/bin/fork-cet-poc` (must stay
   `FORK_CET_POC:PASS`).

**Independent open item — Block 3 perf A/B.** Needs the `off` baseline: build
image B (`M3OS_MITIGATIONS=off cargo xtask image`), run `/bin/perf-bench` on it,
compare `ns_off` to the captured image-C `ns_per_syscall=6128` against the ≤30 %
bound. Tracked in `next-dell-session.md` (A.5 perf box).

**Everything else Phase 110 is validated** (run 2, checked off in
`next-dell-session.md`): A.5 PCID live, B.3 CET live, B.3 ROP `#CP`-kill, A.6
immune-silicon, and 4a fork-CoW.

### Orientation for a fresh agent
Phase 110 Dell/Tiger Lake validation is essentially complete. Run 2 confirmed
KPTI + PCID + CET all live and the ROP / fork-CoW defenses working; the one
remaining correctness gap is **this** nested-signal shadow-stack bug (single-slot
`cet_signal_ssp`), captured with a real fault (`pid=45 rip=0x2014b3` — the nested
handler's `ret`). The bench PoCs live in `userspace/` and ship on PR #327. Start
with the Primary task above; the perf A/B is a quick independent close-out.
