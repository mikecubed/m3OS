---
status: resolved  # Phase 77 Track G.2 (2026-05-28): not reproducible
resolution: "Phase 77 Track G.2 (2026-05-28) — `cargo xtask compositor-stress --timeout 300 --cycles 3 --spawns-per-cycle 4` spawned 12 terminals across 3 workspaces (SUPER+RETURN + SUPER+1/2/3) and completed with NO kernel panic / OOM ('compositor-stress: PASSED (no kernel panic)'; serial under /tmp/g2-oom/serial.log). The Phase 73 single-slide workspace state + SHM-cache fixes (commits listed below) hold up under repeated multi-term spawn pressure. Closed."
branch: feat/phase-73-compositor-polish (PR not yet open)
last-known-good-commit: 43940fc  # current HEAD after the init_buddy fix
fix-commits:
  - 4e4d0fd  # compositor: replace workspace-ghost snapshots with single-slide state
  - 6424ad2  # shm: cache shm_map per shm_id; drop kernel scratch Vec from sys_shm_map
  - 7c5aa0d  # shm: hold strong Rc in cache so double-buffered attach hits
  - 3e34ea0  # shm cache: cap by total bytes (64 MiB), not entry count
  - f94149f  # defence in depth: spawn coalescing + per-process SHM-byte cap
  - f6ed6d7  # qemu: bump RAM from 1 GiB to 4 GiB for 4K compositor  (LATER REVERTED)
  - aa90235  # xtask: add compositor-stress repro + revert RAM to 1 GiB
  - 61ce657  # shm: actually decref creator SHMs on process exit
  - 9aa7ff9  # shm: per-surface buffer cache replaces global byte-budget cache  ← real cache fix
  - 90b3fc7  # shm: instrument every create/destroy + dump buddy state on OOM
  - 43940fc  # mm: fix init_buddy self-deadlock at >1 GiB RAM; bump QEMU to 2 GiB ← real boot fix
date: 2026-05-22
component: userspace/display_server (SHM cache, workspace transitions), kernel/mm (init_buddy, shm registry, syscall diagnostics), xtask (compositor-stress harness, QEMU RAM)
related:
  - docs/roadmap/73-compositor-polish.md
  - docs/handoffs/2026-05-17-less-render-disappearance.md  # qmp / vnc / send-key plumbing this session reused
ruled-out-hypotheses:
  - workspace-switch ghost snapshot allocations (real first symptom, fixed by 4e4d0fd)
  - per-frame sys_shm_map kernel-heap Vec<PhysFrame> scratch (real second symptom, fixed by 6424ad2)
  - Weak-only shm cache thrashing on double-buffer ping-pong (real third symptom, fixed by 7c5aa0d)
  - global byte-budget shm cache thrashing past working set (real fourth symptom, fixed by 9aa7ff9)
  - missing per-process SHM cap enabling runaway clients (defensive; fixed by f94149f)
  - missing creator-ref release on process exit (fixed by 61ce657)
  - "kernel boots fine at 4 GiB" (false; init_buddy self-deadlocks past 1 GiB, fixed by 43940fc)
new-tooling:
  - `cargo xtask compositor-stress` — boots m3OS headlessly under QEMU+VNC+QMP,
    drives `SUPER+RETURN` spawn + `SUPER+1..9` workspace-switch chords through
    `send-key`, watches serial for `KERNEL PANIC` after every keystroke. Bails
    immediately on first panic. Flags: `--kvm --cycles N --spawns-per-cycle N
    --keystroke-gap-ms N --workspaces 1,2,3 --timeout SECS --out PATH`.
  - `xtask/src/qmp.rs::press_chord` — synthesise multi-key chords like
    SUPER+RETURN through QEMU `send-key` (existing `press_key` was single-key
    only).
  - `kernel::mm::frame_allocator::free_frame_count()` — diagnostic, returns
    `(free_pages, total_pages)` summing the bootstrap free list plus the
    buddy. Fed into the new `[shm] create/destroy buddy=X/Y` log lines and
    into `handle_alloc_error` so every future OOM log carries buddy state.
---

## Quick-resume checklist

1. **Branch**: `feat/phase-73-compositor-polish`, HEAD = `43940fc`. Pushed to origin.
2. **Build state**: `cargo xtask check` clean. `cargo xtask smoke-test` passes
   in 14 s at QEMU `-m 2048` (current default; see "QEMU memory" below).
3. **The bug as the user keeps describing it**: launching multiple terms (4–9)
   across workspaces under `cargo xtask run-gui --kvm --fresh` at 4K resolution
   produced `kernel OOM: failed to allocate Layout { size: 64-128, align: 8 }`
   panics. The OOM landed on tiny allocations, meaning total physical memory
   was exhausted — not heap fragmentation. Reproduction was *not* "one
   smoking-gun line of code" but a stack of three independent leaks plus a
   capacity ceiling, listed below.
4. **Status at handoff**: all known leaks fixed, kernel bug that prevented
   booting with >1 GiB RAM also fixed, QEMU memory bumped to 2 GiB. User has
   not yet confirmed the original reproducer no longer crashes. Outstanding
   work below.

## TL;DR — what was wrong

Five distinct issues, debugged in this order. Each fix landed before the next
one surfaced because each masked the next:

1. **Workspace-switch ghost snapshots leaked O(surfaces × buffer_size) per
   switch.** `userspace/display_server/src/animation.rs` had a
   `WorkspaceGhost { pixels: Vec<u8>, ... }` that captured an owned copy of
   every visible surface's framebuffer at switch time. At 4K (3840×2160 ×
   4 B = ~33 MiB) this was 33+ MiB per visible surface per switch. Mash
   `SUPER+1/2/3` enough times and you OOM. Real compositors (Hyprland, KWin,
   Mutter) never snapshot pixels — they render the live surface at a
   transformed offset. **Fix: `4e4d0fd`** replaced ghosts with a single
   `WorkspaceSlide` value driving an x-offset on both workspaces' tiles in
   the compose pass. Zero allocations per transition.

2. **Per-frame `sys_shm_map` kernel-heap allocation churn.** Term and other
   desktop clients re-send `AttachSharedBuffer` every commit as the
   atomic-publication seam (documented at `userspace/term/src/display.rs:444`).
   Each call entered `sys_shm_map` which allocated a `Vec<PhysFrame>` (~63 KiB
   at 4K) as page-table-walk scratch, then freed it. At 60 Hz × N clients this
   fragmented the buddy until the next ~64 KiB contiguous allocation (a fresh
   kernel stack on spawn) failed. **Fix: `6424ad2`** + an in-userspace shm_id
   → mapping cache, plus a kernel-side
   `mm::user_space::map_user_frames_contiguous(base_phys, page_count)` that
   walks the contiguous run directly without the `Vec` scratch.

3. **The cache I added in step 2 was broken twice over.**
   - First it used `Weak<SharedMapping>`. Every cache lookup missed because
     the only `Rc` holder of the previous frame's mapping was the
     `CommittedBuffer` that just got promoted-and-dropped on `CommitSurface`.
     **Fix: `7c5aa0d`** held strong `Rc`s.
   - Strong `Rc`s pin the underlying SHM frames; a 32-entry cap at 4K can pin
     ~1 GiB of physical RAM (32 × 33 MiB). **`3e34ea0`** capped by total
     bytes (64 MiB) instead — but the working set at compositor scale (9+
     terms × 2 buffers × ~16 MiB) exceeded 64 MiB and the cache thrashed
     every frame. Every `AttachSharedBuffer mapped` line in the user's log
     was a cache miss.
   - **The real fix: `9aa7ff9`.** Deleted the global cache. Each
     `ServerSurface` now owns a `BTreeMap<BufferId, Rc<SharedMapping>>`. The
     compositor looks up `(surface_id, buffer_id)` first: a stable
     buffer_id → shm_id pairing (the double-buffer ping-pong case) reuses
     the existing `Rc`. A `shm_id` mismatch (client reallocated, e.g.
     `SurfaceResized` → fresh SHMs) drops the old `Rc` (fires `shm_unmap`
     if we were the last holder) and maps the new one. No global cache, no
     eviction, no thrashing. Memory pinning bounded by client behaviour
     rather than a knob.

4. **`sys_shm_destroy` was the only path that decremented the creator
   refcount.** Clients that exited without calling it (panicked term, killed
   client, exec'd-into-another-binary process) leaked the creator's implicit
   `+1` on every SHM they had created. With another mapper still holding the
   region (e.g. display_server's cache), `decref` never returned the frames
   to the buddy. **Fix: `61ce657`** — `do_full_process_exit` now calls
   `shm::release_creator(pid)`, which walks the registry, finds every
   `ShmEntry` with `creator_pid == pid`, and calls `decref` on it.

5. **`mm::frame_allocator::init_buddy()` self-deadlocked at >1 GiB RAM.** This
   one I sat on for several commits as "a separate pre-existing kernel issue"
   while it was actually blocking my own reproducer and forcing me to debug
   blind at 1 GiB. The deadlock: `init_buddy` held the global
   `FRAME_ALLOCATOR` lock across the per-frame `Vec<u64>` drain push loop
   *and* the `BuddyAllocator::new` bitmap allocation. At ≥2 GiB the Vec's
   grow-doubling crossed the 8 MiB initial-heap ceiling mid-drain, calling
   `grow_heap` → `allocate_frame` → `FRAME_ALLOCATOR.lock()` → spin forever.
   Boot froze silently right after `[mm] bootstrap heap initialized`.
   **Fix: `43940fc`** — restructured `init_buddy` so no allocation happens
   under the lock. Pre-size the Vec to exact capacity before the drain;
   build the `BuddyAllocator` before the drain; drain in a tight
   allocation-free critical section; install under a second brief lock.

## What changed where

### Userspace compositor

| File | Change | Commit |
|---|---|---|
| `userspace/display_server/src/animation.rs` | Replaced `WorkspaceGhost` + ghost lists with a single `WorkspaceSlide { from_ws, to_ws, direction, output_width, progress }`. `tick()` advances the slide; `request_workspace_slide(from, to, dir, width)` starts or retargets. Helpers `from_offset_x()` / `to_offset_x()` produce the per-side x-offset the compose pass applies. | `4e4d0fd` |
| `userspace/display_server/src/workspace.rs` | `WorkspaceLayoutAdapter` gained an `Option<SlideContext>`. When set, `LayoutPolicy::arrange` returns the union of from-ws + to-ws arrangements with each side's x-offset pre-applied. New `arrange_workspace_with_exclusive(idx, ...)` tiles any workspace, not just the active one. | `4e4d0fd` |
| `userspace/display_server/src/main.rs` | Workspace-switch handler now calls `engine.request_workspace_slide(...)` instead of pushing N per-surface animations + N pixel snapshots. Compose builds a `SlideContext` from the engine each frame and feeds the adapter. Per-surface `WindowOpen`/`WindowMove` suppressed during a slide. Also adds `try_consume_spawn_budget()` + `LAST_SPAWN_US` for SUPER+RETURN / SUPER+SPACE coalescing (100 ms gate). | `4e4d0fd`, `f94149f` |
| `userspace/display_server/src/compose.rs` | Removed `blit_workspace_ghost` and the two branches that called it. Transitions now flow through the same surface-blit path as everything else — the offset arrangement makes them invisible to the compose loop. | `4e4d0fd` |
| `userspace/display_server/src/surface.rs` | Big restructure across multiple commits. Final shape: `SharedMapping { shm_id, user_va, len }` is wrapped in `Rc`. `BufferStorage::Shared(Rc<SharedMapping>)`. `ServerSurface` owns `buffer_mappings: BTreeMap<BufferId, Rc<SharedMapping>>`. `AttachSharedBuffer` handler checks the surface's own map first; only on a miss (new buffer_id, or replaced shm_id) does it call `sys_shm_map`. Old global cache (`shm_cache`, `shm_cache_order`, `shm_cache_bytes`, `SHM_CACHE_BUDGET_BYTES`) **deleted**. | `6424ad2`, `7c5aa0d`, `3e34ea0`, `9aa7ff9` |

### Kernel

| File | Change | Commit |
|---|---|---|
| `kernel/src/mm/frame_allocator.rs` | **The `init_buddy` self-deadlock fix.** Lock acquired only briefly: read state, release, build buffers (Vec + buddy bitmaps) off-lock, re-acquire to drain into pre-sized Vec, release, build buddy, brief re-acquire to install. Also added `pub fn free_frame_count() -> (usize, usize)` summing bootstrap + buddy free pages. | `43940fc`, `90b3fc7` |
| `kernel/src/mm/user_space.rs` | Added `map_user_frames_contiguous(mapper, virt_base, base_phys, page_count, flags)` — maps a contiguous physical run directly without building a `&[PhysFrame]` slice. Removed `mapped_pages: Vec` from `map_user_frames`'s rollback path and replaced with an integer counter + `rollback_user_mapping` helper. | `6424ad2` |
| `kernel/src/mm/shm.rs` | `ShmEntry` carries `creator_pid: u32`. New `PROCESS_BYTES: Mutex<BTreeMap<u32, u64>>` tracks per-process live SHM bytes. `create(byte_len, creator_pid)` checks `PER_PROCESS_SHM_CAP_BYTES = 256 MiB` and rejects with new `ShmError::ProcessCapExceeded`. `decref` calls `release_bytes` on the final decref. `pub fn release_creator(pid)` walks the registry and decrefs every entry created by `pid`. | `f94149f`, `61ce657` |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `sys_shm_create` now takes `creator_pid` from `current_pid()` and logs `[shm] create ok pid=X byte_len=N pages=M id=ID buddy=FREE/TOTAL`. `sys_shm_destroy` logs `[shm] destroy pid=X id=ID last_ref=BOOL buddy=FREE/TOTAL`. `do_full_process_exit` calls `shm::release_creator(pid)`. `sys_shm_map` now calls `map_user_frames_contiguous(base_phys, page_count)` instead of building a `Vec<PhysFrame>` of 8 064 entries at 4K. | `6424ad2`, `f94149f`, `61ce657`, `90b3fc7` |
| `kernel/src/lib.rs` | `handle_alloc_error` prints `[alloc_error] layout=L buddy free=F/T pages (FREE MiB free / TOTAL MiB total) pid=P` before panicking, so every future kernel-OOM transcript carries diagnostic state. | `90b3fc7` |

### xtask harness

| File | Change | Commit |
|---|---|---|
| `xtask/src/main.rs` | New `cargo xtask compositor-stress` subcommand. Boots headlessly under QEMU+VNC+QMP, waits for `display_server: registered as 'display.input-owner'` and `TERM_SMOKE:prompt-ready`, then drives SUPER+RETURN spawn-term and SUPER+digit workspace-switch chords on a cadence. Watches serial for `KERNEL PANIC` after every keystroke and bails on first hit. QEMU RAM bumped from 1 GiB to 2 GiB. | `aa90235`, `43940fc` |
| `xtask/src/qmp.rs` | Added `press_chord(&[qcode, ...], hold_ms)` for multi-key chord synthesis. The existing single-key `press_key` couldn't drive SUPER+RETURN. | `aa90235` |

## Open work / next-session targets

1. **Confirm the original reproducer is fixed.** User has not yet run
   `cargo xtask run-gui --kvm --fresh` against `43940fc`. Mash SUPER+RETURN
   across workspaces 1/2/3 with several terms. Expected outcome: no
   `KERNEL PANIC`. The `[shm] create ok ... buddy=X/Y` / `[shm] destroy ...
   buddy=X/Y` lines now in the serial log will show the running waterline;
   `[alloc_error] ... buddy free=F/T pages` will be on the panic line if
   one fires.

2. **Decide what to do with the diagnostic spam.** `90b3fc7` adds a log line
   for *every* `sys_shm_create` and *every* `sys_shm_destroy`. Under heavy
   compositor load that's hundreds of lines a second. Useful for the next
   bug hunt, noisy for steady-state. Options:
   - Strip them once a clean reproducer-passing log exists.
   - Gate behind a kernel cmdline flag or env var.
   - Keep — they're cheap and the log has been the only window into what
     actually consumes memory.

3. **TCG performance at >2 GiB.** 4 GiB KVM smoke is ~16 s; 4 GiB TCG smoke
   times out because `BuddyAllocator::free(pfn, 0)` is called once per frame
   (1 M+ frames at 4 GiB) and each call walks up to `MAX_ORDER = 13` levels.
   A bulk-free variant of `BuddyAllocator::add_region` that recognises
   already-aligned runs and inserts at higher orders without the per-page
   coalesce walk would unlock 4 GiB TCG smoke. Not blocking the compositor
   work; nice-to-have for CI throughput if anyone wants to push past 2 GiB.

4. **The `compositor-stress` harness drops most chords under fast mash.**
   At 50–100 ms gaps the QMP `send-key` path or the PS/2 scancode queue
   eats keystrokes — 60 attempts → 30 actual `term: spawned` lines is
   typical. The harness still proves the OOM is fixed (no panic across
   thousands of `[shm] create`/`[shm] destroy` round-trips), but if you
   want it to faithfully reproduce a true 60-spawn workload, the chord
   delivery needs to land all of them. Likely needs PS/2 controller-side
   queueing or a longer per-chord hold time.

5. **`SHM_CACHE_BUDGET_BYTES` is gone but the related struct fields too —
   diff was large.** A future reviewer reading
   `userspace/display_server/src/surface.rs` won't see any "cache" anymore,
   just `ServerSurface::buffer_mappings`. Worth a quick design-doc / phase
   doc cross-reference if this stack lands as a PR; the journey from
   "global cache with byte budget" → "per-surface buffer map" is not
   obvious from the final code.

6. **Phase 73 design doc.** This whole session was a polish-pass on Phase
   73, but `docs/roadmap/73-compositor-polish.md` doesn't yet mention the
   workspace-slide / SHM per-surface tracking rewrites. Update it (or
   write a Phase 73a follow-up doc) when shipping the PR.

## Hypotheses I burned time on that turned out wrong

- "Strong-Rc cache pinning 1 GiB of RAM" was *partially* right (it could
  pin a lot of RAM under bad cap settings) but the actual user-visible
  symptom was cache **miss** thrashing, not pinning. The byte-budget cap
  in `3e34ea0` was solving the wrong problem at the wrong scale.
- "Greeter exits without destroying its SHM" — false. Greeter explicitly
  calls `shm_destroy` (`userspace/greeter/src/main.rs:251`). The
  process-exit creator-leak path (`61ce657`) is still real defence-in-
  depth, but greeter wasn't actually leaking.
- "QEMU 4 GiB + VNC is a separate kernel bug" — false. Same
  `init_buddy` self-deadlock; manifested only at >1 GiB regardless of
  display mode. Smoke-test happened to pass once at 4 GiB by sheer race
  luck. The deadlock is fully deterministic at ≥2 GiB now that I've
  reproduced it cleanly.
- "Cache thrashing isn't the bug, leak is" — flipped back and forth on
  this several times before realising **both** were happening (cache
  thrashing churning the kernel heap *and* creator-ref leaks pinning
  frames). Each fix individually was insufficient.

## How to actually reproduce

Manual (what the user has been doing):
```
cargo xtask run-gui --kvm --fresh
# In the guest:
SUPER+RETURN ×4    # spawn 4 terms on workspace 1
SUPER+2            # to workspace 2
SUPER+RETURN ×4
SUPER+3
SUPER+RETURN ×4
# Watch the host's serial transcript for KERNEL PANIC.
```

Headless harness (the new tool):
```
cargo xtask compositor-stress --kvm --cycles 6 --spawns-per-cycle 5 \
    --keystroke-gap-ms 100 --timeout 240
# Serial log dropped to /tmp/m3os-compositor-stress/serial.log
# Exits 0 on no-panic, 1 + serial tail on KERNEL PANIC.
```

The harness shares the QEMU memory setting in `qemu_args_with_devices`
(currently 2 GiB), so once `43940fc` landed it automatically picks up the
larger budget. The `--display vnc=unix:` + `-vga std` path used to deadlock
in `init_buddy` at >1 GiB — that was the bug at the bottom of this whole
mess and is now fixed.
