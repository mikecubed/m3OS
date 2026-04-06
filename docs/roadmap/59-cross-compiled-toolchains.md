# Phase 59 - Cross-Compiled Toolchains

**Status:** Planned
**Source Ref:** phase-59
**Depends on:** Phase 36 (Expanded Memory) ✅, Phase 44 (Rust Cross-Compilation) ✅, Phase 45 (Ports System) ✅, Phase 58 (Release 1.0 Gate) ✅
**Builds on:** Turns the existing Rust cross-compilation and ports baseline into a broader post-1.0 developer-toolchain story with larger bundled host-built binaries such as git, Python, and Clang
**Primary Components:** xtask/src/main.rs, ports, kernel/initrd or disk image layout, userspace Rust std pipeline, docs/git-roadmap.md, docs/python-roadmap.md, docs/clang-llvm-roadmap.md

## Milestone Goal

m3OS can host larger post-1.0 development tools built on the host and bundled into the disk image. git, Python, and Clang become routine parts of the developer environment instead of future one-off experiments.

## Why This Phase Exists

Once the project has a defined release boundary, it can grow into a richer developer platform without muddying what 1.0 meant. Larger cross-compiled toolchains are one of the highest-leverage ways to make the OS more useful for real work, but they also bring bigger binaries, larger libraries, and more build-system complexity.

This phase exists to make that growth deliberate and reproducible.

## Learning Goals

- Understand how large host-built toolchains are staged into the m3OS image.
- Learn how disk size, memory pressure, and runtime expectations change once binaries become much larger than the early core utilities.
- See how existing standalone roadmaps for git, Python, and Clang fit into the official phase plan.
- Understand the difference between "toolchain exists" and "toolchain is part of the supported developer workflow."

## Feature Scope

### git for local development workflows

Bundle git in a configuration suitable for local repository work first. Remote workflows are intentionally left to the next phase so this milestone stays self-contained.

### Python interpreter and standard library

Add a host-built Python interpreter with the stdlib needed for scripting, REPL use, and local automation on the supported platform.

### Clang/LLD and larger C/C++ builds

Add a post-TCC toolchain capable of building larger or more optimized native programs and serving as a stepping stone for richer software bring-up later.

### Toolchain staging and image layout

Document how the build pipeline stages, caches, installs, and validates these larger toolchains so the growth is maintainable instead of magical.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Reproducible host-build and staging flow | Large tools are not useful if the image pipeline is brittle |
| Documented disk/RAM expectations | These binaries materially change system resource assumptions |
| Local git, Python, and Clang workflows | They are the core value of the phase |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Release-boundary baseline | Phase 58 has already separated 1.0 commitments from 1.x growth | Add the missing support-boundary documentation first |
| Runtime baseline | Phases 36, 44, and 45 are stable enough for large binaries and installable software | Pull missing memory or packaging cleanup into this phase |
| Image-layout baseline | The system has a documented place for large toolchains and their libraries | Add the missing filesystem or staging-layout work |
| Validation baseline | There is a repeatable way to prove git, Python, and Clang work in the supported environment | Add the missing post-build validation steps |

## Important Components and How They Work

### xtask staging for large binaries

The build pipeline is the real backbone of the phase. It should cache host-built outputs, copy them into the right image locations, and validate the install layout in a reproducible way.

### Toolchain-specific runtime expectations

git, Python, and Clang each bring different expectations: templates, stdlib files, headers, libraries, and large executable footprints. This phase should make those expectations explicit in the disk layout and documentation.

### Developer workflow integration

The toolchains matter only if they fit the supported developer workflow on m3OS. This includes how they are invoked, where they live, and what the project considers the normal supported use cases.

## How This Builds on Earlier Phases

- Builds on Phase 44's Rust cross-compilation baseline and Phase 45's ports and package layout.
- Depends on Phase 58 so this larger ecosystem work is clearly treated as post-1.0 growth instead of hidden release debt.
- Prepares the ground for richer networked developer workflows in the next phases.

## Implementation Outline

1. Define the supported post-1.0 toolchain set and image layout.
2. Implement reproducible host-build and staging flows for git, Python, and Clang.
3. Validate local git workflows, Python scripts/REPL use, and Clang-based compilation inside m3OS.
4. Update developer docs and standalone roadmaps to align with the official phase.
5. Record the disk, RAM, and support-boundary implications of the added toolchains.

## Learning Documentation Requirement

- Create `docs/59-cross-compiled-toolchains.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the staging flow, install layout, memory/disk implications, and how git, Python, and Clang fit the post-1.0 developer story.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/git-roadmap.md`, `docs/python-roadmap.md`, `docs/clang-llvm-roadmap.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update any image-layout or storage docs that describe `/usr`, `/usr/lib`, or bundled toolchains.
- Update evaluation docs only if the post-1.0 growth narrative changes materially.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.59.0`.

## Acceptance Criteria

- git supports the documented local repository workflows inside m3OS.
- Python can run the documented REPL and script workloads inside m3OS.
- Clang/LLD can build and run the documented sample programs inside m3OS.
- The host-build and image-staging flow for the supported toolchains is reproducible and documented.
- Disk, memory, and support-boundary changes introduced by the larger toolchains are documented.

## Companion Task List

- Phase 59 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature systems ship many more toolchains, package feeds, and update mechanisms than m3OS should attempt here.
- The important thing is to make a small number of powerful tools reliable instead of pretending to offer a whole distribution overnight.
- Post-1.0 growth should remain intentional and supportable, not a dumping ground for every interesting package.

## Deferred Until Later

- Networked git operations and GitHub integration
- Python package installation and richer networking-heavy modules
- Self-hosting the larger toolchains inside m3OS
- Broader language/runtime stacks beyond the documented set
