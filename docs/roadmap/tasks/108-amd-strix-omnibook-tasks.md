# Phase 108 — HP OmniBook / AMD Strix Point Bring-up: Task List

**Status:** Planned
**Source Ref:** phase-108
**Depends on:** Phase 96 (bare-metal bring-up + boot rescue) ✅, Phase 55a/67 (AMD-Vi IOMMU coded + host-tested) ✅, Phase 81 (mt792x connac2 Wi-Fi) ✅, Phase 102 (I2C-HID protocol layer), Phase 104 (Wi-Fi supplicant daemon), Phase 107 (Networked & Signed Package Distribution — sequencing prerequisite: starts after the Dell line 99–107)
**Goal:** Boot the HP OmniBook Ultra 14-fd0xxx (Ryzen AI 9 365, Strix Point / Zen 5, board "SBKPF") to a usable login + GUI on bare metal, *after* the Dell line proves the stack. The boot/usability layer is a small delta over the bus-agnostic substrate (GOP FB + xHCI + NVMe + 8-bit-ID xAPIC + the Phase-96 boot-rescue fixes carry over free); the new work is the **MT7925 connac3 Wi-Fi** driver (gating), the **first bare-metal AMD-Vi validation** (risk), the **fam1Ah Zen 5 microcode blob** (trivial), and the **AMD I2C-HID touchpad backend** behind the Phase 102 transport. Every acceptance item is a recorded hardware run under `docs/appendix/bare-metal-validation.md` — there is no QEMU model and no CI safety net.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| Baseline | UEFI-boot the OmniBook to a framebuffer login over GOP FB + xHCI + NVMe + xAPIC with no AMD-specific code | Phases 99–107 (Dell substrate) | Planned |
| A | MT7925 connac3 Wi-Fi — firmware blobs + connac2→connac3 MCU/WFDMA adaptation → `net.nic`; supplicant reuse | Baseline, Phase 81, Phase 104 | Planned |
| B | Bare-metal AMD-Vi validation — `AmdViUnit::bring_up` on real silicon; identity-map fallback floor then isolation milestone | Baseline, Phase 55a/67 | Planned |
| C | fam1Ah (Zen 5) microcode blob added to `amd-ucode.bin` | Baseline | Planned |
| D | AMD I2C-HID touchpad backend — `AMDI0010` DesignWare I2C + `AMDI0030`/`pinctrl-amd` GPIO IRQ behind the Phase 102 transport | Baseline, Phase 101 (ACPI `_CRS`), Phase 102 | Planned |
| E | Keyboard-path confirmation (i8042 PS/2 vs I2C-HID) on bare metal | Baseline, D (if I2C-HID) | Planned |

> **Status convention (HW-only phase):** none of this hardware has a QEMU model. Each acceptance checkbox is closed by a recorded hardware run under the protocol in `docs/appendix/bare-metal-validation.md` and is marked **"Validated-on-HW (run N, date)"** with a serial-capture / network-sink / photo / on-device-render evidence pointer — never a bare "Complete." Host-testable deltas (connac3 parse/opcodes, microcode matcher) additionally carry a `cargo test -p kernel-core` pointer.

---

## Track Baseline — Substrate Carry-Over

### BL.1 — UEFI boot to framebuffer login on the OmniBook

**Files:**
- `userspace/init/src/main.rs` (`BUILTIN_CONFIGS` / service manifest)
- `kernel/src/arch/x86_64/apic.rs` (`current_lapic_id`)
**Symbol:** the boot/login path; `current_lapic_id` (8-bit xAPIC, bits 24–31)
**Why it matters:** Proves the load-bearing paths are CPU-vendor-neutral — the entire premise of sequencing the OmniBook as a *small delta* after the Dell. The Ryzen AI 9 365 is 10-core/20-thread; 20 LAPIC IDs fit in the 8-bit ID field, so the existing xAPIC path suffices (no x2APIC work).

**Acceptance:**
- [ ] The OmniBook UEFI-boots the m3OS image (Phase 106 USB/NVMe path) to a **framebuffer login** over GOP FB + xHCI + NVMe + xAPIC with **no AMD-specific code changes**; boot log captured — *Validated-on-HW (run N, date)*.
- [ ] All 20 hardware threads are enumerated and brought online via the existing 8-bit-ID xAPIC path (no x2APIC), with the Phase 99 SMP hardening intact (no lost-wakeup wedge); serial AP-online count cited — *Validated-on-HW (run N, date)*.
- [ ] A GUI session reaches the greeter on bare metal (the Phase 100 write-combining user framebuffer in use); on-device-render or photo evidence — *Validated-on-HW (run N, date)*.

---

## Track A — MT7925 connac3 Wi-Fi

> Primary reference: upstream `mt76` connac3 (`drivers/net/wireless/mediatek/mt76/mt7925/`) for facts (firmware blob names, MCU command framing, WFDMA offsets) — constants/sequences only. The MT7925 already matches `is_mt792x` (`MT7925_IDS = &[0x7925, 0x0717]`); attachment is correct, firmware + connac3 deltas are the work.

### A.1 — Stage the MT7925 connac3 firmware blobs

**Files:**
- `kernel/initrd/lib/firmware/mt7925/` (new directory)
- `xtask/src/main.rs` (`stage_wifi_firmware`)
- `docs/legal/firmware-licenses.md`
**Symbol:** `stage_wifi_firmware` (already iterates `["mt7961", "mt7922", "mt7925"]`)
**Why it matters:** The MT7925 needs `WIFI_RAM_CODE_MT7925_1_1.bin` + `WIFI_MT7925_PATCH_MCU_1_1_hdr.bin`, which are **not interchangeable** with the mt7961 blobs; without the right blobs the chip degrades with `FW_ABSENT_SENTINEL`.

**Acceptance:**
- [ ] The `mt7925/` staging dir + README document the connac3 blob names; `stage_wifi_firmware` reports them found (or skip-with-reason when absent, build still succeeds) — `cargo xtask` step output cited.
- [ ] `docs/legal/firmware-licenses.md` records the MT7925 blob provenance + redistribution terms (mirroring the mt7961 entry).
- [ ] With the blobs staged, no `MT792X_FW:absent:` is emitted for an MT7925 match — *Validated-on-HW (run N, date)*.

### A.2 — Route `firmware_blob()` to the connac3 bytes for an MT7925 match

**Files:**
- `userspace/drivers/mt792x/src/fw.rs` (`firmware_blob`)
- `userspace/drivers/mt792x/src/main.rs` (`main`, the `firmware_blob()` call)
**Symbol:** `firmware_blob() -> Option<&'static [u8]>`
**Why it matters:** Today `firmware_blob()` returns `None` (the Phase 81 staging seam), so every mt792x part degrades; Track A must return the connac3 ROM-patch + RAM-code for the MT7925 device that was claimed.

**Acceptance:**
- [ ] `firmware_blob()` returns the staged MT7925 ROM-patch + RAM-code (distinct from mt7961) when the claimed device matches `is_mt7925`.
- [ ] `Mt792x::bring_up` receives `Some(blob)` and proceeds into `download_firmware` rather than the degrade path — *Validated-on-HW (run N, date)*.

### A.3 — Adapt the MCU firmware-download handshake to connac3

**Files:**
- `userspace/drivers/mt792x/src/fw_proto.rs` (`cmd::*`, `decode_patch_sem`)
- `userspace/drivers/mt792x/src/fw.rs` (`download_firmware`)
- `kernel-core/src/mt792x/firmware.rs` (parsers)
**Symbol:** `cmd::{PATCH_SEM_CONTROL, PATCH_START_REQ, PATCH_FINISH_REQ, FW_SCATTER, TARGET_ADDRESS_LEN_REQ, FW_START_REQ}`, `download_firmware`, `parse_patch_sections`/`parse_fw_trailer`
**Why it matters:** The Phase 81 opcodes + sequence are connac2 (`mt76_connac_mcu.h`); connac3 changes the MCU command framing and the patch/RAM-code descriptor shape, so the connac2 handshake will not load MT7925 firmware.

**Acceptance:**
- [ ] connac3 MCU command IDs / framing and patch/RAM-code parse deltas are encoded behind a connac-generation selector (connac2 path unchanged for mt7961/mt7921/mt7922).
- [ ] `cargo test -p kernel-core` covers the connac3 opcode constants + patch/trailer parse against the upstream `mt7925` values (the host-test guard, since QEMU has no mt76 model).
- [ ] On hardware the MT7925 reaches firmware-running (ROM-patch + RAM-code uploaded, `FW_START_REQ` accepted, ready predicate observed) — *Validated-on-HW (run N, date)*.

### A.4 — connac3 WFDMA / MCU-ring offsets + chip-ID readback

**Files:**
- `userspace/drivers/mt792x/src/init.rs` (`Mt792x::bring_up`, `soft_reset`)
- `userspace/drivers/mt792x/src/mcu.rs` (`McuRing`)
- `kernel-core/src/mt792x/{regs,mcu}.rs`
**Symbol:** `MT_WFDMA0_GLO_CFG` / `reset_complete` / the WFDMA-enable ordering; `MT_HW_CHIPID` readback; `McuRing::{submit,reap}`
**Why it matters:** connac3 moves some WFDMA ring/CSR offsets and the MCU TXD/RXD descriptor layout; the CRITICAL "rings programmed before TX/RX DMA-enable" ordering must hold so the WFDMA engine never DMAs to a stale/zero ring pointer.

**Acceptance:**
- [ ] connac3 WFDMA + MCU-ring register offsets resolved (via the reg-remap window) and covered host-side in `kernel_core::mt792x::regs`; the rings-before-DMA-enable ordering is preserved.
- [ ] The chip-ID readback returns a plausible MT7925 silicon ID on hardware (`mt792x: chip_id=0x…`) — *Validated-on-HW (run N, date)*.

### A.5 — `net.nic` registration + supplicant association + DHCP over Wi-Fi

**Files:**
- `userspace/drivers/mt792x/src/io.rs` (`run_io_loop`, RX/TX rewrite + EAPOL demux)
- `userspace/drivers/mt792x/src/main.rs` (`net.nic` / `net.nic.ingress` registration)
- Phase 104 `wifi-core` supplicant daemon (reused)
**Symbol:** `SERVICE_NAME = "net.nic"`, `INGRESS_SERVICE_NAME = "net.nic.ingress"`, `SERVER_READY_SENTINEL`
**Why it matters:** Registration on the shared `net.nic` surface is what makes the MT7925 a first-class interface with no network-layer changes; the Phase 104 supplicant + the in-kernel DHCP client then bind a lease with zero new code.

**Acceptance:**
- [ ] The MT7925 driver emits `MT792X_SMOKE:server:READY` and registers `net.nic` + `net.nic.ingress` (kernel binds the Wi-Fi NIC) — *Validated-on-HW (run N, date)*.
- [ ] The Phase 104 supplicant completes association + the WPA2 (or WPA3-SAE where required) handshake against a real AP.
- [ ] A **DHCP lease binds over Wi-Fi** (`[dhcp] bound ip=…/… gw=…` over the `mt792x` NIC), captured over the network sink — *Validated-on-HW (run N, date)*.

---

## Track B — Bare-Metal AMD-Vi Validation

### B.1 — Identity-map fallback floor (must never wedge boot)

**Files:**
- `kernel/src/iommu/mod.rs` (`init`, `build_and_bring_up_amdvi`)
- `kernel/src/iommu/amd.rs` (`AmdViUnit::new`, `bring_up`)
**Symbol:** `build_and_bring_up_amdvi`, `IdentityFallbackReason::AmdViInitFailed`, `install_identity_fallback`
**Why it matters:** AMD-Vi has never run on real AMD silicon; the highest-risk outcome is a bring-up that wedges DMA. The graceful fallback must be proven first — a real AMD-Vi must still boot the machine even if translation fails.

**Acceptance:**
- [ ] With AMD-Vi enabled in OmniBook firmware, m3OS boots to login with NVMe + xHCI DMA intact, logging **either** `iommu.unit.brought_up vendor=amdvi` **or** `iommu.fallback.identity reason=amdvi_init_failed` — never a hang/panic in `init()` — *Validated-on-HW (run N, date)*.
- [ ] The serial log records **which** outcome was reached (translating vs identity fallback) and the IVRS unit count (`iommu init: N IOMMU unit(s) discovered`).

### B.2 — IVRS parse + per-BDF translating domains (isolation milestone)

**Files:**
- `kernel/src/iommu/amd.rs` (`build_bdf_groups_from_ivrs`, `group_bdf_domains`, `claim_device`, `create_domain`)
**Symbol:** `AmdViUnit::{create_domain, claim_device, group_bdf_domains}`, `build_bdf_groups_from_ivrs`
**Why it matters:** The isolation milestone — the first real-silicon proof the device-table programming, IVRS BDF grouping, and page-table reach work against actual AMD hardware rather than QEMU's model.

**Acceptance:**
- [ ] `build_bdf_groups_from_ivrs` parses the OmniBook's real IVRS into non-empty BDF groups; at least one device (NVMe or xHCI) is `claim_device`'d into a translating domain — *Validated-on-HW (run N, date)*.
- [ ] DMA from a claimed device succeeds through its domain page table (NVMe read/xHCI transfer completes) with no spurious event-log fault — *Validated-on-HW (run N, date)*.

### B.3 — COMPLETION_WAIT + fault-IRQ delivery on real silicon

**Files:**
- `kernel/src/iommu/amd.rs` (`submit_and_wait`, `drain_event_log`, `install_fault_handler`, `amdvi_fault_irq_trampoline`)
- `kernel-core/src/iommu/amd.rs` (`decode_event_log_entry`, `AmdViFaultEvent`)
**Symbol:** `CommandEntry::completion_wait`, `submit_and_wait`, `drain_event_log`, `amdvi_fault_irq_trampoline`
**Why it matters:** COMPLETION_WAIT semantics and MSI fault-IRQ delivery are exactly what QEMU does not stress; this proves the barrier completes against a real store and a real fault reaches the decoder.

**Acceptance:**
- [ ] A `COMPLETION_WAIT` posted after a device-table update completes (the completion word is observed set within the bounded poll) on real hardware — *Validated-on-HW (run N, date)*.
- [ ] An induced (or naturally-occurring) IOMMU fault delivers an MSI through `amdvi_fault_irq_trampoline` and `drain_event_log` logs a `decode_event_log_entry`-decoded `AmdViFaultEvent` (structured `iommu.amd.fault` line) — *Validated-on-HW (run N, date)*.

---

## Track C — fam1Ah (Zen 5) Microcode Blob

### C.1 — Add the fam1Ah container to `amd-ucode.bin`

**Files:**
- `kernel/initrd/lib/firmware/amd-ucode.bin`
- `kernel/src/arch/x86_64/microcode.rs` (`apply_microcode_on_cpu`)
- `kernel-core/src/microcode.rs` (`find_applicable_amd_patch`)
**Symbol:** `apply_microcode_on_cpu`, `find_applicable_amd_patch`, `MSR_AMD64_PATCH_LOADER`
**Why it matters:** The bundled blob is fam19h (Zen 4); Strix is fam1Ah (Zen 5), so today the equivalence-table match fails and no microcode is applied. The matcher already handles container parse + equivalence + revision gating, so this is data, not logic.

**Acceptance:**
- [ ] The fam1Ah `linux-firmware` container is added to `amd-ucode.bin` (concatenated/chained); `find_applicable_amd_patch` parses every equivalence table.
- [ ] On the Strix CPU, `apply_microcode_on_cpu` logs `applied patch …` (patch-level MSR readback advanced + verified) **or** a clean `no newer microcode in blob` skip when the BIOS shipped a newer revision — *Validated-on-HW (run N, date)*.
- [ ] Dell (Intel) and QEMU boots log the unchanged skip path (no MSR write on a non-matching CPU) — regression-free; serial cited.

---

## Track D — AMD I2C-HID Touchpad Backend

> The HID protocol layer (descriptor fetch, input-report polling, report-protocol parse → `mouse_server` inject) is the Phase 102 transport, reused unchanged. Only the controller + GPIO-IRQ backend is new. References: `i2c-designware` + `pinctrl-amd` (facts/register layout only).

### D.1 — AMD DesignWare I2C controller backend (`AMDI0010`)

**File:** `kernel/src/drivers/i2c/dw_amd.rs` (new module) — or the Phase 102 I2C controller crate, AMD backend
**Symbol:** new `AmdDwI2c` controller (DesignWare transfer engine at the `AMDI0010` MMIO base / IRQ from ACPI `_CRS`)
**Why it matters:** The OmniBook touchpad sits on an AMD DesignWare I2C block (`AMDI0010`), not the Intel LPSS `dwiic` the Dell uses in Phase 102; the transfer engine register map is largely shared but the MMIO base + IRQ differ and come from ACPI.

**Acceptance:**
- [ ] The `AMDI0010` controller's MMIO base + IRQ resolve from ACPI `_CRS` (Phase 101 enumeration); the DesignWare PIO transfer engine completes a START/address/read/STOP against the touchpad's I2C address — *Validated-on-HW (run N, date)*.
- [ ] The backend exposes the same byte-level I2C read/write the Phase 102 I2C-HID transport consumes (no HID-layer changes) — verified by the Phase 102 transport binding to it.

### D.2 — `AMDI0030` / `pinctrl-amd` GPIO interrupt routing

**File:** `kernel/src/drivers/gpio/pinctrl_amd.rs` (new module)
**Symbol:** new `PinctrlAmd` GPIO bank (`AMDI0030`) — configure the HID `GpioInt` pin + route to an IRQ
**Why it matters:** The I2C-HID transport needs to know when a report is ready; the touchpad's `GpioInt` resource resolves to an AMD GPIO pin that must be configured (input, falling-edge/level) and routed to an interrupt rather than polled.

**Acceptance:**
- [ ] The touchpad's `GpioInt` (from ACPI `_CRS`) is mapped to an `AMDI0030` bank pin, configured as the documented trigger, and delivers an interrupt the I2C-HID transport consumes — *Validated-on-HW (run N, date)*.

### D.3 — Cursor moves via the Phase 102 transport

**Files:**
- the Phase 102 I2C-HID transport + report parser (reused)
- `userspace/.../mouse_server` (inject path, reused)
**Symbol:** the Phase 102 report-protocol parse → `mouse_server` inject
**Why it matters:** The end-to-end milestone — the built-in pointer moving the GUI cursor proves the AMD controller + GPIO backend correctly feed the reused HID stack.

**Acceptance:**
- [ ] Touchpad input reports parse through the Phase 102 report parser and the cursor moves in the bare-metal GUI session (on-device-render or photo evidence of cursor motion + click) — *Validated-on-HW (run N, date)*.

---

## Track E — Keyboard-Path Confirmation

### E.1 — Determine i8042-PS/2 vs I2C-HID keyboard on metal

**Files:**
- `kernel/src/arch/x86_64/ps2.rs` (the existing i8042 path)
- `kernel/src/lib.rs` (the PS/2 init call — already a safe no-op on a pure-I2C-HID laptop)
- Track D backend (if the keyboard is I2C-HID)
**Symbol:** the `ps2` init path / `current` i8042 keyboard handling
**Why it matters:** If the OmniBook keyboard is i8042 PS/2 it already works with zero changes; if it is I2C-HID it rides the Track D controller with the HID keyboard report parser. The branch must be determined on real hardware, not assumed.

**Acceptance:**
- [ ] The keyboard transport (i8042-PS/2 vs I2C-HID) is determined on the OmniBook and recorded with evidence — *Validated-on-HW (run N, date)*.
- [ ] The built-in keyboard produces keystrokes at the login prompt: via the existing `ps2.rs` path (no change) **or** via the Track D I2C-HID backend with the HID keyboard parser — *Validated-on-HW (run N, date)*.

---

## Documentation Notes

- This is a **hardware-only** phase: every track's acceptance is a recorded run under `docs/appendix/bare-metal-validation.md` (Phase 98 Track A.5), marked **"Validated-on-HW (run N, date)"** with a serial/network-sink/photo/on-device-render evidence pointer. The only CI-checkable items are the host-testable connac3 opcode/parse deltas (Track A, `cargo test -p kernel-core`) and the microcode-matcher no-op on Dell/QEMU (Track C).
- Record **which** AMD-Vi outcome was reached (Track B: isolated translating domains vs `amdvi_init_failed` identity fallback) — both are acceptable for the floor, only the former is the isolation milestone; the audit matrix wants the distinction explicit.
- The MT7925 is the project's first **connac3** part; note in the `mt792x` crate header that connac2 (mt7961/mt7921/mt7922) and connac3 (mt7925) share `is_mt792x` device-ID matching but **not** firmware or MCU framing — keep the `mt76`-citation provenance convention.
- The AMD I2C-HID backend (Track D) is **new code** beneath the **reused** Phase 102 transport — keep the controller/GPIO glue cleanly separable from the HID layer so the Intel-LPSS (Dell) and AMD (OmniBook) backends share the protocol parser.
- The fam1Ah blob (Track C) is data-only over the existing matcher; confirm it cannot regress the Dell/QEMU skip path before the OmniBook run.
- Prefer exact files/symbols over directories as the new modules land; update these checkboxes with the recorded-run pointers as tracks complete.
