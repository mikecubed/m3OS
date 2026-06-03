# Phase 81 — Wi-Fi Reference Driver (MediaTek mt792x family): Task List

**Status:** Planned
**Source Ref:** phase-81
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 67 (IOMMU Substrate) ✅, Phase 74 (IPC Capability Grants) ✅, Phase 77 (Pre-1.0 Correctness — DNS resolver / `getaddrinfo` the wireless link must satisfy) ✅, Phase 79 (Modern NIC — establishes the multi-NIC registry + routing path this phase routes Wi-Fi over) ✅
**Goal:** Land m3OS's first Wi-Fi driver as a ring-3 device-host process for the MediaTek **mt792x** PCIe family (bring-up silicon: **MT7921E `0x14C3:0x7961`** / **MT7922E `0x14C3:0x0616`**, connac2; MT7925/connac3 joins the same registry but is not the bring-up target — see Documentation Notes). The driver reuses the entire Phase 55b/67/79 device-host substrate, downloads the mandatory redistributable vendor firmware, brings up the WM MCU command ring + WFDMA data rings, and presents upward as an Ethernet-shaped L2 NIC through the existing `RemoteNic` facade + `driver_ipc::net` seam, so the kernel TCP/IP stack does **not** change. Because **QEMU has no mt76 device**, every bit/protocol/crypto detail that can be checked without a radio is host-tested in `kernel-core` (hardware logic) and the userspace `wifi-core`/`crypto-lib` crates (802.11 mgmt plane + WPA2-PSK supplicant); real-radio association is a hardware-only VFIO/bare-metal track.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | mt792x PCIe driver shell: Wi-Fi family device-ID registry, BAR0 map + WFDMA reset, firmware ROM-patch + RAM-code parsers/download handshake, WM MCU command ring, WFDMA TX/RX data rings (IOVA from `DmaBuffer`), four-place binary wiring + firmware-staging pipeline | Phase 55b, Phase 67 | Planned |
| B | 802.11 mgmt-frame FSM + WPA2-PSK supplicant in **userspace `wifi-core`** + the **`crypto-lib`** primitives it needs (SHA-1, HMAC-SHA1, PBKDF2, AES-Key-Wrap): scan/auth/assoc state machine, EAPOL-Key 4-way handshake, PMK/PTK/EAPOL-MIC/GTK-unwrap with a precise HOST-vs-CHIPSET crypto split | A.5 (the WM MCU command ring the B.7 key-install seam rides on); `crypto-lib` (B.1–B.3) | Planned |
| C | `RemoteNic` facade integration: register on the `net.nic` seam, L2 frame TX/RX with EAPOL demux to the Track-B FSM, link-state event on association, DHCP + DNS over the wireless link, and a **new** link/medium-aware default-route helper for wired-over-wireless preference | A, B, Phase 79 registry | Planned |
| D | Config surface: `/etc/wpa.conf` parser in `wifi-core`, `m3ctl wifi status` over the userspace Wi-Fi control protocol | B, C | Planned |
| E | Validation: host tests for **all** Track-A/B logic, the QEMU-has-no-mt76 reality + `wifi-smoke` skip-with-reason gate, and a hardware-only VFIO/bare-metal runbook + `docs/research/` capture | A–D | Planned |
| F | Release closeout: kernel `0.81.0` bump, learning doc `docs/81-wifi-reference.md`, README row flip + Tasks-cell link, firmware-license doc `docs/legal/firmware-licenses.md`, design-doc reconciliation, `AGENTS.md` gate-table row | A–E landed | Planned |

> **Ordering note.** Because there is **no QEMU mt76 model**, the Phase-80a "land the architecture change against a QEMU-testable device first" pattern does **not** apply. The risk is retired instead by maximizing host-tested pure logic: Track A's `kernel_core::mt792x` register/descriptor/firmware-parser/MCU modules and Track B's `wifi-core` FSM + `crypto-lib` chain are written and unit-tested **before** any hardware bring-up, so `cargo xtask check` proves the encoders/parsers/FSM/crypto independent of the radio. Hardware-only steps (Track E.4) are explicitly marked, following the Phase 79 (igc/r8125) and Phase 80 (HDA-on-AMD) "skip-with-reason + VFIO runbook" precedent. A and B can proceed in parallel — **B depends on A only for the A.5 MCU command ring** that B.7's key-install seam rides on, not for A.4's firmware path.

> **Build-host note.** This build machine is **not** the user's dev laptop, and the laptop's exact radio is unconfirmed — hence the **family** registry (A.1) rather than a single hardcoded ID. This session can author and host-test all pure logic and the runbook, but cannot bind `vfio-pci`, pass a radio through, associate to a real AP, or pull a real DHCP lease. Every "associates / pulls a lease / `ping` / DNS" acceptance item is therefore an operator action gated behind the Track E.4 runbook, exactly as Phase 80's audible-output items were.

---

## Track A — mt792x PCIe driver shell

### A.1 — Wi-Fi family device-ID registry + class predicate in `kernel-core`

**File:** `kernel-core/src/nic_ids.rs`
**Symbol:** new `WIFI_CLASS: u8 = 0x02` / `WIFI_SUBCLASS: u8 = 0x80` / `WIFI_PROG_IF: u8 = 0x00` consts; `VENDOR_MEDIATEK: u16 = 0x14C3`; family slices `MT7921_IDS: &[u16] = &[0x7961, 0x0608]`, `MT7922_IDS: &[u16] = &[0x7922, 0x0616]`, `MT7920_IDS`, `MT7902_IDS`, `MT7925_IDS: &[u16] = &[0x7925, 0x0717]`; predicates `is_mt7921`/`is_mt7922`/`is_mt7920`/`is_mt7902`/`is_mt7925`/`is_mt792x` reusing the existing `matches(...)` helper; a `MT792X_FAMILIES` table for the disjoint-set tests
**Why it matters:** the m3OS NIC-binding discipline is "match by device ID over a bounded registry, never by marketing name" (Phase 79). Wi-Fi controllers are PCI class `0x02` subclass `0x80` ("Other Network controller"), **not** the Ethernet `0x02/0x00` triple, so a distinct class triple plus the MediaTek vendor (`0x14C3` — not the existing Intel/Realtek consts) is required; the research flags real mis-binding (MT7927 sharing `0x7925`'s architecture), so strict by-ID matching is the defense, and the family slice means the laptop's actual chip (unconfirmed) binds without a code change.

**Acceptance:**
- [x] `is_mt7921(0x7961)`, `is_mt7922(0x0616)`, `is_mt7925(0x7925)` return `true`; the Ethernet IDs (`0x100E`, `0x8125`) and a foreign MediaTek BT ID return `false` from all `is_mt792x` predicates (host test `kernel_core::nic_ids::tests::mt792x_predicates`).
- [x] The mt792x family slices are pairwise-disjoint with no intra-family duplicates, asserted by tests modeled on `all_intel_families_pairwise_disjoint` / `no_duplicate_ids_within_a_family` (new `mt792x_families_pairwise_disjoint`, `no_duplicate_ids_within_mt792x_family`).
- [x] `(WIFI_CLASS, WIFI_SUBCLASS, WIFI_PROG_IF) == (0x02, 0x80, 0x00)` and is distinct from `(ETHERNET_CLASS, ETHERNET_SUBCLASS, ETHERNET_PROG_IF)` (host-asserted).
- [x] `MAX_NICS` is **unchanged** — one combined registry covers Ethernet + Wi-Fi NICs and the Wi-Fi NIC consumes one `NicEntry` slot (host-asserted by a registry-bound test).

### A.2 — Wi-Fi PCI enumeration + chipset selection in `driver_runtime`

**Files:**
- `userspace/lib/driver_runtime/src/pci_enum.rs`
- `userspace/drivers/mt792x/src/lib.rs` (new)

**Symbol:** `enumerate_wifi_functions() -> Vec<PciFunctionId>` (calls `enumerate_pci_class(WIFI_CLASS, WIFI_SUBCLASS, WIFI_PROG_IF)` + `read_vendor_device`); `select_mt792x(functions) -> Option<DeviceCapKey>` reusing the generic `select_nic(functions, VENDOR_MEDIATEK, is_mt792x)`
**Why it matters:** Phase 79 already factored device selection into `select_nic(functions, vendor, is_family: fn(u16)->bool)`; the Wi-Fi driver adds exactly one enumerator (the Wi-Fi class triple) and one `select_*` call, reusing the proven `SYS_DEVICE_PCI_ENUMERATE`/`SYS_DEVICE_CONFIG_READ` path and the `/drivers/` exec-path authorization gate with **zero new syscalls**.

**Acceptance:**
- [x] `enumerate_wifi_functions()` enumerates class `0x02`/subclass `0x80` and returns `PciFunctionId{key, vendor, device}` entries (compiles + host-tested where the syscall surface is mockable; live enumeration is exercised under Track E.4).
- [x] `select_mt792x` returns the first MediaTek mt792x-matching function and `None` otherwise — host test over a synthetic `Vec<PciFunctionId>` (`mt792x_driver::tests::select_prefers_mt792x`).
- [x] The driver crate builds `--features os-binary` (lib/bin split, like `r8169`/`r8125`) so the selection + frame-rewrite logic is host-testable in the `lib` target.

### A.3 — BAR0 MMIO map + WFDMA register file + controller reset

**Files:**
- `userspace/drivers/mt792x/src/init.rs` (new)
- `kernel-core/src/mt792x/regs.rs` (new; host-testable register offsets + reset predicate)

**Symbol:** `kernel_core::mt792x::regs` — `MT_WFDMA0_BASE: usize = 0xD4000`, `MT_WFDMA_EXT_CSR_BASE: usize = 0xD7000`; offsets within WFDMA0 `MT_WFDMA0_RST (+0x100)` with `RST_LOGIC_RST = 1<<4` / `RST_DMASHDL_ALL_RST = 1<<5`, `MT_WFDMA0_GLO_CFG (+0x208)` with `TX_DMA_EN = 1<<0` / `TX_DMA_BUSY = 1<<1` / `RX_DMA_EN = 1<<2` / `RX_DMA_BUSY = 1<<3`, `MT_WFDMA0_RST_DTX_PTR (+0x20C)`, `MT_WFDMA0_RST_DRX_PTR (+0x280)`, `MT_WFDMA0_HOST_INT_STA (+0x200)`; `reset_complete(glo_cfg) -> bool` (`TX_DMA_BUSY`/`RX_DMA_BUSY` clear). Driver-side `Mt792x::bring_up(key, fw)`, `soft_reset`, `MT792X_BAR_INDEX = 0`; PCI BME enable + MSI/MSI-X via the existing device-host plumbing.
**Why it matters:** the WFDMA register window lives in PCI **BAR0** (CPU MMIO via `sys_device_mmio_map` — *never* an IOVA); the reset + GLO_CFG ordering (reset DTX/DRX pointers, then enable TX/RX DMA only after rings are programmed) is the documented "WFDMA enable ordering" pitfall — out-of-order leaves `*_DMA_BUSY` stuck. Keeping offsets/predicates in `kernel-core` makes the bit math host-testable like `kernel_core::r8169::REG_*`.

**Acceptance:**
- [x] Host test asserts the register-offset constants equal the values above and that `reset_complete` reports done only when both `TX_DMA_BUSY` and `RX_DMA_BUSY` are clear (`kernel_core::mt792x::regs::tests::offsets`, `reset_predicate`).
- [ ] *(Hardware-only / E.4.)* The driver claims the device, maps BAR0 via `Mmio::map(handle, MT792X_BAR_INDEX, expected_len)`, asserts a plausible chip-id readback (`mt76_chip` raw hex ∈ {`0x7921`,`0x7922`,`0x7920`,`0x7902`,`0x7925`}), and completes `soft_reset` with `*_DMA_BUSY` cleared.
- [ ] *(Hardware-only / E.4.)* The IOMMU fault ISR (Phase 67) is confirmed subscribed/armed **before** the first DMA is issued (driver logs the fault-handler arming) — the research's #1 first-driver hazard.

### A.4 — Firmware ROM-patch + RAM-code blob parsers (host-tested, synthetic fixtures) + download handshake

**Files:**
- `kernel-core/src/mt792x/firmware.rs` (new; host-testable parsers — model on `kernel_core::r8169::parse_rtl_fw` / `validate_good_firmware`, which test against **synthetic crafted** blobs, not vendor firmware)
- `userspace/drivers/mt792x/src/fw.rs` (new; the on-the-wire download sequence)

**Symbol:** `parse_patch_header(blob) -> Result<PatchHdr, FirmwareError>` decoding `mt76_connac2_patch_hdr` (**big-endian** `hw_sw_ver`/`patch_ver`/`checksum` + `desc.n_region`) and the `mt76_connac2_patch_sec` entries; `parse_fw_trailer(blob) -> Result<FwImage, FirmwareError>` decoding the **trailing** `mt76_connac2_fw_trailer` (`chip_id`/`eco_code`/`n_region`/`format_ver`/`fw_ver[10]`/`crc`, **little-endian**) + the `n_region` × `mt76_connac2_fw_region` (`addr`/`len`/`feature_set`/`type`); `FirmwareError{TooShort, BadMagic, BadRegionCount, BadChecksum, UnalignedRegion, TrailerOutOfBounds}`; a `FirmwareSet{ rom_patch, ram_code }` selected by chip-id; the driver-side `download_firmware(...)` implementing, with the **established upstream connac2 `MCU_CMD_*` opcodes** (`mt76_connac_mcu.h`, pinned host-side in `mt792x_hal::fw_proto::cmd`, NOT guesses): (1) `PATCH_SEM_CONTROL` (payload op = get) → `decode_patch_sem` branch on `PATCH_IS_DL` (skip) vs `PATCH_NOT_DL_SEM_SUCCESS` (proceed); (2) for each parsed `mt76_connac2_patch_sec` (`parse_patch_sections`), `PATCH_START_REQ` at the **section's own `addr`** + `FW_SCATTER` of the section's `[offs, offs+size)` slice chunked at **4096 bytes**; (3) `PATCH_FINISH_REQ`; (4) `PATCH_SEM_CONTROL` (payload op = release); (5) per-region RAM `TARGET_ADDRESS_LEN_REQ` with **`addr = region.addr`, `len = region.len`** (each region carries its own load address; mode from `feature_set` honoring `FW_FEATURE_OVERRIDE_ADDR = BIT(5)`) + `FW_SCATTER`; (6) `FW_START_REQ`; (7) poll firmware-running via `kernel_core::mt792x::regs::fw_n9_ready(MT_CONN_ON_MISC)`. (`MCU_PATCH_ADDRESS = 0x200000` remains as the connac default base for blobs that do not carry per-section addresses.)
**Why it matters:** firmware is **mandatory** for mt792x (unlike the *optional* r8169 PHY firmware) — the chip does nothing until the WM MCU is running. The patch header is big-endian and the RAM image is trailer-based little-endian, so an endianness slip corrupts section addresses (research pitfall #4); the patch-semaphore must skip re-download on `PATCH_IS_DL` or it wedges the MCU (pitfall #3); each RAM region loads to its **own** `region.addr`. Host-testing the parsers against **synthetic crafted** headers/trailers catches these before any DMA, exactly as `r8169::validate_good_firmware` does — committing the real vendor blob is deferred to the F.3 license clearance and exercised only on hardware (E.4).

**Acceptance:**
- [x] Host test builds **synthetic** patch/trailer blobs and asserts `parse_patch_header` returns the crafted `n_region` + big-endian version and `parse_fw_trailer` returns regions whose `addr`/`len` are in-bounds (`kernel_core::mt792x::firmware::tests::parse_synthetic_patch`, `parse_synthetic_ram_trailer`).
- [x] Host test asserts every `FirmwareError` variant on crafted-malformed inputs (truncated, bad magic, region count overflowing the blob, trailer past EOF) — no parser panics on adversarial input.
- [x] Host test models the scatter chunking: a blob of length `N` produces `ceil(N/4096)` `FW_SCATTER` chunks, the last short (`chunking_4096`); and the patch-semaphore branch: `PATCH_IS_DL` → zero patch sections downloaded, `PATCH_NOT_DL_SEM_SUCCESS` → all sections downloaded then released (`patch_sem_branch`).
- [x] Host test parses per-section `mt76_connac2_patch_sec` entries — each carries its **own** big-endian `type`/`offs`/`size`/`info.addr` and the download uses that addr (not a single fixed base), with `[offs, offs+size)` bounds-checked against the blob (`parse_synthetic_patch_sections`, `patch_section_offs_past_blob_is_out_of_bounds`). The driver-side opcodes + semaphore decode are pinned host-side (`mt792x_hal::fw_proto::tests::{fw_constants_match_upstream, patch_sem_decode}`).
- [ ] *(Hardware-only / E.4.)* The full handshake completes against the operator-supplied real blob and the firmware-running poll returns ready before any MCU init command is issued. **The poll register offset + mask are now upstream-derived and host-pinned** (`kernel_core::mt792x::regs::{MT_CONN_ON_MISC, MT_TOP_MISC2_FW_N9_RDY, fw_n9_ready}`, from `mt7921/mcu.c`); the only `[UNCERTAIN]` remainder is the **BAR0 reg-remap window** that maps the connac `0x1800_0000` bus range and the live ready-transition timing, both confirmed under E.3.

### A.5 — WM MCU command ring (FWDL/WM TX queues + MCU RX queue) + TXD/TLV encoders

**Files:**
- `userspace/drivers/mt792x/src/mcu.rs` (new)
- `kernel-core/src/mt792x/mcu.rs` (new; host-testable command-frame + TLV encoding + seq matching)

**Symbol:** `kernel_core::mt792x::mcu` — `encode_mcu_txd(cid, s2d_index, set_query, seq, payload) -> [u8; N]` packing the connac2 `mt76_connac2_mcu_txd` (8-dword HW TXD + `cid` + `pkt_type = 0xA0` + `s2d_index` (HOST→WM = `0x00`) + `seq`); a TLV builder `push_tlv(buf, tag, value)`; `match_response(seq, rx_dword) -> McuMatch{Matched, Stale, Mismatch}`; queue identifiers `MT_MCUQ_FWDL`, `MT_MCUQ_WM`, `MT_RXQ_MCU`. Driver-side `McuRing` allocates the queues as `DmaBuffer<T>` (IOVA in `desc_base`), submits on `MT_MCUQ_FWDL` during A.4 and `MT_MCUQ_WM` thereafter, and reaps replies from `MT_RXQ_MCU` with a per-command timeout.
**Why it matters:** MCU commands are DMA-submitted and replies arrive **asynchronously** on a separate RX queue, matched by `seq` — failing to match by sequence number (or to time out) deadlocks on the wrong event (research pitfall #5). First bring-up needs only the **WM** co-processor (skip WA); keeping the TXD/TLV packing + seq-matching in `kernel-core` makes the wire format host-testable.

**Acceptance:**
- [x] Host test asserts `encode_mcu_txd` sets `pkt_type == 0xA0`, the `s2d_index` byte for HOST→WM, and round-trips `cid`/`seq` (`kernel_core::mt792x::mcu::tests::txd_encode`).
- [x] Host test asserts `push_tlv` produces tag/len/value with correct length framing and 4-byte alignment (`tlv_framing`).
- [x] Host test asserts `match_response` returns `Matched` only for the live `seq`, `Stale` for an older `seq`, and `Mismatch` otherwise (`seq_matching`).
- [ ] *(Hardware-only / E.4.)* A `GET_NIC_CAPABILITY` (or equivalent init query) issued on `MT_MCUQ_WM` returns a matched reply on `MT_RXQ_MCU` within the timeout.

### A.6 — WFDMA TX/RX data rings with IOVA from `DmaBuffer` + token model

**Files:**
- `userspace/drivers/mt792x/src/rings.rs` (new)
- `kernel-core/src/mt792x/dma.rs` (new; host-testable descriptor encode + ring-index + token-pool math)

**Symbol:** `kernel_core::mt792x::dma` — `Mt76Desc{buf0, ctrl, buf1, info}` (`#[repr(C)]`, 16-byte, `const _: () = assert!(size_of::<Mt76Desc>() == 16)`); `encode_tx_desc(iova, len, token) -> Mt76Desc` writing the IOVA low/high into `buf0`/`buf1` and length + `MT_DMA_CTL_DMA_DONE`/last-segment bits into `ctrl`; `rx_desc_done(ctrl) -> bool` / `rx_desc_len(ctrl) -> u16`; `split_iova(iova) -> (lo, hi)` (reuse `driver_runtime::net_ring::split_iova` semantics); a `TokenPool` (`idr`-style) with `acquire()`/`release(token)` and `MAX_TOKENS`. Driver-side `DataRings` allocate ring backing + per-descriptor buffers as `DmaBuffer<T>` and program `desc_base`/`ring_size`/`cpu_idx`/`dma_idx`; one data TXQ + one data RXQ for first light.
**Why it matters:** every address in a descriptor (`buf0`/`buf1`), every ring `desc_base`, and every FW-staging buffer is a **device DMA address the chipset dereferences** — under VT-d/AMD-Vi it **must** be the `DmaBuffer` IOVA, never host-physical (research pitfall #1, the single most likely first-driver bug). The TX **token must be allocated before** the buffer list is written into the descriptor, or DMA mappings leak (a mainline CVE-class fix, pitfall #2) — encoding the ordering in the host-tested `encode_tx_desc` + `TokenPool` API makes it impossible to reverse silently.

**Acceptance:**
- [x] Host test asserts `size_of::<Mt76Desc>() == 16` (compile-time `const _`) and that `encode_tx_desc(iova, len, token)` places `split_iova(iova).0` in `buf0` and `.1` in `buf1` (proves the argument is plumbed into the descriptor; `kernel_core::mt792x::dma::tests::tx_desc_iova`).
- [x] Host test asserts `rx_desc_done`/`rx_desc_len`: for a `ctrl` word with the DMA-done bit set and length field `L`, `rx_desc_done` returns `true` and `rx_desc_len` returns `L`; with the done bit clear, `rx_desc_done` returns `false` (`rx_decode`).
- [x] Host test asserts the token-before-buffer ordering at the API level: `encode_tx_desc` **requires** a `token` argument acquired from `TokenPool` (grep-verifiable: no `encode_tx_desc` overload without a token), and `acquire`/`release` round-trip without leaking under `MAX_TOKENS` churn (`token_pool_roundtrip`).
- [ ] *(Hardware-only / E.4.)* The descriptor-IOVA correctness — that the value passed to `encode_tx_desc` and to a ring `desc_base` is `DmaBuffer::iova()` and **not** `user_ptr()` — is confirmed by the IOMMU fault ISR staying silent across sustained DMA; `MT_WFDMA0_GLO_CFG` is set to `TX_DMA_EN | RX_DMA_EN` only **after** rings are programmed and DTX/DRX pointers reset. *(The host test above proves only that the supplied argument is plumbed into the descriptor, not that it is the IOVA — that distinction is hardware-only.)*

### A.7 — Four-place binary wiring (`mt792x` driver) + `wifi-core` workspace member

**Files:**
- `Cargo.toml` (root `members` — add `userspace/drivers/mt792x` **and** `userspace/wifi-core`)
- `xtask/src/main.rs` (`build_userspace` `bins` array + `--features os-binary` map + `populate_ext2_files` service conf)
- `kernel/src/fs/ramdisk.rs` (`generated_initrd_asset!` static + the `/drivers/mt792x` `DRIVERS_ENTRIES`/`BIN_ENTRIES` tuple)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`)
- `kernel/initrd/etc/services.d/mt792x_driver.conf` (via `populate_ext2_files`)

**Symbol:** the four AGENTS.md wiring places for a new userspace binary, applied to `mt792x_driver`, plus the `wifi-core` lib as a workspace member; the service conf `name=mt792x_driver\ncommand=/drivers/mt792x\ntype=daemon\nrestart=on-failure\nmax_restart=5\n` (the service `name` is `mt792x_driver`; `/drivers/mt792x` is the `command=` ramdisk path)
**Why it matters:** AGENTS.md "Adding a New Userspace Binary" requires **four distinct** wiring places — miss the `bins` array and the driver is never built into the image; miss the ramdisk entry and `execve` returns `ENOENT`; miss the `.conf`/`KNOWN_CONFIGS` and `init` never spawns it. `r8169`/`r8125` each appear in all four. `wifi-core` is a **lib only** (no binary, no ramdisk/conf entry) but must still be a workspace member or it is not built or checked.

**Acceptance:**
- [x] `userspace/drivers/mt792x` **and** `userspace/wifi-core` are added to root `Cargo.toml` `members`.
- [x] `mt792x_driver` is added to the `bins` array in `build_userspace` with `needs_alloc = true` (it uses `alloc`/`kernel-core`) and the `--features os-binary` map.
- [x] `static MT792X_DRIVER_ELF = generated_initrd_asset!("mt792x_driver")` + a `/drivers/mt792x` ramdisk tuple are added to `kernel/src/fs/ramdisk.rs`.
- [x] `mt792x_driver.conf` is present in `populate_ext2_files` **and** `KNOWN_CONFIGS`; after `cargo xtask clean` + boot, `init` logs `init: driver.registered name=mt792x_driver` (the daemon spawns).

### A.8 — Firmware-staging pipeline (operator-supplied blob; graceful absence)

**Files:**
- `xtask/src/main.rs` (`build_userspace` / `populate_ext2_files` — the firmware-staging step)
- `userspace/drivers/mt792x/src/fw.rs` (`firmware_blob() -> Option<&'static [u8]>` — `include_bytes!` when present, else `None`)
- `kernel/initrd/lib/firmware/mt7961/` (staging path; **blob bytes are NOT committed until F.3 clears the license**)

**Symbol:** the firmware-delivery seam — `include_bytes!` of the blob inside the driver crate (the `r8169`/`r8125` `firmware_blob()` pattern, currently `None`) **or** the `static_initrd_asset!` / `populate_ext2_files` staging path under `kernel/initrd/lib/firmware/`; a build that **degrades gracefully** (logs a skip-with-reason, like the musl `SKIP`) when the operator has not supplied the blob
**Why it matters:** the design doc's Implementation Outline step 1 is "land the firmware-blob staging path"; the anchors confirm there is **no** `request_firmware` syscall, so the established `include_bytes!`-in-driver-crate pattern is the default. The pipeline must land as code **independently of the blob bytes**, because committing the real MediaTek blob is the F.3 redistribution decision — so an absent blob produces a clean skip, never a build break.

**Acceptance:**
- [x] The xtask firmware-staging step + `firmware_blob()` seam land; with **no** blob present, `cargo xtask check` and the build succeed and the driver logs a clear "firmware blob absent — Wi-Fi disabled, see docs/legal/firmware-licenses.md" message (no panic, no build break).
- [x] The `include_bytes!`-vs-initrd-asset delivery decision is recorded in F.3 (driver-ELF size vs initrd asset trade-off), not silently chosen.
- [ ] When the operator supplies the cleared blob at the staged path, `firmware_blob()` returns `Some(..)` and A.4's download path consumes it (exercised on hardware, E.4).

---

## Track B — 802.11 mgmt-frame FSM + WPA2-PSK supplicant (userspace `wifi-core` + `crypto-lib`)

> **Placement (resolves the layering blockers).** The entire 802.11 management plane and WPA2-PSK supplicant — mgmt-frame builders, the association FSM, the EAPOL-Key codec, and the KDF — live in a **new userspace lib crate `userspace/wifi-core/`** that depends on `crypto-lib`. They do **not** go in `kernel-core`: `kernel-core` cannot depend on `crypto-lib` (which pulls in userspace `syscall-lib` + RustCrypto deps), and `kernel_core::net::*` *is* the ring-0 TCP/IP stack — housing the MLME/supplicant there would be ring-0 policy bloat, violating userspace-first. `wifi-core` is `#![no_std] + alloc`, host-tested like `crypto-lib` (added to the `cargo xtask check` crate list, E.1). The only Wi-Fi additions in `kernel-core` are the `nic_ids` family (A.1), the top-level primitive-free `kernel_core::mt792x` hardware module (A.3–A.6 + B.7's TLV encoder), and the routing helper (C.3).

### B.1 — Add missing crypto primitive: SHA-1 + HMAC-SHA1

**Files:**
- `userspace/crypto-lib/src/hash.rs` (add `sha1` + `hmac_sha1`)
- `Cargo.toml` (`[workspace.dependencies]` — add `sha1` + `hmac` if the RustCrypto route is chosen, else a vendored impl)

**Symbol:** `crypto_lib::hash::sha1(data) -> [u8; 20]`; `crypto_lib::hash::hmac_sha1(key, data) -> [u8; 20]`; `HmacSha1State` (incremental)
**Why it matters:** the anchors confirm SHA-1 and HMAC-SHA1 are **MISSING** from the workspace (`crypto-lib` ships only the SHA-256 family + `hmac_sha256` + HKDF). WPA2-PSK's PRF, the PBKDF2 PMK derivation, and the EAPOL-Key MIC (key-descriptor version 2) all require HMAC-SHA1 — without it the 4-way handshake cannot be computed at all. *(The design doc's claim that "Phase 42 covers SHA-1 and HMAC" is false and is corrected in F.4.)*

**Acceptance:**
- [x] Host test: `sha1(b"abc")` matches the FIPS-180 known-answer vector; `hmac_sha1` matches the RFC 2202 test vectors (`crypto_lib::hash::tests::sha1_kat`, `hmac_sha1_rfc2202`).
- [x] SHA-1 is documented as used **only** for the WPA2 KDF/MIC (not any security-sensitive new use), recorded in the learning doc + Documentation Notes.

### B.2 — Add missing crypto primitive: PBKDF2-HMAC-SHA1 (PMK derivation)

**File:** `userspace/crypto-lib/src/hash.rs` (new `pbkdf2_hmac_sha1`)
**Symbol:** `crypto_lib::hash::pbkdf2_hmac_sha1(passphrase, salt, iterations, out: &mut [u8])`; a thin `wpa_pmk(passphrase, ssid) -> [u8; 32]` calling it with `iterations = 4096`, `dkLen = 32`
**Why it matters:** the anchors confirm **no `pbkdf2` crate** exists. WPA2-PSK derives the 256-bit PMK as `PBKDF2(HMAC-SHA1, passphrase, SSID, 4096, 32)` where the **raw SSID bytes are the salt** (not null-terminated, not hashed). Computed once per network and cached.

**Acceptance:**
- [x] Host test: `wpa_pmk(b"password", b"IEEE")` matches the published IEEE 802.11i PSK test vector (`crypto_lib::hash::tests::wpa_pmk_kat`).
- [x] Host test asserts the SSID is used verbatim as the salt and the iteration count is exactly 4096 (vector-checked, not just structurally).

### B.3 — Add missing crypto primitive: AES Key-Wrap / RFC 3394 (GTK)

**Files:**
- `userspace/crypto-lib/src/symmetric.rs` (new `aes_key_wrap` + `aes_key_unwrap`)
- `Cargo.toml` (the `aes` `0.8` crate is **already a workspace dependency** — currently used for AES-256-CTR via `Aes256`; B.3 adds an `Aes128` instantiation, no new AEAD crate)

**Symbol:** `crypto_lib::symmetric::aes_key_wrap(kek: &[u8;16], key: &[u8]) -> Vec<u8>` and `aes_key_unwrap(kek: &[u8;16], wrapped: &[u8]) -> Result<Vec<u8>, CryptoError>` implementing the RFC 3394 6-iteration wrap/unwrap over `Aes128` ECB encrypt/decrypt
**Why it matters:** the anchors confirm **no `aes-kw` crate** exists, but the `aes` block cipher is already a dependency (used today only via `Aes256`). The GTK delivered in EAPOL M3 key-data is AES-Key-Wrapped under the KEK (PTK bytes 16..32); unwrapping it is the **only** AES primitive the host needs — per-packet CCMP is done by the chipset, so **no software AES-CCM is required**. Implementing both wrap and unwrap lets the canonical RFC 3394 §4.1 vector (a *wrap* vector) be checked directly.

**Acceptance:**
- [x] Host test: `aes_key_wrap` reproduces the RFC 3394 §4.1 128-bit-KEK / 128-bit-key wrap vector and `aes_key_unwrap` inverts it back to the plaintext key (`crypto_lib::symmetric::tests::aes_kw_rfc3394`).
- [x] Host test asserts the integrity-check value (the `A6A6…` IV) is verified and a tampered wrapped blob returns `Err(CryptoError)` (`aes_kw_rejects_tampered`).
- [x] A `// no software AES-CCM` comment + Documentation Notes entry records that CCMP is chipset-offloaded (TK installed via MCU; the host never encrypts a data frame).

### B.4 — 802.11 management-frame builders + RSN IE encode/decode

**File:** `userspace/wifi-core/src/mgmt.rs` (new; host-testable)
**Symbol:** builders `build_probe_request(ssid, rates)`, `build_auth_open(seq)`, `build_assoc_request(ssid, rsn_ie, rates)`; `RsnIe` encoder producing the exact 22-byte CCMP+PSK IE (`30 14 / 01 00 / 00 0F AC 04 / 01 00 / 00 0F AC 04 / 01 00 / 00 0F AC 02 / 00 00`); `parse_probe_response(frame) -> BssInfo{ssid, bssid, channel, rsn}` extracting and validating the AP's RSN IE (element id 48 / `0x30`) for CCMP-pairwise + PSK-AKM
**Why it matters:** WPA2-PSK uses **Open-System** 802.11 auth (the real auth is the later 4-way handshake), then an Assoc-Request carrying the station's RSN IE. The station's emitted RSN IE must be **byte-identical** to the one re-sent in EAPOL M2 (the AP cross-checks it for downgrade) — encoding it once in a host-tested builder guarantees that invariant. This is the soft-MAC management plane the chip does **not** run.

**Acceptance:**
- [x] Host test asserts `RsnIe::ccmp_psk().encode()` equals the exact 22-byte sequence above (`wifi_core::mgmt::tests::rsn_ie_ccmp_psk`).
- [x] Host test round-trips a synthetic Probe-Response and asserts `parse_probe_response` extracts the SSID/channel and accepts CCMP+PSK / rejects a TKIP-only or WPA1 AP (`probe_response_rsn_accept`, `rejects_tkip_only`).
- [x] Host test asserts the Auth frame sets Auth-Algorithm = 0 (Open), Seq = 1, Status = 0 (`auth_open_open_system`).

### B.5 — Association FSM (scan → auth → assoc → handshake → connected)

**File:** `userspace/wifi-core/src/fsm.rs` (new; host-testable pure `on_event` reducer)
**Symbol:** `enum WifiState{Init, Scanning, Authenticating, Associating, Handshake(HandshakeStep), Connected, Failed(FailReason)}`; `WifiFsm::on_event(WifiEvent) -> Vec<WifiAction>` where events are `ProbeResp/AuthResp/AssocResp/Eapol(msg)/Timeout/Deauth` and actions are `SendMgmt(frame)/SendEapol(frame)/InstallKey(KeyMaterial)/PurgeKeys/Emit(status)`; bounded retransmit counters + per-step timeouts (auth/assoc ~200–500 ms; 4-way global ~few seconds)
**Why it matters:** this is the host-supplied 802.11 state machine that `mac80211` + `wpa_supplicant` run in Linux and that mt792x firmware does **not** run for the STA path (soft-MAC-with-offload, not full-MAC). Keeping it a pure reducer in `wifi-core` makes the whole connection lifecycle host-testable with synthetic events, no radio required.

**Acceptance:**
- [x] Host test drives the happy path `Init → Scanning →(ProbeResp)→ Authenticating →(AuthResp)→ Associating →(AssocResp)→ Handshake →(M1..M4)→ Connected` and asserts the emitted action sequence (`wifi_core::fsm::tests::happy_path`).
- [x] Host test asserts each failure edge: AssocResp status 43/45 → `Failed(BadRsnParams)` with no retry; 4-way global timeout → `Deauth` + `Failed(HandshakeTimeout)` (the "wrong passphrase" manifestation); M3 MIC-verify failure → frame dropped, **no** `InstallKey` emitted (`assoc_status_fail`, `handshake_timeout`, `m3_mic_fail_no_install`).
- [x] Host test asserts `Deauth`/disconnect emits `PurgeKeys` so stale keys leave the chipset (`disconnect_purges_keys`).
- [x] Host test asserts replay-counter handling: the FSM answers the **latest** EAPOL replay counter and ignores a stale one (`replay_counter_monotonic`).

### B.6 — WPA2-PSK 4-way handshake: PTK derivation + EAPOL-Key MIC + GTK unwrap

**Files:**
- `userspace/wifi-core/src/eapol.rs` (new; host-testable EAPOL-Key frame codec + key-info bits)
- `userspace/wifi-core/src/kdf.rs` (new; PTK derivation orchestrating the B.1–B.3 `crypto-lib` primitives)

**Symbol:** `eapol::EapolKeyFrame` codec over the byte layout (802.1X header + descriptor type 2 + Key Information + Key Length + Replay Counter + Nonce + MIC + Key-Data); `eapol::KeyInfo` bitfield helpers (desc-version 2; per-message constants M1 `0x008A` / M2 `0x010A` / M3 `0x13CA` / M4 `0x030A`); `kdf::derive_ptk(pmk, aa, spa, anonce, snonce) -> Ptk{kck, kek, tk}` implementing `PRF-512(PMK, "Pairwise key expansion", min(aa,spa)||max(aa,spa)||min(anonce,snonce)||max(anonce,snonce))` via HMAC-SHA1 counter bytes 0..3 (first 64 of 80 bytes); `eapol::mic_sha1_128(kck, frame_with_zeroed_mic) -> [u8; 16]`; `kdf::unwrap_gtk(kek, m3_keydata) -> Gtk` calling `crypto_lib::symmetric::aes_key_unwrap`
**Why it matters:** this realizes the **HOST-vs-CHIPSET crypto split** precisely. **Host** computes PMK (B.2), the random SNonce (`crypto_lib` CSPRNG), PTK (PRF-512), the EAPOL-Key MIC (HMAC-SHA1-128 under KCK over the zeroed-MIC body), and the GTK unwrap (RFC 3394 under KEK). **Chipset** does per-packet CCMP once the 16-byte TK is installed. The byte-exact frame layout + key-info constants + min/max byte-wise nonce/MAC ordering are the easy-to-corrupt details, so they are host-tested against published vectors.

**Acceptance:**
- [x] Host test: `derive_ptk` reproduces a published WPA2 PTK vector (PMK + AA/SPA + ANonce/SNonce → KCK/KEK/TK), including the `min`/`max` byte-wise lexicographic ordering of MACs and nonces (`wifi_core::kdf::tests::ptk_vector`).
- [x] Host test: `KeyInfo` encodes M1/M2/M3/M4 to `0x008A`/`0x010A`/`0x13CA`/`0x030A` and decodes the Install/ACK/MIC/Secure/Encrypted bits back (`eapol::tests::key_info_per_message`).
- [x] Host test: `mic_sha1_128` is checked against a **reproducible** vector — the KCK from the same published PTK vector (above) is used to MIC a fixed, documented EAPOL M2/M4 body with the MIC field zeroed, so the expected MIC is deterministic and reviewer-recomputable; a one-bit corruption flips the MIC (`eapol::tests::mic_zeroed_field`). *(If a captured frame is used instead, a named pcap — e.g. Wireshark `wpa-Induction.pcap` — is checked in with its provenance recorded, mirroring A.4's fixture discipline.)*
- [x] Host test: `unwrap_gtk` extracts the GTK from a synthetic AES-Key-Wrapped M3 key-data blob and rejects a tampered one (`kdf::tests::gtk_unwrap`).
- [x] Host test asserts the M2 RSN IE equals the B.4 Assoc-Request RSN IE byte-for-byte (downgrade-protection invariant) (`eapol::tests::m2_rsn_ie_matches_assoc`).

### B.7 — Key-install seam (host → chipset TK/GTK via MCU STA_REC)

**Files:**
- `userspace/drivers/mt792x/src/key.rs` (new; bridges the FSM `InstallKey(KeyMaterial)` action to the MCU encoder)
- `kernel-core/src/mt792x/mcu.rs` (the `STA_REC` / `STA_REC_KEY` TLV encoder — primitive-free byte packing, no crypto)

**Symbol:** `kernel_core::mt792x::mcu::encode_sta_rec_key(wcid, cipher, key_idx, key: &[u8])`; driver-side `install_pairwise_key(wcid, tk: &[u8;16])` / `install_group_key(wcid, gtk)` emitting a `STA_REC_UPDATE` MCU command with a `STA_REC_KEY` TLV against the station's WTBL entry; `KeyMaterial` is defined in `wifi-core` and produced by the B.6 KDF
**Why it matters:** this is the one point where the host's derived keys cross into the chipset. After the 4-way handshake, the 16-byte TK (and GTK) are installed into the WTBL via MCU command, and **all subsequent CCMP encrypt/decrypt + replay is done in hardware** — the host hands the chip plaintext frames thereafter. Installing the TK only **after** validating M3 (enforced by B.5) avoids the nonce-reuse/forgery class. The TLV encoder is pure byte-packing of an already-derived key, so it belongs in `kernel_core::mt792x` (no crypto dependency), while the key derivation stays in `wifi-core`.

**Acceptance:**
- [x] Host test asserts the `STA_REC_KEY` TLV encoding for a CCMP pairwise key (cipher selector, key index, 16-byte TK) (`kernel_core::mt792x::mcu::tests::sta_rec_key_ccmp`).
- [x] Host test asserts `install_pairwise_key` is only reachable from the FSM `InstallKey` action (structural/grep check: no path installs a key before `Handshake` reaches the install step).
- [ ] *(Hardware-only / E.4.)* After a real association the TK install MCU command is acknowledged and data frames flow (CCMP done by the chip).

---

## Track C — RemoteNic facade integration

### C.1 — Present the Wi-Fi NIC as an L2 Ethernet NIC over `driver_ipc::net` (with EAPOL demux)

**Files:**
- `userspace/drivers/mt792x/src/io.rs` (new; model on `userspace/drivers/r8169/src/io.rs`)
- `kernel-core/src/driver_ipc/net.rs` (reused unchanged for the data path)

**Symbol:** `run_io_loop(nic, command_endpoint, ingress)` using `NetServer::new(ep).with_ingress_endpoint(ep)` + `NetServer::handle_next(tx_handler, irq_handler)`; the driver registers `ipc_register_service(ep, "net.nic")` (and resolves `"net.nic.ingress"`); TX rewrites the kernel's Ethernet-framed `NET_SEND_FRAME` payload into an 802.11 data frame (LLC/SNAP + 802.11 MAC header) before posting on the data TXQ; RX strips 802.11 → Ethernet. **An RX demux step intercepts EAPOL frames** (LLC/SNAP ethertype `0x888E`) and delivers them to the Track-B FSM as `WifiEvent::Eapol(..)` instead of emitting `NET_RX_FRAME`.
**Why it matters:** the anchors confirm `RemoteNic` registers purely by service name `net.nic` + `net.nic.ingress` and the `driver_ipc::net` seam is **L2-frame-only** — a Wi-Fi NIC presenting Ethernet-shaped frames plugs into the kernel TCP/IP stack with **zero kernel changes** ("Wi-Fi terminates at the data-link layer"). The EAPOL demux is essential: the 4-way-handshake frames arrive as 802.11 *data* frames and must reach the supplicant FSM, not the kernel IP stack — without the demux, M1/M3 would be handed to TCP/IP and the handshake would never complete.

**Acceptance:**
- [x] The driver registers `net.nic` and emits a `MT792X_SMOKE:server:READY\n` sentinel before its event loop (model on `R8169_SMOKE:server:READY`).
- [x] Host test asserts the Ethernet→802.11 TX rewrite (LLC/SNAP `AA AA 03 00 00 00` + ethertype + 802.11 header) and the 802.11→Ethernet RX rewrite round-trip a frame (`mt792x_driver::io::tests::eth_80211_roundtrip`).
- [x] Host test asserts the RX demux: a frame with LLC/SNAP ethertype `0x888E` is routed to `WifiEvent::Eapol` and **not** emitted as `NET_RX_FRAME`; a normal IPv4 frame is emitted as `NET_RX_FRAME` (`io::tests::eapol_demux`).
- [x] `MAX_FRAME_BYTES` (1522) and the `NetFrameHeader` framing are reused unchanged from `driver_ipc::net` (no new L2 message labels).

### C.2 — Link-state event on association + userspace Wi-Fi control protocol

**Files:**
- `userspace/wifi-core/src/control.rs` (new; the userspace↔userspace Wi-Fi control labels — **not** in `kernel-core`)
- `userspace/drivers/mt792x/src/io.rs`
- `kernel-core/src/driver_ipc/net.rs` (reused: only `NET_LINK_STATE` is touched)

**Symbol:** on successful association, the driver emits the existing `NET_LINK_STATE` (`0x5513`) with `NetLinkEvent{up: true, mac, speed_mbps}` so the kernel's `RemoteNic::handle_link_state` → `apply_link_event` marks the link up (and `tcp::on_link_down()` fires on disassociation); a **new userspace** control family `wifi_core::control::{WIFI_SCAN_REQ, WIFI_SCAN_RESULT, WIFI_CONNECT_REQ, WIFI_STATUS, WifiControlError::NotAssociated}` carried `mt792x driver ↔ m3ctl`
**Why it matters:** link-state reuses `NET_LINK_STATE` verbatim on the kernel-consumed seam (so TCP retransmit reacts to a Wi-Fi drop with no kernel change), but scan/connect/status are **userspace policy** — per the userspace-first rule they must flow driver↔`m3ctl` and must **not** pass through the kernel `RemoteNic` facade. Putting the control labels in `wifi-core` (not adjacent to the kernel-consumed `driver_ipc::net`) keeps the kernel out of scan/connect parsing.

**Acceptance:**
- [ ] On association the driver emits `NET_LINK_STATE{up:true,...}` and the kernel `RemoteNic` registry marks the NIC link-up (host test of the `apply_link_event` path + a Track-E.4 live observation); on deauth it emits `up:false` and `tcp::on_link_down()` is invoked.
- [x] The `wifi_core::control` labels encode/decode `WIFI_SCAN_RESULT{bssid, ssid, rssi, channel}` and `WIFI_STATUS{ssid, rssi, ipv4}` round-trip byte-for-byte (host test `wifi_core::control::tests::roundtrip`).
- [x] `WifiControlError::NotAssociated` lives in the userspace control protocol (not `NetDriverError`); the kernel `driver_ipc::net` seam gains **no** Wi-Fi-specific variant (host-asserted: `NetDriverError::to_byte()` mappings unchanged).

### C.3 — DHCP + DNS over the wireless link + link/medium-aware default route

**Files:**
- `kernel-core/src/nic_ids.rs` (new `default_route_index_by_link`)
- `kernel/src/net/remote.rs` (feed per-NIC kind + link-state into the new helper; reuse the Phase-79 `NicEntry` registry)

**Symbol:** a **new** pure helper `default_route_index_by_link(nics: &[NicRoute]) -> Option<usize>` where `struct NicRoute{ is_wireless: bool, link_up: bool }` and the rule is "first link-up wired, else first link-up wireless, else `None`"; the kernel plumbing in `RemoteNic::register`/route-selection passes each `NicEntry`'s medium + link-state into it. The existing count-based `default_route_index` is retained as the degenerate single-NIC case.
**Why it matters:** the design doc's acceptance requires DHCP + DNS over wireless **and** "the routing default picks wired over wireless when both are available." The existing `default_route_index(nic_count)` is **purely count-based** — it always returns `Some(0)` and has no notion of medium or link state, so it **cannot** express wired-over-wireless. This is therefore a **genuine (small) kernel change**, not free reuse: a new link/medium-aware helper plus the plumbing to feed it. (Relying on registration order is rejected — the Wi-Fi driver could register before a wired NIC links up, making the default route non-deterministic.)

**Acceptance:**
- [x] Host test: `default_route_index_by_link` returns the wired index when a link-up wired + link-up Wi-Fi NIC are present; the Wi-Fi index when only Wi-Fi is up; `None` when all are down (`kernel_core::nic_ids::tests::route_prefers_wired_when_both_up`, `falls_back_to_wifi`, `none_when_all_down`).
- [x] The QEMU-testable `dns-smoke` and `multi-nic-smoke` gates still **PASS** after the new route helper + plumbing land (no Phase 77/79 regression — the wired/QEMU path *is* testable even though the radio is not).
- [ ] *(Hardware-only / E.4.)* After association the existing DHCP client pulls a lease over the wireless interface; `ping <gateway>` returns ICMP echo replies (0% loss over N packets).
- [ ] *(Hardware-only / E.4.)* The Phase 77 DNS resolver succeeds over the wireless link — `getaddrinfo("github.com", ...)` returns ≥1 A record.
- [ ] *(Hardware-only / E.4.)* With a wired NIC also up, the default route is the wired NIC (the helper's preference is exercised end-to-end).

---

## Track D — Configuration surface

### D.1 — `/etc/wpa.conf` parser (in `wifi-core`)

**Files:**
- `userspace/wifi-core/src/config.rs` (new; host-testable parser)
- `/etc/wpa.conf` — **operator-supplied** at runtime, NOT committed/staged (it holds the live SSID + PSK credentials, like the operator-staged firmware blob in A.8). The driver reads it best-effort: absent/malformed ⇒ `mt792x_driver: no usable /etc/wpa.conf — passive L2 mode`. The **service** `.conf` (`mt792x_driver.conf`) + `KNOWN_CONFIGS` wiring is A.7 and is unrelated to this credential file.

**Symbol:** `wifi_core::config::parse_wpa_conf(text) -> Result<WpaConfig, ConfigError>` parsing `ssid=...`, `psk=...`, optional `freq=2.4|5` into `WpaConfig{ ssid, psk, freq: Band }`
**Why it matters:** the design doc scopes config to a single static `/etc/wpa.conf` read at boot (no `wpa_supplicant` daemon at 1.0). Keeping the parser in `wifi-core` makes the (untrusted, on-disk) config parsing host-testable. The PSK→PMK conversion (B.2) happens at config load and the plaintext passphrase is zeroed once the PMK is cached.

**Acceptance:**
- [x] Host test parses `ssid=Home\npsk=secret123\nfreq=5\n` into `WpaConfig{ssid, psk, freq: Band::Ghz5}` and rejects malformed/missing-PSK input with a typed `ConfigError` (`wifi_core::config::tests::parse_valid`, `rejects_missing_psk`).
- [x] Host test asserts the 8–63-char passphrase length bound (the PBKDF2 input constraint) is enforced (`rejects_short_psk`).
- [x] The PSK is converted to the PMK via B.2 at config load and the plaintext passphrase buffer is volatile-zeroed afterward (`config.rs` `zero_secret`) — `WpaConfig` exposes only the derived PMK (`pmk()`), never the raw passphrase (`config::tests::no_passphrase_getter`).

### D.2 — `m3ctl wifi status` read-only diagnostics

**Files:**
- `userspace/m3ctl/src/main.rs` (add a `wifi status` subcommand)
- `userspace/wifi-core/src/control.rs` (`WIFI_STATUS` query, reused from C.2)

**Symbol:** `m3ctl wifi status` issuing a `WIFI_STATUS` query to the `mt792x` driver over the userspace control protocol and printing associated SSID, signal strength (RSSI), and assigned IPv4
**Why it matters:** the design doc's acceptance requires `m3ctl wifi status` to report SSID, signal strength, and IPv4 — a read-only diagnostic so the operator can confirm association without a packet capture. It reuses the C.2 control labels (userspace↔userspace), adding no kernel path.

**Acceptance:**
- [x] Host test asserts the `m3ctl wifi status` formatter renders a `WIFI_STATUS{ssid, rssi, ipv4}` value into the expected human-readable lines (`m3ctl::tests::wifi_status_format`).
- [x] When not associated, `m3ctl wifi status` prints "not associated" (driven by `WifiControlError::NotAssociated`) rather than erroring.
- [x] The driver-side responder logic — mapping supplicant state → `WifiStatus` (associated ⇒ SSID populated; otherwise empty-SSID) — is host-tested (`wifi_core::control::WifiStatus::for_connection` + `control::tests::status_for_connection`).
- [ ] *(Hardware-only / E.4.)* `m3ctl wifi status` on the dev laptop reports the associated SSID, a plausible RSSI, and the DHCP-assigned IPv4. **Driver-side wiring note:** the driver's `run_io_loop` blocks on the single `net.nic` `NetServer::handle_next` endpoint, and `NetServer` does not surface non-net labels; multiplexing the read-only `wifi.control` responder onto the driver's IPC endpoint (so `m3ctl` can query it live) is the remaining E.4 hardware step — the `WifiStatus::for_connection` responder it calls is implemented + host-tested above, but serving it live is exercised only on the radio (no QEMU mt76 model). Until then `m3ctl wifi status` degrades to "not associated".

---

## Track E — Validation

### E.1 — Host-test coverage for all Track-A/B logic

**Files:**
- `kernel-core/src/mt792x/{regs,firmware,mcu,dma}.rs` (`#[cfg(test)] mod tests`)
- `userspace/wifi-core/src/{mgmt,fsm,eapol,kdf,config,control}.rs` (`#[cfg(test)] mod tests`)
- `userspace/crypto-lib/src/{hash,symmetric}.rs` (new test vectors)
- `xtask/src/main.rs` (`cmd_check` — add `mt792x_driver` and `wifi-core`; `crypto-lib` and `kernel-core` are already listed)

**Symbol:** the full `tests` modules behind every Track-A/B symbol; the `cargo xtask check` crate list
**Why it matters:** because **QEMU cannot exercise the radio**, the host tests are the *primary* correctness gate — they cover firmware parsing (synthetic crafted fixtures), MCU/TXD/TLV encoding, ring/descriptor/token math, the mgmt-frame builders + RSN IE, the association FSM, and the entire WPA2 crypto chain (PMK/PTK/MIC/GTK) against published vectors. This mirrors Phase 79/80 putting `nic_ids`/`r8169`/`hda` host tests in `kernel-core`, and `crypto-lib`'s existing host-test treatment now extends to `wifi-core`.

**Acceptance:**
- [x] `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` passes the new `mt792x` test modules; `cargo test -p wifi-core --target x86_64-unknown-linux-gnu` passes the mgmt/fsm/eapol/kdf/config/control modules.
- [x] `cargo xtask check` (clippy `-D warnings` + rustfmt + host tests) passes with `mt792x_driver` and `wifi-core` added to the check list and the new `crypto-lib` vectors green.
- [x] Firmware-parser tests use **synthetic crafted** blobs (BE patch header + LE trailer, every `FirmwareError` variant) — the `r8169` precedent — so the real vendor blob is **not** a checked-in CI fixture (license-gated; parsed against shipping firmware only on hardware, E.4).

### E.2 — `wifi-smoke` xtask gate (skip-with-reason; no QEMU mt76 model)

**Files:**
- `xtask/src/main.rs` (`cmd_wifi_smoke` — model on `cmd_multi_nic_smoke`'s igc/r8169/r8125 skip-with-reason arm)
- `AGENTS.md` (opt-in gate table)

**Symbol:** `cmd_wifi_smoke`; the skip-with-reason branch printing "no QEMU mt76 model — run on real hardware via VFIO; `kernel-core`/`wifi-core` host tests cover the firmware-parser/MCU-encoder/FSM/crypto logic" unless `M3OS_WIFI_REGRESSION=1`
**Why it matters:** the anchors confirm `multi-nic-smoke` already handles QEMU-unmodeled NICs (igc/r8169/r8125) by **skipping with a reason** and pointing at the VFIO runbook + the host tests. A Wi-Fi NIC has **no** QEMU model at all, so `wifi-smoke` is structurally a skip-with-reason gate whose real assertion is "the host tests passed" — it must not silently masquerade as a radio test.

**Acceptance:**
- [x] `cargo xtask wifi-smoke` without `M3OS_WIFI_REGRESSION=1` prints the skip-with-reason and exits success, explicitly stating QEMU has no mt76 model and that the host tests are the coverage.
- [x] With `M3OS_WIFI_REGRESSION=1` the gate references the E.3 VFIO runbook and (on the dev laptop only) asserts `init: driver.registered name=mt792x_driver` + `MT792X_SMOKE:server:READY` + the association sentinel.
- [x] The gate is registered in the AGENTS.md opt-in table with env var `M3OS_WIFI_REGRESSION=1`.

### E.3 — Hardware-only VFIO / bare-metal validation runbook + `docs/research/` capture

**Files:**
- `scripts/mt792x-vfio-validate.md` (new; model on `scripts/r8125-vfio-validate.md` + `scripts/hda-vfio-validate.md`)
- `docs/research/mt792x-wifi-capture.md` (new; empirical register/MCU/firmware-state capture, model on `docs/research/hda-realtek-capture.md`)

**Symbol:** the operator runbook (identify the radio BDF + `[14c3:79xx]`, confirm IOMMU-group isolation, `vfio-pci new_id 14c3 <id>`, boot QEMU with `-device vfio-pci,host=<bdf>` pinned to a slot clear of the fixed-BDF driver sentinels, expect `mt792x_driver: spawned` → firmware download → MCU ready → association); the capture doc recording on-silicon behavior QEMU cannot model
**Why it matters:** Phase 79 (`r8125-vfio-validate.md`) and Phase 80 (`hda-vfio-validate.md` + `hda-realtek-capture.md`) established that QEMU-unmodeled hardware is validated via a VFIO passthrough runbook plus a `docs/research/` capture. Wi-Fi is the strongest case: **none** of the radio path is QEMU-testable, and the firmware-running poll register is explicitly unknown until observed on hardware.

**Acceptance:**
- [x] `scripts/mt792x-vfio-validate.md` exists with the full unbind-host → bind-vfio-pci → pass-through → restore sequence + the expected serial sentinels, pinned to a PCI slot clear of the e1000/nvme/ac97/xhci fixed-BDF sentinels.
- [x] `docs/research/mt792x-wifi-capture.md` exists and records (or has placeholder slots for) the real chip-id, firmware version, the **resolved firmware-running poll register/value** (the A.4 `[UNCERTAIN]` item), and the MCU init sequence.
- [x] The runbook is explicit that this build host is **not** the user's laptop and the association/DHCP/`ping` steps require operator root + a real AP.

### E.4 — Real-radio bring-up on the dev laptop (hardware-only)

**Files:**
- (no new source; the E.3 runbook + the driver from Tracks A–D)
- `docs/research/mt792x-wifi-capture.md` (capture the run)

**Symbol:** the end-to-end path: PCI claim → BAR0 map → reset → firmware download → MCU ready → scan → open-auth → assoc → WPA2-PSK 4-way handshake → TK install → DHCP lease → `ping` → DNS
**Why it matters:** this is the design doc's headline acceptance. It is **operator-only** on real hardware — like Phase 80's audible-output and Phase 79's r8125 items — because this build host cannot bind vfio-pci, pass the radio through, or reach a real AP.

**Acceptance:**
- [ ] *(Hardware-only / operator action.)* On the dev laptop the driver claims the mt792x radio, downloads the operator-supplied firmware, and the WM MCU reports running.
- [ ] *(Hardware-only.)* The driver associates with the WPA2-PSK AP in `/etc/wpa.conf`, installs the TK/GTK via MCU, and pulls a DHCP lease over the wireless interface.
- [ ] *(Hardware-only.)* `ping <gateway>` over the wireless interface returns ICMP echo replies (0% loss over N packets) and `getaddrinfo("github.com", ...)` returns ≥1 A record.
- [ ] *(Hardware-only.)* With a wired NIC also present + up, `default_route_index_by_link` selects the wired NIC (no Phase 79 regression).
- [ ] The run (success or the precise failure point) is captured in `docs/research/mt792x-wifi-capture.md`.

---

## Track F — Release closeout

### F.1 — Bump kernel version to `0.81.0`

**Files:**
- `kernel/Cargo.toml` (`version = "0.80.0"` → `"0.81.0"`)
- `AGENTS.md` (`kernel **v0.80.0**` → `**v0.81.0**`; add **one** capability bullet for the new Wireless capability class per the file's "keep it small" policy)

**Symbol:** `version` (Cargo manifest) + the AGENTS.md capability-inventory version string + a new "Wireless" capability bullet
**Why it matters:** the kernel version is the release marker for the phase; the AGENTS.md maintenance policy permits the version bump and (because Wi-Fi is a genuinely new capability class) exactly one new capability bullet.

**Acceptance:**
- [x] `kernel/Cargo.toml` reads `version = "0.81.0"` and `AGENTS.md` reads `kernel **v0.81.0**`; `cargo xtask check` passes.
- [x] A scoped check confirms the **kernel release marker** no longer reads `0.80.0`: `kernel/Cargo.toml` reads `version = "0.81.0"` and AGENTS.md reads `kernel **v0.81.0**`. The only remaining matches of `grep -rn '0\.80\.0' kernel/ userspace/ kernel-core/ xtask/ --include=*.toml --include=*.rs` are `userspace/drivers/hda/Cargo.toml` and `userspace/drivers/ac97/Cargo.toml` — independently-versioned Phase-80 audio-driver crate manifests, NOT the kernel release marker, which Phase 81 deliberately does not touch. (The broad repo grep is not used; landed phase docs under `docs/roadmap/` legitimately retain prior versions.)
- [x] AGENTS.md gains one Wireless bullet (e.g. "**Wireless**: ring-3 MediaTek mt792x Wi-Fi driver — firmware-blob download, WM MCU command ring, WFDMA TX/RX rings, soft-MAC 802.11 mgmt FSM + WPA2-PSK 4-way handshake (host crypto in `wifi-core`/`crypto-lib`) with chipset CCMP offload, presenting as an L2 `RemoteNic`").

### F.2 — Author `docs/81-wifi-reference.md` learning doc + cross-link

**Files:**
- `docs/81-wifi-reference.md` (new)
- cross-link from `docs/roadmap/81-wifi-reference.md`

**Symbol:** new learning doc following the design-doc template sections in `docs/appendix/doc-templates.md`
**Why it matters:** AGENTS.md mandates a learning doc per phase (Phase 79 shipped `docs/79-modern-nic.md`, Phase 80 `docs/80-intel-hda-audio.md`).

**Acceptance:**
- [x] `docs/81-wifi-reference.md` exists and conforms to the design-doc template sections.
- [x] It covers: the layering (PCIe MMIO + firmware download → WM MCU → host 802.11 mgmt FSM → IP stack); why mt792x is **soft-MAC-with-offload** not full-MAC (host runs the MLME + the WPA2 supplicant **inside the ring-3 driver**, not a daemon); the precise **HOST-vs-CHIPSET crypto split** (host: PMK/PTK/EAPOL-MIC/GTK-unwrap; chipset: per-packet CCMP); the **IOVA-vs-host-phys-vs-MCU-address** distinction; the firmware ROM-patch + RAM-code format + download handshake + patch-semaphore branch; **where the code lives** (`kernel_core::mt792x` hardware logic vs userspace `wifi-core` supplicant vs `crypto-lib` primitives, and why kernel-core cannot host the crypto); and why the kernel TCP/IP stack is unchanged (Wi-Fi terminates at L2 and emits Ethernet-shaped frames through `RemoteNic`).

### F.3 — Firmware-redistribution license doc (prerequisite for committing any real blob)

**Files:**
- `docs/legal/firmware-licenses.md` (new — the `docs/legal/` directory does not yet exist)
- `kernel/initrd/lib/firmware/mt7961/` (staging path; bytes committed **only after** this review clears)

**Symbol:** the firmware-license record reproducing linux-firmware's `WHENCE` "Redistributable" block for `WIFI_MT7961_patch_mcu_1_2_hdr.bin` / `WIFI_RAM_CODE_MT7961_1.bin` (MT7921), `WIFI_MT7922_*` / `WIFI_RAM_CODE_MT7922_1.bin` (MT7922), and (if MT7925 staged) `mt7925/WIFI_*`
**Why it matters:** the design doc's acceptance requires the MediaTek firmware redistribution license to be reviewed and recorded **before merge**, and Tracks A.4/E.1 are explicitly written to need **no** committed vendor blob (synthetic fixtures + operator-supplied bytes) precisely so this review is not bypassed. The blobs are "Redistributable" in linux-firmware's `WHENCE` (MediaTek's terms, not GPL) — shippable unmodified but the exact license block must be reproduced; same model as Intel `iwlwifi`.

**Acceptance:**
- [x] `docs/legal/firmware-licenses.md` exists and reproduces the verbatim `WHENCE` "Redistributable" block for each mt792x blob the project intends to ship, with the upstream linux-firmware source path + commit recorded.
- [x] The doc states the blobs are shipped unmodified and names the exact filenames staged under `kernel/initrd/lib/firmware/`.
- [x] The A.8 firmware-delivery decision (`include_bytes!`-in-driver vs `generated_initrd_asset!`/`populate_ext2_files` initrd asset) is recorded, with the driver-ELF-size vs initrd-asset trade-off noted.

### F.4 — Roadmap README row flip + design-doc reconciliation + gate table

**Files:**
- `docs/roadmap/README.md` (Phase 81 row)
- `docs/roadmap/81-wifi-reference.md` (the design doc — several factual corrections on landing)
- `AGENTS.md` (opt-in gate table — add the `M3OS_WIFI_REGRESSION` row)

**Symbol:** README row 81 Status + Tasks cells; the design-doc corrections enumerated below
**Why it matters:** the roadmap README is the canonical status index, and the design doc currently contains several claims that this plan contradicts and must reconcile so an implementer reading only the design doc is not misled.

**Acceptance:**
- [x] On this planning PR, README row 81 Tasks cell links `./tasks/81-wifi-reference-tasks.md` (replacing "Deferred until implementation planning"); on landing, Status flips `Planned → Complete` (or the Phase-80-style honest "Driver-side complete; radio validation hardware-only").
- [x] **Chipset target reconciled:** the design doc names MT7925; this plan targets the **mt792x family** (MT7921/MT7922 connac2 first, MT7925 in the same registry). The design doc's `Builds on`, `Feature Scope` Track A, `Acceptance Criteria`, and `Deferred Until Later` are updated to the family framing (the laptop's exact chip is unconfirmed; the family registry binds whatever is present).
- [x] **False crypto claim struck:** the design-doc lines "Reuses Phase 42's HMAC-SHA1 and PBKDF2" and "The Phase 42 crypto primitives cover SHA-1 and HMAC; PBKDF2 is a thin wrapper" are corrected — SHA-1/HMAC-SHA1/PBKDF2/AES-Key-Wrap are **absent** from the workspace and are introduced by Track B.1–B.3.
- [x] **`wifi-core` relocation recorded:** the design doc's Primary Components `userspace/drivers/wifi-core/` and `kernel-core/src/net/wifi/` are corrected to `userspace/wifi-core/` (a lib linked into the driver) + the top-level `kernel_core::mt792x` hardware module — reflecting that the supplicant/MLME is userspace policy and kernel-core cannot depend on `crypto-lib`.
- [x] **Dangling reference repointed:** the design doc's "Phase 74a §3 documents the laptop reality" is repointed to `docs/appendix/audit-status/74a-pre-1.0-audit.md` (74a is an audit artifact, not a phase).
- [x] **Cross-OS section made honest:** the design doc's "How Real OS Implementations Differ" is updated to state Wi-Fi is greenfield with no peer Rust-microkernel reference (Redox/Managarm/SerenityOS are wired-only) and to cite Fuchsia's SME/MLME split and FreeBSD `net80211` + userspace `wpa_supplicant` + hardware CCMP as the borrowed references.
- [x] AGENTS.md gate table lists `wifi-smoke` under `M3OS_WIFI_REGRESSION=1` in the exact `| Gate | Env var |` row shape used by `multi-nic-smoke`/`hda-smoke`.

---

## Documentation Notes

- **Scoping decisions (one chipset family, one band, one auth method).** Phase 81 is explicitly a *stub of a real Wi-Fi stack*, documented as such: the MediaTek mt792x PCIe family only; one band (5 GHz preferred, 2.4 GHz fallback); WPA2-PSK only (no WPA3-SAE, 802.1X/EAP, OWE). Roaming, BSS transitions, mesh, power-save, regulatory database, AP/Wi-Fi-Direct modes, Bluetooth coexistence on combo chips, and a real `wpa_supplicant`/`iwd` daemon are all deferred. The connection is read from `/etc/wpa.conf` once at boot.
- **Connac2 first, MT7925/connac3 in the same registry.** The design doc names the laptop's **MT7925** (connac3, the unified `MCU_UNI_CMD` surface + MLO). The research recommends bringing up **MT7921/MT7922 (connac2, legacy `MCU_CMD`)** first because its command surface is simpler and far better documented, then adding MT7925 to the **same `nic_ids` family registry** (A.1). Because this build host is not the user's laptop and the laptop's exact chip is unconfirmed, the family registry is the correct mechanism regardless — binding MT7925 later is additive, not a rewrite.
- **Where the code lives (and why — resolves the two architecture blockers).** The 802.11 management plane + WPA2-PSK supplicant (mgmt builders, FSM, EAPOL codec, KDF, `wpa.conf` parser, control protocol) live in the **userspace `wifi-core` lib crate**, which depends on `crypto-lib`. They are **not** in `kernel-core`, for two independent reasons: (1) `kernel-core` cannot depend on `crypto-lib` (which pulls in userspace `syscall-lib` + RustCrypto deps), so the KDF/MIC code would not even compile there; and (2) `kernel_core::net::*` *is* the ring-0 TCP/IP stack — housing the MLME/supplicant under `kernel_core::net::wifi` would risk compiling policy into ring 0, violating the userspace-first rule. The **only** Wi-Fi additions in `kernel-core` are the `nic_ids` family slices/predicates (A.1), the top-level **primitive-free** `kernel_core::mt792x` hardware module (register/firmware-parser/MCU-TXD/DMA-descriptor/STA_REC_KEY-TLV math — the convention `r8169`/`hda` follow), and the link/medium-aware route helper (C.3). The crypto primitives go in `crypto-lib`. This also reconciles the design doc's intended `wifi-core` crate (relocated from `userspace/drivers/` to `userspace/wifi-core/` as a lib).
- **HOST-vs-CHIPSET crypto split (the central scoping answer).** mt792x is **soft-MAC-with-offload**, not full-MAC: the **host** runs the entire 802.11 management plane (scan/auth/assoc — B.4/B.5) and the WPA2-PSK key management (PMK via PBKDF2-HMAC-SHA1, PTK via PRF-512, the EAPOL-Key MIC via HMAC-SHA1-128, and the GTK unwrap via RFC 3394 — B.6), while the **chipset** does per-packet CCMP (AES-CCM) encrypt/decrypt + replay entirely in hardware once the 16-byte TK is installed in the WTBL (B.7). The only AES primitive the host needs is the raw AES-128 block cipher (for the RFC-3394 GTK unwrap) — **no software AES-CCM is implemented**. Host crypto footprint: PBKDF2-HMAC-SHA1, HMAC-SHA1 (PRF + MIC), AES-128-ECB (key-wrap/unwrap), and a CSPRNG for SNonce.
- **The chosen chipset is the *opposite* of the minimal-host-code path — an accepted cost.** The research's biggest scope lever is that a **full-MAC** chipset would push the 802.11 MLME (and sometimes the 4-way handshake) into NIC firmware, shrinking the host to a thin command/event driver with **no** Track B at all. m3OS instead targets mt792x (soft-MAC-with-offload) because that is the family the dev laptop ships, so m3OS owns the full mgmt plane + key management (the large Track B). This is a deliberate trade — targeting real hardware over the research's minimum-effort path — and is why Track B, not Track A, is the bulk of the work.
- **Missing crypto primitives are added, not assumed.** The anchors confirm the workspace **lacks** SHA-1, HMAC-SHA1, PBKDF2, and AES-Key-Wrap (RFC 3394); `crypto-lib` ships only the SHA-256 family + ChaCha/AES-256-CTR + Ed25519/X25519. B.1–B.3 add exactly those four. The `aes 0.8` crate is already a workspace dependency (used today via `Aes256` for AES-256-CTR); B.3 adds an `Aes128` instantiation — no new AEAD crate. SHA-1 is added *only* for the WPA2 KDF/MIC and documented as such. The design doc's contrary "Phase 42 covers SHA-1/HMAC/PBKDF2" claim is false and is corrected in F.4.
- **IOVA vs host-phys vs MCU-address is the #1 first-driver hazard.** Every descriptor `buf0`/`buf1`, every ring `desc_base`, and every firmware-scatter buffer is a **device DMA address** and under VT-d/AMD-Vi must be `DmaBuffer::iova()` — never host-physical. The WFDMA register offsets (`0xD4000 + …`) are **CPU MMIO** into BAR0 (host VA via `Mmio::map`). The firmware load addresses (`0x200000`, per-region `region.addr`) are **chip-internal MCU addresses** passed opaquely inside MCU payloads — neither host-phys nor IOVA. The host test (A.6) proves only that `encode_tx_desc`'s argument is plumbed into the descriptor; that the argument *is* the IOVA (not `user_ptr()`) is confirmed hardware-only via the IOMMU fault ISR staying silent.
- **TX token allocate-before-buffer-write ordering** is a real mainline CVE-class fix: the token must be acquired from the `TokenPool` before the buffer list is written into the descriptor, or DMA mappings leak. The `encode_tx_desc(iova, len, token)` API encodes this by *requiring* the token argument.
- **Firmware is mandatory and blob-format-specific.** Unlike the *optional* r8169 PHY firmware, mt792x does nothing until the WM MCU is running. The patch header is **big-endian**; the RAM image is **trailer-based little-endian**; each RAM region loads to its own `region.addr`; the patch-semaphore must skip re-download on `PATCH_IS_DL`. Parsers are host-tested against **synthetic crafted** fixtures (A.4/E.1), following the `r8169::validate_good_firmware` precedent — the real vendor blob is **not** a checked-in CI fixture (license-gated, F.3) and is parsed against shipping firmware only on hardware (E.4). The firmware-running poll register/value is `[UNCERTAIN]` upstream and is resolved/captured in E.3.
- **No firmware-load syscall exists.** The device-host ABI has no `request_firmware`/`sys_device_firmware*`. The established pattern is `include_bytes!` of the blob inside the driver crate via `firmware_blob()` (A.8); if the blobs (hundreds of KB) bloat the driver ELF, the alternative is initrd assets — recorded in F.3, not silently decided. The build degrades gracefully (skip-with-reason) when no blob is supplied.
- **The RX EAPOL demux is a load-bearing seam.** The 4-way-handshake EAPOL frames arrive as 802.11 *data* frames (LLC/SNAP ethertype `0x888E`); the driver must divert them to the Track-B FSM (C.1), **not** emit them as `NET_RX_FRAME` to the kernel IP stack, or the handshake never completes. A-MSDU de-aggregation / fragmentation on RX is handled by the chip (the host sees already-deaggregated frames) for 1.0; anything the chip does not handle is deferred.
- **The wired-over-wireless route is a genuine (small) kernel change, not free reuse.** The existing `default_route_index(nic_count)` is purely count-based and cannot express medium preference, so C.3 adds a link/medium-aware helper + the kernel plumbing to feed it. Registration-order is explicitly rejected (the Wi-Fi driver could register before a wired NIC links up).
- **QEMU cannot test this — host tests are the primary gate.** There is **no QEMU mt76 device**; neither the serial smoke harness nor the QMP/PPM framebuffer harness can exercise the radio. Everything from "submit a descriptor to WFDMA" onward is hardware-only. The phase maximizes host-tested pure logic and the `wifi-smoke` gate is a **skip-with-reason** pointing at those host tests + the VFIO runbook — the Phase 79 igc/r8125 and Phase 80 HDA-on-AMD precedent.
- **This build host is NOT the user's laptop.** This session can author + host-test all pure logic and the runbook but cannot bind vfio-pci, pass a radio through, associate, or pull a lease. Every "associates / DHCP / `ping` / DNS" item (E.4) is an operator action behind the VFIO runbook, mirroring Phase 80's audible-output items.
- **Honest cross-OS comparison.** **Redox has NO Wi-Fi stack** (wired-only over smoltcp) — it must **not** be cited as a Wi-Fi reference; on this axis Redox is exactly where m3OS is. **Managarm** and **SerenityOS** are likewise wired-only. **Genode** is the *most directly comparable microkernel precedent*: it ported ~215k LOC of Linux `iwlwifi`+`mac80211` via DDE-Linux and runs `wpa_supplicant` as a **separate userspace component**. **Haiku** ports BSD `iwm`/`iwx`. The architectural references worth citing are **Fuchsia's SME/MLME split** (a userspace management entity, supplicant in-house) and **FreeBSD/NetBSD `net80211` + userspace `wpa_supplicant`**. m3OS deliberately makes the **Fuchsia-style** choice — the supplicant is the WPA2 crypto chain in `wifi-core` executed **inside the ring-3 `mt792x` driver process** (folded in), **not** a separate daemon (Genode/BSD-style) — justified by the 1.0 scope (no enterprise/EAP, single static config). This still satisfies the "supplicant belongs in userspace, not the kernel" technique because the driver runs in ring 3, distinct from the kernel TCP/IP stack. Note `net80211` offloads CCMP to hardware *when available* but retains a software CCMP path; m3OS deliberately **requires** hardware CCMP on its one target family to avoid implementing software AES-CCM at all — m3OS's own scoping choice layered on the net80211 model, not net80211's design.
- **The driver-to-firmware half is the small half.** The real cost is the software 802.11 management plane + the 4-way handshake (Track B), not the WFDMA/MCU driver shell (Track A). Track B is budgeted accordingly and lives entirely in host-tested `wifi-core`/`crypto-lib` so the radio-free `cargo xtask check` proves it.
- Line-number references are omitted; the function/symbol names are the durable anchors (locate by symbol — `select_nic`, `enumerate_pci_class`, `DmaBuffer`, `RemoteNic::register`, `NetServer::handle_next`, `populate_ext2_files`, `KNOWN_CONFIGS`, `cmd_multi_nic_smoke` — not by line). Update each acceptance checkbox as the corresponding behavior lands.
