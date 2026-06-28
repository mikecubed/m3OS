# Phase 100 — Bare-Metal GUI Session (Dell Tiger Lake): Task List

**Status:** Implemented (HW-unvalidated) — all five tracks' CI-testable surface is green (per-track acceptance below). Terminal `Validated-on-HW (run N, date)` awaits a recorded run on the physical Dell Precision 5560 for the un-modelable arms (panel render, real USB pointer + focus-on-click, WC blit-latency ratio, USB-kbd text login, flat idle-CPU).
**Source Ref:** phase-100
**Depends on:** Phase 99 (SMP & Scheduler Robustness Hardening) ✅, Phase 56/68/71/72/73 (display_server / compositor clients / greeter / session_manager) ✅, Phase 96 (Bare-Metal Bring-up + console-FB write-combining) ✅
**Goal:** Boot the physical Dell Precision 5560 (Tiger Lake) to a usable graphical session — `display_server` takes the framebuffer, `greeter` renders the login, an interim USB mouse moves the cursor with focus following, and the keyboard works — by (A) spawning the existing-but-unspawned graphical stack on init's diskless bare-metal boot path, (B) finishing the Phase 96 write-combining work for the *userspace* framebuffer in `sys_framebuffer_mmap`, (C) bare-metal-validating the `usb-hid → mouse_server → InputDispatcher` pointer datapath, (D) folding in the open Phase 96 input-polish handoff (USB text-mode keyboard + de-busy-polling `usb-hid`/`usbhub`), and (E) defining the on-metal "the screen shows the greeter" validation method. HW-only: there is no QEMU model for the real panel/MMIO-FB/pointer behavior, so validation follows `docs/appendix/bare-metal-validation.md` and the status convention is **Validated-on-HW (run N, date)**, not a bare "Complete."

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Spawn `display_server`/`mouse_server`/`session_manager`/`greeter` (+`audio_server`/`term`) on init's bare-metal `BUILTIN_CONFIGS` path + resolve the graphical skip-filter so init yields the tty to the greeter | — | Implemented (CI-green) |
| B | Write-combining user framebuffer — WC PAT attribute on the `sys_framebuffer_mmap` VMA + present-path `sfence` + blit-latency measurement | — | Implemented (CI-green; B.3 blit-ratio HW-pending) |
| C | Interim USB-mouse pointer — bare-metal-validate `usb-hid` PointerEvent → `mouse_server` → `InputDispatcher` focus routing on the dock-hub topology | A, B | Sentinels in place (CI-green decode/inject + dispatch host tests); cursor-on-panel + focus-on-real-click HW-pending |
| D | Input polish — `stdin_feeder` USB-keyboard text-mode drain + convert `usb-hid`/`usbhub` busy-poll to notification-driven waits | A | Implemented (CI-green; idle-CPU + USB-kbd-echo HW-pending; full notification → Phase 103) |
| E | Bare-metal validation method ("the screen shows the greeter") — on-device render assertion over the log sink + photo evidence convention | A, C | E.1 Implemented (CI-green); E.2 photo + recorded HW run pending |

---

## Track A — Spawn the Graphical Stack on the Bare-Metal Boot Path

### A.1 — Add the graphical-stack entries to `BUILTIN_CONFIGS`

**File:** `userspace/init/src/main.rs`
**Symbol:** `Manager::add_builtin_defaults` / `BUILTIN_CONFIGS`
**Why it matters:** A diskless bare-metal USB boot has no `/etc/services.d`, so init falls back to `add_builtin_defaults()`; today that list is the Phase 96 minimal set (`console`/`kbd`/`stdin_feeder`/USB/`ure`/`telnetd`/`sshd`) and explicitly "no display/greeter/audio", so the laptop comes up at a text console with the compositor never spawned.

**Acceptance:**
- [x] `BUILTIN_CONFIGS` gains `display`, `mouse_server`, `session_manager`, `audio_server`, and `greeter` entries, each parsed through `parse_service_def` like the existing entries. (`term` is intentionally omitted from the builtin set — it is a post-login client launched by `session_manager`, and the data-disk path skips `term.conf` in default mode too.)
- [x] Dependency edges match the data-disk `KNOWN_CONFIGS` semantics: `greeter` `depends=display,kbd,mouse_server,audio_server` (verbatim from data-disk `greeter.conf`, xtask:26713); `audio_server depends=display`; `session_manager` has no deps (it supervises); `mouse_server` has no graphical dep (peer of `kbd`).
- [x] Host-test-covered: `builtin_graphical_stack_dep_graph_has_no_cycles` + per-entry parse tests assert the new entries parse and form an acyclic graph; the data-disk path is unchanged (`compositor-stress` PASS).

### A.2 — Resolve the graphical skip-filter on the builtin path

**File:** `userspace/init/src/main.rs`
**Symbol:** `skip_for_greeter_filter` / `graphical_only_enabled` / `GRAPHICAL_ONLY_MARKER_PATH` (`/etc/m3os-graphical-only`)
**Why it matters:** The greeter-vs-text decision is driven by the `/etc/m3os-graphical-only` marker file, which lives on the **absent** data disk; `skip_for_greeter_filter` also runs only on the `KNOWN_CONFIGS`/dir-scan path, not the builtin path — so without a builtin-path toggle init would either skip the greeter or start a text login competing for the same console.

**Acceptance:**
- [x] The builtin path makes the same greeter-vs-text choice via a pure `decide_graphical(boot_override, diskless, marker_present)` + `effective_graphical_mode()`: the diskless builtin-defaults path defaults to graphical (standing in for the absent `/etc/m3os-graphical-only` marker; an explicit `/proc/m3os-boot-mode=serial` override still forces text). 4 host truth-table tests cover the decision; documented in code comments referencing the marker.
- [x] In graphical mode the serial autologin is gated on the **same** `effective_graphical_mode()` as the greeter filter, so init does **not** bring up a competing foreground text login on the diskless graphical boot (the two decision sites can never disagree).
- [x] `display_server` exec path `/bin/display_server`; `stdin_feeder` backs off PS/2-to-stdin once `display_server` registers the `display.input-owner` IPC service (`compositor-stress` log confirms registration).

### A.3 — No regression to the QEMU / data-disk boot

**Files:**
- `userspace/init/src/main.rs`
- `xtask/src/main.rs` (`populate_ext2_files`)

**Symbol:** the `KNOWN_CONFIGS` dir-scan path vs the `add_builtin_defaults` fallback
**Why it matters:** The data-disk GUI boot under QEMU must be byte-for-byte unchanged — the builtin path is a *fallback*, and a stack started twice (once from disk, once from builtin) would double-claim the FB.

**Acceptance:**
- [x] A data-disk boot still loads the graphical stack from `/etc/services.d/*.conf` and **not** from `add_builtin_defaults` (`diskless_builtin` is set only on the `count == 0` fallback in `load_services`).
- [x] The existing QEMU GUI-session boot reaches the compositor + greeter render with no new double-spawn / FB-claim error: `compositor-stress PASSED (no kernel panic)`, `display_server registered as 'display.input-owner' (first Toplevel mapped)` once.

---

## Track B — Write-Combining User Framebuffer

### B.1 — Add the WC PAT attribute to the user FB mapping

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_framebuffer_mmap` (the `PageTableFlags` built for the `map_user_frames` call, ~line 14483)
**Why it matters:** The compositor FB is mapped `PRESENT | WRITABLE | USER_ACCESSIBLE | NO_EXECUTE | BIT_11` with **no `NO_CACHE`/PCD bit**, so it decodes write-back; on real MMIO that makes every per-frame blit a stream of single-store bus transactions (10–50× slower than WC). Phase 96 fixed only the kernel console FB.

**Acceptance:**
- [x] The `sys_framebuffer_mmap` PTE flags include `PageTableFlags::NO_CACHE` (PCD set), with `WRITE_THROUGH`/PWT **clear** and the PAT bit **clear**, so a 4 KiB leaf selects PAT index 2 (= WC after `pat::init`).
- [x] The recorded `MemoryMapping` for the FB VMA is unchanged in `prot`/`flags` semantics (the `1 | FB_MAPPING_FLAG` line is untouched); the mapping still survives the existing `tlb_shootdown_range` + `bump_generation`.
- [x] **Validated in QEMU**: the readback sentinel `[fb-wc] user FB leaf flags: PCD=1 PWT=0 PAT=0 (WC idx2)` appears in the `compositor-stress` serial log — confirms PCD=1 / PWT=0 / PAT=0 (WC index 2).

### B.2 — Confirm per-core PAT programming + present-path ordering

**Files:**
- `kernel/src/arch/x86_64/pat.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_framebuffer_pageflip`)

**Symbol:** `pat::init` (BSP early-init + `smp::boot::ap_entry`); the present/page-flip path
**Why it matters:** PAT is per-core — the Intel SDM requires every logical CPU mapping a shared WC region to have the same PAT — and WC is weakly ordered, so the compositor's present must `sfence` before signalling the flip or a half-written frame can be latched.

**Acceptance:**
- [x] `pat::init` is confirmed to run on every core the compositor can be scheduled on (BSP `kernel/src/lib.rs:309` + each AP `kernel/src/smp/boot.rs:423`, before any WC user mapping is faulted in); documented at the `sys_framebuffer_mmap` change site.
- [x] The present / `sys_framebuffer_pageflip` path issues an inline-asm `sfence` before the flip is signalled (`crate::fb::vbe::pageflip`), so WC writes are globally visible first. (Soft-float-safe: `sfence` touches no XMM.)

### B.3 — Measure blit latency on the laptop (WC vs write-back)

**File:** `docs/appendix/bare-metal-validation.md` results reference (or a `scripts/*-validate.md` results appendix)
**Symbol:** a full-screen-fill timing captured over the log sink
**Why it matters:** The phase's premise is "the WC user-FB measurably improves blit latency"; on QEMU the RAM FB makes WC vs WB negligible, so this is a bare-metal-only measurement and must be recorded as real evidence, not assumed.

**Acceptance:**
- [ ] **HW-pending**: a recorded full-screen-fill (or representative blit) timing on the laptop shows the WC mapping is materially faster than a write-back baseline (target order-of-magnitude on real MMIO; the exact ratio recorded). QEMU's RAM-FB makes WC≈WB, so this ratio is a Dell-only measurement.
- [x] The measurement method (how the timing is taken and emitted over `usb-logsink`/network sink) is documented in `scripts/phase-100-bare-metal-validate.md` so the run is reproducible.

---

## Track C — Interim USB-Mouse Pointer (Bare-Metal-Validated)

### C.1 — Bare-metal-validate the `usb-hid → mouse_server` inject path on the dock-hub topology

**Files:**
- `userspace/drivers/usb-hid/src/main.rs`
- `userspace/mouse_server/src/main.rs`

**Symbol:** `usb-hid::inject_pointer` (`MOUSE_EVENT_INJECT = 3`) → `mouse_server` `enqueue`/`dequeue` → `MOUSE_EVENT_PULL = 1`
**Why it matters:** The decode + inject path has only ever run against an emulated PS/2 mouse under QEMU; the laptop's pointer is a USB mouse behind the dock/`usbhub` walker, so this is the first time the path runs against a real pointer through a real hub on bare metal.

**Acceptance:**
- [x] `usb-hid` decodes pointer reports into `PointerEvent`s and injects them; the `USB_HID:pointer-injected count=<n>` sentinel fires in `usb-smoke` (serial: `USB_HID:mouse … moved=1` → `USB_HID:pointer-injected count=`). The inject path is CI-exercised; the **dock-hub topology** specificity is the HW arm.
- [x] `mouse_server` serves injected events ahead of its PS/2 pipeline (Phase 78c `PendingEdges` injected-first drain, unchanged) and `display_server`'s pointer source pulls them on `MOUSE_EVENT_PULL` — exercised end-to-end by `usb-smoke` (mouse decoded → injected → served → rendered at the term prompt).

### C.2 — Cursor motion + focus-follows-pointer on the panel

**Files:**
- `userspace/display_server/src/input.rs`
- `kernel-core/src/input/dispatch.rs`

**Symbol:** `InputDispatcher` / `PointerRouteDecision` (`focus_change`)
**Why it matters:** The dispatcher and its focus routing are pure-logic + QEMU-PS/2-validated only; this proves the compositor cursor and focus state respond to a real pointer on hardware (the falsifiable "the pointer works" arm).

**Acceptance:**
- [ ] **HW-pending**: moving the USB mouse moves the compositor cursor on the panel — captured as the small-`rows_changed` `RENDER_FP` delta (Track E reuse). `usb-smoke` confirms the `moved=1` decode + inject; cursor-on-real-panel is the HW arm.
- [ ] **HW-pending capture** (code done): a button-down over a `Toplevel` produces a `PointerRouteDecision.focus_change` and emits `INPUT:pointer-focus-change surface=<id>`. The focus-on-click logic is host-tested (`pointer_button_down_on_toplevel_requests_focus_change`) and the sentinel is wired at the apply site; capturing it on a real click is HW.
- [x] No new dispatch logic was added — `kernel-core/src/input/dispatch.rs` is untouched and its 3 `pointer_button_down` tests pass; the change is confined to the two log sentinels.

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
- [x] `stdin_feeder` also drains typed `KeyEvent`s on `KBD_EVENT_PULL` (honoring the `KBD_EVENT_NONE` empty/timeout sentinel) and converts them to stdin bytes (`kernel_core::input::hid_poll::key_event_to_stdin`, host-tested), alongside the existing PS/2 `KBD_TRY_READ` path. `termios-smoke` PASS confirms the PS/2 line-discipline path is unregressed.
- [x] The two drains do not starve `display_server`'s concurrent `KBD_EVENT_PULL` requests — `stdin_feeder` keeps the non-blocking probe and stands down entirely once `display.input-owner` is registered.
- [ ] **HW/passthrough-pending**: on the laptop (or via `--usb-passthrough` of a USB keyboard), typing on a USB keyboard echoes at the framebuffer text login before the compositor takes the FB.

### D.2 — Convert `usb-hid` from 5 ms busy-poll to a notification-driven wait

**File:** `userspace/drivers/usb-hid/src/main.rs`
**Symbol:** the main `loop` + `POLL_INTERVAL_NS` (5 ms `nanosleep_for` cadence)
**Why it matters:** `usb-hid` polls every device's interrupt-IN endpoint every 5 ms forever, pinning a core at idle (and burning battery the moment Phase 103 lands); the xHCI server already captures reports on its IRQ, so the driver can block on a notification instead.

**Acceptance:**
- [x] `usb-hid` uses a bounded adaptive backoff that idles the core (fast 5 ms cadence preserved while reports arrive — `usb-smoke` confirms live HID decode latency is unchanged — growing to a 100 ms idle cap via the host-tested `next_hid_backoff_ns`) rather than spinning on the fixed 5 ms cadence. *(Full xHCI transfer-event notification needs xHCI-server + `usb-core` protocol changes → deferred to Phase 103, as the design doc scopes; the adaptive backoff is the Phase 100 bring-up step.)*
- [x] Hot-plug attach/detach reconcile (now a monotonic ~200 ms timestamp, independent of backoff) still works without the busy spin: `usb-hotplug-smoke` PASS (3 attach/detach cycles, no slot exhaustion / daemon restart).
- [ ] **HW/long-run-pending**: recorded idle-CPU evidence shows `usb-hid` no longer keeps a core hot at idle. The `USB_HID:idle ticks=<n> backoff_ns=<n>` sentinel is in place; the idle-occupancy measurement is recorded on a longer/HW run.

### D.3 — Convert `usbhub` walker off the busy-poll

**File:** `userspace/drivers/usbhub/src/main.rs`
**Symbol:** the hub-walk loop (`nanosleep_for` settle/reset spins)
**Why it matters:** The hub walker similarly spins; combined with `usb-hid` it keeps cores hot at idle on the laptop.

**Acceptance:**
- [x] `usbhub` uses a bounded steady-state monitoring loop (50 ms → 200 ms backoff via host-tested `hub_next_backoff_ns`) rather than a tight walk once enumeration is steady; a `wPortChange` on `GET_PORT_STATUS` still triggers prompt (re)enumeration — `usb-hub-smoke` PASS (tier-2 enumeration via route string works).
- [ ] **HW/long-run-pending**: recorded idle-CPU evidence shows the hub walker no longer pins a core. The `USB_HUB:idle ticks=<n> backoff_ns=<n>` sentinel is in place.

---

## Track E — Bare-Metal Validation Method ("the screen shows the greeter")

### E.1 — On-device render assertion over the log sink

**Files:**
- `userspace/display_server` (or `userspace/greeter`) render-assertion hook
- `docs/appendix/bare-metal-validation.md` (the protocol this implements)

**Symbol:** a changed-scanline-count / cheap-hash of the compositor's own output, emitted over `usb-logsink` + the network sink
**Why it matters:** There is no QMP/PPM screendump on bare metal (the QEMU-only path the `less-render-probe`/`claude_tui_render_arm` gates use), so "the screen shows the greeter" needs a falsifiable on-metal substitute — the on-device analog of the PPM band-diff.

**Acceptance:**
- [x] The compositor computes and prints a cheap render fingerprint (changed-scanline count + sampled FNV hash) over the log sink on each damage-driven compose. Sentinel: **`RENDER_FP frame=<n> rows_nonblank=<R> rows_changed=<C> hash=0x<8hex>`**. Falsifiable: a blank/background frame yields `rows_nonblank=0` (host test `all_background_yields_zero_nonblank`); rendered content yields a non-trivial value — confirmed in QEMU (`compositor-stress` log: `RENDER_FP frame=2 rows_nonblank=1072 rows_changed=1064 …`, `rows_changed=0` when static). Threshold: `rows_nonblank ≥ 50` (≥ 200 on 1080p) distinguishes "rendered" from "black."
- [x] The sentinel string `RENDER_FP … rows_changed=<C>` is quoted here and reused for the Track C cursor-motion delta (a small `rows_changed` value on pointer motion).

### E.2 — Photo evidence convention + recorded HW run

**Files:**
- `docs/appendix/bare-metal-validation.md` (results convention)
- a phase evidence pointer (committed photo and/or runbook results appendix)

**Symbol:** the recorded `Validated-on-HW (run N, date)` entry
**Why it matters:** Per the bare-metal validation strategy, a HW-only phase is Validated only when the host/QEMU surface is green **and** a recorded physical run cleared the un-modelable remainder — captured, not asserted from memory.

**Acceptance:**
- [ ] **HW-pending**: a dated panel photo (and/or the captured boot.log + `RENDER_FP` sentinel) is committed/referenced as the evidence artifact for the greeter render on `Dell Precision 5560 / Tiger Lake`. The capture convention + sentinel index are documented in `scripts/phase-100-bare-metal-validate.md`.
- [x] The phase design doc + README Status now carry **`Implemented (HW-unvalidated)`** (the correct intermediate — never a bare "Complete"); they flip to `Validated-on-HW (run N, YYYY-MM-DD)` with the machine + evidence pointer once the run is recorded.
- [ ] **HW-pending**: the recorded run confirms all five HW acceptance arms together: greeter renders, USB mouse moves the cursor + focus follows, WC blit-latency win measured, USB keyboard works in text mode, and `usb-hid`/`usbhub` idle-CPU is flat.

---

## Documentation Notes

- This phase **finishes** the Phase 96 write-combining work: 96 applied WC (`pat::set_range_write_combining`, `IA32_PAT` index 2) to the *kernel console* FB only; Track B applies the same WC slot to the *userspace compositor* FB in `sys_framebuffer_mmap`. Record that the user-FB was write-back until now.
- Track A only adds the *spawn* of an already-existing, QEMU-validated graphical stack on the diskless boot path — no compositor/greeter logic changes. Note that the data-disk `KNOWN_CONFIGS` GUI boot is intentionally left untouched (the builtin path is a fallback).
- The USB mouse is an **interim** pointer; the real built-in pointer is the Phase 102 I2C-HID touchpad (gated on Phase 101 ACPI `_CRS`). Track C's `mouse_server`/`InputDispatcher` inject seam is the exact point the touchpad will reuse — keep it bus-agnostic.
- Track D folds the open Phase 96 keyboard handoff; the busy-poll removal is a *partial* step toward power efficiency — full USB runtime power management rides Phase 103.
- HW-only phase: follow `docs/appendix/bare-metal-validation.md` and use `Validated-on-HW (run N, date)` rather than `Complete`. Maximize the CI-testable surface (host/unit tests for the init parse + the WC PTE flags + the input decode/routing logic) so the un-modelable remainder is as small as possible.
- Prefer exact files/symbols over directories as these land; update the checkboxes and Track Layout statuses as tracks complete, and add the recorded HW-run pointer to Track E when validated.
