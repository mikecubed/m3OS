---
status: open  # active investigation, reproducer pinpointed to PCI-hole crossing
branch: feat/phase-73-compositor-polish (PR not yet open)
last-known-good-commit: 316d351  # current HEAD after the diagnostic + escape-hatch work
fix-commits:
  - 352e04c  # xtask: add -m / --memory flag (override QEMU guest RAM)
  - 1784c45  # compositor: invalidate cached arrangement per frame during workspace slide
  - 3b22620  # sched: surface real caller of deep IrqSafeMutex nesting in [preempt-depth]
  - 316d351  # xtask: M3OS_GUI_BACKEND / M3OS_GUI_VGA escape hatches
date: 2026-05-24
component: xtask (-m flag, gui backend overrides), userspace/display_server (slide cache invalidation), kernel/task/scheduler (preempt-depth diagnostic), and an OPEN guest-side bug at 4 GiB guest RAM on Zen 5 + Linux 7.0 hosts
related:
  - docs/handoffs/2026-05-22-compositor-shm-leak-multi-term-oom.md  # immediate predecessor
  - docs/roadmap/73-compositor-polish.md
ruled-out-hypotheses:
  # 4 GiB hang investigation
  - SDL display backend bug (VNC via M3OS_GUI_BACKEND=vnc reproduces the black screen byte-for-byte)
  - AMD AVIC IPI virtualisation regression (disabling avic=0 on user's Zen 5 / Linux 7.0 host had no effect)
  - KVM vs TCG (4 GiB hangs under both)
  - SMP / cross-core IPI delivery (M3OS_SMP=1 + KVM + 4 GiB still hangs)
  - Compositor-side animation rendering (workspace slide rendering was a real bug, fixed in 1784c45, but is unrelated to the 4 GiB symptom)
  - "preempt-depth 36→260 means deeply nested locks" (false — the warning system's own log path
    recurses through `_kernel_print → DMESG_RING.lock → IrqSafeMutex::lock → preempt_disable_at`,
    inflating depth by ~1 per recursion; real deepest depth is 5 from `kernel/src/smp/ipi.rs:56`
    or `kernel/src/smp/tlb.rs:264`)
not-ruled-out-but-likely:
  - QEMU + OVMF + `-vga std` mapping of the VGA BAR at 0xc0000000 (in the 3 GiB–4 GiB PCI hole)
    becomes incoherent once guest RAM crosses into high-memory above 4 GiB
  - Same problem in m3OS's kernel-side framebuffer mapping path — bootloader_api returns
    `Using framebuffer at 0xc0000000` but kernel paging may handle it differently when
    high-RAM regions exist
new-tooling:
  - `xtask` env vars (`M3OS_GUI_BACKEND=sdl|gtk|vnc`, `M3OS_GUI_VGA=<qemu-display-device>`)
    let the operator swap display backends without rebuilding xtask. Used here to prove the
    4 GiB hang is *not* SDL-specific.
  - `-m` / `--memory` / `M3OS_MEM=` CLI knob on `run`, `run-gui`, `test` etc. — lets us
    bisect the memory-size threshold (3 GiB works, 4 GiB hangs) without editing source.
  - `#[track_caller]` plumbed through `IrqSafeMutex::lock` / `try_lock` →
    `preempt_disable_at(location)` → `[preempt-depth]` warning, so the diagnostic
    now points at the real call site instead of the line inside the mutex helper.
---

## Quick-resume checklist

1. **Branch**: `feat/phase-73-compositor-polish`, HEAD = `316d351`. Pushed to origin.
2. **Build state**: `cargo xtask check` clean. `cargo xtask smoke-test --kvm` passes in ~9 s.
3. **The new symptom**: `cargo xtask run-gui --kvm -m 4g --fresh` shows a black SDL window
   *and* a black VNC frame on the user's host (Zen 5 / Linux 7.0.x / QEMU 8.2.2). The same
   binary boots to greeter in ~10 s on the original handoff author's sandbox (Zen 4 /
   Linux 6.8 / QEMU 8.2.2). The serial log on the user's host stops at `[preempt-depth]
   count=5 caller=kernel/src/smp/tlb.rs:264` after exhausting the 32-slot warning budget;
   no further serial output appears, suggesting the kernel is genuinely hung.
4. **The reproducer is bisected**: the threshold is **exactly the 3 GiB → 4 GiB PCI-hole
   crossing**. 2 GiB and 3 GiB boots succeed. 4 GiB fails. Tested under KVM, TCG, and
   SMP=1 — all fail at 4 GiB on the user's host, all succeed at 3 GiB.
5. **Outstanding**: identify whether the bug is in (a) m3OS kernel paging when high-RAM
   exists above the PCI hole, (b) the OVMF GOP / `-vga std` interaction with high RAM,
   (c) a KVM SVM regression on Zen 5 specific to the high-RAM layout. The single
   observation that the sandbox's Zen 4 + Linux 6.8 stack boots the same binary fine
   while the user's Zen 5 + Linux 7.0 stack hangs is the wedge to investigate first.

## TL;DR — what was done this session

Five distinct pieces of work, in chronological order:

1. **`-m` / `--memory` / `M3OS_MEM=` knob on `xtask`.** Previously the QEMU
   `-m 2048` was hard-coded. Now `cargo xtask run-gui --kvm -m 4g` overrides
   it without recompiling. Flag flows through `extract_device_flags` so every
   subcommand that accepts `--kvm` / `--device` automatically supports `-m`.
   Validates: minimum 256 MiB; > 2 GiB under TCG prints a one-time slow-boot
   warning. Default stays 2 GiB so smoke/regression budgets are unchanged.
   **Commit: `352e04c`.**

2. **Workspace slide rendering fix.** The Phase 73 `WorkspaceSlide`
   infrastructure was wired end-to-end but visually nothing moved — workspaces
   snapped instead of sliding. Root cause: `ComposeContext::cached_arrangement`
   is keyed on the toplevel id set, and the set stays constant across a slide
   (both workspaces' surfaces are pinned in the merged compose filter).
   `LayoutPolicy::arrange` ran once at slide-start, baked in the frame-0
   offsets, and the cached rects survived the 260 ms duration. **Fix**: after
   the per-frame `animation_engine.tick`, if a slide is in flight, call
   `compose_ctx.invalidate_arrangement_cache()` so the next compose pass
   recomputes the arrangement with the live slide progress. **Commit:
   `1784c45`.**

3. **`[preempt-depth]` warning gave the wrong caller.** Every warning
   reported `caller=kernel/src/task/scheduler.rs:308`, which is just the line
   inside `IrqSafeMutex::lock` that calls `preempt_disable`. Refactored:
   `preempt_disable` is now a thin wrapper around a new
   `preempt_disable_at(location)` helper. `IrqSafeMutex::lock` and
   `try_lock` gain `#[track_caller]` and pass `Location::caller()` through.
   Diagnostic now points at the real user of the mutex. **Commit: `3b22620`.**

4. **Demystified the "depth 36 → 260" panic-flavoured numbers.** With the
   new `#[track_caller]` plumbing, ~31 of every 32 warnings show
   `caller=kernel/src/serial.rs:40` (the recursive log path) and only 1 shows
   the real culprit (`kernel/src/smp/ipi.rs:56` for `wait_icr_idle`, or
   `kernel/src/smp/tlb.rs:264` for `tlb_shootdown_range_kernel`). The "depth
   260" we previously saw was log-path recursion inflating the count, not a
   real 260-deep call chain. **Real maximum legitimate depth is 5** —
   exactly one level above the documented max of 4.

   *Side note*: a prototype per-core re-entry guard
   (`PREEMPT_WARN_IN_FLIGHT[MAX_CORES]`) cleanly suppressed the recursion
   noise but broke 4 GiB GUI boot — the multi-ms recursive-log delay was
   inadvertently giving `wait_icr_idle`'s IPI handshake the time it needs to
   complete. Removing the delay exposes a latent IPI-timing race in
   `wait_icr_idle`. The guard was reverted; the comment on
   `PREEMPT_LEAK_LOG_BUDGET` documents this for the next debugger. **(Not
   committed — code reverted; rationale lives in commit message of `3b22620`.)**

5. **`M3OS_GUI_BACKEND` and `M3OS_GUI_VGA` escape hatches.** Investigation
   into "SDL black at 4 GiB" turned up several plausible QEMU SDL/VGA bugs
   (gitlab qemu-project/qemu#2048, #1902; OVMF GOP mode-list ceiling around
   2560×1600; `vgamem_mb=32` at 98.9% utilization for a single 4K
   framebuffer). Rather than guess, gave the operator env-var knobs:
   `M3OS_GUI_BACKEND=vnc` opens an RFB server on `:0` and prints a connect
   hint; `M3OS_GUI_VGA=bochs-display` swaps the `-vga` device. The VNC
   escape hatch was decisive: VNC reproduces the black-screen symptom byte
   for byte, ruling out SDL as the cause. **Commit: `316d351`.**

## TL;DR — what's still open

The 4 GiB guest-RAM hang on the user's host. Reproducer:

```bash
cargo xtask run-gui --kvm -m 4g --fresh
```

Host environment where it fails:
* CPU: AMD Ryzen AI 9 365 (Strix Point, Zen 5)
* Kernel: Linux 7.0.x
* QEMU: 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.16)
* AVIC: `kvm_amd.avic=1` originally; disabling (`avic=0`) made no difference

Host environment where it succeeds (sandbox / original author):
* CPU: AMD Ryzen 5 7600 (Zen 4)
* Kernel: Linux 6.8.x
* QEMU: 8.2.2 (same package)

The symptom is identical regardless of:
* Display backend (`M3OS_GUI_BACKEND=sdl` vs `=vnc` — both black)
* SMP count (`M3OS_SMP=1` vs default 4 — both hang)
* Accelerator (`--kvm` vs TCG — both hang at 4 GiB)

The symptom **does not appear** when:
* `cargo xtask run-gui --kvm --fresh` (default 2 GiB) — boots to greeter
* `cargo xtask run-gui --kvm -m 3g --fresh` — boots to greeter

So the bisected threshold is **exactly the 3 GiB → 4 GiB PCI-hole boundary**.

Suspect surface area:

1. **Kernel paging for split low/high RAM regions.** With ≤ 3 GiB QEMU does
   not need the high-RAM range; with ≥ 4 GiB QEMU gives the guest
   discontiguous physical regions split by the 0xc0000000–0x100000000 PCI
   hole. The kernel's frame allocator handles this on the original author's
   sandbox at 4 GiB without issue, but the user's host may exercise a
   path that the sandbox doesn't (Zen 5 / Linux 7.0 differences in how
   KVM/TCG presents the memory map).

2. **VGA BAR at 0xc0000000 collides with the kernel's interpretation of
   the PCI hole.** The bootloader prints `Using framebuffer at
   0xc0000000`. With high RAM above 4 GiB, the kernel must page-map both
   the framebuffer (MMIO at 3 GiB) AND high-RAM as RAM. If the kernel's
   page-table setup treats the framebuffer page as RAM-cached, writes go
   to RAM instead of to the QEMU VGA device, and the SDL/VNC view never
   updates. This is consistent with a "kernel boots fine but screen
   stays black" symptom.

3. **OVMF + `-vga std` interaction at 4 GiB.** OVMF GOP's default mode
   list only goes up to 2560×1600 per upstream docs; the kernel is
   requesting 3840×2160 and getting it (the serial log shows successful
   framebuffer init). But the path that *delivers* that framebuffer may
   behave differently when the firmware sees high RAM above 4 GiB. Worth
   trying `M3OS_GUI_VGA=bochs-display` on the user's host to see if a
   different display device changes the outcome.

## What changed where

### `xtask`

| File | Change | Commit |
|---|---|---|
| `xtask/src/main.rs` | Added `memory_mib: Option<u32>` to `DeviceSet`; new `parse_memory_spec` (accepts `4g`/`4G`/`512m`/`512M`/bare-MiB; min 256 MiB); `extract_device_flags` parses `-m`, `-m=`, `--memory`, `--memory=`; honors `M3OS_MEM=` env-var alias; `qemu_args_with_devices_resolved` substitutes the override into the `-m` arg with a TCG-slow-boot warning above 2 GiB. | `352e04c` |
| `xtask/src/main.rs` (GUI display path) | Added `M3OS_GUI_BACKEND=sdl\|gtk\|vnc` and `M3OS_GUI_VGA=<device>` env overrides for the GUI arm of `qemu_args_with_devices_resolved`. `vnc` opens an RFB server on `:0` and prints a connect-hint line to stderr. | `316d351` |

### Userspace compositor

| File | Change | Commit |
|---|---|---|
| `userspace/display_server/src/main.rs` | After `animation_engine.tick(delta_ms)`, if `animation_engine.workspace_slide().is_some()`, call `compose_ctx.invalidate_arrangement_cache()`. This forces `LayoutPolicy::arrange` to rerun every frame while a slide is in flight so the per-frame `from_offset_x` / `to_offset_x` actually drive on-screen motion (the cached rects were previously frozen at frame 0 because the slide doesn't change the toplevel id set). | `1784c45` |

### Kernel

| File | Change | Commit |
|---|---|---|
| `kernel/src/task/scheduler.rs` | Split `preempt_disable` into thin `#[track_caller]` wrapper + new `preempt_disable_at(location)` helper. Added `#[track_caller]` to `IrqSafeMutex::lock` / `IrqSafeMutex::try_lock` and routed `core::panic::Location::caller()` through `preempt_disable_at`, so the `[preempt-depth]` warning surfaces the real mutex user instead of `scheduler.rs:308`. Updated comment on `PREEMPT_LEAK_LOG_BUDGET` documenting why the obvious "per-core re-entry guard" fix is load-bearing for boot timing and was reverted. | `3b22620` |

## Open work / next-session targets

1. **Confirm whether the kernel is actually hung at 4 GiB on user's host or
   just running silently.** The serial log stops at the last preempt warning
   because the budget exhausts there. Bump `PREEMPT_LEAK_LOG_BUDGET` to e.g.
   `1024` (the boot-time-spike fits easily and the per-frame steady-state
   noise is bounded by the budget) and re-test on the user's host. If we
   see more log lines after the warnings — especially `[init] /sbin/init
   registered as pid 1` or `[init] service set started — yielding` — the
   kernel itself is fine and we have a pure framebuffer-mapping bug. If we
   still see nothing past the warnings, the kernel is genuinely hung in
   the TLB-shootdown spin loop and we need to dig into IPI delivery on
   Zen 5 + Linux 7.0.

2. **Try `M3OS_GUI_VGA=bochs-display` on user's host at 4 GiB.** QEMU docs
   recommend `bochs-display` over `-vga std` for UEFI guests because it's
   a pure linear framebuffer with no legacy VGA cruft. If bochs-display
   renders correctly at 4 GiB on the user's host, the bug is in the
   `-vga std` / OVMF GOP path at high memory. Quick win.

3. **Try `cargo xtask smoke-test --kvm -m 4g` (headless, no compositor)
   on user's host.** The smoke runner uses serial-stdout exclusively; it
   doesn't touch the framebuffer. If smoke passes at 4 GiB while
   `run-gui` hangs, the kernel boots fine and the bug is purely in the
   display path. If smoke also hangs at 4 GiB, the bug is kernel-wide
   (paging, frame allocator, something hitting the PCI hole boundary).

4. **Dump the kernel's PML4 / page-table state for the 0xc0000000–
   0x100000000 range at 4 GiB.** With high RAM in play, the framebuffer
   physical page is in the PCI hole *between* two RAM regions. If our
   page-table setup is treating this range as cacheable RAM (instead of
   MMIO/uncached), writes from the compositor land in cache lines that
   never reach the VGA device. A `[mm/debug]` dump similar to the
   existing `[mm/debug] no reserved regions below 1 MiB found` line,
   covering the PCI hole flags, would catch this.

5. **The latent IPI-timing race in `wait_icr_idle`.** The reverted
   per-core re-entry guard fix exposed it. Worth a dedicated investigation:
   instrument `wait_icr_idle` with a timeout + diagnostic dump, then run
   without the recursion-delay-as-workaround.

6. **The `PREEMPT_WARN_IN_FLIGHT` re-entry guard is the right fix
   long-term.** Once the latent IPI race is closed, land the guard. The
   one-line patch is in `3b22620`'s prose only; the code is gone.

## Hypotheses I burned time on that turned out wrong

- "depth 36 / depth 260 means the kernel has a 260-deep lock chain" —
  false. It's the warning system's own log path recursing through
  `_kernel_print` → `DMESG_RING.lock` → `IrqSafeMutex::lock` → back into
  `preempt_disable_at` → another log::warn. Real max depth is 5.
- "QEMU SDL on Wayland is broken at QEMU 8.2 (gitlab #2048)" — real
  bug, plausible candidate, but ruled out: VNC reproduces the same
  black-screen symptom byte-for-byte.
- "AMD AVIC IPI virtualisation bug (Zen1/Zen2 era, partial Zen3/Zen4
  follow-on regressions)" — well-documented in upstream patches, real
  bug, plausible candidate on Zen 5 + Linux 7.0, but ruled out:
  disabling `kvm_amd.avic=0` had no effect on the user's symptom.
- "It's an SMP / IPI delivery problem at 4 GiB" — ruled out by
  `M3OS_SMP=1` test (single-CPU guest also hangs at 4 GiB).
- "It's a KVM regression" — ruled out by the TCG test (no-`--kvm` also
  hangs at 4 GiB).
- "The compositor stopped emitting frames because the cache hung" —
  false. The compositor's `compose#N` logs are budget-limited to the
  first 5 entries; the steady-state compose loop runs silently. The
  byte-identical framebuffer screenshots at 2 GiB and 4 GiB (taken via
  QMP `screendump` on the sandbox) prove the compositor *is* painting
  at 4 GiB on at least one host configuration.

## How to actually reproduce

User's host:
```bash
# Reliably black:
cargo xtask run-gui --kvm -m 4g --fresh
M3OS_GUI_BACKEND=vnc cargo xtask run-gui --kvm -m 4g --fresh   # then `vncviewer localhost:5900`
M3OS_SMP=1 cargo xtask run-gui --kvm -m 4g --fresh
cargo xtask run-gui -m 4g --fresh                              # TCG, also black

# Reliably succeeds:
cargo xtask run-gui --kvm --fresh                              # 2 GiB default
cargo xtask run-gui --kvm -m 3g --fresh                        # 3 GiB
```

Original author's sandbox (control case where 4 GiB works):
```bash
# Built the image manually and ran QEMU directly so we could capture both
# serial and a QMP screendump:
qemu-system-x86_64 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=target/x86_64-unknown-none/release/boot-uefi-m3os.img \
  -serial file:/tmp/m3os-debug/serial.log \
  -m 4096 -smp 4 -enable-kvm -cpu host \
  -display vnc=unix:/tmp/m3os-debug/vnc.sock \
  -qmp unix:/tmp/m3os-debug/qmp.sock,server,nowait \
  ... (same VGA / netdev / disk / audio args as `run-gui`)
# Greeter appears in ~10 s; QMP `screendump` shows the rendered wallpaper.
```
