# Phase 51 - Service Model Maturity

**Status:** Planned
**Source Ref:** phase-51
**Depends on:** Phase 39 (Unix Domain Sockets) ✅, Phase 43 (SSH) ✅, Phase 46 (System Services) ✅, Phase 48 (Security Foundation) ✅, Phase 50 (IPC Completion) ✅
**Builds on:** Hardens and extends the Phase 46 service manager, logging, and admin surface so later extracted services can be supervised like real first-class system components
**Primary Components:** userspace/init, userspace/syslogd, userspace/crond, userspace/coreutils-rs, kernel/src/arch/x86_64/syscall.rs, docs/46-system-services.md

## Milestone Goal

m3OS turns its new service-management baseline into a reliable lifecycle model: boot ordering is explicit, restart semantics are trustworthy, shutdown behavior is deterministic, logs are coherent, and extracted ring-3 services can plug into the same operator workflow instead of inventing their own.

## Why This Phase Exists

Phase 46 gave m3OS a real service manager, syslog daemon, cron daemon, and admin commands. That was the right first step, but it was still the first step. The project now needs to prove that this service model can carry more than a handful of ordinary daemons and can act as the backbone for the later microkernel transition.

This phase exists to turn "services exist" into "the service model is mature enough that later architecture work can depend on it."

## Learning Goals

- Understand how service definition formats, status models, restart policy, and shutdown semantics fit together.
- Learn why supervision is part of the microkernel story, not just operational polish.
- See how logging, service control, and status reporting become part of the release surface.
- Understand how a service manager grows without immediately becoming systemd-sized.

## Feature Scope

### Service definition contract

Stabilize the configuration model for service identity, dependencies, restart behavior, privileges, and startup ordering. This phase should make the service graph explicit enough that later extracted services can join it without inventing new conventions.

### Supervision and failure handling

Strengthen restart semantics, crash classification, dependency handling, and service-state visibility. The goal is to make service failure a manageable event instead of a reason to reboot or inspect the tree manually.

### Logging and operator visibility

Make service output, syslog, kernel diagnostics, and operator-facing status tools converge into one coherent operational story. A more modular system only gets harder to operate if this part remains fuzzy.

### Shutdown, reboot, and admin surface

Turn controlled teardown into a reliable contract rather than an optimistic best effort. This includes service-stop order, timeouts, status inspection, and commands that behave predictably enough to form part of a release gate.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Stable service-definition contract | Later extracted services need a consistent place to declare dependencies and privileges |
| Trusted restart/status semantics | Restartability is part of the value proposition of moving services to ring 3 |
| Deterministic shutdown/reboot flow | Release and recovery claims depend on it |
| Coherent logging/status path | A supervised system is not operable if failures remain opaque |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Phase 46 baseline | PID 1 supervision, syslogd, crond, and admin commands are working on current main | Pull baseline bug fixes or missing admin paths into this phase |
| Security baseline | Phase 48 defaults and privilege rules are in place so the service model is not built on unsafe assumptions | Add missing service-identity or default-hardening work here |
| IPC/service control path | Phase 50 is strong enough to support extracted services and future control channels | Add the missing control-path cleanup before calling the model mature |
| Operator workflow | Boot, inspect, stop, restart, and shut down services through one coherent path | Add missing status or log-surface work to close the gap |

## Important Components and How They Work

### PID 1 as the service graph owner

The init process becomes the owner of the system's declared service graph, not just a launcher. It should understand dependency order, status, restart policy, and shutdown coordination well enough to supervise both existing daemons and later extracted core services.

### Logging contract and service visibility

`syslogd`, kernel diagnostics, and service output together define how operators understand the system. This phase should make that contract explicit so later phases can rely on it instead of inventing per-service observability.

### Admin surface and recovery semantics

Commands like `service`, `shutdown`, and `reboot` are the human interface to the lifecycle model. Their semantics must be stable enough that smoke tests, release docs, and later service phases can build on them.

## How This Builds on Earlier Phases

- Extends Phase 46 by treating its current service/logging/admin baseline as real infrastructure rather than a one-off milestone.
- Builds on Phase 39 by reusing Unix-domain communication where local control paths benefit from it.
- Depends on Phase 48 so service defaults and identities are not built on a broken security floor.
- Depends on Phase 50 so the matured service model can later host genuine ring-3 system services.

## Implementation Outline

1. Audit the Phase 46 service model against the needs of later extracted services.
2. Stabilize service definition fields, dependency semantics, and service states.
3. Strengthen restart, crash classification, and shutdown behavior.
4. Harden the status and logging surfaces used by operators and validation tooling.
5. Extend the admin/control path so it remains stable as the service graph grows.
6. Add validation for restart, shutdown, service status, and log visibility.
7. Update phase, subsystem, and evaluation docs to reflect the matured service model.

## Learning Documentation Requirement

- Create `docs/51-service-model-maturity.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the service-definition contract, the state machine, restart behavior, logging model, and how this phase differs from the simpler Phase 46 baseline.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/46-system-services.md`, `docs/README.md`, `docs/roadmap/README.md`, and any service-graph diagrams or admin workflow documentation.
- Update `docs/evaluation/usability-roadmap.md`, `docs/evaluation/roadmap/R04-service-model.md`, and `docs/evaluation/roadmap/R06-hardening-and-operational-polish.md`.
- Update any initrd/default-service docs if service configuration or boot policy changes.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.51.0`.

## Acceptance Criteria

- Service definitions are stable enough to describe dependencies, restart rules, and privileges without ad hoc per-service behavior.
- Service restart, stop, status, and log inspection behave predictably for the shipped daemon set.
- Shutdown and reboot drain services in a defined order and leave logs/status information useful for diagnosis.
- Validation coverage exists for service restart, shutdown, and operator-facing status flows.
- Later extracted services have a documented and working path into the service model.

## Companion Task List

- Phase 51 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature init systems ship with many more features: socket activation, sandboxing, journal retention, dependency conditions, watchdogs, and complex unit types.
- m3OS should aim for a small, predictable service model that supports its architecture goals, not a clone of systemd.
- The project should copy the discipline of explicit lifecycle rules and observability rather than the full feature surface of a production init stack.

## Deferred Until Later

- Advanced service sandboxing and capability confinement
- Socket activation and readiness protocols
- Rich health probes, backoff tuning, and multi-instance orchestration
- Structured journaling and long-term log retention policy
