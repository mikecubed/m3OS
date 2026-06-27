# Phase 108 - HP OmniBook / AMD Strix Point Bring-up

**Status:** Planned
**Source Ref:** phase-108
**Depends on:** Phase 96 (bare-metal bring-up + boot rescue) ✅, Phase 55a/67 (AMD-Vi IOMMU coded + host-tested) ✅, Phase 81 (mt792x connac2 Wi-Fi) ✅, Phase 102 (I2C-HID protocol layer — Track D backend), Phase 104 (Wi-Fi supplicant daemon — Track A reuse)
**Builds on:** Reuses the bus-agnostic substrate proven on the Dell Tiger Lake laptop across Phases 96–104 (UEFI GOP framebuffer, the xHCI host stack, the ring-3 NVMe block driver, the 8-bit-ID xAPIC, and the Phase-96 boot-rescue fixes), and the Phase 102 I2C-HID protocol/transport layer — adding an **AMD** controller backend rather than a new protocol. Extends the Phase 81 connac2 `mt792x` driver to **connac3** (the MT7925 / Filogic 360 Wi-Fi 7 part). Performs the first real-silicon exercise of the Phase 55a/67 AMD-Vi IOMMU.
**Primary Components:** `userspace/drivers/mt792x` (connac3 MCU/WFDMA adaptation + MT7925 firmware seam), `kernel-core/src/mt792x/{firmware,mcu,regs}.rs` (connac3 parse/opcode deltas), `kernel/initrd/lib/firmware/mt7925/` (new — MT7925 blobs) + `xtask` `stage_wifi_firmware`, `kernel/src/iommu/amd.rs` (`AmdViUnit` bare-metal validation) + `kernel/src/iommu/mod.rs` (`build_and_bring_up_amdvi` / identity fallback), `kernel/initrd/lib/firmware/amd-ucode.bin` + `kernel/src/arch/x86_64/microcode.rs` (fam1Ah blob), the **new** AMD I2C-HID controller backend (`AMDI0010` DesignWare-MMIO I2C + `pinctrl-amd`/`AMDI0030` GPIO) behind the Phase 102 transport, `kernel/src/arch/x86_64/ps2.rs` (keyboard-path confirmation)

## Milestone Goal

Boot the **HP OmniBook Ultra 14-fd0xxx** (Ryzen AI 9 365, Strix Point / Zen 5, board "SBKPF") to a usable login + GUI on bare metal — sequenced *after* the Dell Tiger Lake line (Phases 99–107) proves the stack. Most of the bring-up is a **small delta**: the load-bearing boot/usability paths are CPU-vendor-neutral and already done, so the OmniBook should reach a framebuffer login over GOP + xHCI + NVMe + xAPIC with essentially no new code. The genuinely new work is the four AMD-specific deltas: the **MT7925 connac3 Wi-Fi** driver, the **first bare-metal validation of AMD-Vi**, the **fam1Ah (Zen 5) microcode blob**, and the **AMD I2C-HID touchpad backend**.

## Why This Phase Exists

The forward-arc charter (Phase 98) sequences a second physical machine after the Dell so the project proves its hardware substrate is **portable**, not Tiger-Lake-specific. The OmniBook is the right second target precisely because it shares almost everything at the boot/usability layer — UEFI GOP framebuffer, the xHCI host stack, the ring-3 NVMe driver, and an xAPIC (the Ryzen AI 9 365 is a 10-core / 20-thread part, and 20 LAPIC IDs fit comfortably in the 8-bit `current_lapic_id` field that `kernel/src/arch/x86_64/apic.rs` reads from bits 24–31, so the existing xAPIC path suffices; no x2APIC work). The Phase-96 boot-rescue fixes (USB log persistence, PS/2 fallback, framebuffer write-combining) carry over unchanged.

What does *not* carry over is everything that is **CPU-vendor- or chipset-specific**, and that is what this phase exists to do:

1. **Wi-Fi.** The OmniBook's Wi-Fi is a MediaTek MT7925. Its PCI device IDs already *match* `is_mt792x` (`MT7925_IDS = &[0x7925, 0x0717]` in `kernel-core/src/nic_ids.rs`), so `select_mt792x` will claim it — but the Phase 81 driver is **connac2** and ships only the mt7961 firmware path. The MT7925 is **connac3** (Filogic 360, Wi-Fi 7), with **non-interchangeable** firmware blobs and MCU/WFDMA command differences. This is the gating driver.
2. **AMD-Vi.** The AMD-Vi IOMMU is fully coded and host-tested (`kernel/src/iommu/amd.rs` `AmdViUnit`, brought up via `build_and_bring_up_amdvi`), but has **never run on real AMD silicon** — only QEMU's `-device amd-iommu` and host unit tests. This is the highest-risk item; the codepath already fails graceful to an identity-map fallback (`IdentityFallbackReason::AmdViInitFailed`), so the floor is "boots with DMA intact," and the ceiling is "isolated translating domains."
3. **Microcode.** The bundled `amd-ucode.bin` is a **fam19h** (Zen 4) container; Strix Point is **fam1Ah** (Zen 5). On Strix today the equivalence-table match in `find_applicable_amd_patch` simply fails and the load is a clean no-op skip — correct, but it means no microcode is applied. Adding the fam1Ah container is trivial because the matcher already handles container parse + equivalence + revision gating.
4. **Touchpad.** The built-in pointer is I2C-HID, but behind an **AMD** I2C controller (`AMDI0010`, a DesignWare-MMIO block) with the HID interrupt line on an AMD GPIO (`pinctrl-amd` / `AMDI0030`) — different controller + IRQ glue from the Intel LPSS `dwiic` the Dell uses in Phase 102. The HID protocol layer above the transport is identical, so only the controller/IRQ backend is new.

## Learning Goals

- How a driver that already *matches* a device by PCI ID can still be wrong: connac2 vs connac3 share the `is_mt792x` predicate but not the firmware ABI or the MCU/WFDMA command layout — device-ID match gates *attachment*, not *correctness*.
- Why an IOMMU's first real-silicon run is high-risk even when host-tested: IVRS quirks, device-table reach, completion-wait semantics, and fault-IRQ delivery are exactly the things QEMU's model does not stress, and why a *graceful identity-map fallback* is the design that makes that risk survivable.
- How AMD microcode containers differ from Intel's, and why a family-gated equivalence-table match makes a wrong-family blob a safe no-op rather than a `#GP`.
- How a transport-abstracted HID stack pays off a second time: the same I2C-HID report parser drives the pointer whether the underlying I2C controller is Intel LPSS or an AMD DesignWare block — only the controller register map and the GPIO-interrupt routing change.
- The discipline of a **bare-metal-only validation phase**: no QEMU model exists for any of this, so every acceptance item is a recorded hardware run under the Phase 98 protocol, not a CI gate.

## Feature Scope

### Track A — MT7925 connac3 Wi-Fi (gating)

The OmniBook will not be a daily driver without Wi-Fi (no Ethernet port). The Phase 81 `mt792x` driver claims the MT7925 already (`select_mt792x` → `is_mt792x(0x7925)`), brings up the WFDMA engine, allocates the MCU ring, and degrades cleanly with `FW_ABSENT_SENTINEL` when no blob is staged — but it is connac2. Track A:

- **Bundles the MT7925 firmware blobs** — `WIFI_RAM_CODE_MT7925_1_1.bin` (RAM code) + `WIFI_MT7925_PATCH_MCU_1_1_hdr.bin` (ROM patch), staged under the **new** `kernel/initrd/lib/firmware/mt7925/` directory. These are **not interchangeable** with the mt7961 blobs the README already documents; the `xtask` `stage_wifi_firmware` step already iterates `["mt7961", "mt7922", "mt7925"]`, so the staging plumbing exists — the work is the device→blob routing and the `firmware_blob()` seam returning the connac3 bytes for an MT7925 match.
- **Adapts the connac2 driver to connac3 MCU/WFDMA differences** — the connac2 MCU opcodes in `userspace/drivers/mt792x/src/fw_proto.rs` (`cmd::{PATCH_SEM_CONTROL, PATCH_START_REQ, FW_SCATTER, TARGET_ADDRESS_LEN_REQ, FW_START_REQ}`) and the firmware-download sequence in `fw.rs` (`download_firmware`) are the connac2 handshake; connac3 changes the MCU command framing (the `MCU_CMD`/`MCU_WM_UNI` split and the patch/RAM-code descriptor shape) and some WFDMA ring/CSR offsets. The connac3 deltas live in the host-tested `kernel-core/src/mt792x/{firmware,mcu,regs}.rs` modules so the opcode/parse changes are covered on the host even though no QEMU mt76 model exists.
- **Reuses the Phase 104 Wi-Fi supplicant** — the connect/auth/WPA2 (and WPA3-SAE where the AP requires it) state machine and `wifi-core` supplicant daemon land in Phase 104 against the Dell's AX201; Track A reuses that unchanged once the MT7925 registers its `net.nic` / `net.nic.ingress` endpoints, so association + DHCP-over-Wi-Fi is supplicant work, not driver work.

### Track B — Bare-metal AMD-Vi validation (risk)

The first real exercise of `AmdViUnit::bring_up` with the IOMMU enabled in the OmniBook firmware. The codepath is complete: `AmdViUnit::new` allocates the device-table / command-buffer / event-log, `bring_up` programs the BARs and toggles `EVENT_LOG_EN` → `CMD_BUF_EN` → `IOMMU_EN` in documented order, `submit_and_wait` posts a `COMPLETION_WAIT` barrier, `drain_event_log` decodes faults via `decode_event_log_entry`, and `install_fault_handler` wires the MSI fault IRQ through `amdvi_fault_irq_trampoline`. Track B:

- **Validates the identity-map fallback floor first** — confirm that with AMD-Vi present m3OS either brings up translating domains *or* engages `install_identity_fallback` / `log_identity_fallback(AmdViInitFailed)` with NVMe + xHCI DMA still functional (the device still boots and reaches login). This is the non-negotiable acceptance: a real AMD-Vi must never wedge boot.
- **Then the isolation milestone** — IVRS parse on real firmware (`build_bdf_groups_from_ivrs`), per-device domains via `group_bdf_domains` / `claim_device`, `COMPLETION_WAIT` completing against the real hardware store, and the fault IRQ delivering a decoded event-log entry. Record **which** outcome was reached (isolated domains vs identity fallback) per the validation protocol.

### Track C — fam1Ah (Zen 5) microcode blob (trivial)

Add the Strix Point (fam1Ah) AMD microcode container to `kernel/initrd/lib/firmware/amd-ucode.bin` (or alongside it, concatenated — the container format chains multiple equivalence tables). `apply_microcode_on_cpu` and `kernel_core::microcode::find_applicable_amd_patch` already do container parse, equivalence-table CPUID-signature matching, and strictly-newer-revision gating, so the only work is sourcing the fam1Ah blob from `linux-firmware` and confirming the matcher applies it (or cleanly skips when the BIOS already shipped a newer revision). On the Dell (Intel) and on QEMU the fam1Ah entry is a clean no-op, so this cannot regress existing boots.

### Track D — AMD I2C-HID touchpad backend

The OmniBook's touchpad is I2C-HID, the same protocol Phase 102 implements for the Dell, but reached through an AMD controller. Track D adds, behind the Phase 102 I2C-HID transport (HID descriptor fetch, input-report polling, report-protocol parse → `mouse_server` inject — all unchanged):

- An **AMD DesignWare I2C controller backend** (`AMDI0010`) — the same DesignWare IP core as Intel LPSS but at the AMD MMIO base / IRQ from ACPI `_CRS` (which Phase 101 ACPI enumeration supplies); the transfer engine register map is largely shared with the Intel `dwiic` reference.
- **`pinctrl-amd` / `AMDI0030` GPIO** for the HID interrupt line — the touchpad's `GpioInt` resource resolves to an AMD GPIO bank pin that must be configured (input, falling-edge / level) and routed to an IRQ so the I2C-HID transport knows when a report is ready, rather than polling.

The HID report parser and the `mouse_server` injection path are reused verbatim from Phase 102; only the controller + GPIO-IRQ glue is new.

### Track E — Keyboard-path confirmation

Determine, on real hardware, whether the OmniBook's built-in keyboard is i8042 PS/2 (the existing `kernel/src/arch/x86_64/ps2.rs` path, which `kernel/src/lib.rs` already notes is a safe no-op on a pure-I2C-HID laptop) or I2C-HID. If PS/2, no work — it already comes up. If I2C-HID, the keyboard rides the same Track D controller backend with the HID keyboard report parser. This is a small confirmation/branch, recorded with evidence.

## Important Components and How They Work

### `userspace/drivers/mt792x` + `kernel-core/src/mt792x/*` — connac2 → connac3

The driver's *attachment* is already correct: `select_mt792x` claims the MT7925 by ID. Bring-up (`Mt792x::bring_up` in `init.rs`) resets WFDMA, allocates the `McuRing` (`mcu.rs`), and calls `firmware_blob()` (`fw.rs`) — which today returns `None`, so the MT7925 currently degrades with `FW_ABSENT_SENTINEL`. Track A makes `firmware_blob()` return the staged connac3 bytes for an MT7925 match and adapts `download_firmware` + the `fw_proto::cmd` opcodes and `kernel_core::mt792x` parsers to the connac3 MCU/WFDMA layout. The pure parse/opcode logic stays host-testable (`cargo test -p kernel-core`); the live MCU handshake + ring DMA are `#[cfg(not(test))]` and validated only on the chip.

### `kernel/src/iommu/amd.rs` — `AmdViUnit` on real silicon

The unit is built behind a `Box` so the fault-IRQ trampoline (`amdvi_fault_irq_trampoline`) can hold a raw pointer for `drain_event_log`. `bring_up` is idempotent and programs the device-table / command-buffer / event-log BARs before toggling the control bits; `submit_and_wait` serializes a command with a `COMPLETION_WAIT` against a store address and polls the completion word. The whole construction is wrapped by `build_and_bring_up_amdvi` in `mod.rs`, whose `Err` arm demotes the slot to `IdentityUnit` and logs `iommu.fallback.identity` — the graceful path Track B validates as the floor.

### The new AMD I2C-HID backend (Track D)

A **new module** (no I2C/DesignWare/pinctrl code exists in the tree today — only the PS/2 path and comments referencing a "pure I2C-HID laptop"). It implements the DesignWare I2C transfer engine against the `AMDI0010` MMIO base and configures an `AMDI0030` GPIO pin as the HID interrupt source, exposing the same byte-level I2C read/write the Phase 102 transport consumes. It does **not** reimplement the HID layer.

### `kernel/src/arch/x86_64/microcode.rs` — fam1Ah (Track C)

Unchanged logic; new data. `apply_microcode_on_cpu` reads `CPUID.1:EAX` (`cpuid_signature`), looks the signature up in every equivalence table the blob carries, and writes `MSR_AMD64_PATCH_LOADER` only on an exact match with a strictly-newer revision, verifying the apply via the patch-level MSR readback. Adding a fam1Ah container makes the Strix CPU match instead of silently skipping.

## How This Builds on Earlier Phases

- **Reuses the Dell substrate from Phases 99–107** — GOP framebuffer + the Phase 100 write-combining user FB, the xHCI host stack, the ring-3 NVMe driver + the Phase 106 NVMe-root bootstrap, the xAPIC, and the Phase 99 SMP/lost-wakeup hardening (the OmniBook is 20-thread; it cannot pin `-smp 1`).
- **Extends Phase 81 (`mt792x` connac2)** to connac3 — the same crate, device-ID registry (`MT7925_IDS`), and `firmware_blob()`/`FW_ABSENT_SENTINEL` degrade contract, with new firmware blobs and connac3 MCU/WFDMA deltas.
- **Reuses Phase 102 (I2C-HID protocol layer)** unchanged and adds only the AMD controller/IRQ backend beneath it.
- **Reuses Phase 104 (Wi-Fi supplicant)** — the connect/WPA daemon built against the Dell's AX201 drives the MT7925 once it registers `net.nic`.
- **First-validates Phase 55a/67 (AMD-Vi)** and **Phase 77/Track-E microcode** on real AMD silicon — work that was coded/host-tested but never run on metal, exactly the audit-debt the Phase 98 charter scheduled against the physical laptops.
- **Reuses the Phase 96 bring-up workflow** — `cargo xtask run --usb-passthrough`, `scripts/ure-vfio-validate.md` SOL capture, and `scripts/m3os-logsink.sh` — generalized under the Phase 98 bare-metal validation protocol.

## Implementation Outline

1. **Bring-up baseline** — UEFI-boot the OmniBook image (Phase 106 USB/NVMe path) and confirm GOP FB + xHCI + NVMe + xAPIC reach a framebuffer login with no AMD-specific work, capturing the boot log per the validation protocol.
2. **Track C (microcode, trivial)** — source the fam1Ah `linux-firmware` container, add it to `kernel/initrd/lib/firmware/amd-ucode.bin`, and confirm `apply_microcode_on_cpu` applies or cleanly skips it; verify Dell/QEMU boots are unaffected.
3. **Track B (AMD-Vi, risk)** — boot with AMD-Vi enabled in firmware; record whether `build_and_bring_up_amdvi` reaches translating domains or `AmdViInitFailed` identity fallback, with NVMe + xHCI DMA intact either way; then drive the isolation milestone (IVRS parse, per-BDF domains, `COMPLETION_WAIT`, fault IRQ).
4. **Track A (Wi-Fi, gating)** — stage the MT7925 blobs under `mt7925/`, route `firmware_blob()` to the connac3 bytes, adapt `download_firmware` + `fw_proto::cmd` + `kernel_core::mt792x` parsers to connac3, bring the chip to firmware-running, register `net.nic`, and run the Phase 104 supplicant to associate + bind a DHCP lease.
5. **Track D (touchpad)** — implement the `AMDI0010` DesignWare I2C backend + `AMDI0030`/`pinctrl-amd` GPIO-interrupt routing beneath the Phase 102 I2C-HID transport; confirm the cursor moves.
6. **Track E (keyboard)** — determine i8042-PS/2 vs I2C-HID on metal; branch the keyboard to the Track D backend only if I2C-HID.

## Acceptance Criteria

All acceptance is **bare-metal-only** — QEMU models none of this hardware. Each item is recorded under the protocol in `docs/appendix/bare-metal-validation.md` (the Phase 98 Track A.5 deliverable) and carries the status convention **"Validated-on-HW (run N, date)"** with a serial/photo/on-device-render evidence pointer, not a bare "Complete."

- **Boot baseline:** the OmniBook reaches a framebuffer login (and a GUI session) on bare metal over GOP FB + xHCI + NVMe + xAPIC with no AMD-specific code, captured boot log cited — *Validated-on-HW (run N, date)*.
- **Track A (Wi-Fi):** the MT7925 loads the connac3 firmware (firmware-running observed, no `FW_ABSENT_SENTINEL`), associates to a real AP via the Phase 104 supplicant, and **binds a DHCP lease over Wi-Fi** (`[dhcp] bound ip=…` over the `mt792x` `net.nic`), captured over the network sink — *Validated-on-HW (run N, date)*. The connac3 opcode/parse deltas pass `cargo test -p kernel-core`.
- **Track B (AMD-Vi):** on real AMD-Vi, m3OS either brings up isolated translating domains (`iommu.unit.brought_up vendor=amdvi` + at least one per-BDF domain + a `COMPLETION_WAIT` completing + a decoded fault-IRQ event) **or** cleanly engages identity-map fallback (`iommu.fallback.identity reason=amdvi_init_failed`) — with NVMe + xHCI DMA intact and boot reaching login in **both** cases. The serial log records **which** outcome was reached — *Validated-on-HW (run N, date)*.
- **Track C (microcode):** on the Strix CPU, `apply_microcode_on_cpu` logs either `applied patch …` (level advanced, readback-verified) or a clean skip (`no newer microcode in blob`); Dell (Intel) and QEMU boots log the unchanged skip path — *Validated-on-HW (run N, date)*.
- **Track D (touchpad):** the AMD I2C-HID touchpad moves the cursor in the GUI session via the Phase 102 transport, with the `AMDI0010` controller and `AMDI0030` GPIO-interrupt line driving report delivery (on-device-render or photo evidence of cursor motion) — *Validated-on-HW (run N, date)*.
- **Track E (keyboard):** the keyboard-transport determination (i8042-PS/2 vs I2C-HID) is recorded with evidence, and the built-in keyboard produces keystrokes at the login prompt — *Validated-on-HW (run N, date)*.

## Companion Task List

- [Phase 108 Task List](./tasks/108-amd-strix-omnibook-tasks.md)

## How Real OS Implementations Differ

- **Linux** drives the MT7925 with `mt7925e` (a distinct connac3 module sharing only the `mt76` core with `mt7921e`), with full firmware-version negotiation, multiple WFDMA queues, MLO/Wi-Fi-7 features, and runtime PM; this phase targets the bring-up subset (single TX/RX path, association + DHCP, no Wi-Fi-7 multi-link).
- **Linux AMD-Vi** (`drivers/iommu/amd/`) handles the full IVRS feature matrix (IOMMUv2 PASID/ATS/PRI, interrupt remapping, guest translation, per-device quirk tables); m3OS validates the baseline DMA-remapping + event-log + completion-wait path and leans on the identity-map fallback for anything it cannot bring up.
- **Production microcode loading** uses an early-initramfs `microcode` cpio the firmware-loader applies before SMP, with the full per-family container set; m3OS embeds one container at compile time and applies it per-CPU at boot — adding fam1Ah is just another equivalence table.
- **`i2c-designware` + `pinctrl-amd`** in Linux are large subsystems (DMA-mode transfers, ACPI/`_DSD` clock-rate properties, full GPIO controller with debounce/wake); m3OS implements the PIO transfer engine + a single GpioInt routing sufficient to deliver HID input reports.
- Real bring-up of a new laptop uses the vendor's ACPI tables, a hardware I2C/USB analyzer, and `dmesg`; this phase substitutes the Phase 96 USB-passthrough/SOL/network-sink workflow and on-device render assertions because that is what the reference machine exposes.

## Deferred Until Later

- **Wi-Fi 7 (802.11be) features** on the MT7925 — MLO / multi-link, 320 MHz, the full connac3 queue set; bring-up targets a single associated link.
- **AMD-Vi advanced features** — IOMMUv2 PASID/ATS/PRI, interrupt remapping, and per-device IVRS quirk handling beyond the baseline DMA-remap + fallback.
- **AMD laptop power/ACPI** (battery, brightness, thermal, S0ix suspend on Strix) — rides the Phase 103 power-management work once it is validated on the Dell; the OmniBook-specific deltas are a follow-on.
- **SoundWire / SOF audio on Strix** — the OmniBook's audio path determination + driver follows the Phase 109 bare-metal-audio investigation.
- **AMD GPU / display acceleration** — the framebuffer is GOP-only; no AMD display-engine or 3D driver.
- **WPA3-SAE / 802.1X** beyond what the Phase 104 supplicant ships — enterprise auth is supplicant work, deferred there.
