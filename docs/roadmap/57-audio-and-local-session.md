# Phase 57 - Audio and Local Session

**Status:** Planned
**Source Ref:** phase-57
**Depends on:** Phase 47 (DOOM) ✅, Phase 55 (Hardware Substrate) ✅, Phase 56 (Display and Input Architecture) ✅
**Builds on:** Extends the first graphical architecture into a minimally complete local-system story by adding audio output and a coherent graphical session flow
**Primary Components:** kernel or userspace audio driver path, future audio device API, display/session services, userspace terminal or launcher, docs/29-pty-subsystem.md

## Milestone Goal

m3OS supports a minimal local interactive session that feels like a system rather than a demo: there is a defined graphical session entry path, a basic terminal or launcher workflow, and audible PCM output on the supported platform.

## Why This Phase Exists

Once the display and input architecture exists, the remaining gap to a believable local system is no longer "can pixels move?" It is whether the system has the rest of the basic human-facing substrate: entering a session, launching something useful, and producing sound.

This phase exists to turn the graphical architecture into a small but coherent local-session experience without pretending that a full desktop ecosystem already exists.

## Learning Goals

- Understand how audio output fits into a minimally useful graphical system.
- Learn how session entry, launcher/terminal behavior, and recovery rules make a UI feel like an operating environment instead of a technology demo.
- See how DMA- or device-driven audio differs from text and graphics subsystems in its latency and buffering requirements.
- Understand which parts of local-session polish are essential and which can wait.

## Feature Scope

### Audio output path

Implement the first supported audio-output contract on the supported target, with a userspace-facing API and a clearly documented driver choice. Single-client or otherwise simplified audio is acceptable if the behavior is explicit.

### Local-session entry and launcher flow

Define how a user reaches the local graphical session, how a minimal launcher or terminal is started, and how the system returns to a recoverable administration path if the session fails.

### Graphical terminal and application baseline

Provide at least one genuinely useful local graphical client, such as a terminal emulator, plus the basic launcher/session glue needed to treat it as part of the system rather than a standalone demo.

### Session shutdown and recovery behavior

The local session needs a clear stop, restart, and fallback path just like the headless service model does.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Audible PCM output on the supported target | Audio is the core new subsystem being added here |
| A defined local-session entry path | The phase must produce a session, not just a device driver |
| At least one useful graphical client workflow | Otherwise the local system remains a pure demo |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Display/session baseline | Phase 56's display and input architecture is already stable and documented | Pull missing session or compositor work into this phase |
| Hardware baseline | Phase 55 identifies and validates the supported audio target or fallback environment | Add the missing hardware-driver or validation work here |
| Recovery baseline | The service/session model can recover from local-session failure | Add missing fallback or restart behavior before closing |
| Scope discipline | The phase defines the minimum useful local-system story and what remains later | Add the missing non-goals and support-boundary documentation |

## Important Components and How They Work

### Audio device contract

The audio path should define how userspace writes PCM data, what buffering model exists, and what simplified assumptions are acceptable for the first supported target.

### Local-session startup flow

The session entry path connects the existing service model to the graphical stack. It should be clear who starts the session, how the first useful app appears, and what happens on failure.

### Terminal or launcher baseline

The first useful local client is the difference between a graphical stack and a local system. This component anchors how users actually interact with the new session.

## How This Builds on Earlier Phases

- Builds on Phase 55's hardware strategy for the first supported audio target.
- Uses the Phase 47 graphics proof as the earlier validation that full-screen graphical workloads already run on the system.
- Extends Phase 56's display/input model into a minimally complete local-session experience.
- Prepares the optional local-system branch that the release gate can either include or defer explicitly.

## Implementation Outline

1. Choose the first supported audio target and userspace-facing API.
2. Implement the minimum audio-output path needed for the local-system story.
3. Define and wire the graphical session entry flow.
4. Ship at least one useful graphical client workflow, such as a terminal plus launcher.
5. Validate shutdown, recovery, and fallback behavior for the local session.
6. Update docs to distinguish the supported local-system path from later desktop ambitions.

## Learning Documentation Requirement

- Create `docs/57-audio-and-local-session.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the audio contract, session flow, launcher/terminal behavior, and how this phase differs from a full desktop environment.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/29-pty-subsystem.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update `docs/evaluation/usability-roadmap.md`, `docs/evaluation/gui-strategy.md`, and `docs/evaluation/roadmap/R09-display-and-input-architecture.md`.
- Update hardware/audio support docs and any session-startup or local-login documentation.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.57.0`.

## Acceptance Criteria

- The supported target can produce audible PCM output through the documented audio contract.
- There is a documented and working path into a local graphical session.
- A user can launch and use at least one genuinely useful graphical client, such as a terminal.
- Session shutdown, crash recovery, and fallback to administration are documented and tested.
- The docs clearly distinguish this minimal local session from a broader future desktop ecosystem.

## Companion Task List

- Phase 57 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature desktop operating systems ship with richer sound servers, multimedia stacks, login managers, and application ecosystems.
- m3OS should begin with a deliberately small local-session story that proves the concept and stays operable.
- The right comparison is not "does this match Linux desktop polish?" but "does this create a coherent local-system milestone?"

## Deferred Until Later

- Rich desktop audio routing and mixing
- Media playback, recording, and advanced codecs
- Multiple graphical sessions or richer display-manager features
- Full desktop shell, notifications, settings panels, and broader app ecosystems
