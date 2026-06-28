# Phase 100 — Bare-Metal GUI Session (Dell Tiger Lake): Task List

**Status:** In progress (target: `Implemented (HW-unvalidated)` — see Track E; terminal `Validated-on-HW` requires a recorded run on the physical Dell Precision 5560)
**Source Ref:** phase-100
**Depends on:** Phase 99 (SMP & Scheduler Robustness Hardening) ✅, Phase 56/68/71/72/73 (display_server / compositor clients / greeter / session_manager) ✅, Phase 96 (Bare-Metal Bring-up + console-FB write-combining) ✅
**Goal:** Boot the physical Dell Precision 5560 (Tiger Lake) to a usable graphical session — `display_server` takes the framebuffer, `greeter` renders the login, an interim USB mouse moves the cursor with focus following, and the keyboard works — by (A) spawning the existing-but-unspawned graphical stack on init's diskless bare-metal boot path, (B) finishing the Phase 96 write-combining work for the *userspace* framebuffer in `sys_framebuffer_mmap`, (C) bare-metal-validating the `usb-hid → mouse_server → InputDispatcher` pointer datapath, (D) folding in the open Phase 96 input-polish handoff (USB text-mode keyboard + de-busy-polling `usb-hid`/`usbhub`), and (E) defining the on-metal "the screen shows the greeter" validation method. HW-only: there is no QEMU model for the real panel/MMIO-FB/pointer behavior, so validation follows `docs/appendix/bare-metal-validation.md` and the status convention is **Validated-on-HW (run N, date)**, not a bare "Complete."

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Spawn `display_server`/`mouse_server`/`session_manager`/`greeter` (+`audio_server`/`term`) on init's bare-metal `BUILTIN_CONFIGS` path + resolve the graphical skip-filter so init yields the tty to the greeter | — | In progress |
| B | Write-combining user framebuffer — WC PAT attribute on the `sys_framebuffer_mmap` VMA + present-path `sfence` + blit-latency measurement | — | In progress |
| C | Interim USB-mouse pointer — bare-metal-validate `usb-hid` PointerEvent → `mouse_server` → `InputDispatcher` focus routing on the dock-hub topology | A, B | Planned |
| D | Input polish — `stdin_feeder` USB-keyboard text-mode drain + convert `usb-hid`/`usbhub` busy-poll to notification-driven waits | A | Planned |
| E | Bare-metal validation method ("the screen shows the greeter") — on-device render assertion over the log sink + photo evidence convention | A, C | Planned |

---

## Track A — Spawn the Graphical Stack on the Bare-Metal Boot Path

### A.1 — Add the graphical-stack entries to `BUILTIN_CONFIGS`

**File:** `userspace/init/src/main.rs`
**Symbol:** `Manager::add_builtin_defaults` / `BUILTIN_CONFIGS`
**Why it matters:** A diskless bare-metal USB boot has no `/etc/services.d`, so init falls back to `add_builtin_defaults()`; today that list is the Phase 96 minimal set (`console`/`kbd`/`stdin_feeder`/USB/`ure`/`telnetd`/`sshd`) and explicitly "no display/greeter/audio", so the laptop comes up at a text console with the compositor never spawned.

**Acceptance:**
- [ ] `BUILTIN_CONFIGS` gains `display_server`, `mouse_server`, `session_manager`, and `greeter` entries (and `audio_server`/`term` as needed), each parsed through `parse_service_def` like the existing entries.
- [ ] Dependency edges match the data-disk `KNOWN_CONFIGS` semantics: `greeter`/`term` `depends=display_server`; `session_manager` supervises the session; `mouse_server` has no graphical dep (peer of `kbd`).
- [ ] On a diskless boot the new services appear in the dependency graph and start in topological order (no `unresolvable dependency` / `deps not ready` log for them).

### A.2 — Resolve the graphical skip-filter on the builtin path

**File:** `userspace/init/src/main.rs`
**Symbol:** `skip_for_greeter_filter` / `graphical_only_enabled` / `GRAPHICAL_ONLY_MARKER_PATH` (`/etc/m3os-graphical-only`)
**Why it matters:** The greeter-vs-text decision is driven by the `/etc/m3os-graphical-only` marker file, which lives on the **absent** data disk; `skip_for_greeter_filter` also runs only on the `KNOWN_CONFIGS`/dir-scan path, not the builtin path — so without a builtin-path toggle init would either skip the greeter or start a text login competing for the same console.

**Acceptance:**
- [ ] The builtin path makes the same greeter-vs-text choice as `skip_for_greeter_filter` (greeter selected when graphical mode is on), via a toggle that does not require the absent marker file (e.g. a builtin-defaults graphical flag / a kernel-cmdline or FB-present heuristic), documented in a code comment referencing the marker it stands in for.
- [ ] In graphical mode on the diskless boot, init does **not** also bring up a foreground text login on the FB console — the tty is yielded to the greeter (the `GREETER_ONLY_SKIPPED_CONFS` / `GRAPHICAL_ONLY_SKIPPED_CONFS` intent holds on the builtin path).
- [ ] `display_server` claims the FB via `try_yield_console` with `raw_input_enabled=false` (exec path `/bin/display_server`), so `stdin_feeder` backs off PS/2-to-stdin once the compositor owns the console.

### A.3 — No regression to the QEMU / data-disk boot

**Files:**
- `userspace/init/src/main.rs`
- `xtask/src/main.rs` (`populate_ext2_files`)

**Symbol:** the `KNOWN_CONFIGS` dir-scan path vs the `add_builtin_defaults` fallback
**Why it matters:** The data-disk GUI boot under QEMU must be byte-for-byte unchanged — the builtin path is a *fallback*, and a stack started twice (once from disk, once from builtin) would double-claim the FB.

**Acceptance:**
- [ ] A data-disk boot (QEMU `run-gui`) still loads the graphical stack from `/etc/services.d/*.conf` and **not** from `add_builtin_defaults` (the builtin path runs only when `/etc/services.d` is absent/unreadable).
- [ ] The existing QEMU GUI-session boot reaches the compositor + greeter render with no new double-spawn / FB-claim error in the log.

---

## Track B — Write-Combining User Framebuffer

### B.1 — Add the WC PAT attribute to the user FB mapping

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_framebuffer_mmap` (the `PageTableFlags` built for the `map_user_frames` call, ~line 14483)
**Why it matters:** The compositor FB is mapped `PRESENT | WRITABLE | USER_ACCESSIBLE | NO_EXECUTE | BIT_11` with **no `NO_CACHE`/PCD bit**, so it decodes write-back; on real MMIO that makes every per-frame blit a stream of single-store bus transactions (10–50× slower than WC). Phase 96 fixed only the kernel console FB.

**Acceptance:**
- [ ] The `sys_framebuffer_mmap` PTE flags include `PageTableFlags::NO_CACHE` (PCD set), with `WRITE_THROUGH`/PWT **clear** and the PAT bit **clear**, so a 4 KiB leaf selects PAT index 2 (= WC after `pat::init`) — mirroring `pat.rs::set_range_write_combining`'s type selection.
- [ ] The recorded `MemoryMapping` for the FB VMA is unchanged in `prot`/`flags` semantics; the mapping still survives the existing `tlb_shootdown_range` + `bump_generation`.
- [ ] A QEMU boot reads back the mapped leaf's flags and confirms PCD=1 / PWT=0 / PAT=0 (WC index 2) — falsifiable without real MMIO.

### B.2 — Confirm per-core PAT programming + present-path ordering

**Files:**
- `kernel/src/arch/x86_64/pat.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_framebuffer_pageflip`)

**Symbol:** `pat::init` (BSP early-init + `smp::boot::ap_entry`); the present/page-flip path
**Why it matters:** PAT is per-core — the Intel SDM requires every logical CPU mapping a shared WC region to have the same PAT — and WC is weakly ordered, so the compositor's present must `sfence` before signalling the flip or a half-written frame can be latched.

**Acceptance:**
- [ ] `pat::init` is confirmed to run on every core the compositor can be scheduled on (BSP + each AP at `ap_entry`, before any WC user mapping is faulted in); documented at the `sys_framebuffer_mmap` change site.
- [ ] The present / `sys_framebuffer_pageflip` path issues an `sfence` (or equivalent store barrier) before the flip is signalled, so WC writes are globally visible first.

### B.3 — Measure blit latency on the laptop (WC vs write-back)

**File:** `docs/appendix/bare-metal-validation.md` results reference (or a `scripts/*-validate.md` results appendix)
**Symbol:** a full-screen-fill timing captured over the log sink
**Why it matters:** The phase's premise is "the WC user-FB measurably improves blit latency"; on QEMU the RAM FB makes WC vs WB negligible, so this is a bare-metal-only measurement and must be recorded as real evidence, not assumed.

**Acceptance:**
- [ ] A recorded full-screen-fill (or representative blit) timing on the laptop shows the WC mapping is materially faster than a write-back baseline (target order-of-magnitude on real MMIO; the exact ratio recorded).
- [ ] The measurement method (how the timing was taken and emitted over `usb-logsink`/network sink) is documented so the run is reproducible.

---

## Track C — Interim USB-Mouse Pointer (Bare-Metal-Validated)

### C.1 — Bare-metal-validate the `usb-hid → mouse_server` inject path on the dock-hub topology

**Files:**
- `userspace/drivers/usb-hid/src/main.rs`
- `userspace/mouse_server/src/main.rs`

**Symbol:** `usb-hid::inject_pointer` (`MOUSE_EVENT_INJECT = 3`) → `mouse_server` `enqueue`/`dequeue` → `MOUSE_EVENT_PULL = 1`
**Why it matters:** The decode + inject path has only ever run against an emulated PS/2 mouse under QEMU; the laptop's pointer is a USB mouse behind the dock/`usbhub` walker, so this is the first time the path runs against a real pointer through a real hub on bare metal.

**Acceptance:**
- [ ] With a USB mouse attached (behind the dock/`usbhub`), `usb-hid` decodes its reports into `PointerEvent`s and injects them; a **non-zero** injected-event count is captured in the log over the dock-hub topology.
- [ ] `mouse_server` serves the injected events ahead of its PS/2 pipeline (injected-first drain) and `display_server`'s pointer source pulls them on `MOUSE_EVENT_PULL`.

### C.2 — Cursor motion + focus-follows-pointer on the panel

**Files:**
- `userspace/display_server/src/input.rs`
- `kernel-core/src/input/dispatch.rs`

**Symbol:** `InputDispatcher` / `PointerRouteDecision` (`focus_change`)
**Why it matters:** The dispatcher and its focus routing are pure-logic + QEMU-PS/2-validated only; this proves the compositor cursor and focus state respond to a real pointer on hardware (the falsifiable "the pointer works" arm).

**Acceptance:**
- [ ] Moving the USB mouse moves the compositor cursor on the panel (captured as continuous `PointerEvent` motion + a render-assertion delta, cross-referenced with Track E).
- [ ] A button-down over a `Toplevel` produces a `PointerRouteDecision` with a `focus_change`, and the focus sentinel is captured in the log — focus follows the click.
- [ ] No new dispatch logic was added (this is a validation arm); any change is confined to logging/sentinels.

---

## Track D — Input Polish (Open Phase 96 Handoff)

> Folds `docs/handoffs/2026-06-25-usb-log-persistence-and-keyboard.md` (keyboard) + the `usb-hid`/`usbhub` busy-poll item.

### D.1 — `stdin_feeder` drains USB keyboard events in text mode

**Files:**
- `userspace/stdin_feeder/src/main.rs`
- `userspace/kbd_server/src/main.rs`

**Symbol:** `stdin_feeder` main drain loop; `kbd_server` `KBD_EVENT_PULL = 2` / `KBD_EVENT_NONE` (and the existing `KBD_TRY_READ` PS/2 path)
**Why it matters:** `stdin_feeder` only drains PS/2 scancodes (`KBD_TRY_READ`); a USB keyboard's input reaches `kbd_server` as typed `KeyEvent`s via `usb-hid`'s `KBD_EVENT_INJECT`, which `stdin_feeder` never reads — so a USB keyboard is dead at the text login on a machine with no PS/2 keyboard.

**Acceptance:**
- [ ] `stdin_feeder` also drains typed `KeyEvent`s on `KBD_EVENT_PULL` (honoring the `KBD_EVENT_NONE` empty/timeout sentinel) and converts them to stdin bytes, alongside the existing PS/2 `KBD_TRY_READ` path.
- [ ] The two drains do not starve `display_server`'s concurrent `KBD_EVENT_PULL` requests (the non-blocking-probe discipline is preserved).
- [ ] On the laptop (or via `--usb-passthrough` of a USB keyboard), typing on a USB keyboard echoes at the framebuffer text login before the compositor takes the FB.

### D.2 — Convert `usb-hid` from 5 ms busy-poll to a notification-driven wait

**File:** `userspace/drivers/usb-hid/src/main.rs`
**Symbol:** the main `loop` + `POLL_INTERVAL_NS` (5 ms `nanosleep_for` cadence)
**Why it matters:** `usb-hid` polls every device's interrupt-IN endpoint every 5 ms forever, pinning a core at idle (and burning battery the moment Phase 103 lands); the xHCI server already captures reports on its IRQ, so the driver can block on a notification instead.

**Acceptance:**
- [ ] `usb-hid` blocks on an xHCI transfer-event notification (or a bounded wait that idles the core) rather than spinning on the fixed 5 ms cadence; input latency stays below one report period when events arrive.
- [ ] Hot-plug attach/detach reconcile (the existing ~200 ms cadence behavior) still works without the busy spin.
- [ ] Recorded idle-CPU evidence shows `usb-hid` no longer keeps a core hot at idle.

### D.3 — Convert `usbhub` walker off the busy-poll

**File:** `userspace/drivers/usbhub/src/main.rs`
**Symbol:** the hub-walk loop (`nanosleep_for` settle/reset spins)
**Why it matters:** The hub walker similarly spins; combined with `usb-hid` it keeps cores hot at idle on the laptop.

**Acceptance:**
- [ ] `usbhub` waits on a notification / bounded idle rather than a tight walk loop once enumeration is steady; port-status-change still triggers (re)enumeration promptly.
- [ ] Recorded idle-CPU evidence shows the hub walker no longer pins a core.

---

## Track E — Bare-Metal Validation Method ("the screen shows the greeter")

### E.1 — On-device render assertion over the log sink

**Files:**
- `userspace/display_server` (or `userspace/greeter`) render-assertion hook
- `docs/appendix/bare-metal-validation.md` (the protocol this implements)

**Symbol:** a changed-scanline-count / cheap-hash of the compositor's own output, emitted over `usb-logsink` + the network sink
**Why it matters:** There is no QMP/PPM screendump on bare metal (the QEMU-only path the `less-render-probe`/`claude_tui_render_arm` gates use), so "the screen shows the greeter" needs a falsifiable on-metal substitute — the on-device analog of the PPM band-diff.

**Acceptance:**
- [ ] The compositor/greeter computes and prints a cheap render fingerprint (changed-scanline count or hash) over the log sink; a blank screen yields ≈0 and the rendered greeter yields a non-trivial value (threshold recorded), so the sentinel falsifiably distinguishes "rendered" from "black."
- [ ] The sentinel string is quoted in the Track-E / phase acceptance and reused for the Track C cursor-motion delta.

### E.2 — Photo evidence convention + recorded HW run

**Files:**
- `docs/appendix/bare-metal-validation.md` (results convention)
- a phase evidence pointer (committed photo and/or runbook results appendix)

**Symbol:** the recorded `Validated-on-HW (run N, date)` entry
**Why it matters:** Per the bare-metal validation strategy, a HW-only phase is Validated only when the host/QEMU surface is green **and** a recorded physical run cleared the un-modelable remainder — captured, not asserted from memory.

**Acceptance:**
- [ ] A dated panel photo (and/or the captured boot.log + render sentinel) is committed/referenced as the evidence artifact for the greeter render on `Dell Precision 5560 / Tiger Lake`.
- [ ] The phase design doc + README Status carry `Validated-on-HW (run N, YYYY-MM-DD)` with the machine + evidence pointer once the run is recorded (Status stays `Planned` / `Implemented (HW-unvalidated)` until then — never a bare "Complete").
- [ ] The recorded run confirms all five HW acceptance arms together: greeter renders, USB mouse moves the cursor + focus follows, WC blit-latency win measured, USB keyboard works in text mode, and `usb-hid`/`usbhub` idle-CPU is flat.

---

## Documentation Notes

- This phase **finishes** the Phase 96 write-combining work: 96 applied WC (`pat::set_range_write_combining`, `IA32_PAT` index 2) to the *kernel console* FB only; Track B applies the same WC slot to the *userspace compositor* FB in `sys_framebuffer_mmap`. Record that the user-FB was write-back until now.
- Track A only adds the *spawn* of an already-existing, QEMU-validated graphical stack on the diskless boot path — no compositor/greeter logic changes. Note that the data-disk `KNOWN_CONFIGS` GUI boot is intentionally left untouched (the builtin path is a fallback).
- The USB mouse is an **interim** pointer; the real built-in pointer is the Phase 102 I2C-HID touchpad (gated on Phase 101 ACPI `_CRS`). Track C's `mouse_server`/`InputDispatcher` inject seam is the exact point the touchpad will reuse — keep it bus-agnostic.
- Track D folds the open Phase 96 keyboard handoff; the busy-poll removal is a *partial* step toward power efficiency — full USB runtime power management rides Phase 103.
- HW-only phase: follow `docs/appendix/bare-metal-validation.md` and use `Validated-on-HW (run N, date)` rather than `Complete`. Maximize the CI-testable surface (host/unit tests for the init parse + the WC PTE flags + the input decode/routing logic) so the un-modelable remainder is as small as possible.
- Prefer exact files/symbols over directories as these land; update the checkboxes and Track Layout statuses as tracks complete, and add the recorded HW-run pointer to Track E when validated.
