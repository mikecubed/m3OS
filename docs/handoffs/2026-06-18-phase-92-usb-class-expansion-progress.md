# Phase 92 — USB Class Expansion: parallel-impl batch summary / handoff

**Date:** 2026-06-18
**Branch:** `feat/phase-92-usb-class-expansion` → PR [#252](https://github.com/mikecubed/m3OS/pull/252) (open)
**Status:** In progress — **Phase 92 core landed + validated** (foundation, hot-plug, mass-storage BOT transport + data-IN + sector R/W, live hub discovery, live HID Report-descriptor parse). The deep / kernel-invasive / hardware-only remainder is **split into sub-phases 92a–92e** (see the task doc's *Sub-Phase Schedule*); version bump + learning doc held until the core is signed off.
**Source Ref:** phase-92 (`docs/roadmap/tasks/92-usb-class-expansion-tasks.md`)

This is the durable batch summary for the `flow:parallel-impl` run that implemented Phase 92 incrementally. Each landed piece was validated (host tests + QEMU gates) before commit by the coordinator. The **second run** (2026-06-18, breadth-first per the user's steer) closed the data-IN blocker and landed hub discovery + the live HID Report parse, then split the remainder into numbered sub-phases.

## Merged tracks (committed + pushed, validated)

| Track | What landed | Validation |
|---|---|---|
| **H** (foundation) | `GetDescriptors` enumeration-cache (`SlotContext.device_desc/config_desc` + server arm); H.2 control/command-transfer event capture (`drain_for_transfer_event`/`drain_for_command_completion` route non-matching IN completions through `capture_interrupt_report` + re-arm); H.3 `Trb::disable_slot` builder + `Controller::disable_slot` (DCBAA clear + SlotContext drop). H.4 sequenced to D.5. | `cargo xtask check`; kernel-core + usb-core host tests; `xhci-bringup/enum/usb-smoke` PASS |
| **C** (hot-plug) | `on_port_status_change` now queues `PortChange::{Connect,Disconnect}`; server `process_port_events` drives dynamic `enumerate_port` (publishes `AttachNotice`), detach (`attached:false`), + `disable_slot` reclamation. | **`usb-hotplug-smoke` (new) PASS** — 3 QMP `device_add`/`device_del` cycles, no slot exhaustion |
| **D.1/D.2** (mass storage) | `kernel-core::usb::mass_storage` BOT CBW/CSW + SCSI CDB codec (31 host tests); new `usb-storage` daemon (4-place wired) binds `CLASS_MASS_STORAGE` + drives SCSI-over-BOT. **Data-IN phase + sector R/W now work** via a new synchronous single-TRB `SubmitBulkIn` request/`Controller::submit_bulk_in` (one bulk-IN TRB per BOT phase, no streaming auto-re-arm) + a data-OUT `bot_command_write`. | **`usb-storage-smoke` PASS** — `GET_MAX_LUN` + `TEST UNIT READY` + **INQUIRY + READ CAPACITY** (`USB_MASS_STORAGE:ready`) + a **WRITE(10)/READ(10) sector round-trip byte-identical** (`USB_STORAGE:rw-ok`) on a real SuperSpeed device |
| **A.1/A.2** (hub discovery) | server `device_info_from_ctx` surfaces `CLASS_HUB`; the `usbhub` daemon (dormant since 78b) is now a resident `NextAttach` walker — binds a hub, reads `GET_DESCRIPTOR(Hub)` over EP0, powers every port (`SET_FEATURE(PORT_POWER)`), and runs the `GET_PORT_STATUS`→`PORT_RESET`→poll-for-enable→`CLEAR_FEATURE` sequence. | **`usb-hub-smoke` (new) PASS** — `-device usb-hub`; asserts `usbhub: bound hub` + `XHCI_HUB:enumerated` + `USB_HUB:ready` |
| **B.1** (HID Report) | `usb-hid` reads `GET_DESCRIPTOR(Report)` over EP0 for each `CLASS_HID` interface at bind, parses via `parse_report_descriptor` (was zero live call sites), stores the `ReportField` layout per device. | **`usb-smoke` PASS** — asserts `USB_HID:report-parsed`; boot kbd/mouse decode path untouched |
| **A.3** (host-logic) | `kernel-core::usb::hub` `get_port_status` encoder + port-status bitmap helpers | 36 hub host tests |
| **B.2/B.3** (host-logic) | `hid_report` Usage Min/Max ranges + Report IDs (`ReportField.report_id`); consumer-page keycodes (`hid_consumer_usage_to_keycode` + keymap consts) | 47 hid + 38 keymap host tests |
| **G.1/G.2** (host-logic) | `kernel-core::usb::cdc` CDC functional-descriptor parse + NTB-16 build/parse round-trip | 23 cdc host tests |

## Remaining work — scheduled as sub-phases 92a–92e

See the task doc's **Sub-Phase Schedule** for the authoritative breakdown. In short:

- **92a** — USB tier-2 enumeration (A.4/A.5 device-behind-hub via route string) + mass-storage mount/UAS (D.3/D.4/D.5). D.4 needs kernel **multi-remote-block-device routing** (`blk::remote` is a single-backend singleton today). The hard USB data path (D.1/D.2) is already done.
- **92b** — HID Report Protocol live decode (B.2-live multi-axis/scroll + `usb-tablet` QMP-abs gate, B.3-live consumer routing, B.4 LED `SET_REPORT`). The `ReportField` layout (B.1) is already stored.
- **92c** — USB isochronous (E.3 isoch TRB scheduling, E.1 UAC, E.2 UVC). Deepest controller work.
- **92d** — multi-controller concurrency (F.1/F.2) — ring-3 driver threading, risk-isolated from the working single-loop server.
- **92e** — live `usb-net` CDC-ECM/NCM (G.1/G.2/G.3) — bare-metal/VFIO-only (no QEMU CDC-ECM model). Host-logic done.
- **Track I** — version bump `0.91.0`→`0.92.0` + `docs/92-usb-class-expansion.md` learning doc + AGENTS.md gate rows: held until the 92 core is signed off.

## Key finding — RESOLVED (was: SuperSpeed bulk-IN data-phase gap)

The `usb-storage` data-IN phase originally stalled (`xfer ERR cc=6`, STALL on bulk-IN) while the no-data BOT round-trip worked. The earlier hypothesis (SS Endpoint-Companion / Max-Burst) was a **red herring** — a 36-byte INQUIRY fits in one packet, so burst size is irrelevant.

**Actual root cause:** the Phase 96 bulk-IN path is a *streaming, depth-4 auto-re-arm* discipline (it keeps `RX_QUEUE_DEPTH`=4 IN TRBs outstanding and re-arms after every completion — correct for a NIC that always has another frame). BOT is the opposite: a strict request/response protocol where the device sends data only when commanded and returns to a CBW-wait state. After the device sends its data + CSW, the surplus auto-re-armed IN TRBs issue IN tokens while the device is back in CBW-wait → the device STALLs the bulk-IN endpoint (`cc=6`), wedging it.

**Fix (committed `fd662307`):** a new synchronous, single-TRB `UsbRequest::SubmitBulkIn` + `Controller::submit_bulk_in` (modeled on `submit_bulk_out`) — arm exactly one Normal TRB of the exact phase length, ring the doorbell once, wait for that one completion, never re-arm. No surplus IN token is ever issued. The `usb-storage` daemon drives the BOT data + CSW phases over it; INQUIRY + READ CAPACITY(10) now round-trip. `usb-storage-smoke` asserts `USB_MASS_STORAGE:ready`. **D.4 (mount) is now unblocked.**

## Validations run

`cargo xtask check` (clippy `-D warnings` + rustfmt + all host tests) — green after every commit (also enforced by the pre-commit hook). `cargo test -p kernel-core` / `-p usb-core` — all module tests pass (incl. the new `SubmitBulkIn` wire round-trip). QEMU gates run + PASS this session by the coordinator: `usb-storage-smoke` (now asserts `USB_MASS_STORAGE:ready` + `USB_STORAGE:rw-ok`), `usb-hub-smoke` (new), `usb-smoke` (now asserts `USB_HID:report-parsed`; re-run after every shared-`server.rs`/`controller.rs` change to guard the HID boot path — no regression).

## Integration / publication status

- Integration branch `feat/phase-92-usb-class-expansion`: committed + pushed (commits for H, C, D.1/D.2, gates, and the 4 cherry-picked host-logic agent commits).
- PR #252: open (draft), body kept current with per-track verified status.
- Temporary worktrees: the 4 implementer-agent worktrees under `.claude/worktrees/agent-*` were harness-managed (commits cherry-picked into the integration branch); they can be pruned.

## Workflow outcome measures

- **discovery-reuse:** the coordinator's substrate read (protocol/server/controller) was reused as the brief for all delegated agents (each got a scoped slice).
- **rescue-attempts:** 0 (no agent stalled; all 6 delegated host-logic tasks returned clean single commits).
- **abandonment-events:** 0 tracks abandoned. D.1 data-IN was *scoped* (not abandoned) to its validated transport milestone with a documented follow-up.
- **re-review-loops:** 0 (each agent's diff was reviewed + host-tests re-run by the coordinator; no resend rounds needed).
- **delegation:** run 1 — 6 host-logic tasks (A.3, B.2/B.3, D.2, G.1/G.2) to 4 sonnet subagents. Run 2 (breadth) — two read-only `Explore` agents mapped the driver state + the `RemoteBlockDevice` registration path (their findings reused as the brief); all integration (SubmitBulkIn, sector R/W, the hub walker, B.1) + every QEMU gate done by the coordinator, since the remaining work is intricate shared-xHCI integration that can't be safely parallelized across the contended `server.rs`/`controller.rs`, and validation is the coordinator's per the run's terms.
- **run 2 outcome:** closed the data-IN blocker (red-herring root-cause corrected: streaming auto-re-arm vs BOT, not SS Max-Burst), landed A.1/A.2 + B.1, and split the deep/hardware-only remainder into sub-phases 92a–92e so each lands + verifies independently.

## Next-session entry points (sub-phases, schedulable independently)

1. **Phase 92a** — tier-2 hub enumeration (A.4/A.5: route the surfaced hub's downstream-port connection into a server-side Enable-Slot/Address-Device at the `PortTopology` route string) + mass-storage mount (D.4: the `usb-storage` daemon becomes a resident `BlockServer` registering `usb-msc.block`; the kernel `blk` layer needs multi-remote-block-device routing or a root-on-USB path mirroring `ahci-root-smoke`). The bidirectional sector R/W path is already proven (`USB_STORAGE:rw-ok`).
2. **Phase 92b** — HID Report-Protocol live decode: read `HidDevice.report_fields` (already stored) to decode a `usb-tablet`'s absolute X/Y + buttons; needs QMP abs-input gate plumbing.
3. **Phase 92c** — isochronous TRB scheduling (E.3) → UAC (E.1, `-device usb-audio`).
4. **Phase 92d** — per-controller IRQ threads (F, second `qemu-xhci`).
5. **Phase 92e** — live `usb-net` CDC-ECM/NCM (bare-metal/VFIO; host-logic done).
6. **Track I close-out** — version bump `0.92.0` + learning doc, once the core is signed off.
