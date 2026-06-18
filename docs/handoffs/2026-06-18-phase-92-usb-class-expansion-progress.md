# Phase 92 — USB Class Expansion: parallel-impl batch summary / handoff

**Date:** 2026-06-18
**Branch:** `feat/phase-92-usb-class-expansion` → PR [#252](https://github.com/mikecubed/m3OS/pull/252) (open)
**Status:** In progress — foundation + hot-plug complete; mass-storage BOT transport **including the data-IN phase** complete; remaining live daemons (hub walker, HID Report Protocol, mount) + version bump + learning doc pending.
**Source Ref:** phase-92 (`docs/roadmap/tasks/92-usb-class-expansion-tasks.md`)

This is the durable batch summary for the `flow:parallel-impl` run that implemented Phase 92 incrementally. Each landed piece was validated (host tests + QEMU gates) before commit by the coordinator.

## Merged tracks (committed + pushed, validated)

| Track | What landed | Validation |
|---|---|---|
| **H** (foundation) | `GetDescriptors` enumeration-cache (`SlotContext.device_desc/config_desc` + server arm); H.2 control/command-transfer event capture (`drain_for_transfer_event`/`drain_for_command_completion` route non-matching IN completions through `capture_interrupt_report` + re-arm); H.3 `Trb::disable_slot` builder + `Controller::disable_slot` (DCBAA clear + SlotContext drop). H.4 sequenced to D.5. | `cargo xtask check`; kernel-core + usb-core host tests; `xhci-bringup/enum/usb-smoke` PASS |
| **C** (hot-plug) | `on_port_status_change` now queues `PortChange::{Connect,Disconnect}`; server `process_port_events` drives dynamic `enumerate_port` (publishes `AttachNotice`), detach (`attached:false`), + `disable_slot` reclamation. | **`usb-hotplug-smoke` (new) PASS** — 3 QMP `device_add`/`device_del` cycles, no slot exhaustion |
| **D.1/D.2** (mass storage) | `kernel-core::usb::mass_storage` BOT CBW/CSW + SCSI CDB codec (31 host tests); new `usb-storage` daemon (4-place wired) binds `CLASS_MASS_STORAGE` + drives SCSI-over-BOT. **Data-IN phase now works** via a new synchronous single-TRB `SubmitBulkIn` request/`Controller::submit_bulk_in` (one bulk-IN TRB per BOT phase, no streaming auto-re-arm). | **`usb-storage-smoke` PASS** — bind + `GET_MAX_LUN` + `TEST UNIT READY` + **INQUIRY + READ CAPACITY** (`USB_MASS_STORAGE:ready`) on a real SuperSpeed device |
| **A.3** (host-logic) | `kernel-core::usb::hub` `get_port_status` encoder + port-status bitmap helpers | 36 hub host tests |
| **B.2/B.3** (host-logic) | `hid_report` Usage Min/Max ranges + Report IDs (`ReportField.report_id`); consumer-page keycodes (`hid_consumer_usage_to_keycode` + keymap consts) | 47 hid + 38 keymap host tests |
| **G.1/G.2** (host-logic) | `kernel-core::usb::cdc` CDC functional-descriptor parse + NTB-16 build/parse round-trip | 23 cdc host tests |

## Retained / pending tracks (not yet started or partial)

- **A remainder** — live `usbhub` resident walker (A.1/A.2/A.4/A.5): drive the hub via control transfers + tier-2 route-string slot assignment. (A.3 helpers ready.)
- **B remainder** — wire `parse_report_descriptor` into the live `usb-hid` path (B.1/B.2-live), consumer-key routing (B.3-live), LED `SET_REPORT` (B.4). (Host-logic ready; needs a Report-Protocol QEMU arm.)
- **D remainder** — the **data-IN phase** (INQUIRY/READ CAPACITY) + D.3 UAS + D.4 `RemoteBlockDevice`/`/mnt/usb<n>` mount + D.5 page-grant. Blocked on the finding below.
- **G remainder** — live `usb-net` CDC-ECM/NCM daemon (bare-metal/VFIO-only — no QEMU CDC-ECM model).
- **E** (UAC/UVC isochronous), **F** (per-controller IRQ threads) — not started.
- **I remainder** — kernel `0.91.0`→`0.92.0` bump (do last), `docs/92-usb-class-expansion.md` learning doc, remaining gates (`usb-audio-smoke`, multi-controller, Report-Protocol arm) + AGENTS.md rows.

## Key finding — RESOLVED (was: SuperSpeed bulk-IN data-phase gap)

The `usb-storage` data-IN phase originally stalled (`xfer ERR cc=6`, STALL on bulk-IN) while the no-data BOT round-trip worked. The earlier hypothesis (SS Endpoint-Companion / Max-Burst) was a **red herring** — a 36-byte INQUIRY fits in one packet, so burst size is irrelevant.

**Actual root cause:** the Phase 96 bulk-IN path is a *streaming, depth-4 auto-re-arm* discipline (it keeps `RX_QUEUE_DEPTH`=4 IN TRBs outstanding and re-arms after every completion — correct for a NIC that always has another frame). BOT is the opposite: a strict request/response protocol where the device sends data only when commanded and returns to a CBW-wait state. After the device sends its data + CSW, the surplus auto-re-armed IN TRBs issue IN tokens while the device is back in CBW-wait → the device STALLs the bulk-IN endpoint (`cc=6`), wedging it.

**Fix (committed `fd662307`):** a new synchronous, single-TRB `UsbRequest::SubmitBulkIn` + `Controller::submit_bulk_in` (modeled on `submit_bulk_out`) — arm exactly one Normal TRB of the exact phase length, ring the doorbell once, wait for that one completion, never re-arm. No surplus IN token is ever issued. The `usb-storage` daemon drives the BOT data + CSW phases over it; INQUIRY + READ CAPACITY(10) now round-trip. `usb-storage-smoke` asserts `USB_MASS_STORAGE:ready`. **D.4 (mount) is now unblocked.**

## Validations run

`cargo xtask check` (clippy `-D warnings` + rustfmt + all host tests, incl. new `usb_storage` crate) — green after every track. `cargo test -p kernel-core` — all new module tests pass. QEMU gates PASS: `xhci-bringup-smoke`, `xhci-enum-smoke`, `usb-smoke`, `usb-hotplug-smoke` (new), `usb-storage-smoke` (new).

## Integration / publication status

- Integration branch `feat/phase-92-usb-class-expansion`: committed + pushed (commits for H, C, D.1/D.2, gates, and the 4 cherry-picked host-logic agent commits).
- PR #252: open (draft), body kept current with per-track verified status.
- Temporary worktrees: the 4 implementer-agent worktrees under `.claude/worktrees/agent-*` were harness-managed (commits cherry-picked into the integration branch); they can be pruned.

## Workflow outcome measures

- **discovery-reuse:** the coordinator's substrate read (protocol/server/controller) was reused as the brief for all delegated agents (each got a scoped slice).
- **rescue-attempts:** 0 (no agent stalled; all 6 delegated host-logic tasks returned clean single commits).
- **abandonment-events:** 0 tracks abandoned. D.1 data-IN was *scoped* (not abandoned) to its validated transport milestone with a documented follow-up.
- **re-review-loops:** 0 (each agent's diff was reviewed + host-tests re-run by the coordinator; no resend rounds needed).
- **delegation:** 6 host-logic tasks (A.3, B.2/B.3, D.2, G.1/G.2) to 4 sonnet subagents; intricate shared-xHCI integration (H, C) + the `usb-storage` daemon + all validation by the coordinator.

## Next-session entry points

1. Resolve the SuperSpeed bulk-IN data-phase gap (the highest-leverage unblock — lights up D.1 data phase + D.4 mount).
2. Track A live `usbhub` walker (A.3 helpers ready) — CI-viable via qemu `usb-hub`.
3. Track B live `usb-hid` Report-Protocol wiring (host-logic ready).
4. Track F multi-controller IRQ threads (second `qemu-xhci`).
5. Track I close-out: version bump (last), learning doc, remaining gates.
