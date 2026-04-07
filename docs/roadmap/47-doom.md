# Phase 47 - DOOM

**Status:** Complete
**Source Ref:** phase-47
**Depends on:** Phase 9 (Framebuffer and Shell) ✅, Phase 12 (POSIX Compat) ✅, Phase 24 (Persistent Storage) ✅, Phase 46 (System Services) ✅
**Builds on:** Uses the framebuffer, storage, and service baseline to prove that m3OS can host a real full-screen graphical program, while explicitly avoiding the mistake of treating one graphical app as the finished GUI architecture
**Primary Components:** kernel/src/fb, kernel/src/arch/x86_64/syscall.rs, userspace graphics demo or DOOM port, xtask/src/main.rs, docs/09-framebuffer-and-shell.md

## Milestone Goal

m3OS can run DOOM as a real full-screen graphical application. The game loads its WAD data from disk, renders through the framebuffer path, accepts keyboard input for gameplay, and returns the system to a usable administration path when it exits.

## Why This Phase Exists

The DOOM milestone is the classic proof that an OS has crossed out of the purely synthetic-demo stage and into "real program under real load" territory. It exercises framebuffer ownership, interactive input, timing, file I/O, memory behavior, asset packaging, and the practical problem of handing control back to the rest of the system when the program exits.

This phase exists to provide that visible proof point without confusing it for the long-term GUI architecture. DOOM proves that graphical userspace is real; it does not solve composition, multiple windows, focus policy, or a desktop session model.

## Learning Goals

- Understand what a real graphical program needs from the OS beyond text output.
- Learn how framebuffer access, timing, input events, and large file I/O interact in one concrete workload.
- See how a thin platform layer such as `doomgeneric` bridges a larger C codebase to a new OS.
- Understand why "it runs DOOM" is a useful milestone but not the same thing as a display server or desktop stack.

## Feature Scope

### Framebuffer access contract

Expose a durable userspace path for graphical rendering. That can be a device-style framebuffer contract or a similarly explicit transitional interface, but the phase must document how userspace gains framebuffer access and how the text console yields and recovers.

### Raw keyboard input for gameplay

Provide the input path needed by DOOM without baking long-term window-system policy into the kernel. Keyboard support is the requirement for this phase; richer pointer-driven UI policy belongs later.

### `doomgeneric` or equivalent port

Port DOOM through a small platform layer that maps m3OS framebuffer, timing, and input services to the game. The value of the phase comes from running a real program with real assets, not from a synthetic graphics test.

### Asset packaging, launch, and recovery

Integrate the WAD file and binary into the supported image/build flow, make startup from the normal system path explicit, and document how the operator returns to the shell or admin environment after exit or failure.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Real DOOM or equivalent full-screen workload | The point is to validate the OS with a meaningful graphical program |
| Documented framebuffer and input path | The milestone must teach how graphics access actually works |
| Clean console handoff and recovery | Graphics takeover cannot strand the operator |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Framebuffer baseline | The Phase 9 console/framebuffer path is stable enough for userspace handoff | Add missing framebuffer cleanup or handoff rules here |
| Binary/runtime baseline | The Phase 12 userspace environment can run the chosen port correctly | Add missing userspace or ABI fixes required by the game |
| Asset/storage baseline | The WAD and related assets load through the supported storage path | Add missing image-layout or file-loading work here |
| Recovery baseline | The system can return to a usable admin path after exit or crash | Add missing session or supervisor cleanup needed for takeover/release |

## Important Components and How They Work

### Framebuffer userspace interface

The framebuffer contract is the hardware-facing substrate for the game. It should tell the user program enough about dimensions, pitch, format, and ownership to render correctly while still leaving room for later display-service ownership.

### Input and timing path

DOOM needs immediate keyboard events and predictable timing. This phase should keep that interface focused on the game's needs and document where a later general input architecture will replace or subsume it.

### Port and asset integration

The port proves more than graphics. It validates that m3OS can package a larger application, load its assets from disk, and survive a real interactive workload end-to-end.

## How This Builds on Earlier Phases

- Builds directly on Phase 9's framebuffer console and Phase 12's ability to run real userspace binaries.
- Reuses the Phase 24 storage story for WAD assets and the Phase 46 service/session baseline for startup and recovery.
- Provides the graphics proof point that later display and local-session phases can build on without mistaking it for a full GUI model.

## Implementation Outline

1. Define the documented userspace framebuffer and keyboard path used by the port.
2. Implement or finish the platform layer for DOOM.
3. Integrate the WAD and binary into the supported image/build flow.
4. Validate launch, rendering, gameplay input, and exit/recovery behavior.
5. Document what this phase proves and what it does not prove about the later GUI stack.

## Learning Documentation Requirement

- Create `docs/47-doom.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the framebuffer contract, input path, porting layer, WAD/asset story, and why DOOM is a bring-up milestone rather than a full GUI architecture.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/09-framebuffer-and-shell.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update `docs/evaluation/gui-strategy.md`, `docs/evaluation/usability-roadmap.md`, and `docs/evaluation/roadmap/R09-display-and-input-architecture.md`.
- Update any build or image docs that describe graphical-mode boot, DOOM assets, or graphical program launch.
- This phase ships as `0.47.0`; keep later release/version references aligned with that milestone.

## Acceptance Criteria

- DOOM or the chosen equivalent graphical workload launches from m3OS and renders through the documented framebuffer path.
- The application loads its assets from the supported storage path without undocumented host-side tricks.
- Interactive keyboard input works well enough for real use of the application.
- Exiting or crashing the graphical program returns the system to a usable administration path.
- The docs explicitly describe this milestone as a graphics proof point, not a complete GUI stack.

## Companion Task List

- [Phase 47 task list](./tasks/47-doom-tasks.md)

## How Real OS Implementations Differ

- Mature systems usually expose standardized graphics and input interfaces rather than one milestone app driving the first visible proof.
- Real desktops rely on compositors, richer input stacks, audio, toolkit layers, and often hardware acceleration.
- m3OS should use this phase to validate the substrate with a real workload, not to freeze the wrong long-term abstraction.

## Deferred Until Later

- Multi-application composition and windowing
- Pointer-driven GUI policy
- Audio output for the graphical session
- Hardware-accelerated rendering
