# Phase 103 — Laptop Power Management (battery, backlight, thermal, suspend): Task List

**Status:** Planned
**Source Ref:** phase-103
**Depends on:** Phase 101 (ACPI namespace + SCI) ✅ — AML interpreter + `_HID` namespace walk + SCI/GPE `Notify` dispatch; bare-metal validation strategy (Phase 98 Track A) — `docs/appendix/bare-metal-validation.md`
**Goal:** Consume the Phase 101 ACPI namespace + SCI/GPE event routing to surface battery/AC, brightness, thermal, lid-switch + power-button, and CPU P-states to userspace through a kernel mechanism surface (`acpi::power` + `cpufreq` + `SYS_POWER_*` + `/proc/power`) and a ring-3 `powerd` policy daemon, with S3/S0ix suspend-resume as a stretch — making the Dell Tiger Lake laptop a usable daily driver. All control-method-result decode + the governor are pure logic host-tested on captured ACPI objects; the live datapaths are bare-metal-only and carry a `Validated-on-HW (run N, date)` status.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Battery + AC substrate: `acpi::power` control-method eval, `kernel-core::power::battery` decode, `/proc/power`, `SYS_POWER_*`, `powerd` scaffold, `m3ctl battery`/`power status` | Phase 101 | Planned |
| B | Backlight/brightness (`_BCL`/`_BCM`/`_BQC`; GPU-PWM fallback documented) + `m3ctl backlight` + restore-on-resume | A | Planned |
| C | Thermal zones (`_TZ`/`_TMP`/`_CRT`/`_PSV`/`_ACx`) + decode + passive/critical policy hook | A, E | Planned |
| D | Lid-switch + power-button via SCI/GPE `Notify` → kernel power-event notification → `powerd` → `session_manager` | A | Planned |
| E | P-states / cpufreq: HWP + legacy `IA32_PERF_CTL` mechanism + conservative governor | A | Planned |
| F | Suspend/resume (stretch): sleep-state discovery + device quiesce/restore + S3 entry/resume (S0ix follow-on) | A, B, C, D, E | Planned (stretch) |
| G | Host tests + `power-smoke` CI gate + bare-metal validation (`Validated-on-HW`) | A–F | Planned |

---

## Track A — Battery + AC (the power substrate)

### A.1 — Kernel ACPI power surface + device discovery

**File:** `kernel/src/acpi/power.rs` (new)
**Symbol:** `power::init`, `power::evaluate` (thin wrapper over the Phase 101 `acpi::aml::evaluate`), the `PowerDevices` cache keyed by `_HID`
**Why it matters:** This is the single kernel-side binding between Phase 101's AML interpreter / namespace and this phase's device semantics; without it nothing can evaluate `_BST`/`_PSR`/`_TMP`/`_BCM`. It must add **no** AML or SCI machinery — only the device-class layer on top.

**Acceptance:**
- [ ] Walks the Phase 101 namespace once at init and caches the object paths of the `PNP0C0A` battery and `ACPI0003` AC-adapter nodes (and logs `[power] battery=<path> ac=<path>` or `none`).
- [ ] `power::evaluate(path)` returns the typed AML result from the Phase 101 interpreter and is the only call site that touches the interpreter (no duplicated AML logic in this file).
- [ ] On a no-battery machine (QEMU/desktop) init logs `[power] no battery device` and the rest of the surface degrades to "AC online, no battery" rather than faulting.

### A.2 — Battery/AC decode + percentage (pure logic, host-tested)

**File:** `kernel-core/src/power/battery.rs` (new), `kernel-core/src/power/mod.rs` (new)
**Symbol:** `BatteryInfo` / `BatteryStatus` decoders, `battery::percent(&status, &info)`, `ac_online(psr) -> bool`
**Why it matters:** A battery percentage is computed, not read — `_BST` reports remaining capacity in the units `_BIF`/`_BIX` declare, and the rate-vs-capacity / `mWh`-vs-`mAh` units gotcha is exactly the falsifiable logic a host test pins, independent of any hardware.

**Acceptance:**
- [ ] Decodes a captured `_BST` package (state, present rate, remaining capacity, present voltage) and a captured `_BIF`/`_BIX` package (power-unit flag, design + last-full capacity, design voltage) into typed structs.
- [ ] `percent()` returns `remaining / last_full_capacity` clamped to 0..=100, correct for both the `mWh` and `mAh` power-unit cases, and returns a sentinel (not a panic) when `last_full_capacity` is 0 or `0xFFFFFFFF` (unknown).
- [ ] `ac_online()` maps `_PSR` `1→online` / `0→offline`.
- [ ] Host tests in `kernel-core` (`cargo test -p kernel-core --target x86_64-unknown-linux-gnu`) cover charging/discharging/critical states and the unknown-capacity edge case on **captured ACPI object bytes**.

### A.3 — Power-state cache + `/proc/power` synthetic surface

**Files:**
- `kernel/src/acpi/power.rs` (the `PowerSnapshot` cache + periodic refresh tick)
- `kernel/src/fs/procfs.rs` (whitelist + renderer)

**Symbol:** `PowerSnapshot`, `power::snapshot()`, the `procfs` `"power"` arm (alongside `"blkstats" | "metacache"`)
**Why it matters:** A read-only `/proc/power` is a zero-policy state surface that works even if `powerd` is down (defense in depth), mirroring the Phase 38 `/proc/blkstats` / `/proc/metacache` convention.

**Acceptance:**
- [ ] `acpi::power` caches a `PowerSnapshot` (battery %, charging state, AC online, per-zone temperature, current brightness, governor mode) refreshed on a periodic kernel tick and on every relevant `Notify`.
- [ ] `cat /proc/power` renders the snapshot in a stable line-oriented format; on QEMU it reads `ac=online battery=none`.
- [ ] `/proc/power` is added to the procfs path whitelist and `stat`s cleanly (synthetic inode, like the existing entries).

### A.4 — `SYS_POWER_*` syscall / IPC surface

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `SYS_POWER_QUERY` (snapshot), `SYS_POWER_SET_BRIGHTNESS`, `SYS_POWER_SET_GOVERNOR`, `SYS_POWER_REQUEST_SLEEP`, `SYS_POWER_WAIT_EVENT` (the new `SYS_POWER_*` family)
**Why it matters:** `powerd` (and `m3ctl`) need a capability-gated way to read state, drive mechanism, and block on power events; this is the kernel mechanism/userspace policy boundary for the whole phase.

**Acceptance:**
- [ ] `SYS_POWER_QUERY` copies the `PowerSnapshot` to a user buffer (the same bytes `/proc/power` renders).
- [ ] `SYS_POWER_SET_BRIGHTNESS` / `SYS_POWER_SET_GOVERNOR` invoke the Track B / Track E mechanism and are capability-gated (an unprivileged caller gets `EPERM`).
- [ ] `SYS_POWER_WAIT_EVENT` blocks the caller on the Track D power-event notification and returns the event kind on wake (decoded `EINTR`/timeout semantics documented).
- [ ] The ABI struct is shared via a `kernel_core::power` type so userspace decodes the same layout.

### A.5 — `powerd` daemon scaffold + `power_control` service + `m3ctl` query verbs

**Files:**
- `userspace/powerd/Cargo.toml`, `userspace/powerd/src/main.rs` (new)
- `Cargo.toml` (workspace `members`), `xtask/src/main.rs` (`bins` in `build_userspace`), `kernel/src/fs/ramdisk.rs` (`include_bytes!` + `BIN_ENTRIES`), `xtask/src/main.rs` `populate_ext2_files` + `userspace/init/src/main.rs` `KNOWN_CONFIGS` (a `services.d/powerd.conf`)
- `userspace/m3ctl/src/lib.rs` (`ParsedVerb`, `parse_verb`, `format_power_status`, `format_battery`), `userspace/m3ctl/src/main.rs` (`dispatch_power_status`)

**Symbol:** `powerd::main`, the `power_control` IPC service name, `ParsedVerb::PowerStatus` / `ParsedVerb::Battery`
**Why it matters:** Per the userspace-first rule, *policy* lives in ring 3; `powerd` is the policy daemon and the Phase 105 settings-panel backend, modelled exactly on the Phase 81 `wifi_control` daemon + `m3ctl wifi status` path. Missing any of the four new-binary wiring points means the daemon is not built/embedded/launched.

**Acceptance:**
- [ ] `cargo xtask check` builds `powerd`; it is embedded in the ramdisk and launched from `services.d/powerd.conf`; it defines `#[global_allocator] = syscall_lib::heap::BrkAllocator` and enables `syscall-lib`'s `alloc` feature (`needs_alloc = true`).
- [ ] `powerd` publishes a `power_control` IPC service and answers a status query with the current `PowerSnapshot`.
- [ ] `m3ctl power status` and `m3ctl battery` look up `power_control` via `lookup_with_backoff` and print the decoded battery %/AC/governor (the `dispatch_wifi_status` shape); a `parse_verb` host test covers the new verbs.

---

## Track B — Backlight / Brightness

### B.1 — `_BCL` / `_BCM` / `_BQC` control-method wrappers

**File:** `kernel/src/acpi/power.rs`
**Symbol:** `power::brightness_levels()` (`_BCL`), `power::set_brightness(level)` (`_BCM`), `power::current_brightness()` (`_BQC`)
**Why it matters:** Brightness uses the firmware-exposed ACPI path so it rides the Phase 101 interpreter with no GPU-register reverse engineering; this is the "pick the firmware-exposed path" decision from the spec.

**Acceptance:**
- [ ] `_BCL` is evaluated on the GPU child display node and returns the supported brightness-level list (first two entries = AC/battery defaults per the spec); the list is parsed and cached.
- [ ] `set_brightness(pct)` maps a 0..=100 percent to the nearest supported `_BCL` level and evaluates `_BCM`; `current_brightness()` reads it back via `_BQC`.
- [ ] The `_BCL` level-list parse is pure logic in `kernel-core::power` and host-tested.

### B.2 — Intel GPU PWM fallback (documented)

**File:** `kernel/src/acpi/power.rs` (a `cfg`/runtime fallback stub + a doc comment)
**Symbol:** `power::pwm_fallback` (documented `BLC_PWM_CTL` path)
**Why it matters:** Some modern panels expose a stub `_BCM`; the native Intel GPU PWM (`BLC_PWM_CTL` via the GT MMIO BAR) is the real backlight on those — but it is GPU-register work, so it is scoped as a documented fallback, not the primary path.

**Acceptance:**
- [ ] A doc comment records the `BLC_PWM_CTL` register + GT MMIO BAR path and the condition under which it would be needed (`_BCL` absent or `_BCM` a no-op on the reference panel).
- [ ] If the reference panel's `_BCM` proves to be a stub during HW validation, the fallback is flagged as a follow-on (it is in *Deferred Until Later*), not silently failed.

### B.3 — `m3ctl backlight` + powerd brightness apply + restore-on-resume

**Files:** `userspace/m3ctl/src/{lib,main}.rs`, `userspace/powerd/src/main.rs`
**Symbol:** `ParsedVerb::Backlight`, `powerd` brightness state + the resume hook
**Why it matters:** Brightness is the most-used laptop control; it must be a one-liner and survive a suspend/resume cycle.

**Acceptance:**
- [ ] `m3ctl backlight <pct>` and `m3ctl backlight up|down` drive `SYS_POWER_SET_BRIGHTNESS` through `powerd`; the level is reflected in `/proc/power`.
- [ ] `powerd` records the last-set brightness and re-applies it on the Track F resume event.
- [ ] `parse_verb` host test covers the `<pct>` / `up` / `down` argument forms.

---

## Track C — Thermal Zones + Policy Hook

### C.1 — Thermal-zone enumerate + read + decode

**Files:** `kernel/src/acpi/power.rs`, `kernel-core/src/power/thermal.rs` (new)
**Symbol:** `power::thermal_zones()`, `thermal::celsius_from_decikelvin(raw)`, `thermal::classify(temp, &trips)`
**Why it matters:** Thermal awareness keeps the machine from cooking under load; the decikelvin→Celsius conversion and trip classification are pure logic falsifiable in CI.

**Acceptance:**
- [ ] Enumerates `_TZ` zones and reads `_TMP` (current), `_CRT` (critical), `_PSV` (passive), `_ACx` (active), `_TSP` (sample period) for each.
- [ ] `celsius_from_decikelvin` converts ACPI decikelvin to Celsius; host-tested against captured `_TMP`/`_CRT`/`_PSV` values (e.g. `0x0BB8` → 25 °C).
- [ ] `classify` returns `Normal` / `Passive` / `Active(n)` / `Critical` against the parsed trip points; host-tested across the boundaries.

### C.2 — Passive + critical policy hook

**Files:** `kernel/src/acpi/power.rs`, `kernel/src/arch/x86_64/cpufreq.rs`
**Symbol:** `power::thermal_tick`, the governor thermal clamp, the critical-shutdown path
**Why it matters:** Reading temperatures is useless without acting on them; this wires thermal into the governor (passive cooling) and the shutdown path (thermal-runaway safety).

**Acceptance:**
- [ ] Above `_PSV` the governor's max target is clamped (passive cooling) and `/proc/power` reflects the active trip state; the clamp lifts when temperature drops below `_PSV` with hysteresis.
- [ ] At `_CRT` the kernel initiates a critical shutdown (logged `[power] CRITICAL temp <C> >= _CRT, shutting down`).
- [ ] A `Notify(TZ, 0x80)` re-reads `_TMP` and re-runs the policy.

---

## Track D — Lid-Switch + Power-Button → Session Events

### D.1 — Power-node `Notify`/GPE routing → kernel power-event notification

**File:** `kernel/src/acpi/power.rs`
**Symbol:** `power::register_event_handlers` (on the Phase 101 GPE/`Notify` dispatcher), the kernel `POWER_EVENT` notification object
**Why it matters:** This is where the Phase 101 SCI/GPE plumbing becomes user-visible — firmware raises `Notify(BAT0,0x80)`/`Notify(LID,0x80)` and the dispatcher must turn it into a single subscribable event an idle daemon wakes on, the ACPI analog of the `IrqNotificationContract` the PCI drivers use.

**Acceptance:**
- [ ] Registers handlers on the Phase 101 dispatcher for battery `0x80`, AC adapter `0x80`, thermal zone `0x80`, lid `0x80`, and the control-method power/sleep button.
- [ ] Each handler refreshes the relevant snapshot field and signals the `POWER_EVENT` notification with a decoded event kind (battery / ac / thermal / lid / power-button).
- [ ] `SYS_POWER_WAIT_EVENT` wakes exactly once per event with the correct kind (no lost-wakeup — re-check after register, consistent with the Phase 99 blocking-primitive rules).

### D.2 — Fixed-feature power/sleep button

**Files:** `kernel/src/acpi/mod.rs` (`parse_fadt` PM1 block retain), `kernel/src/acpi/power.rs`
**Symbol:** the `PM1` `PWRBTN_STS` / `PWRBTN_EN` / `SLPBTN_*` handling
**Why it matters:** Some firmware uses the ACPI **fixed-feature** power/sleep button (a `PM1` status bit) rather than a control-method `PNP0C0C` device; both must route to the same event so the button works regardless of how the firmware models it.

**Acceptance:**
- [ ] `parse_fadt` retains the PM1a/b event + control block descriptors (extending the current `IAPC_BOOT_ARCH`-only parse; reuses the Phase 101 FADT parse where available).
- [ ] `PWRBTN_EN` is set, and a `PWRBTN_STS` on the SCI raises the same `POWER_EVENT` power-button kind as the control-method path; the status bit is cleared (write-1-to-clear).

### D.3 — `powerd` event routing → `session_manager` + `m3ctl power suspend|off`

**Files:** `userspace/powerd/src/main.rs`, `userspace/session_manager/src/lifecycle.rs`, `userspace/m3ctl/src/{lib,main}.rs`
**Symbol:** `powerd` event loop, a `session_manager` power-action handler (over `StopMachine`/`begin_stop`), `ParsedVerb::Suspend` / `ParsedVerb::PowerOff`
**Why it matters:** Suspend-on-lid and the power menu are *high-level policy* and per the userspace-first rule must live in ring 3; `session_manager` already supervises the display-critical services and owns the stop lifecycle.

**Acceptance:**
- [ ] `powerd` blocks on `SYS_POWER_WAIT_EVENT` and routes: **lid close → session_manager** (suspend, or lock + DPMS-off when suspend is unsupported), **power button → session_manager** (power menu / graceful shutdown).
- [ ] `session_manager` gains a power-action entry point that maps the event to a `begin_stop` / lock action without wedging the display-critical supervision.
- [ ] `m3ctl power suspend` requests `SYS_POWER_REQUEST_SLEEP` (Track F) and `m3ctl power off` performs a graceful shutdown; both are capability-gated.

---

## Track E — P-States / cpufreq (Conservative Governor)

### E.1 — HWP P-state mechanism

**File:** `kernel/src/arch/x86_64/cpufreq.rs` (new)
**Symbol:** `cpufreq::enable_hwp`, `cpufreq::set_hwp_request(min, max, desired, epp)`
**Why it matters:** On Tiger Lake the P-state mechanism is HWP; this is the privileged per-core MSR write that the governor's decision actuates, reusing the `Msr::new(IA32_*)` pattern from `cpuid.rs`/`microcode.rs`.

**Acceptance:**
- [ ] Detects HWP support (CPUID `06H:EAX[7]`), sets `IA32_PM_ENABLE` bit 0, reads `IA32_HWP_CAPABILITIES` (highest/guaranteed/efficient/lowest perf), and programs `IA32_HWP_REQUEST` per core.
- [ ] Logs the discovered HWP perf range at init (`[cpufreq] HWP perf hi=<n> lo=<n>`).
- [ ] The write is done on each online core (per-CPU, not BSP-only).

### E.2 — Legacy `IA32_PERF_CTL` / `_PSS` fallback

**Files:** `kernel/src/arch/x86_64/cpufreq.rs`, `kernel/src/acpi/power.rs`
**Symbol:** `cpufreq::set_perf_ctl`, `power::pss_states()` (`_PSS`/`_PCT`)
**Why it matters:** Pre-HWP parts and VMs expose only the legacy ACPI P-state objects; the fallback keeps cpufreq functional (and host-testable) without HWP.

**Acceptance:**
- [ ] When HWP is absent, `_PSS`/`_PCT` are evaluated into a P-state table and `IA32_PERF_CTL` is written for a selected state.
- [ ] The `_PSS` package decode is pure logic in `kernel-core::power` and host-tested.

### E.3 — Conservative governor (pure logic) + mode select

**Files:** `kernel-core/src/power/governor.rs` (new), `kernel/src/arch/x86_64/cpufreq.rs`
**Symbol:** `governor::Governor::next(load, thermal_cap) -> TargetPerf`, governor-mode select via `SYS_POWER_SET_GOVERNOR`
**Why it matters:** The governor (load → target perf) is portable pure logic decoupled from the privileged MSR write; the mode is policy and must be settable from userspace without a kernel change.

**Acceptance:**
- [ ] `Governor::next` implements a conservative ramp (step up on sustained high load, down on idle) clamped by the Track C thermal cap; host-tested across a load sweep and with a clamp applied.
- [ ] Governor mode (`performance` / `powersave` / `conservative`) is settable through `SYS_POWER_SET_GOVERNOR` and reported in `/proc/power`.
- [ ] The kernel ticks the governor against per-core load and applies the result via E.1/E.2 mechanism.

---

## Track F — Suspend / Resume (Stretch)

### F.1 — Sleep-state discovery (S3 vs S0ix)

**Files:** `kernel/src/acpi/power.rs`, `kernel/src/acpi/mod.rs`
**Symbol:** `power::sleep_states()` (`_S3`/`_S4`/`_S5` packages + the FADT PM1_CNT block), S0ix capability detection
**Why it matters:** Whether the machine supports classic S3 or only S0ix decides the entire suspend strategy; detecting it up front avoids attempting an unsupported sleep.

**Acceptance:**
- [ ] Reads the `_Sx` packages from the Phase 101 namespace to learn the `SLP_TYP` values and which sleep states are supported; logs `[power] sleep states: S3=<y/n> S0ix=<y/n>`.
- [ ] Detects S0ix support (the `LPS0`/`_LPI` presence) and records it as the modern follow-on path when `_S3` is absent.

### F.2 — Device quiesce/restore choreography

**Files:** `userspace/powerd/src/main.rs`, `userspace/session_manager/src/lifecycle.rs`, driver quiesce hooks
**Symbol:** the `powerd` suspend orchestration, a driver `quiesce`/`restore` hook
**Why it matters:** The hard part of suspend is not the register poke — it is stopping device rings + saving register state before power-down and restoring after, ordered so nothing DMAs into freed memory.

**Acceptance:**
- [ ] `powerd` + `session_manager` quiesce the display/input/storage/NIC drivers (stop rings, save state) before requesting the sleep, and restore them after resume.
- [ ] A quiesce failure aborts the suspend and fails closed to a live session (no half-suspended state).

### F.3 — S3 entry + resume (S0ix noted)

**File:** `kernel/src/acpi/power.rs`
**Symbol:** `power::enter_sleep(state)` (`_PTS`/`_GTS` + `PM1a/b_CNT` `SLP_TYP|SLP_EN` + FACS waking vector), the resume path + `_WAK`
**Why it matters:** This is the kernel mechanism for the sleep transition; the FACS waking vector + CPU-state re-establish is the resume contract.

**Acceptance:**
- [ ] `enter_sleep(S3)` evaluates `_PTS(3)`/`_GTS(3)`, installs the FACS waking vector, and writes `SLP_TYP|SLP_EN` into `PM1a/b_CNT` after F.2 quiesce.
- [ ] On resume the kernel re-establishes CPU state, evaluates `_WAK(3)`, and signals the `POWER_EVENT` resume kind so `powerd` restores brightness + drivers.
- [ ] **Validated-on-HW**: an S3 (or S0ix) suspend/resume round-trips to a live session, **or** the attempt fails closed; the outcome is recorded in the Track G run entry (a partial/closed-fail is an acceptable stretch outcome).

---

## Track G — Host Tests, CI Gate, and Bare-Metal Validation

### G.1 — Host tests on captured ACPI objects

**Files:** `kernel-core/src/power/{battery,thermal,governor}.rs` test modules
**Symbol:** the `#[cfg(test)]` modules
**Why it matters:** QEMU models no battery/thermal/brightness, so the pure-logic decoders are the only CI-falsifiable surface; the spec requires control-method evaluation to be host-tested on captured ACPI objects.

**Acceptance:**
- [ ] `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` passes tests decoding captured `_BST`/`_BIF`/`_BIX`/`_PSR`/`_TMP`/`_CRT`/`_PSV`/`_BCL`/`_PSS` byte sequences and stepping the governor across a load sweep with a thermal clamp.
- [ ] The captured ACPI object bytes (from the reference DSDT / object dumps) are committed as test fixtures with a provenance note.

### G.2 — `power-smoke` CI gate (QEMU + skip-with-reason)

**Files:** `xtask/src/main.rs` (`cmd_power_smoke`, new), `AGENTS.md` (gate-table row)
**Symbol:** `cmd_power_smoke`, `M3OS_POWER_REGRESSION`
**Why it matters:** The plumbing (namespace, `/proc/power`, the syscall surface, the governor) is testable in QEMU even though the device datapaths are not; the gate must be present, assert what it can, and skip-with-reason on the HW-only arms, mirroring `ure-smoke`/`wifi-smoke`.

**Acceptance:**
- [ ] `power-smoke` boots m3OS and asserts `/proc/power` is present and renders the VM case (`ac=online battery=none`, governor mode reported), and that `m3ctl power status` round-trips through `powerd`.
- [ ] The live battery/brightness/thermal/lid/suspend arms print a skip-with-reason (no QEMU model), and the gate returns success in CI.
- [ ] `M3OS_POWER_REGRESSION=1` row added to the `AGENTS.md` gate table with the same skip-vs-pass semantics wording as the `ure-smoke`/`wifi-smoke` rows; `docs/roadmap/README.md` Phase 103 row + mermaid node depending on Phase 101 added.

### G.3 — Bare-metal validation pass

**File:** `scripts/power-baremetal-validate.md` (new — generalized from `scripts/ure-vfio-validate.md`), results appendix
**Symbol:** the recorded bare-metal run + the `Validated-on-HW (run N, date)` status
**Why it matters:** The phase's headline claim is a usable daily-driver laptop; per `docs/appendix/bare-metal-validation.md` this records the end-to-end physical run with captured evidence, since no CI safety net exists for any of these datapaths.

**Acceptance:**
- [ ] On `Dell Precision 5560 / Tiger Lake`: battery % + AC status read correctly and **AC flips online→offline on charger unplug** with the percentage decreasing (serial: `Notify(ADP,0x80)` + `_BST` re-read captured).
- [ ] A `m3ctl backlight <pct>` change **visibly takes effect** (dated photo evidence, per the protocol — brightness is not serial-assertable).
- [ ] A **lid close emits a suspend event** and a **power-button press emits a power event** to `session_manager` (both `Notify`/fixed-event + routed-action lines captured).
- [ ] Thermal readings are **plausible** (sane laptop range, rising under load); `_CRT`/`_PSV` reported.
- [ ] **(stretch)** S3/S0ix suspend/resume round-trips or fails closed; the outcome is recorded.
- [ ] Evidence captured per the protocol (AMT SOL pre-network, `usb-logsink` boot.log / network sink post-network, photo for brightness); the README/task-doc Status set to `Validated-on-HW (run N, date)` — not a bare `Complete`.

---

## Documentation Notes

- Phase 103 adds **no** AML or SCI machinery — it consumes the Phase 101 interpreter (`acpi::aml::evaluate`), `_HID` namespace walk, and GPE/`Notify` dispatcher. Keep that boundary explicit in `acpi::power` (it is the binding layer, not a second interpreter); record it when both land.
- `powerd` is the **first power-policy daemon** and the Phase 105 settings-panel backend — note that it follows the Phase 81 `wifi_control` / Phase 57 `display_control` mechanism/policy split, so the settings panel reuses the `power_control` IPC surface unchanged.
- The pure-logic split (`kernel-core::power` decode + governor, host-tested; a thin kernel tick applies it) mirrors `kernel-core::net::dhcp`; keep all hardware-independent logic in `kernel-core` so CI exercises it.
- This phase is **HW-only**: QEMU models none of the battery/brightness/thermal/lid/suspend datapaths. Per `docs/appendix/bare-metal-validation.md` the status is `Validated-on-HW (run N, date)`, never a bare `Complete`; an unrecorded run leaves it `Planned` / `Implemented (HW-unvalidated)`.
- The AMD power path (`MSR_AMD_CPPC`/`amd_pstate`, AMD lid/thermal quirks) is carried to Phase 108 (HP OmniBook / Strix Point), reusing the `kernel-core::power` logic and the `cpufreq.rs` mechanism seam — note the cross-reference when Phase 108 lands.
- Prefer exact files/symbols over directories as these land; update this list's checkboxes as tracks complete, and bump the validation run number on every re-validation.
