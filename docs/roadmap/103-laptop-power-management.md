# Phase 103 - Laptop Power Management (battery, backlight, thermal, suspend)

**Status:** Planned
**Source Ref:** phase-103
**Depends on:** Phase 101 (ACPI namespace + SCI) ✅ — the AML interpreter, `_HID` namespace walk, and SCI/GPE event dispatch this phase consumes; bare-metal validation strategy (Phase 98 Track A) — `docs/appendix/bare-metal-validation.md`
**Builds on:** Consumes the Phase 101 ACPI namespace + SCI/GPE event routing to surface battery/AC, brightness, thermal, and lid/power-button to userspace. Reuses the microkernel mechanism/policy split established by the Phase 81 `wifi_control` and Phase 57 `display_control`/`session_manager` IPC services (a kernel mechanism surface + a ring-3 policy daemon), the Phase 38 synthetic `procfs` backend (`/proc/blkstats`, `/proc/metacache`) for a read-only state surface, and the `IA32_*` MSR access pattern already used by Phase 84 (`cpuid.rs`) and the microcode loader (`microcode.rs`).
**Primary Components:** `kernel/src/acpi/power.rs` (new — control-method evaluation wrappers `_BST`/`_BIF`/`_BIX`/`_PSR`/`_TMP`/`_BCL`/`_BCM`/`_LID` over the Phase 101 AML interpreter, the power-state cache, and the GPE/`Notify` → kernel-notification routing), `kernel-core/src/power/` (new — pure-logic `battery`/`thermal`/`governor` decode + policy, host-tested on captured ACPI objects), `kernel/src/arch/x86_64/cpufreq.rs` (new — HWP / `IA32_PERF_CTL` P-state mechanism), `kernel/src/acpi/mod.rs` (`parse_fadt` extended to retain the PM1a/b control-block + GPE-block descriptors for the sleep write — the SCI/PM/GPE parse is a Phase 101 deliverable this reuses), `kernel/src/fs/procfs.rs` (`/proc/power` synthetic node), `kernel/src/arch/x86_64/syscall/mod.rs` (`SYS_POWER_*` family), `userspace/powerd` (new — the ring-3 power policy daemon + `power_control` IPC service), `userspace/m3ctl` (`power`/`battery`/`backlight` verbs), `userspace/session_manager` (lid/power-button → session action routing)

## Milestone Goal

Make the Dell Tiger Lake laptop usable as a daily driver instead of a wall-powered demo. After this phase the machine reports **battery percentage and AC-present** (and both update live when the charger is unplugged), the screen **brightness is adjustable**, **thermal zones** are read with a basic throttle/critical policy, a **lid close emits a suspend event** and a **power-button press emits a power event** routed to the session, **CPU P-states** scale under a conservative governor, and — as a stretch — an **S3/S0ix suspend-resume round-trips**. These are table-stakes laptop functions, and they are the natural backend behind the Phase 105 settings panel.

## Why This Phase Exists

There is **zero** power-management code in the tree. `kernel/src/acpi/mod.rs` does static-table discovery only (RSDP/RSDT/XSDT/MADT/FADT/MCFG/DMAR/IVRS); `parse_fadt` reads exactly one field (`IAPC_BOOT_ARCH`) and ignores `SCI_INT`, the PM1 event/control blocks, and the GPE blocks. There is no battery or AC-adapter device, no backlight control, no thermal zone, no lid or power-button handling, no P-state / cpufreq mechanism, and no suspend path. A GUI session (Phase 100) on a laptop with no battery indicator, no brightness control, and no suspend-on-lid-close is not a usable workstation — it is a kiosk that dies silently when the battery runs out.

Phase 101 supplies the missing substrate: an AML interpreter, an `_HID`-keyed namespace, and an SCI/GPE event source. But Phase 101 stops at "the namespace exists and an SCI fires" — it does not know what a battery, a brightness curve, a thermal trip point, or a lid switch *mean*. This phase is the consumer that turns those primitives into the device classes a laptop needs, and it deliberately keeps the **mechanism** (control-method evaluation, MSR writes, the sleep register write) in ring 0 while the **policy** (suspend-on-lid, the brightness step, the governor mode, the power menu) lives in a ring-3 `powerd` daemon — the userspace-first rule applied to power.

## Learning Goals

- Understand the ACPI **power device classes**: the `PNP0C0A` Control-Method Battery (`_BST` dynamic status, `_BIF`/`_BIX` static info, `_STA` presence), the `ACPI0003` AC adapter (`_PSR` online/offline), the `PNP0C0D` lid (`_LID`), the `PNP0C0C` power button, and the `_TZ` thermal zone (`_TMP`/`_CRT`/`_PSV`/`_ACx`) — all reached by *evaluating named control methods*, not by touching MMIO.
- See how a battery percentage is *computed*, not read: `_BST` reports remaining capacity in the units `_BIF`/`_BIX` declare, so percentage is `remaining / last_full_capacity` with a units-and-rate-vs-capacity gotcha that is exactly the kind of pure logic a host test pins.
- Learn how **GPE / `Notify`** turns a level-triggered SCI into a per-device event: firmware raises `Notify(BAT0, 0x80)` / `Notify(LID, 0x80)`, the Phase 101 GPE dispatcher decodes the affected node, and this phase routes it to a subscribable kernel notification an `idle`-blocked daemon wakes on — the ACPI analog of the `IrqNotification` contract the PCI drivers use.
- Understand x86 **P-state mechanism vs policy**: on Tiger Lake the mechanism is HWP (`IA32_PM_ENABLE` / `IA32_HWP_REQUEST`), legacy `IA32_PERF_CTL` + `_PSS` as a fallback; the *governor* (load → target perf) is portable pure logic that the kernel ticks, decoupled from the privileged MSR write.
- Confront the practical reality of **suspend on a modern laptop**: classic S3 (suspend-to-RAM via `_PTS` + `PM1_CNT` `SLP_TYP|SLP_EN` + the FACS waking vector) is the textbook path, but many Tiger Lake machines advertise only **S0ix / Modern Standby** (the `LPS0` `_DSM` package-C10 path) and report `_S3` absent — so the device quiesce/restore choreography matters more than the register poke.

## Feature Scope

### Track A — Battery + AC (the power substrate)

A new `kernel/src/acpi/power.rs` discovers the `PNP0C0A` battery and `ACPI0003` AC-adapter nodes by walking the Phase 101 namespace for their `_HID`s, and evaluates their control methods through the Phase 101 AML interpreter: `_STA` (present), `_BIF`/`_BIX` (design + last-full capacity, units, voltage), `_BST` (charging/discharging/critical state, present rate, remaining capacity, voltage), and `_PSR` (AC online 1 / offline 0). The *decode* of those package results — and the battery-percentage computation — is pure logic in a new `kernel-core/src/power/battery.rs`, host-tested against **captured ACPI objects** (real `_BST`/`_BIF` package bytes dumped from the reference DSDT) so the math is falsifiable in CI even though QEMU models no battery. The kernel caches the latest snapshot, exposes it as a read-only `/proc/power` synthetic file (alongside `blkstats`/`metacache`), and serves it to userspace over a new `SYS_POWER_*` surface. A new ring-3 `powerd` daemon owns the periodic refresh trigger and publishes a `power_control` IPC service; `m3ctl battery` / `m3ctl power status` query it (mirroring `m3ctl wifi status`).

### Track B — Backlight / brightness

Brightness uses the **firmware-exposed ACPI path**: `_BCL` (query the supported brightness-level list) and `_BCM` (set a level), with `_BQC` to read the current level, evaluated on the GPU's child display node through the Phase 101 interpreter. This is chosen over reverse-engineering the Intel GPU PWM precisely because it rides the interpreter with no GPU-register knowledge. The Intel GPU `BLC_PWM_CTL` PWM path (via the GT MMIO BAR) is documented as a **fallback** for panels whose `_BCM` is a firmware stub, but is lower priority. `m3ctl backlight <pct>` / `backlight up|down` drive the setter through `powerd` / the `SYS_POWER_*` surface; `powerd` restores the last brightness on resume.

### Track C — Thermal zones + policy hook

`acpi::power` enumerates `_TZ` thermal zones and reads `_TMP` (current temperature, decikelvin), `_CRT` (critical trip), `_PSV` (passive trip), `_ACx` (active-cooling trips), and `_TSP` (sample period). The decikelvin→Celsius decode and trip-point comparison are pure logic in `kernel-core/src/power/thermal.rs` (host-tested). A **basic policy hook** wires thermal into the rest of the phase: above `_PSV` the governor (Track E) caps the maximum P-state (passive cooling); at `_CRT` the kernel initiates a critical shutdown; temperatures and the active trip state surface in `/proc/power` and as a `powerd` notification.

### Track D — Lid-switch + power-button → session events

This is where the Phase 101 SCI/GPE plumbing becomes user-visible. `acpi::power` registers handlers on the Phase 101 GPE/`Notify` dispatcher for the power-relevant nodes (battery `0x80`, AC adapter `0x80`, thermal zone `0x80`, lid `0x80`, power/sleep button), and translates them into a single subscribable kernel **power-event notification**. It also handles the **fixed-feature** power/sleep button (the `PM1` `PWRBTN_STS`/`PWRBTN_EN` path) for firmware that uses the fixed event rather than a control-method `PNP0C0C`. `powerd` blocks on the power-event notification and routes: **lid close → session_manager** (suspend, or lock + DPMS-off if suspend is unsupported), **power button → session_manager** (power menu / graceful shutdown). `m3ctl power suspend|off` are the manual equivalents.

### Track E — P-states / cpufreq (conservative governor)

A new `kernel/src/arch/x86_64/cpufreq.rs` provides the MSR mechanism: enable HWP (`IA32_PM_ENABLE` bit 0), read `IA32_HWP_CAPABILITIES`, and program `IA32_HWP_REQUEST` (min/max/desired/EPP) per core — with a legacy `IA32_PERF_CTL` + ACPI `_PSS`/`_PCT` fallback for pre-HWP parts and VMs. The **governor** (a conservative load→target-perf state machine) is portable pure logic in `kernel-core/src/power/governor.rs` (host-tested), ticked by the kernel against per-core load and clamped by the Track C thermal cap. The governor mode (`performance` / `powersave` / `conservative`) is settable through the `SYS_POWER_*` surface so `powerd` / the settings panel can change policy without a kernel change.

### Track F — Suspend / resume (stretch)

Sleep-state discovery reads the `_Sx` packages from the Phase 101 namespace and the FADT PM1 blocks to detect S3 vs S0ix capability. The hard part is the **device quiesce/restore choreography**: `powerd` + `session_manager` tell drivers to quiesce (stop rings, save register state), the kernel evaluates `_PTS`/`_GTS`, writes `SLP_TYP|SLP_EN` into `PM1a/b_CNT`, installs the FACS waking vector, and on resume re-establishes CPU state and evaluates `_WAK`, after which drivers restore and the session un-blanks. Classic S3 is the primary target; **S0ix** (the `LPS0` `_DSM` / package-C10 modern-standby path most Tiger Lake laptops actually use) is noted as the realistic modern follow-on. This track is explicitly a stretch — a non-round-tripping suspend that fails closed (returns to a live session) is an acceptable partial outcome.

### Track G — Host tests, CI gate, and bare-metal validation

Because QEMU models none of this hardware, the quality story splits per `docs/appendix/bare-metal-validation.md`: **host tests** pin every pure-logic decode (battery%/`_BST`/`_BIF`, `_PSR`, decikelvin thermal, `_BCL` level list, the governor) against captured ACPI objects; a `power-smoke` **QEMU arm** asserts the namespace plumbing + that `/proc/power` exists and reports the desktop/VM case (`AC online, no battery`); and the live battery/brightness/thermal/lid/suspend datapaths are **skip-with-reason** in CI (mirroring `ure-smoke`/`wifi-smoke`) and carry a **Validated-on-HW (run N, date)** status from the reference machine.

## Important Components and How They Work

### `kernel/src/acpi/power.rs` (new) — the ACPI power surface

The single kernel-side owner of every power control method. It walks the Phase 101 namespace once at init to find the battery/AC/lid/button/thermal nodes by `_HID`, caches their object paths, and offers typed wrappers (`battery_status()` → evaluate `_BST`; `ac_online()` → `_PSR`; `thermal_temp(zone)` → `_TMP`; `set_brightness(level)` → `_BCM`; `lid_state()` → `_LID`) that call into the Phase 101 `acpi::aml::evaluate` entry point and hand the raw package to the `kernel-core::power` decoders. It holds the cached `PowerSnapshot` that `/proc/power` and `SYS_POWER_*` read, refreshes it on a periodic tick and on every relevant `Notify`, and owns the GPE/`Notify` → power-event-notification routing. No AML evaluation logic lives here — it is the *binding* between Phase 101's interpreter and this phase's device semantics.

### `kernel-core/src/power/` (new) — pure-logic decode + governor

`battery.rs`, `thermal.rs`, and `governor.rs` are `no_std`/`std`-dual pure logic with no kernel dependencies, exactly like `kernel-core/src/net/dhcp.rs`. They take raw control-method results (or load samples) and produce typed state and decisions. This is the host-testable core: `battery::percent(&bst, &bif)`, `thermal::celsius_from_decikelvin(raw)` + trip classification, `governor::Governor::next(load, thermal_cap) -> TargetPerf`. Every function is exercised in CI on captured ACPI bytes, so the un-modelable HW remainder is as small as possible.

### `kernel/src/arch/x86_64/cpufreq.rs` (new) — the P-state mechanism

Reuses the established `x86_64::registers::model_specific::Msr::new(MSR).read()/.write()` pattern from `cpuid.rs`/`microcode.rs`. It enables HWP and programs `IA32_HWP_REQUEST` per core (or `IA32_PERF_CTL` on the legacy path), driven by the kernel-core governor's `TargetPerf`. It is mechanism only: the decision of *what* target to request comes from the governor; the decision of *which mode* the governor runs in comes from userspace through `SYS_POWER_*`.

### `userspace/powerd` (new) — the policy daemon

A small ring-3 daemon, wired the four standard ways (workspace member, `xtask` `bins`, ramdisk `BIN_ENTRIES`, `services.d/powerd.conf` + `KNOWN_CONFIGS`). It blocks on the kernel power-event notification, refreshes the battery/thermal snapshot on a timer, applies brightness, owns the governor-mode setting, routes lid/button events to `session_manager`, and publishes a `power_control` IPC service that `m3ctl` and the Phase 105 settings panel query — the direct analog of the mt792x `wifi_control` daemon. Keeping it in ring 3 honors the userspace-first rule: all *policy* decisions (suspend-on-lid, brightness step, governor mode, power menu) live here; the kernel only provides mechanism.

### `userspace/m3ctl` + `userspace/session_manager` — the user-facing edges

`m3ctl` gains `power status` / `battery` / `backlight <pct|up|down>` / `power suspend|off` verbs added to `ParsedVerb` + `parse_verb` in `lib.rs`, with `dispatch_*` functions in `main.rs` that look up the `power_control` service (via `lookup_with_backoff`) and render with new `format_power_status`/`format_battery` helpers — the exact shape of the existing `dispatch_wifi_status` / `format_wifi_status` path. `session_manager` gains a power-action handler that maps a lid-close to a suspend/lock and a power-button to a power menu / graceful stop, reusing its `StopMachine` / `begin_stop` lifecycle.

## How This Builds on Earlier Phases

- **Consumes Phase 101 (ACPI namespace + SCI)** end to end: the AML interpreter evaluates every control method, the `_HID` namespace walk finds the power devices, and the SCI/GPE/`Notify` dispatcher is the event source this phase subscribes to. Phase 103 adds *no* AML or SCI machinery — it adds the *device-class meaning* on top.
- **Extends `kernel/src/acpi/mod.rs::parse_fadt`** to retain the PM1a/b control-block + GPE-block descriptors needed for the sleep write (Track F) and the fixed-feature button (Track D) — the FADT `SCI_INT`/PM/GPE parse itself is a Phase 101 deliverable that this reuses rather than duplicates.
- **Reuses the Phase 38 synthetic `procfs` backend** (`kernel/src/fs/procfs.rs`) by adding `/proc/power` to its whitelist, exactly as `blkstats`/`metacache` were added — a zero-policy read-only state surface.
- **Reuses the Phase 81 `wifi_control` / Phase 57 `display_control` IPC-service pattern** for `powerd`'s `power_control` service and the `m3ctl` verbs, and the Phase 57 `session_manager` lifecycle for the lid/button → session routing.
- **Reuses the Phase 84 MSR access pattern** (`Msr::new(IA32_*)` in `cpuid.rs`/`microcode.rs`) for the Track E P-state writes, and the `kernel-core::net::dhcp` "pure logic + host tests + a thin kernel tick" pattern for the battery/thermal/governor logic.
- **Continues the Phase 96 bare-metal line and the Phase 98 validation strategy** — the `--usb-passthrough` / AMT-SOL / `usb-logsink` capture toolkit and the `Validated-on-HW (run N, date)` convention are how Track G records the un-modelable datapaths.

## Implementation Outline

1. **Track A** — add `kernel/src/acpi/power.rs` (namespace walk for `PNP0C0A`/`ACPI0003`, the `_BST`/`_BIF`/`_BIX`/`_STA`/`_PSR` wrappers, the `PowerSnapshot` cache); add `kernel-core/src/power/battery.rs` + host tests on captured `_BST`/`_BIF` bytes; add `/proc/power` to `procfs.rs`; add the `SYS_POWER_*` query syscalls; scaffold `userspace/powerd` (four-place wiring) + the `power_control` service; add `m3ctl battery`/`power status`.
2. **Track B** — `_BCL`/`_BCM`/`_BQC` wrappers in `acpi::power`; the brightness setter through `SYS_POWER_*`; `m3ctl backlight`; document the Intel GPU `BLC_PWM_CTL` fallback; `powerd` brightness-restore-on-resume.
3. **Track C** — `_TZ`/`_TMP`/`_CRT`/`_PSV`/`_ACx`/`_TSP` enumerate + read; `kernel-core/src/power/thermal.rs` decode + trip classification (host tests); the passive-cap + critical-shutdown policy hook; surface in `/proc/power`.
4. **Track D** — register power-node `Notify`/GPE handlers on the Phase 101 dispatcher; add the fixed-feature `PWRBTN` path; the kernel power-event notification; `powerd` event routing → `session_manager`; `m3ctl power suspend|off`.
5. **Track E** — `kernel/src/arch/x86_64/cpufreq.rs` (HWP + legacy `IA32_PERF_CTL`/`_PSS`); `kernel-core/src/power/governor.rs` conservative governor (host tests); per-core apply + thermal clamp + governor-mode select.
6. **Track F (stretch)** — sleep-state discovery; the device quiesce/restore choreography (`powerd` + `session_manager` + driver hooks); S3 `_PTS`/`PM1_CNT`/FACS/`_WAK` entry + resume; S0ix `LPS0 _DSM` noted as follow-on.
7. **Track G** — host tests for every pure-logic decoder + the governor; the `power-smoke` QEMU/skip-with-reason gate + `M3OS_POWER_REGRESSION` row in `AGENTS.md`; the bare-metal validation runbook + recorded `Validated-on-HW` run.

## Acceptance Criteria

- **Host-testable (CI, always-on):** `kernel-core::power` has passing host tests that decode captured `_BST`/`_BIF`/`_BIX` packages into a battery percentage (with the units/rate gotcha covered), decode `_PSR` to AC online/offline, convert `_TMP` decikelvin to Celsius and classify it against `_CRT`/`_PSV`, parse a `_BCL` level list, and step the conservative governor across a load sweep with a thermal cap applied — control-method evaluation is host-tested on captured ACPI objects exactly as the spec requires.
- **CI plumbing:** a `power-smoke` gate boots m3OS, asserts `/proc/power` is present and renders the VM/desktop case (`AC online`, no battery, governor mode reported), and skips-with-reason on the live battery/brightness/thermal/lid/suspend arms; the `M3OS_POWER_REGRESSION` row is documented in `AGENTS.md`.
- **Validated-on-HW (run N, date)** on `Dell Precision 5560 / Tiger Lake`, per `docs/appendix/bare-metal-validation.md`, with captured serial evidence:
  - Battery percentage and AC status read correctly; **unplugging the charger flips AC online→offline and the percentage begins decreasing** within one refresh interval (`Notify(ADP,0x80)` + `_BST` re-read observed in the log).
  - A brightness change via `m3ctl backlight <pct>` **visibly takes effect** on the panel (`_BCM` evaluated; evidence: a dated photo per the protocol, since brightness is not serial-assertable).
  - A **lid close emits a suspend event** to `session_manager` and a **power-button press emits a power event** (both `Notify`/fixed-event lines + the routed session action captured in the log).
  - Thermal readings are **plausible** (within a sane laptop range, rising under load) and the `_CRT`/`_PSV` trip points are reported.
  - **(stretch)** An S3 (or S0ix) suspend/resume **round-trips** to a live session, or fails closed; the recorded outcome (full round-trip / partial / closed-fail) is documented in the run entry.
- The phase carries `Validated-on-HW (run N, date)` rather than a bare `Complete`; an unrecorded phase stays `Planned` / `Implemented (HW-unvalidated)`.

## Companion Task List

- [Phase 103 Task List](./tasks/103-laptop-power-management-tasks.md)

## How Real OS Implementations Differ

- **Linux** drives all of this through a full ACPICA interpreter + the `acpi/battery.c`, `acpi/ac.c`, `acpi/thermal.c`, `acpi_video`/`intel_backlight`, `button.c`, and `intel_pstate`/`amd_pstate` cpufreq governors, surfaced via `/sys/class/power_supply`, `/sys/class/backlight`, `/sys/class/thermal`, and `cpufreq` sysfs — plus a userspace `upower`/`thermald`/`systemd-logind` policy layer. m3OS reaches the bring-up subset: the firmware-exposed control methods, a single conservative governor, and a `powerd` policy daemon, with `/proc/power` standing in for the sysfs class trees.
- Real backlight on modern Intel panels is frequently the **`intel_backlight` GPU PWM**, not ACPI `_BCM` — Linux prefers the native interface and demotes `acpi_video`. m3OS takes the firmware (`_BCL`/`_BCM`) path first because it rides the interpreter with no GPU-register work, and documents the PWM path as a fallback.
- Modern laptops increasingly support **S0ix / Modern Standby only** (no S3); Linux gates suspend on `mem_sleep` and runs an elaborate `LPS0 _DSM` device-constraint protocol with `s2idle`. m3OS targets classic S3 first and treats S0ix as a follow-on, accepting fail-closed suspend as a partial outcome.
- Production cpufreq samples load and applies P-states at kHz-to-100Hz cadence per core with EPP/EPB tuning, idle-state (C-state) coordination, and turbo/thermal/RAPL power-capping; m3OS ships a single conservative governor with a thermal clamp and no C-state or RAPL coordination.
- Real OSes treat suspend's **device quiesce/restore** as a first-class, per-driver `suspend()`/`resume()` callback contract with ordered freezer/thaw phases; m3OS orchestrates it ad hoc through `powerd` + `session_manager` for the bring-up.

## Deferred Until Later

- **S0ix / Modern Standby** (`LPS0 _DSM`, package-C10, the s2idle device-constraint protocol) — the realistic modern-Tiger-Lake suspend path; deferred behind the classic-S3 stretch.
- **Intel GPU `BLC_PWM_CTL` native backlight** — the GPU-PWM brightness path for panels with a stub `_BCM`; documented as a fallback, not implemented in the firmware-first scope.
- **C-state idle coordination, RAPL power capping, turbo/EPP tuning, and per-driver `suspend`/`resume` callbacks** — cpufreq/power depth beyond a conservative governor + ad-hoc quiesce.
- **AMD `MSR_AMD_CPPC` / `amd_pstate` + AMD lid/thermal/battery quirks** — the Strix Point (HP OmniBook) power path; carried into Phase 108 (AMD bring-up), reusing this phase's `kernel-core::power` logic and the `cpufreq.rs` mechanism seam.
- **A full `/sys/class/power_supply`-style hierarchy and `upower`-class D-Bus surface** — `/proc/power` + the `power_control` IPC service are the bring-up substitute; a richer surface can ride the Phase 105 settings work.
- **Multi-battery and smart-battery (`PNP0C0A`-multi / `ACPI0002` SBS) support** — single-battery only for bring-up.
