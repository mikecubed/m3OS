# Phase 98 — Roadmap Audit & Re-Charter (toward a real-hardware GUI workstation)

**Status:** Proposed (2026-06-26)
**Source Ref:** phase-98
**Depends on:** the full Phase 55→96 arc (hardware substrate → drivers → toolchains → bare-metal bring-up) being nominally "done"
**Builds on:** Phase 58 (Documentation Reconciliation Pass) and `docs/appendix/audit-status/` established the precedent that a phase's *claimed* status and its *validated* status can drift; Phase 96 just surfaced a live example (its phase doc still read `Planned (post-1.0)` while the work was hardware-validated). This phase makes that reconciliation a deliberate gate before charting the next arc.

**Primary Components:** `docs/roadmap/` (every phase doc + the README status table), `docs/appendix/audit-status/`, the `cargo xtask check` + smoke/regression/opt-in gate suite (the falsifiable evidence each phase's status should rest on), and — for Track B scoping — `kernel/src/arch/x86_64/` (a future I2C/LPSS substrate), the `usb`/HID input pipeline, the graphical stack (`display_server`/`greeter`), and the networking stack (`RemoteNic`).

## Milestone Goal

Two deliverables:

1. **An honest, evidence-backed audit** of the completed roadmap — every phase's Status field reflects what is *actually validated* (a passing gate, a recorded HW run, or a host test), with "claimed-but-unvalidated" items explicitly flagged.
2. **A re-chartered roadmap (Phases 99+)** for the next arc: a **usable GUI workstation on the real Tiger Lake laptop** — booting to a graphical session with working input (keyboard + pointer) and networking, the natural continuation of the Phase 96 bare-metal bring-up.

## Why This Phase Exists

The project has reached the end of its original roadmap: the hardware substrate (55–55c), display/input/audio (56–57), the long toolchain series (85–95: clang, Python, Go, Node, Rust, Claude Code), the dynamic-C runtime (93), and bare-metal networking on real silicon (96). At this inflection point two things are worth doing *before* piling on new features:

- **Verify before extending.** Phase 96's stale `Planned` status (while hardware-validated) shows the docs can lag reality; the reverse is also possible (a phase marked Complete whose gate has since regressed or was never run on the target). A deliberate audit turns the roadmap back into a trustworthy map.
- **Re-charter with intent.** The remaining big-ticket items (a real GUI session, a pointer, wireless) are substantial and interdependent; sequencing them deliberately beats accreting them ad hoc.

## Feature Scope

### Track A — Phase-quality / completeness audit
- Walk every phase doc; reconcile its Status against falsifiable evidence (a named passing gate, a recorded HW validation, or host tests). Flip stale fields both directions.
- Produce/refresh `docs/appendix/audit-status/` with a per-phase verdict: **Validated** (gate/HW evidence cited) / **Claimed-unvalidated** (no current evidence) / **Regressed** (gate now fails).
- Identify "completed" phases whose gates are opt-in/flaky and never run in CI (e.g. the bare-metal-only `ure` live arm, the `--kvm`/PKU gates, the Phase 97 dlopen TCG stall) and record their true coverage.
- Output: a short audit report + the corrected README status table.

### Track B — New roadmap: real-hardware GUI workstation
Candidate phases to charter (numbers TBD), roughly in dependency order:
- **GUI session bring-up on bare metal** — `display_server` + `greeter` on the now-fast (write-combining) framebuffer; resolve the focus/seat path for a real seat. The compositor stack already exists; this is integration + bare-metal validation.
- **I2C-HID touchpad driver (the GUI pointer)** — Intel LPSS DesignWare I2C controller + I2C-HID transport + multitouch report parsing (OpenBSD `dwiic` + `imt` references; the HID Report-Protocol decode home is Phase 92b). This is the gating dependency for a usable GUI (the laptop has no PS/2 pointer; USB mouse is the only interim pointer).
- **Input polish** — USB keyboard in text mode (`stdin_feeder` to also drain `usb-hid`'s `KBD_EVENT_PULL` events), and taming the `usb-hid`/`usbhub` CPU-hog busy-poll.
- **Intel AX201 / CNVi Wi-Fi** — OpenBSD `iwx(4)` reference; the only built-in networking on the laptop (no Ethernet port). A large phase; deferred-but-charted.
- **Fold in the open follow-ups** — Phase 97 (dlopen TCG stall), generic CDC-ECM USB-NIC, USB NIC offloads, bring-up-diagnostic cleanup.

## Implementation Outline

1. **Audit pass (Track A).** For each phase: locate its gate(s), run or confirm last-run status, cite evidence, set Status. Batch the README table edits. Write the audit report.
2. **Re-charter workshop (Track B).** Draft phase docs (template-conformant) for the GUI-session, I2C-HID-touchpad, and Wi-Fi phases with Milestone Goals + Acceptance Criteria; sequence them; add README rows.
3. **Decide CI posture** for opt-in/HW-only gates surfaced by the audit (keep opt-in with skip-with-reason vs. promote vs. retire).

## Acceptance Criteria

- Every Phase 55→97 README row Status is backed by cited evidence (gate name + last result, HW run, or host test) — no bare "Complete" without a pointer.
- `docs/appendix/audit-status/` (or an equivalent report) records a per-phase **Validated / Claimed-unvalidated / Regressed** verdict.
- At least the GUI-session, I2C-HID-touchpad, and Wi-Fi follow-on phases exist as template-conformant docs with Milestone Goal + Acceptance Criteria and README rows.
- A written, sequenced next-arc roadmap (the "real-hardware GUI workstation" line) is agreed.

## How This Builds on Earlier Phases

Phase 58 reconciled docs once; this phase institutionalizes the *evidence* standard (a gate or a recorded run behind every Status) and pairs the backward audit with a forward re-charter, so the project exits the toolchain era with a trustworthy map and a deliberate next arc.

## How Real OS Implementations Differ

Production projects run this continuously (CI dashboards, release readiness reviews, RFC/roadmap processes) rather than as a discrete phase. A teaching OS benefits from a single explicit "stop, verify, and re-plan" beat at the end of a long roadmap.

## Deferred Until Later

- The actual implementation of the Track-B follow-on phases (GUI session, I2C-HID touchpad, Wi-Fi) — this phase **charters** them; they execute as their own phases.
- Any 1.0 release-engineering / packaging work, unless the audit surfaces it as a blocker.
