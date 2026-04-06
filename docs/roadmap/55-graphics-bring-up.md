# Phase 55 - Graphics Bring-Up

**Status:** Planned
**Source Ref:** phase-55
**Depends on:** Phase 9 (Framebuffer and Shell) ✅, Phase 12 (POSIX Compat) ✅, Phase 24 (Persistent Storage) ✅, Phase 46 (System Services) ✅, Phase 54 (Hardware Substrate) ✅
**Builds on:** Uses the framebuffer, storage, and service baseline to prove that m3OS can host a real full-screen graphical program, while explicitly avoiding the mistake of treating that proof as the finished GUI architecture
**Primary Components:** kernel/src/fb, kernel/src/arch/x86_64/syscall.rs, userspace graphics demo or DOOM port, xtask/src/main.rs, docs/09-framebuffer-and-shell.md

## Milestone Goal

m3OS can run a real full-screen graphical application, such as DOOM, through a device-style framebuffer/input contract. The phase proves that the OS can support graphics-capable userspace, large assets, timing, and interactive input without yet pretending that one full-screen app equals a desktop environment.

## Why This Phase Exists

The project needs a visible graphics proof point before it attempts a full display server and local desktop story. That proof is valuable because it exercises framebuffer ownership, asset loading, timing, input handling, and userspace rendering with a real program instead of a synthetic test.

This phase exists to provide that proof while keeping the architecture honest: the result is a graphics bring-up milestone, not the long-term GUI model.

## Learning Goals

- Understand what a real graphical application needs from the OS beyond simple text output.
- Learn how framebuffer access, timing, input events, and large file I/O interact in one userspace program.
- See why a device-style graphics contract is better than a one-off graphics syscall ABI.
- Understand the difference between a graphics proof point and a multi-application desktop architecture.

## Feature Scope

### Framebuffer access contract

Expose the framebuffer through a device-style or similarly durable userspace interface that can later be owned by a display service. The contract should provide mode information, mapping or blit access, and a clean handoff from the text console.

### Raw or minimally processed input path

Provide the input events required by the showcase application without baking long-term window-system policy into the kernel. Keyboard input is the minimum; pointer support may remain for later phases if not required here.

### Real graphical application port

Port a meaningful full-screen application such as DOOM or an equivalent graphics-heavy demo that loads assets from disk, renders at an interactive frame rate, and exercises the whole path end-to-end.

### Console handoff and recovery

Make graphical takeover and exit behavior explicit so the system can return to text-mode administration cleanly after the application exits or crashes.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Device-style framebuffer contract | This phase must not lock the project into a dead-end API |
| Real full-screen application proof | The point is to validate the OS with a real workload |
| Clean console handoff and recovery | Graphics takeover cannot strand the operator |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Framebuffer baseline | The Phase 9 console/framebuffer path is stable and documented | Add missing framebuffer cleanup needed for userspace handoff |
| Storage/asset baseline | The chosen demo can load large assets from the supported storage path | Add missing disk-image or file-loading support needed for the demo |
| Service/session baseline | The system can enter and leave the graphical app without corrupting the operator path | Add missing service or session-handoff work here |
| Architecture guardrail | The phase explicitly states that it is not the finished display-server model | Add the missing documentation and non-goals before closing |

## Important Components and How They Work

### Framebuffer userspace interface

The framebuffer contract is the hardware-facing substrate for the demo. It should give userspace enough information and access to render while remaining compatible with later ownership by a display service.

### Input and timing path

The application needs a practical input and timing contract for interactive rendering. This phase should keep that contract simple and honest rather than smuggling in desktop policy.

### Demo integration and asset loading

A real demo such as DOOM proves the whole path: file I/O, memory mapping or buffered rendering, input, timing, and screen updates under actual program load.

## How This Builds on Earlier Phases

- Builds directly on Phase 9's framebuffer console and Phase 12's ability to run real userspace binaries.
- Reuses the Phase 24 storage story for large assets and Phase 46's service/session baseline for clean startup and recovery.
- Creates the graphical proof point that later display/input architecture work can build on without mistaking it for the final UI model.

## Implementation Outline

1. Define the userspace framebuffer contract and the rules for console takeover and release.
2. Expose the input and timing path needed by the chosen graphical demo.
3. Port the demo application and integrate its assets into the build or image flow.
4. Validate rendering, input, and exit/recovery behavior under QEMU and any relevant reference target.
5. Document what this phase proves and, just as importantly, what it does not prove.

## Learning Documentation Requirement

- Create `docs/55-graphics-bring-up.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the framebuffer contract, input path, asset-loading story, and why this phase is a bring-up milestone rather than a finished GUI architecture.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/09-framebuffer-and-shell.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update `docs/evaluation/gui-strategy.md`, `docs/evaluation/usability-roadmap.md`, and `docs/evaluation/roadmap/R09-display-and-input-architecture.md`.
- Update any build or image docs that describe graphical-mode boot or demo assets.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.55.0`.

## Acceptance Criteria

- A real full-screen graphical application launches from m3OS and renders through the documented framebuffer contract.
- The application can load its assets from the supported storage path without manual host-side tricks beyond the documented image flow.
- Interactive input works well enough for real use of the showcase application.
- Exiting or crashing the graphical application returns the system to a usable administration path.
- The docs explicitly describe this milestone as graphics bring-up, not a complete GUI stack.

## Companion Task List

- Phase 55 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature systems usually expose standardized graphics and input interfaces, not demo-specific contracts.
- Real desktops rely on compositors, window managers, richer input subsystems, and often hardware acceleration.
- m3OS should use this phase to validate the substrate and learn from a real workload, not to freeze the wrong long-term abstraction.

## Deferred Until Later

- Multi-application composition and windowing
- Pointer-driven GUI policy
- Audio output for the graphical app
- Hardware-accelerated rendering
