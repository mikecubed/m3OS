# PR 131 Phase 57b-57e Roadmap Fourth Review

**Review date:** 2026-04-29
**Branch:** `docs/phase-57b-preemption-plan`
**PR:** 131, `docs: split Phase 57b kernel preemption into 57b/57c/57d/57e`
**Commit reviewed:** `7cc6975` (`docs: address PR-131 third review (lifecycle IF-window, ring-0 frame normalization)`)
**Prior reviews:**
- `docs/appendix/reviews/pr-131-phase-57b-57e-roadmap-review.md`
- `docs/appendix/reviews/pr-131-phase-57b-57e-roadmap-rereview.md`
- `docs/appendix/reviews/pr-131-phase-57b-57e-roadmap-third-review.md`

## Summary

The latest commit fixes most of the third-review issues:

- 57b now explicitly says switch-out and switch-in pointer retargets use their own `cli` windows and do not inherit IF=0 from `switch_context`.
- 57d now owns ring-0 frame normalization instead of deferring the existence of `rsp` / `ss` slots to 57e.
- 57d Track B.4 now uses the local `x86_64` raw `set_handler_addr` shape.
- The README 57e row now uses per-trigger latency wording.

I still would not treat the roadmap as ready implementation guidance. The new 57d asm sketch is internally inconsistent: the ring-0 synthetic slots are inserted below the CPU-pushed frame, but the declared `PreemptTrapFrame` layout expects `rip, cs, rflags, rsp, ss` after the GPR block. As written, the Rust handler would not receive the normalized frame the docs promise.

## Remaining Findings

### 1. Blocking: 57d ring-0 normalization inserts synthetic slots at the wrong offset

**Where:**
- `docs/roadmap/57d-voluntary-preemption.md:53-86`
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:77`
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:91-102`
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md:106-117`

The task list says `PreemptTrapFrame` is:

```text
gprs[rax..r15], rip, cs, rflags, rsp, ss
```

For a ring-3 interrupt, the current push order can match that: after pushing GPRs, the CPU-pushed frame starts at `frame + gprs_size`, with `rip` at offset 0, `cs` at offset 8, `rflags` at offset 16, `rsp` at offset 24, and `ss` at offset 32.

For a ring-0 interrupt, the pseudocode does this before pushing GPRs:

```asm
sub  rsp, 16
mov  qword ptr [rsp + 8], 0
mov  qword ptr [rsp + 0], 0
```

That creates the synthetic words *below* the CPU-pushed `rip/cs/rflags`, so the post-GPR frame tail becomes:

```text
offset +120: synthetic ss
offset +128: synthetic rsp
offset +136: rip
offset +144: cs
offset +152: rflags
```

That is not the declared `PreemptTrapFrame` layout. It also makes the return-side ring test wrong: after popping GPRs, `test qword ptr [rsp + 8], 3` tests the synthetic `rsp` slot, not `cs`. This happens to choose the ring-0 path when the synthetic value is zero, but it proves the normalized frame is not shaped as documented.

The roadmap needs to choose an implementable normalization strategy:

- Build a separate normalized trap frame below the CPU frame and copy `rip/cs/rflags` into the declared offsets, leaving the original CPU frame intact for the non-preempting `iretq`; or
- Move the CPU frame into a normalized in-place layout and restore the original 3-field frame before `iretq`; or
- Use separate ring-0 and ring-3 trap-frame views in 57d and only introduce a fully uniform five-field view when the stack choreography is specified precisely.

The important rule is that the pointer passed to Rust as `&mut PreemptTrapFrame` must actually point at bytes laid out as `gprs, rip, cs, rflags, rsp, ss` for both ring cases.

### 2. High: 57d stack-alignment guidance is still not safe enough for an arbitrary interrupt boundary

**Where:**
- `docs/roadmap/57d-voluntary-preemption.md:72-76`
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:94-101`

The docs now make 57d self-contained, which is good, but the alignment math is too brittle:

- The prose says the stub pushes GPRs "with explicit padding", but the pseudocode and task list then say no pad is needed.
- The task list assumes the relevant CPU-entry stack position is 16-byte aligned and that a fixed 160-byte count proves the pre-call alignment. That is not a robust implementation rule for an interrupt that can arrive at arbitrary kernel instruction boundaries.
- The call-site rule that matters is simple: immediately before `call timer_handler_with_frame`, `%rsp` must satisfy the ABI's pre-call alignment requirement. The stub should compute or enforce that after its own frame-normalization work, and any alignment pad must be excluded from the `PreemptTrapFrame` layout or explicitly represented in the layout.

**Recommended fix:** Replace the fixed-count proof with a concrete stub invariant: after creating the trap frame and before the Rust call, adjust `%rsp` so the Rust/C ABI pre-call alignment holds, record whether a pad was inserted, and undo that pad before the GPR-pop / `iretq` path. Keep the `movaps` regression test, but make it verify both ring-0 and ring-3 paths.

### 3. Medium: 57b still has one stale `switch_context` IF=0 comment in the code sample

**Where:**
- `docs/roadmap/57b-preemption-foundation.md:141-145`

The surrounding 57b text now correctly says pointer retargeting uses explicit `cli` windows and does not rely on `switch_context` preserving IF=0. The `preempt_disable()` code sample still says:

```rust
// Dispatch updates the pointer
// atomically with respect to switch_context's IF=0 window, so no IRQ can
// observe a torn pointer.
```

That is the stale claim the latest commit otherwise removed. Because this is in the sample implementation comment, it is easy for an implementer to copy into code and preserve the wrong mental model.

**Recommended fix:** Rewrite the comment to say the pointer is changed only by the C.2/C.3 dispatch retarget points, each under explicit interrupt masking, and that atomic pointer load/store prevents torn pointer values.

### 4. Medium: 57e design intro still overgeneralizes latency wins

**Where:**
- `docs/roadmap/57e-full-kernel-preemption.md:11`
- `docs/roadmap/57e-full-kernel-preemption.md:34`

The README row and the Feature Scope now correctly use per-trigger wording. The 57e milestone and learning-goal text still says round-trip IPC/syscall wake-up latency floors are set by IRQ-handler runtime and "drop into the microsecond range" after 57e.

That is still too broad. Later sections correctly say only cross-core reschedule-IPI and `preempt_enable` zero-crossing paths can plausibly hit that floor; same-core wakeups remain tied to timer / voluntary / zero-crossing behavior, and timer-only preemption remains tick-bounded.

**Recommended fix:** Change the intro and learning goal to match the per-trigger section: cross-core IPI and safe `preempt_enable` zero-crossing paths can improve to IRQ/runtime scale, same-core and timer-only paths are benchmarked separately and must not regress.

## Third-Review Findings Status

- **57b pointer-retarget lifecycle:** mostly addressed. The lifecycle text and task list now use explicit `cli` windows; only the stale code-sample comment remains.
- **57d ring-0 `PreemptTrapFrame` validity:** not fully addressed. The responsibility moved into 57d, but the described stack operations do not actually construct the declared uniform layout.
- **57d ABI / IDT details:** partially addressed. IDT raw-handler guidance is fixed; stack alignment still needs a precise, enforceable invariant.
- **57e README latency overclaim:** addressed in the README row.
- **57d raw-IDT API wording:** addressed.

## Verification Performed

- Re-read the changed sections in 57b, 57d, 57e, and the README at commit `7cc6975`.
- Searched for stale third-review phrases in `docs/roadmap/`.
- Checked `docs/roadmap/57d-voluntary-preemption.md` pseudocode against the declared `PreemptTrapFrame` field order in the 57d task list.
- Ran `git diff --check`.

No build or QEMU test was run; this is a roadmap/documentation review.
