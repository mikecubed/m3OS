# Phase 100 - Bare-Metal GUI Session (Dell Tiger Lake)

**Status:** Implemented (HW-unvalidated) — the full CI-testable surface is green (init builtin-path parse + diskless graphical-mode decision host tests; WC PTE-flag `[fb-wc] PCD=1 PWT=0 PAT=0` readback + `compositor-stress`/`usb-smoke`/`termios-smoke` gates; `usb-hid`/`usbhub` adaptive-backoff host tests; `RENDER_FP` render-fingerprint host tests + QEMU sentinel; pointer inject/focus sentinels). The remaining un-modelable arms (greeter on the physical panel, USB mouse moving the cursor + focus-on-click on the dock-hub, WC-vs-WB blit-latency ratio, USB keyboard at the text login, flat idle-CPU) require a recorded **`Dell Precision 5560 / Tiger Lake`** run per [`docs/appendix/bare-metal-validation.md`](../appendix/bare-metal-validation.md) before this becomes **`Validated-on-HW (run N, date)`**.
**Source Ref:** phase-100
**Depends on:** Phase 99 (SMP & Scheduler Robustness Hardening) ✅, Phase 56/68/71/72/73 (display_server / compositor clients / greeter / session_manager) ✅, Phase 96 (Bare-Metal Bring-up: boot rescue, USB log persistence, PS/2 keyboard, console-FB write-combining) ✅
**Builds on:** **Finishes** the Phase 96 write-combining framebuffer work — 96 reprogrammed `IA32_PAT` index 2 to WC and remapped only the *kernel console* FB mapping (`kernel/src/arch/x86_64/pat.rs::set_range_write_combining`), but the *userspace compositor* framebuffer that `sys_framebuffer_mmap` hands `display_server` is still mapped write-back — and wires the already-built-but-unspawned graphical stack into the bare-metal boot path (init's `add_builtin_defaults` omits it today). No new compositor code: this is integration + the WC user-FB fix + bare-metal validation.
**Primary Components:** `userspace/init/src/main.rs` (`add_builtin_defaults` / `BUILTIN_CONFIGS` + the graphical skip-filter on the builtin path), `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_framebuffer_mmap` — WC PTE attribute on the user FB VMA), `kernel/src/arch/x86_64/pat.rs` (the WC PAT slot the user mapping reuses), `userspace/drivers/usb-hid` (`inject_pointer` → `MOUSE_EVENT_INJECT`), `userspace/mouse_server` (the injected-event queue → `MOUSE_EVENT_PULL`), `userspace/display_server/src/input.rs` + `kernel-core/src/input/dispatch.rs` (`InputDispatcher` / `PointerRouteDecision` focus routing), `userspace/stdin_feeder` (text-mode USB keyboard drain), `docs/appendix/bare-metal-validation.md` (the HW evidence protocol)

## Milestone Goal

Boot the physical **Dell Precision 5560 (Tiger Lake)** laptop to a **usable graphical session**: `display_server` takes the framebuffer, the `greeter` GUI login renders on the panel, a working pointer (an **interim USB mouse** through the existing `usb-hid → mouse_server` inject path) moves the compositor cursor with focus following it, and the already-validated PS/2 (plus, in text mode, USB) keyboard works — all over the Phase 96 bare-metal substrate. The compositor stack already exists and is QEMU-validated; this phase is the integration that makes it *appear on real hardware*, the write-combining user-framebuffer fix that makes per-frame blits viable on real MMIO, and the bare-metal validation that records it.

## Why This Phase Exists

The laptop boots today to a **text framebuffer-console login**, not a desktop, for two concrete reasons that this phase removes:

1. **The graphical stack is never spawned on bare metal.** `display_server`, `mouse_server`, `session_manager`, and `greeter` exist and run under QEMU, but they are launched only from the **data-disk** service manifests (`/etc/services.d/*.conf`, the `KNOWN_CONFIGS` path). A bare-metal USB boot has no ext2 data disk, so init falls back to `add_builtin_defaults()` / `BUILTIN_CONFIGS`, which the Phase 96 bring-up deliberately kept minimal — its own comment reads *"Kept minimal — no display/greeter/audio: a USB-only boot uses the kernel framebuffer console + the ramdisk root."* The result is `telnetd`/`sshd` + the text console and nothing graphical.

2. **The userspace framebuffer is mapped write-back.** `sys_framebuffer_mmap` maps the FB frames into `display_server` with PTE flags `PRESENT | WRITABLE | USER_ACCESSIBLE | NO_EXECUTE | BIT_11` — **no `NO_CACHE`/PCD bit**, so the mapping decodes as write-back. On QEMU the FB is host RAM and write-back is fine; on **real MMIO** every pixel store becomes an uncacheable-ish bus transaction and a full-screen compositor blit crawls (the same ~0.2 s-per-scrolled-line pathology Phase 96 fixed for the *console* FB, but the compositor never got it). The Phase 98 charter flags this explicitly as the "now-fast framebuffer" false premise. WC merges adjacent stores into burst writes — the standard ~10–50× win for streaming framebuffer writes.

The laptop also has **no PS/2 pointer** — its built-in pointer is an I2C-HID touchpad with no driver yet (that is Phase 102). The cheapest unblocker is a **USB mouse** routed through the existing `usb-hid` decode → `mouse_server` inject → `InputDispatcher` focus path; that path is real but has only ever been exercised against an emulated PS/2 mouse under QEMU, never against a real pointer or on bare metal.

## Learning Goals

- Understand the **memory-type plumbing** for a userspace device mapping: how a PTE's PCD/PWT/PAT bits index `IA32_PAT`, why a freshly-mapped 4 KiB leaf with `NO_CACHE` (PCD=1, PWT=0, PAT=0) selects PAT index 2 — the slot Phase 96 reprogrammed to WC — and why WC is weakly ordered so the present/page-flip path must `sfence` before signalling the flip.
- See how a microkernel keeps the input datapath out of ring 0: a USB HID report is decoded in `usb-hid` (ring 3), injected as a typed `PointerEvent` into `mouse_server`, pulled by `display_server`, and routed by the **pure-logic** `InputDispatcher` (`kernel-core`) that owns no compositor state — the same seam a PS/2 mouse, a USB mouse, and (later) an I2C-HID touchpad all share.
- Learn the difference between *bus-agnostic input plumbing that is unit-tested* and *a datapath validated against real silicon*, and why a serial sentinel ("the pointer injected N events") plus an on-device render assertion ("the panel changed") is the bare-metal substitute for the QMP/PPM screendump that does not exist on metal.
- Understand polite-idle daemon design: why a 5 ms busy-poll loop (`usb-hid`) and a hub walker spin pin a core at idle, and how to convert them to interrupt/notification-driven waits on a single-threaded server.

## Feature Scope

### Track A — Spawn the graphical stack on the bare-metal boot path

Add `display_server`, `mouse_server`, `session_manager`, `greeter` (and, as needed, `audio_server` and `term`) to `init`'s `add_builtin_defaults()` / `BUILTIN_CONFIGS` — the list a no-data-disk bare-metal boot falls back to — with the same dependency edges the data-disk `KNOWN_CONFIGS` entries use (`greeter`/`term` depend on `display_server`; `session_manager` supervises the session). Then resolve the **graphical skip-filter** on the builtin path: the data-disk path runs `skip_for_greeter_filter`, which chooses `greeter.conf` (graphical-only) vs the default serial path based on the `/etc/m3os-graphical-only` marker file — but that marker lives on the **absent** data disk, so the builtin path needs its own toggle to put init into "yield the tty to the greeter" mode on bare metal. The end state: on the laptop, `display_server` claims the FB via `try_yield_console` (raw PS/2-to-stdin disabled because the exec path is `/bin/display_server`), `greeter` renders the login, and init does not also start a competing text login on the same console.

### Track B — Write-combining user framebuffer (finish the Phase 96 WC work)

Give the userspace FB VMA the WC attribute it lacks. In `sys_framebuffer_mmap` (`kernel/src/arch/x86_64/syscall/mod.rs`, the PTE `flags` at the `map_user_frames` call), add `PageTableFlags::NO_CACHE` so the leaf decodes to PAT index 2 (= WC after `pat::init`), mirroring the type-selection logic in `pat.rs::set_range_write_combining` (PCD set, PWT clear, PAT bit clear). Confirm `pat::init` has run on every core the compositor can be scheduled on (BSP early-init + `smp::boot::ap_entry` AP bring-up — the per-core PAT requirement the Intel SDM imposes). Mark the recorded VMA's `prot`/`flags` so the mapping survives the existing TLB shootdown + generation bump unchanged. Add an `sfence` to the present/`sys_framebuffer_pageflip` path so weakly-ordered WC writes are visible before the flip is signalled. Then **measure** the blit-latency improvement on the laptop (full-screen fill timing, WC vs write-back) — this is a bare-metal-only measurement because QEMU's RAM FB makes WC vs WB negligible.

### Track C — Interim USB-mouse pointer, bare-metal-validated

The code path exists end to end: `usb-hid` decodes a boot/Report-Protocol mouse report into a `kernel_core::input::events::PointerEvent` and calls `inject_pointer` (`MOUSE_EVENT_INJECT = 3`) into `mouse_server`; `mouse_server` enqueues it (injected events drain first, ahead of its own PS/2 pipeline) and serves it on `MOUSE_EVENT_PULL = 1`; `display_server`'s `MouseInputSource` (`poll_pointer`) pulls it and feeds the `InputDispatcher`, which produces a `PointerRouteDecision` (cursor motion + a focus change on button-down over a `Toplevel`). Track C **validates this on the laptop's real dock-hub topology** — a USB mouse behind the dock/`usbhub` walker, decoded by `usb-hid`, moving the compositor cursor with focus following — since the dispatcher and `display_server/src/input.rs` are QEMU-PS/2-validated only and have never seen a real pointer or run on bare metal. No new dispatch logic; this is the falsifiable "the pointer works on hardware" proof.

### Track D — Input polish (folds the open Phase 96 keyboard handoff)

Two cleanups carried from the Phase 96 bring-up handoff (`docs/handoffs/2026-06-25-usb-log-persistence-and-keyboard.md`):

- **USB keyboard in text mode.** `stdin_feeder` bridges keyboard input to the text login by draining `kbd_server`'s PS/2 scancode path (`KBD_TRY_READ`) only; a **USB** keyboard's input arrives at `kbd_server` as typed `KeyEvent`s via `usb-hid`'s `KBD_EVENT_INJECT`, which `stdin_feeder` never reads. Teach `stdin_feeder` to also drain the typed `KeyEvent` queue (`KBD_EVENT_PULL = 2`, with the `KBD_EVENT_NONE` empty/timeout sentinel) and convert those events to stdin bytes, so a USB keyboard types at the framebuffer login (before the compositor takes the FB) on a machine with no PS/2 keyboard.
- **Stop pinning a core at idle.** `usb-hid` runs a `loop` that polls every device's interrupt-IN endpoint on a fixed 5 ms cadence (`POLL_INTERVAL_NS`), and `usbhub` similarly busy-walks; on the laptop this keeps a core hot at idle (and wastes battery — a real concern the moment Phase 103 adds power management). Convert both to interrupt/notification-driven waits: the xHCI server already captures reports on its IRQ, so `usb-hid` should block on a notification rather than spin, waking on a real transfer event.

### Track E — Bare-metal validation method ("the screen shows the greeter")

There is **no QMP screendump on bare metal** (the QEMU-only path the existing `less-render-probe`/`claude_tui_render_arm` gates use). Per `docs/appendix/bare-metal-validation.md` this phase ships one of the two accepted falsifiable substitutes: an **on-device render assertion** — the compositor (or greeter) computes a cheap changed-scanline-count / hash of its own output and prints it over the `usb-logsink` boot.log + the network sink (the on-metal analog of the PPM band-diff) — and/or a **dated photograph** of the panel committed as the evidence artifact. The captured sentinel + photo are what flips the phase Status to `Validated-on-HW (run N, date)`.

## Important Components and How They Work

### `userspace/init/src/main.rs` — `add_builtin_defaults` + the builtin graphical skip-filter

`add_builtin_defaults` parses an embedded `BUILTIN_CONFIGS` array through the same `parse_service_def` the on-disk configs use, so dependency ordering and restart policy behave identically. Track A appends the graphical-stack entries and gates the greeter-vs-text decision on a builtin-path toggle that stands in for the absent `/etc/m3os-graphical-only` marker (`graphical_only_enabled()` reads a file that does not exist on a diskless boot). Today `skip_for_greeter_filter` runs only on the `KNOWN_CONFIGS`/dir-scan path; the builtin path must make the same choice. Net effect: the laptop boots straight into the GUI session instead of the text console, with no regression to the QEMU/data-disk boots (which keep their existing `KNOWN_CONFIGS` path untouched).

### `sys_framebuffer_mmap` + `pat.rs` — the WC user mapping

`sys_framebuffer_mmap` translates the kernel FB virtual address to its physical frames, claims the console atomically via `try_yield_console`, maps the frames into the caller with a fixed PTE flag set, records the VMA, and shoots down the TLB range. Track B's one-bit change — adding `NO_CACHE` to those flags — makes the leaf select the WC PAT slot Phase 96 already programmed (`PAT_WITH_WC` puts WC at index 2, the slot a PCD-alone PTE picks). Because the FB frames are MMIO (the `BIT_11` "device frame — do not free on teardown" marker), there is no cache-coherency hazard with normal RAM; WC's weak ordering is handled by the present-path `sfence`. This is the literal completion of the Phase 96 WC work for the compositor surface.

### `usb-hid` / `mouse_server` / `InputDispatcher` — the pointer datapath

`usb-hid` owns no hardware; it talks IPC to the xHCI `usb` server and to `mouse_server`. `inject_pointer` fires a `PointerEvent` wire packet at `MOUSE_EVENT_INJECT`; `mouse_server`'s `enqueue`/`dequeue` slot ring serves injected events ahead of its PS/2 pipeline; `display_server`'s pointer source pulls on `MOUSE_EVENT_PULL` and hands the event to the `InputDispatcher`, whose `PointerRouteDecision` carries the delivery target and an optional focus change. The dispatcher is pure logic in `kernel-core` and owns no compositor state — the compositor wraps its registry/focus tracker per call. Track C changes none of this; it proves it against a real USB mouse on the dock-hub topology.

### `stdin_feeder` — text-mode keyboard bridge

A pure scancode-to-byte bridge: it requests one scancode at a time from `kbd_server` via the non-blocking `KBD_TRY_READ` probe (so `display_server`'s concurrent `KBD_EVENT_PULL` requests are never starved), sleeps ~5 ms when empty, and yields PS/2 ownership once a graphical client owns the FB. Track D adds a second drain — typed `KeyEvent`s on `KBD_EVENT_PULL` — so USB keyboards work at the text login, not just PS/2.

## How This Builds on Earlier Phases

- **Finishes Phase 96's WC framebuffer work.** 96 reprogrammed `IA32_PAT` index 2 to WC and remapped the *kernel console* FB; Track B applies the same WC slot to the *userspace compositor* FB in `sys_framebuffer_mmap` — the one mapping 96 left write-back.
- **Continues the Phase 96 bare-metal line** — reuses its boot-rescue kernel fixes (bounded LAPIC calibration, bounded COM1 RX drain), the `usb-logsink` boot.log, the AMT-SOL capture runbook, and the network log sink as the Track E evidence path.
- **Reuses the Phase 56/68 compositor + the Phase 71/72 greeter/session-manager** stack unchanged — Track A only adds the missing *spawn* on the diskless path; the compositor, `InputDispatcher`, focus routing, and greeter login form already work under QEMU.
- **Depends on Phase 99 (SMP robustness)** because the laptop is 8-core and cannot pin `-smp 1` the way the toolchain gates do; the compositor + input servers run live across cores, so the lost-wakeup / SMP-fault bug class Phase 99 retires must be closed first.
- **Sits before Phase 102 (I2C-HID touchpad)** — the USB mouse is the *interim* pointer; the real built-in pointer (and its ACPI `_CRS` prerequisite, Phase 101) comes later. Track C's `mouse_server`/`InputDispatcher` seam is exactly where the touchpad will inject.

## Implementation Outline

1. **Track A** — append `display_server`/`mouse_server`/`session_manager`/`greeter` (+ `audio_server`/`term` as needed) to `BUILTIN_CONFIGS` with correct `depends=` edges; add a builtin-path graphical toggle standing in for the absent `/etc/m3os-graphical-only` marker; apply the greeter-vs-text skip decision on the builtin path so init yields the tty to the greeter; verify the QEMU/data-disk boot is unchanged.
2. **Track B** — add `PageTableFlags::NO_CACHE` to the `sys_framebuffer_mmap` PTE flags; assert `pat::init` ran on every compositor-eligible core; add the present-path `sfence`; measure full-screen blit latency WC vs WB on the laptop.
3. **Track C** — bare-metal-validate the `usb-hid → mouse_server → InputDispatcher` pointer datapath through the dock/`usbhub` topology (cursor motion + focus-on-click), capturing the injected-event count + a focus-change sentinel.
4. **Track D** — teach `stdin_feeder` to drain `KBD_EVENT_PULL` typed `KeyEvent`s for USB keyboards in text mode; convert `usb-hid` (and `usbhub`) from the 5 ms busy-poll to a notification-driven wait; confirm idle core occupancy drops.
5. **Track E** — add the on-device render assertion (changed-scanline/hash over the log sink) and/or the committed panel photo; run the full bare-metal protocol from `docs/appendix/bare-metal-validation.md` and record `Validated-on-HW (run N, date)`.

## Acceptance Criteria

- **CI / host-testable** (everything QEMU and the host *can* test stays a real gate): the QEMU GUI-session boot still reaches the compositor + greeter render with the data-disk path **unchanged** (no regression in the existing GUI gates); `add_builtin_defaults` builtin-path entries parse through `parse_service_def` (host/unit coverage); `sys_framebuffer_mmap` returns a mapping whose PTE has PCD set / PWT clear / PAT clear (WC index 2) — assertable in a QEMU boot by reading back the leaf flags; the `usb-hid → mouse_server → InputDispatcher` decode + focus-routing logic stays green in its existing unit/host tests.
- **Validated-on-HW (run N, date)** — `Dell Precision 5560 / Tiger Lake`; evidence captured per `docs/appendix/bare-metal-validation.md`:
  - On the laptop, `display_server` takes the framebuffer and the `greeter` login **renders on the panel**, proven by the Track E on-device render assertion (a non-trivial changed-scanline/hash sentinel over the `usb-logsink` boot.log + network sink) and/or a committed dated panel photo — not by a bare "it looked right."
  - A **USB mouse moves the compositor cursor and focus follows** the pointer (button-down over a `Toplevel` changes focus), captured as a non-zero injected `PointerEvent` count + a `PointerRouteDecision` focus-change sentinel in the log.
  - The **WC user-FB measurably improves blit latency** vs write-back on the laptop — a recorded full-screen-fill timing showing the WC mapping is materially faster than the write-back baseline (target order-of-magnitude on real MMIO; the exact ratio recorded in the run).
  - A **USB keyboard works in text mode** via `stdin_feeder` (typed `KeyEvent` drain) at the framebuffer login on the laptop.
  - `usb-hid`/`usbhub` **no longer pin a core at idle** — recorded idle-CPU evidence (e.g. an idle-tick/occupancy sentinel) shows the busy-poll is gone.

## Companion Task List

- [Phase 100 Task List](./tasks/100-bare-metal-gui-session-tasks.md)

## How Real OS Implementations Differ

- A production OS maps a GPU framebuffer through a **DRM/KMS** driver that negotiates the optimal memory type per-surface (WC for the scanout buffer, write-back for shadow buffers, with explicit `clflush`/`sfence` on present); m3OS uses a single WC mapping of the firmware GOP framebuffer with no real GPU — there is no acceleration, just CPU blits the WC mapping makes tolerable.
- Real desktops drive the pointer from a **kernel input subsystem** (Linux `evdev`, libinput acceleration/palm-rejection) feeding a Wayland/X compositor; m3OS keeps the decode in a ring-3 class driver and the routing in a pure-logic `InputDispatcher`, trading features for a clean microkernel seam.
- A daily-driver laptop reaches the desktop with the **built-in** pointer (I2C-HID touchpad) and keyboard; using an external USB mouse as the pointer is an explicit **interim** scaffold here (Phase 102 replaces it) — production bring-up would not ship that way.
- Mature stacks idle their USB class drivers via **interrupt-driven URBs + runtime power management**; m3OS's busy-poll is a bring-up shortcut Track D only partially retires (full USB runtime PM is later, with Phase 103 power management).
- Production projects validate "the screen shows X" with automated panel capture rigs or HDMI grabbers; m3OS substitutes an on-device render assertion + a photo because there is no capture hardware in the loop — a deliberate teaching-OS compromise documented in the bare-metal validation strategy.

## Deferred Until Later

- **I2C-HID touchpad** (the *real* built-in pointer — Intel LPSS DesignWare I2C + I2C-HID + multitouch parse) → **Phase 102**, which depends on the **Phase 101 ACPI** `_CRS` enumeration that supplies the controller's I2C address + GPIO interrupt. The USB mouse here is the interim stand-in.
- **Laptop power management** (battery, brightness, lid/power-button SCI, cpufreq) → **Phase 103**; full USB runtime power management (the complete fix for the Track D busy-poll) rides that arc.
- **Intel AX201 / CNVi Wi-Fi + supplicant** → **Phase 104** — the laptop's only built-in NIC; until then networking is the Phase 96 `ure` USB dongle.
- **Native GUI toolkit + core desktop apps** (a widget toolkit, clipboard, screenshot tool, settings/control panel) → **Phase 105**; this phase only brings up the *session* (compositor + greeter login + pointer), not applications.
- **GPU acceleration / per-surface memory-type management / multi-monitor** — out of scope; the single WC firmware-FB mapping is the ceiling here.
- **Bare-metal audio** (HDA vs SoundWire+SOF on Tiger Lake) → **Phase 109**; `audio_server` may be spawned by Track A but its real-hardware codec path is not validated here.
