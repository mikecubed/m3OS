# Headless Hardening

**Aligned Roadmap Phase:** Phase 53
**Status:** In Progress
**Source Ref:** phase-53
**Supersedes Legacy Doc:** (none -- new content)

## Overview

Phase 53 turns the current QEMU-first system into an explicit
headless/reference-system promise. Instead of adding one brand-new subsystem, it
converges the already-shipped security floor, service model, storage layout,
logging path, build tooling, and validation harness into one supported operator
workflow. The point of the phase is to make the release claim narrow, testable,
and honest before the roadmap expands into broader hardware, GUI, or large
runtime work.

Phase 53 is also intentionally not "done" just because the workflow is written
down. The workflow, gate bundle, and support boundary are published now, but the
phase does not close until those gates pass on an image that includes the
Phase 53a allocator changes.

## What This Doc Covers

- The single normal headless/reference workflow: boot, login, service control,
  storage/log checks, build basics, recovery, shutdown, and reboot
- The exact automated and manual validation gates that back the Phase 53 claim
- The operator model: PID 1 supervision, persistent ext2 root, syslog-backed
  logs, and SSH-first remote administration
- The support boundary and explicit non-goals for this milestone
- The Phase 53 / Phase 53a closure contract

## Core Implementation

### Supported headless/reference workflow

Phase 53 narrows the "normal way to use m3OS" to one QEMU/OVMF path. The
operator is expected to work from the serial/headless environment, not from a
graphical session or a broad bare-metal support matrix.

| Step | Normal action | What this proves |
|---|---|---|
| 1. Boot and login | Reach `login:` in QEMU and authenticate with the seeded root or user account | The Phase 48 security floor is exercised in the routine boot path |
| 2. Service inspection and control | `service list`, `service status sshd`, `service restart crond` | PID 1 supervision is the real operator surface, not a demo-only layer |
| 3. Storage verification | `mount`, `ls /root`, `touch /root/test && rm /root/test` | The ext2 root filesystem is writable and trustworthy |
| 4. Log inspection | `logger "check"` plus `cat /var/log/messages` | syslogd, `/dev/log`, and persistent log files form a usable evidence trail |
| 5. Package and build basics | `tcc --version` and a small TCC compile; optional ports/Rust std demos | The image supports a bounded development workflow without claiming a full ecosystem |
| 6. Failure recovery | Use crash diagnostics, trace rings, and `service restart <name>` | Headless operation includes recovery, not just happy-path boot |
| 7. Clean shutdown and reboot | `shutdown` and `reboot` | Service stop ordering and reboot/halt are part of the supported lifecycle |

Ports and Rust `std` are part of the supported story, but not in the same way
as the smoke bundle. The Phase 53 baseline keeps them as documented **manual
validation surfaces**: `/usr/bin/port` plus the in-repo ports tree always ship,
and the five musl-linked Rust demo binaries are the supported Rust `std`
reference set when their host prerequisites are present.

### Validation gates

Phase 53 publishes exact commands instead of vague references to "smoke" or
"regression." The automated gate bundle is:

| Tier | Command | Why it is in the bundle |
|---|---|---|
| Static analysis | `cargo xtask check` | Keeps formatting, clippy, and host-test drift out of the baseline |
| Host logic tests | `cargo test -p kernel-core` | Verifies pure logic outside QEMU |
| Concurrency tests | `RUSTFLAGS='--cfg loom' cargo test -p kernel-core --target x86_64-unknown-linux-gnu --test allocator_loom` | Proves allocator-sensitive ordering on the host |
| Boot smoke | `cargo xtask smoke-test --timeout 300` | Exercises boot/login, service list, storage, logs, and TCC compile |
| Regression suite | `cargo xtask regression --timeout 90` | Covers SMP-sensitive paths plus the headless operator regressions |
| Nightly sustaining evidence | `cargo xtask stress --test ssh-overlap --iterations 50 --timeout 90` | Repeats timing-sensitive overlap paths without turning stress into a per-PR blocker |

Manual release-candidate checks remain part of the contract: service lifecycle,
storage round-trip, log pipeline, SSH login, shutdown, reboot, failure
recovery, and `su`/`passwd` authentication checks are still performed once per
candidate image. `docs/43c-regression-stress-ci.md` explains where the
automated artifacts land; the roadmap doc keeps the authoritative full gate
table.

### Operator model and support boundary

The supported operator model is intentionally narrow:

- **Boot target:** QEMU x86_64 with OVMF, serial/headless as the reference path
- **Remote admin:** SSH is the supported remote path; telnet exists only as an
  explicit opt-in build posture
- **Lifecycle control:** init PID 1 plus `service`, `logger`, `shutdown`, and
  `reboot`
- **Persistent state:** ext2 root filesystem at `/`, with `/var/log/messages`
  and `/var/log/kern.log` surviving reboot
- **Build/tooling floor:** TCC, build tools, the bounded ports tree, and the
  supported Rust `std` demos
- **Diagnostics:** crash diagnostics, trace rings, serial logs, and persistent
  syslog output

That support boundary is what keeps later-scope work honest. GUI/compositor
work, mouse/audio, broad bare-metal certification, outbound DNS/HTTPS/git
tooling, package feeds, dynamic linking, and large runtime ecosystems are
intentionally **not** Phase 53 blockers.

### Phase 53 / Phase 53a closure contract

Phase 53 defines the headless/reference promise now, but Phase 53a decides when
that promise can be closed. The rule is simple: Phase 53 is not complete until
the published gate bundle passes on an image that includes the Phase 53a
allocator changes.

That means the remaining gap is **evidence**, not a reopened feature list. The
gates, operator workflow, and support boundary are already the contract; the
post-53a pass is the proof that the contract survives the allocator-sensitive
baseline that Phase 58 will inherit.

## Key Files

| File | Purpose |
|---|---|
| `docs/roadmap/53-headless-hardening.md` | Canonical phase scope, gate bundle, support boundary, and 53/53a closure contract |
| `xtask/src/main.rs` | Implements smoke, regression, stress, and the headless workflow steps those commands drive |
| `docs/43c-regression-stress-ci.md` | Maps automated gates, CI tiers, and artifact locations for the published bundle |
| `docs/24-persistent-storage.md` | Explains the persistent ext2 root and the storage checks used in the operator workflow |
| `docs/46-system-services.md` | Explains the service/logging/shutdown model that Phase 53 treats as normal operations |
| `docs/45-ports-system.md` | Documents the bounded ports baseline and host-cache caveats that remain manual validation |
| `docs/44-rust-cross-compilation.md` | Documents the supported Rust `std` demos and their host prerequisites |

## How This Phase Differs From Later Work

- Phase 53 defines a **bounded QEMU headless/reference promise**, not a broad
  server distribution or desktop release.
- Phase 53 keeps ports and Rust `std` in the story as documented validation
  surfaces, not as a claim that m3OS already supports large third-party
  ecosystems.
- Phase 54 and Phase 55 can broaden architecture and hardware scope later, but
  they do not get to silently redefine the Phase 53 support boundary.
- Phase 56 and Phase 57 are where local graphical-session work becomes
  first-class; Phase 53 explicitly keeps GUI/input/audio out of its closure
  criteria.
- Phase 58 treats the Phase 53 gates and support boundary as fixed inputs to the
  1.0 release decision.

## Related Roadmap Docs

- [Phase 53 roadmap doc](./roadmap/53-headless-hardening.md)
- [Phase 53 task doc](./roadmap/tasks/53-headless-hardening-tasks.md)

## Deferred or Later-Phase Topics

- GUI / display compositor / graphical session / local desktop claims
- Mouse input and audio output
- Broad bare-metal certification beyond the QEMU x86_64 + OVMF reference target
- Outbound HTTPS/TLS clients, DNS resolution, git remotes, and GitHub tooling
- Large runtime ecosystems such as Python, Node.js, and the JVM
- Package feeds, remote repositories, and dynamic linking
- Marking Phase 53 complete before the post-53a gate evidence exists
