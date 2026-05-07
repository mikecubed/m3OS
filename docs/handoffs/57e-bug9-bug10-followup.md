# Handoff: Bug #9 (FS-volume mutex contention) and Bug #10 (sporadic Doom GPF)

**Status:** Open follow-ups, decoupled from Phase 57e disposition.
**Source Ref:** Phase 57e Track G (Bug #9) + standalone Bug #10
**Depends on:** SMP discipline infrastructure landed in 57b/c/d/e (preempt_count counter, IrqSafeMutex F.1 wiring, the Phase 57a wake protocol, the wake-bracket race-shape closures). All retained post-deferral.
**Goal:** Land Option B Arc-clone refactor for FS-volume read paths (Bug #9 closure); confirm or rule out Bug #10 reproducibility.

> **Context — Phase 57e is deferred.** Both bugs were identified during the Phase 57e bug-fix cycle but are independent of preempt-full's disposition. The full bug-by-bug history is at [`docs/handoffs/57e-preempt-full-userspace-hangs.md`](./57e-preempt-full-userspace-hangs.md); the deferral rationale is at [`docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md`](../post-mortems/2026-05-07-57e-preempt-full-deferred.md). This handoff extracts only the parts that pertain to picking up the two remaining open issues; you do not need to read the 57e handoff to act on this one.

---

## Quick-start

* **Bug #9** is the more concrete piece of work — ~8 hours of mechanical refactoring (Option B Arc-clone across ~25 callsites) plus soak validation. Lower urgency post-deferral (see § "Post-deferral severity adjustment" below) but still a real logic bug worth closing.
* **Bug #10** is exploratory — try to reproduce via repeated Doom launches on real hardware. If it does not reproduce in 50+ launches, downgrade to "watch-list" status. If it does reproduce, the section below has the signature to look for.

If picking up only one: do **Bug #9 first**. The refactor is straightforward and the validation harness is already in place (`cargo xtask soak`). Bug #10 needs an open-ended hardware testing window which is harder to schedule.

---

## Bug #9 — `IrqSafeMutex` guard outliving `block_current_until`

### Mechanism

Any `IrqSafeMutex` whose guard outlives a call into `block_current_until` (typically via `virtio_blk::do_request`'s wait-for-completion, or any IPC / futex / wait block) leaves a `+1` in the holder's `preempt_count` after `block_current_until`'s post-resume `preempt_enable` runs. The Phase 57a wake protocol's `block_current_until` saves the task's state, switches to scheduler, and on resume calls `preempt_enable` — but the `IrqSafeMutex` guard is still alive in the caller's stack frame. The guard's own `preempt_disable` (from the lock acquire) was already paired with the wake protocol's restoration; the guard's `Drop`-time `preempt_enable` then double-decrements once the caller eventually releases the lock, leaving `preempt_count` net `+1` per blocking call inside a held guard.

The historical worst case (`FAT32_VOLUME` / `EXT2_VOLUME` held across `kernel_read_fd_at` → `virtio_blk::do_request`) accumulated this leak per disk read. With long file reads (mmap of a large binary), the count compounded into the dozens.

### Post-deferral severity adjustment

Pre-deferral, the leaked `preempt_count` suppressed kernel-mode timer preemption — `peek_preempt_count_irq` read non-zero in the timer ISR, `check_and_preempt_kernel` skipped the preempt, and the running task monopolised its core for the full syscall while co-resident Ready tasks starved (the "stale-ready" / "cpu-hog" fingerprint).

Post-deferral, there is no kernel-mode timer preemption, so the suppression no longer matters operationally. **The leak is now a pure logic bug** — `IrqSafeMutex`'s discipline contract says `preempt_count` returns to zero on guard `Drop`; a non-zero residual is still wrong, and would surface immediately if any future preemption-model phase re-introduces a `peek_preempt_count_irq`-gated path. The release-build clamp at `assert_preempt_count_zero_at_user_return` (commit `cdd0c0f`) still fires its `[preempt] count=N pid=X at user-mode return — clamping to 0 (Bug #9 mitigation)` warning when the leak occurs; the warning is the post-deferral stuck-task fingerprint.

**Urgency: medium-low.** Worth closing for discipline / future-proofing; not blocking any current functionality.

### Why Option A (type-swap) failed

The original Phase 57b authors documented the FS-volume mutexes as "safe to convert to `spin::Mutex` — pure auto-deref change for callsites" because no ISR ever reaches them. Session 15 attempted this swap (commit `9292aec`); the QEMU 10-iter soak passed cleanly. **Real-hardware testing exposed two regressions** (which is also the procedural lesson that drove the deferral):

1. **GUI input lag.** `IrqSafeMutex` does two jobs: raise `preempt_count` AND mask IRQs. Switching to `spin::Mutex` removed both. Holders of `FAT32_VOLUME` / `EXT2_VOLUME` / `TMPFS` became preemptible mid-critical-section under timer-preempt. Other cores spinning on the lock waited through the holder's preemption duration — every disk/tmpfs touch became 1–10 ms of cross-core stall.
2. **Kernel-mode GPF on Doom launch.** Faulting RIP was garbage (`0x4847474947479b46`) with ASCII "GHIJK" register pattern — stack-corruption / wild-call signature. **This became Bug #10** (see below).

Reverted in commit `962d787` (later un-reverted in `6826deb` for unrelated reasons; the FS-mutex type swap itself is no longer in the tree).

The doc-comment promise (`// Phase 57b G.3 — IrqSafeMutex inherits Track F.1's preempt-discipline ... Type swap is a pure auto-deref change for callsites.`) is **technically correct under voluntary mode** (no kernel-mode preempt windows) and was correct pre-57e. Post-57e-deferral, the type-swap argument is again technically valid but no longer useful — Option B is cleaner, addresses the real leak, and does not depend on which preemption model is active.

### Why Option B (Arc-clone) is the right path

Keep `IrqSafeMutex`'s preempt-discipline and IRQ-masking intact. **Drop the guard before the blocking call** instead of suppressing what the lock does. The pattern:

```rust
// Take the lock briefly, clone the Arc, drop guard, then call methods on the Arc.
let vol: Option<Arc<Fat32Volume>> = {
    let guard = FAT32_VOLUME.lock();   // IrqSafeMutex still — preempt_count +1, IF=0
    guard.as_ref().cloned()             // Clone the Arc, NOT the volume
};                                       // guard drops — preempt_count back to 0, IF restored
match vol {
    Some(v) => v.read_file(...),         // Now safe to block; preempt_count is 0
    None => Err(...),
}
```

When `block_current_until` runs inside `read_file`, the FS-volume guard is no longer alive, so the `+1` from the guard is no longer present in the leaked count. The lock's contract (preempt-discipline, IRQ-masking, fairness) is preserved; only the *scope* of the guard shrinks.

### Step-by-step

**Step 1 — Wrap volumes in `Arc`.** Three changes:

```rust
// kernel/src/fs/fat32.rs:202
pub static FAT32_VOLUME: IrqSafeMutex<Option<Arc<Fat32Volume>>> = IrqSafeMutex::new(None);

// kernel/src/fs/ext2.rs:69
pub static EXT2_VOLUME: IrqSafeMutex<Option<Arc<Ext2Volume>>> = IrqSafeMutex::new(None);
```

The `Option<Volume>` → `Option<Arc<Volume>>` lift is mechanical. `Fat32Volume` is `Send + Sync` (only contains `u64` fields and a copyable `Fat32Bpb`). `Ext2Volume` already contains an inner `IrqSafeMutex<BTreeMap<u32, Vec<u8>>>` `block_cache`, so it's also `Send + Sync` — no field changes needed.

The `mount_fat32` and `mount_ext2` paths that construct the volume need:

```rust
*FAT32_VOLUME.lock() = Some(Arc::new(volume));
```

**Step 2 — Refactor read sites in `kernel_read_fd_at`** (`kernel/src/arch/x86_64/syscall/mod.rs:8387-8465`). Two callsites:

```rust
FdBackend::Fat32Disk { start_cluster, file_size, .. } => {
    let start_cluster = *start_cluster;
    let file_size = *file_size;
    if start_cluster < 2 || offset >= file_size as usize {
        return Ok(0);
    }
    let vol = {
        let guard = crate::fs::fat32::FAT32_VOLUME.lock();
        guard.as_ref().cloned()
    };
    match vol {
        Some(v) => v.read_file(start_cluster, file_size, offset, buf)
            .map_err(|_| NEG_EIO as i64),
        None => Err(NEG_EIO as i64),
    }
}

FdBackend::Ext2Disk { inode_num, .. } => {
    let inode_num = *inode_num;
    let vol = {
        let guard = crate::fs::ext2::EXT2_VOLUME.lock();
        guard.as_ref().cloned()
    };
    match vol {
        Some(v) => match v.read_inode(inode_num) {
            Ok(inode) => v.read_file_data(&inode, offset as u64, buf)
                .map_err(|_| NEG_EIO as i64),
            Err(_) => Err(NEG_EIO as i64),
        },
        None => Err(NEG_EIO as i64),
    }
}
```

**Step 3 — Audit and refactor every other `FAT32_VOLUME.lock()` / `EXT2_VOLUME.lock()` callsite** in `kernel/src/arch/x86_64/syscall/mod.rs`. Session 15's grep found ~25 sites at (approximate, may have shifted with subsequent commits — verify with fresh grep): lines 305, 329, 356, 378, 670, 685, 758, 767, 778, 823, 830, 841, 3674, 5328, 5389, 5785, 5859, 6285, 6296, 6320, plus the `kernel_read_fd_at` sites.

Each site needs the same lock-then-clone-then-drop treatment for **read paths**.

**Write paths** (`set_fat32_meta_and_save`, `vol.write_file`, `vol.update_dir_entry`) can stay as-is for now since writes are rare and the existing serialisation is correct (the original concern was read-while-virtio-blk-blocks). Consider revisiting once the read-path soak is clean and any residual `[preempt] count=N at user-mode return` warnings have been characterised.

Order of operations: do read paths first (closes the dominant Bug #9 contributor), validate via soak, then optionally clean up write paths if any residual fingerprints remain.

**Step 4 — Refactor `Ext2Volume.block_cache` access.** The cache miss path in `kernel/src/fs/ext2.rs:138-170` already does the right thing (allocate-outside-lock, take-lock-only-to-insert). Audit `read_block` and `read_block_into_dst` to confirm no path holds the cache lock across `crate::blk::read_sectors`. Likely already correct; verify and document.

**Step 5 — TMPFS** does not need changes for Bug #9 closure. TMPFS is in-memory; its `read_file` / `write_file` paths do NOT call `block_current_until` (no virtio_blk involvement). TMPFS holders never block while held, so no leak path exists. Leave `TMPFS: IrqSafeMutex<Tmpfs>` as-is.

**Step 6 — `FAT32_PERMISSIONS`** is similar to TMPFS — purely in-memory `BTreeMap<String, Fat32FileMeta>`, no I/O while held. Leave as `IrqSafeMutex`.

### Validation

Build:

```bash
cargo xtask check
```

Acceptance bar:

* `cargo xtask check` — clean (clippy + rustfmt + kernel-core host tests + driver_runtime host tests).
* `cargo xtask test` — all kernel-in-QEMU tests pass.
* `cargo xtask smoke-test` — clean.
* `cargo xtask soak --duration 30m --max-runs 10` — passes 10/10 attempt-1 with **zero** `[preempt] count=N pid=X at user-mode return — clamping to 0 (Bug #9 mitigation)` warnings across all 10 runs.
* (If 30m × 10 runs is clean) Promote to 24-hour soak: `cargo xtask soak`. Default 24 h. Acceptance: zero `[preempt] count=` warnings across the entire window.
* Real-hardware GUI smoke (mandatory acceptance gate post-deferral, see post-mortem § Lessons learned):
  1. Boot on `omarchy` (write image, or `cargo xtask run-gui` if QEMU exposes the same workload).
  2. Login.
  3. Open a few apps; type at the prompt; move the mouse — confirm no input lag (matches voluntary baseline).
  4. Run `fb-takeover doom`. Doom must launch and run; no GPF, no kernel page fault. (This is also the Bug #10 reproduction window — see § Bug #10.)
  5. Exit Doom; framebuffer must restore cleanly.

Note: The `cargo xtask soak` harness emits `target/soak/run-<ts>/soak-result.md`. The `[preempt] count=` regex is the Bug #9 closure signal post-deferral; the `[sched] stale-ready` and `[sched] cpu-hog` fingerprints are no longer reliable indicators because timer-driven kernel-mode preemption is gone (those fingerprints relied on the IRQ-side preempt suppression that the leak triggered).

### Files to pre-load for the next session

* `kernel/src/fs/fat32.rs:198-202` — `FAT32_VOLUME` definition (lift to `Arc<Fat32Volume>`); also `kernel/src/fs/fat32.rs:30-37` for `FAT32_PERMISSIONS` (leave as-is, but useful for context).
* `kernel/src/fs/ext2.rs:55-69` — `EXT2_VOLUME` plus internal `block_cache`.
* `kernel/src/fs/tmpfs.rs:24-37` — `TMPFS` (leave as-is, for context).
* `kernel/src/arch/x86_64/syscall/mod.rs:8387-8465` — `kernel_read_fd_at` (the dominant read path).
* `kernel/src/arch/x86_64/syscall/mod.rs:305-841` — clustered read/write callsites.
* `kernel/src/blk/virtio_blk.rs:881-913` — `do_request` (the actual `block_current_until` site that creates the leak window — for context, no changes needed here).
* `kernel/src/task/scheduler.rs:1610-1900` — `preempt_disable` / `preempt_enable` / `peek_preempt_count_irq` / `assert_preempt_count_zero_at_user_return` (clamp + warning location).

### Acceptance criteria

1. `cargo xtask check` green.
2. `cargo xtask test` green.
3. `cargo xtask smoke-test` clean.
4. 10-iteration `cargo xtask soak --duration 30m --max-runs 10` — zero `[preempt] count=` warnings across all runs.
5. Real-hardware GUI smoke clean (mouse / keyboard responsive, Doom launches and exits cleanly).
6. (Optional but encouraged) 24-hour `cargo xtask soak` — zero zero-tolerance fingerprints.

When (1)–(5) pass, Bug #9 closes. The "Track G's 24 h soak gate" framing from the original 57e doc is no longer applicable because there's no preempt-full mode to validate.

---

## Bug #10 — sporadic kernel-mode GPF on Doom launch

### Single observation

Captured in the Session 15 m3os.log (during the `9292aec` `spin::Mutex` experiment, which was reverted). Faulting RIP `0x4847474947479b46`. Register pattern showed sequential ASCII bytes "GHIJK" — the hallmark of a stack-corruption / wild-call signature. Hit core 3's scheduler dispatch loop after Doom mapped its framebuffer and emitted two PTY lines.

### Why we don't know the cause

* **Did not reproduce on the second Doom launch** in the same session.
* **Did not reproduce in any subsequent hardware testing** under either `9292aec` or post-revert `962d787`.
* Without a reproducer, the cause is undiagnosable from the single capture.

### Two plausible explanations

1. **`9292aec`-specific.** A holder of a now-`spin::Mutex` got preempted mid-critical-section under preempt-full's 1 ms timer (the same regression that drove Bug #9's Option A failure). Some downstream invariant that depended on "I run to completion atomically on this core" broke. **Closed by the `9292aec` revert.** Post-deferral, this scenario cannot recur because (a) the FS mutexes are still `IrqSafeMutex`, and (b) timer-driven kernel-mode preemption is gone.
2. **Independent latent stack-corruption / wild-call.** The corruption pattern (garbage RIP, sequential-byte registers, scheduler dispatch loop) suggests stack-overflow or wild-call somewhere in the Doom / fb-takeover / framebuffer_mmap path. The framebuffer mmap path (`37b9d9c`'s `sys_mmap_file_backed` refactor) is the most recent code change in that area. If this is the cause, the GPF could reproduce again.

### How to confirm or close

The only way to make progress is **repeated real-hardware testing**. Recommended protocol:

1. Boot under voluntary mode (the only mode now). Login.
2. Run `fb-takeover doom` 50+ times across multiple boots. Capture m3os.log every time.
3. After each Doom session: exit cleanly (Esc, then `q` in the menu, or kill the process if hung).

Acceptance for **closing Bug #10**:
* If 50+ Doom launches clean across at least 3 separate boots, downgrade to "watch-list" status. Disposition: closed-as-not-reproduced, watch for return.
* If ≥1 GPF reproduces with the `0x4847474947479b46` / "GHIJK" signature (or any garbage-RIP scheduler-loop GPF), capture the m3os.log and the faulting register state. Triage targets in priority order:
  1. **`framebuffer_mmap` path** in `kernel/src/arch/x86_64/syscall/mod.rs` (around `sys_mmap_file_backed`). Most recently touched (Phase 57e Bug #9 Step 1, commit `37b9d9c`).
  2. **`fb-takeover doom`'s mode transition** — the userspace `fb-takeover` binary in `userspace/fb-takeover/`.
  3. **Doom's PTY writes** during launch — the first two PTY lines that appear on serial were correlated with the GPF in the original capture.
  4. **Scheduler dispatch loop on core 3** — the GPF hit this loop. Look for any path that could write past a stack bound or call through a stale function pointer.

### What the GPF capture should include

* Faulting RIP (`0x...`).
* RSP at the time of fault.
* Full register dump (look especially for sequential ASCII patterns indicating data-as-instructions corruption).
* The core ID (Bug #10 hit core 3).
* The serial output immediately before the GPF (PTY lines, compose#, etc.).
* If reproducible: try with `M3OS_KERNEL_FEATURES="sched-trace"` to capture the full state-transition log around the GPF.

### Files to pre-load when investigating

* `kernel/src/arch/x86_64/syscall/mod.rs` — `sys_mmap_file_backed` (around the framebuffer mmap path).
* `userspace/fb-takeover/src/main.rs` — the userspace caller that launches Doom in fullscreen-takeover mode.
* `userspace/doom/src/main.rs` and `userspace/doom/dg_m3os.c` — Doom's m3os adaptation layer.
* `kernel/src/task/scheduler.rs` — the dispatch loop (around line 4200+; look for `dispatch_preempted_and_resume` and the surrounding switch_context / pick_next path).
* `kernel/src/fb/` — framebuffer ownership / takeover path.

### Status

**Open, low priority, not blocking GUI use.** Doom is reliably launchable on real hardware in subsequent attempts. The original capture is preserved in the Session 15 m3os.log capture chain (referenced from the 57e handoff doc); if it's been overwritten by subsequent runs, the `0x4847474947479b46` / "GHIJK" signature is the search query for any future capture.

---

## Validation harness reference

Both bugs use the same validation surface:

| Command | What |
|---|---|
| `cargo xtask check` | clippy + rustfmt + host tests; pre-commit gate |
| `cargo xtask test` | full QEMU-based kernel test suite |
| `cargo xtask smoke-test` | quick boot + service-up smoke |
| `cargo xtask soak --duration 30m --max-runs 10` | 10-iteration soak with structured grep result |
| `cargo xtask soak` | default 24-hour soak |
| `cargo xtask run-gui` | QEMU GUI boot for interactive testing (limited fidelity vs hardware) |
| `cargo xtask run-gui --fresh` | same, but recreate disk first |

Real-hardware testing requires writing the disk image to a USB stick or otherwise booting `omarchy` (or the equivalent test machine). QEMU TCG is **not** sufficient — Phase 57e's central procedural lesson was that TCG masked Bugs #11 and #12 for the entire QEMU-only soak window. See the post-mortem § Lessons Learned for the procedural detail.

---

## What's NOT in scope for this handoff

* **Phase 57e — full kernel preemption.** Deferred. See post-mortem.
* **`preempt-full` Cargo feature.** Retired in the cleanup. Do not re-introduce as part of Bug #9 / #10 work.
* **`cond_resched`-style explicit yield points** (Linux PREEMPT_VOLUNTARY mechanism). Future work, separate phase, not blocked on these bugs and these bugs are not blocked on it.
* **Track G.1.b 24 h soak under preempt-full.** Original acceptance gate; replaced post-deferral with the validation list above. The 24 h soak is still a useful endurance test under voluntary mode but the `[sched] stale-ready` / `[sched] cpu-hog` fingerprints it was originally tuned for are no longer reliable Bug #9 indicators (the IRQ-side preempt suppression those fingerprints relied on doesn't exist anymore). Use `[preempt] count=N at user-mode return` warnings as the Bug #9 signal instead.

---

## Related

* [`docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md`](../post-mortems/2026-05-07-57e-preempt-full-deferred.md) — Phase 57e deferral disposition.
* [`docs/handoffs/57e-preempt-full-userspace-hangs.md`](./57e-preempt-full-userspace-hangs.md) — 18-session bug log; full Bug #9 mechanism analysis is in § Session 15; Bug #10 single-observation capture context is in § Session 15.
* [`docs/roadmap/57e-full-kernel-preemption.md`](../roadmap/57e-full-kernel-preemption.md) — phase doc with Outcome and Learning Outcomes sections.
* [`docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md`](../roadmap/tasks/57e-full-kernel-preemption-tasks.md) — track-level disposition. Track G is decoupled from 57e and tracked here.
