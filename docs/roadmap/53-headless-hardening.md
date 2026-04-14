# Phase 53 - Headless Hardening

**Status:** Complete
**Source Ref:** phase-53
**Depends on:** Phase 43c (Regression and Stress) ✅, Phase 44 (Rust Cross-Compilation) ✅, Phase 45 (Ports System) ✅, Phase 46 (System Services) ✅, Phase 48 (Security Foundation) ✅, Phase 51 (Service Model Maturity), Phase 52d (Kernel Completion) ✅, Phase 53a (Kernel Memory Modernization)
**Builds on:** Turns the now-shipped Rust std, ports, services, and first extracted-service work into a trustworthy headless/reference-system baseline with explicit validation and support boundaries
**Primary Components:** xtask/src/main.rs, kernel-core, userspace/init, userspace/coreutils-rs, ports, docs/43c-regression-stress-ci.md, docs/45-ports-system.md

## Milestone Goal

m3OS becomes a deliberately operable headless/reference system: the security floor is repaired, the service model is trustworthy, the basic developer workflow is boringly repeatable, and the project has explicit validation gates for what it now claims to support.

## Why This Phase Exists

By this point the project has real services, Rust std support, ports, diagnostics, and the first proof of ring-3 extraction. What it still lacks is enough polish and discipline to turn those capabilities into a release-quality headless story. Without that, the project risks remaining a strong demo image with an increasingly impressive feature list but weak operational confidence.

This phase exists to make the headless/reference-system claim honest before the roadmap broadens into real hardware, GUI work, or large post-1.0 runtimes.

## Learning Goals

- Understand the difference between "the feature exists" and "the feature is reliable enough to anchor a release claim."
- Learn how validation, support boundaries, and operator docs become part of system design.
- See how ports, Rust std binaries, services, and diagnostics interact in day-to-day system use.
- Understand why release discipline is a prerequisite for later scope growth.

---

## Supported Headless/Reference Workflow

The following workflow is the single normal operator path through the headless
system. Every step must be exercised by the gate bundle (§ Gate Bundle) before
Phase 53 can feed into Phase 58.

### 1. Boot and login

The system boots unattended in QEMU (serial-only mode) and reaches a `login:`
prompt. The operator logs in as `root` using the pre-seeded password. The
shipped image includes working password hashes for `root` and `user` accounts
(seeded by `xtask`); the login binary also supports initial password creation
when a shadow entry is locked (`!`), but the default image ships with active
hashes, not locked accounts.

**Phase 48 security floor exercised here:** kernel-enforced `setuid`/`setgid`
transitions, `getrandom()`-backed salted password hashes, shadow-file-based
authentication.

### 2. Service inspection and control

After login the operator verifies managed services:

```
service list            # enumerate supervised daemons
service status sshd     # inspect a specific service
service restart crond   # restart a daemon with backoff
```

The init daemon (PID 1) supervises services with restart backoff, crash
classification, per-service shutdown timeouts, and init-to-syslog integration
(Phase 46/51 baseline).

### 3. Storage verification

The operator confirms persistent storage is mounted and writable:

```
ls /root                # ext2 root filesystem is writable
touch /root/test && rm /root/test   # write/remove round-trip
mount                   # inspect current mounts (ext2 root, tmpfs)
```

### 4. Log inspection

System logs are accessible through the syslog socket and on-disk files:

```
cat /var/log/messages   # read aggregated system log
logger "test message"   # inject a message via /dev/log
```

### 5. Package and build basics

The Rust std cross-compilation path and ports system are usable for routine
builds:

```
tcc --version                                # TCC is available
tcc -static /usr/src/hello.c -o /tmp/hello   # compile + run a C program
```

Ports (`port install lua`, `port install bc`) and Rust std binaries
(`hello-rust`, `sysinfo-rust`) are expected to work when attempted; reliability
of the fetch/build/install cycle is covered by Tracks B–C.

### 6. Failure recovery

The operator can diagnose a misbehaving service or crashed process using crash
diagnostics (Phase 43a), trace ring dumps (Phase 43b), and serial log output.
Service restart via `service restart <name>` returns the daemon to a known
state.

### 7. Clean shutdown and reboot

```
shutdown                # orderly halt — init stops services in dependency order
reboot                  # orderly reboot — same sequence, then warm restart
```

Phase 48 security floor exercised here: orphan reaping, per-service stop
timeouts, signal-based termination sequence.

### Remote administration posture

- **SSH** is the default and supported remote-administration path (Phase 43 +
  Phase 48 hardening). The system generates host keys with `getrandom()`-backed
  entropy at first boot.
- **Telnet** is available only when the image is built with the explicit
  `--enable-telnet` flag (`cargo xtask image --enable-telnet`). It is a
  non-default testing/debugging posture and is **not** part of the supported
  headless release claim.

---

## Gate Bundle

The gate bundle is the concrete set of automation and manual checks that must
pass before Phase 53 claims headless readiness. Generic references to "smoke"
or "regression" are not sufficient; the bundle names the exact commands.

### Automated gates

| Gate | Command | What it proves |
|---|---|---|
| Static analysis | `cargo xtask check` | clippy + rustfmt + kernel-core host tests pass |
| Unit + property tests | `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` | Host-side pure-logic invariants hold |
| Loom concurrency tests | `RUSTFLAGS='--cfg loom' cargo test -p kernel-core --target x86_64-unknown-linux-gnu --test allocator_loom` | Lock-free allocator paths are linearizable |
| QEMU boot smoke test | `cargo xtask smoke-test --timeout 300` | Boot → login → UID check → service list → ext2 write/verify/delete → log pipeline → TCC compile |
| Regression suite | `cargo xtask regression --timeout 90` | 10 registered SMP-sensitive and headless-operator scenarios pass |
| Stress suite (nightly only) | `cargo xtask stress --test ssh-overlap --iterations 50 --timeout 90` | No timing-dependent failures across repeated runs |

### Manual checks (performed once per release-candidate image)

| Check | Procedure | Pass criterion |
|---|---|---|
| Service lifecycle | `service list`, `service status sshd`, `service restart crond` | Services enumerate, report status, restart cleanly |
| Storage round-trip | `touch /root/test && cat /root/test && rm /root/test` | Write, read, delete succeed on ext2 root |
| Log pipeline | `logger "gate check" && grep "gate check" /var/log/messages` | Message appears in system log |
| SSH remote login | Connect from host via `ssh -p 2222 root@localhost` | Session opens, shell prompt appears |
| Shutdown/reboot | `shutdown` from shell | QEMU exits cleanly, no panic in serial log |
| Reboot | `reboot` from shell | System returns to login prompt |
| Failure recovery | `service stop sshd && service start sshd` | Service stops and restarts without side effects |
| su/passwd auth (security floor) | `su root` from a user shell with the correct password; `passwd user` to change a user password | `su` authenticates via shadow hash; `passwd` writes a fresh salted hash |

### Gate status

**Smoke test** (`cargo xtask smoke-test --timeout 300`) now covers headless
workflow steps 1–5: boot/login with security-floor verification (`id` shows
uid 0), service inspection (`service list` header + core daemon entry),
storage verification (ext2 touch/ls/rm), log inspection (`logger` +
`grep /var/log/messages`), and package/build basics (TCC compile). Step
labels use `guest/` prefixes to distinguish guest-side failures from harness
failures in CI output.

**Regression suite** (`cargo xtask regression`) covers 10 scenarios:

| Category | Tests |
|---|---|
| SMP-sensitive paths | fork-overlap, ipc-wake, pty-overlap, signal-reset, exit-group-teardown, kbd-echo |
| Headless operator workflows | service-lifecycle, storage-roundtrip, log-pipeline, security-floor |

The `security-floor` regression explicitly verifies: (a) `id` confirms
uid=0 after login (kernel-enforced setuid/setgid), (b) `/etc/shadow`
contains a SHA-256-family shadow hash (`$sha256$` on the pre-seeded image,
`$sha256i$10000$` after first-boot password setup or `passwd`), (c)
`/bin/su` can drop to a user shell and authenticate back to root via
shadow-backed password verification, and (d) `whoami` resolves the
authenticated uid.

The Phase 48 security floor also includes `getrandom()`-backed salt generation,
`su` authentication, `passwd` hash rewriting, and non-root privilege
enforcement. Those remain explicit manual checks because the current automated
guest flows do not exercise the first-boot or interactive password-change
paths on every run.

**Shutdown/reboot** (headless workflow §7) is verified by the manual
release checklist. Automated shutdown verification requires QEMU-exit
coordination that is fragile under CI load.

**Gate artifact locations** are documented in a single table in
`docs/43c-regression-stress-ci.md` § Gate Artifact Locations.

**CI alignment:** PR and main-branch workflows run the same gate bundle:
`cargo xtask check` (which already runs the host-side
`cargo test -p kernel-core --target x86_64-unknown-linux-gnu` tier),
`RUSTFLAGS='--cfg loom' cargo test -p kernel-core --target
x86_64-unknown-linux-gnu --test allocator_loom`,
`cargo xtask smoke-test --timeout 300`, and
`cargo xtask regression --timeout 90`. Nightly stress
(`cargo xtask stress --test ssh-overlap --iterations 50 --timeout 90`) is
sustaining evidence, not a merge gate.

### Post-53a closure evidence

Phase 53 closes only after the allocator-sensitive post-53a image passes the
same final-close bundle that the docs publish:

1. `cargo xtask check` (this includes the host-side `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` tier)
2. `RUSTFLAGS='--cfg loom' cargo test -p kernel-core --target x86_64-unknown-linux-gnu --test allocator_loom`
3. `cargo xtask smoke-test --timeout 300`
4. `cargo xtask regression --timeout 90`
5. The manual checks listed above (service lifecycle, storage round-trip, log pipeline, SSH login, shutdown, reboot, failure recovery, and `su`/`passwd`)

The targeted stress command `cargo xtask stress --test ssh-overlap --iterations
50 --timeout 90` remains sustaining/nightly evidence. It is tracked to catch
timing-sensitive allocator regressions over time, but it is not a per-PR gate
and not part of the final-close rerun bundle.

Closure evidence lives in the same places the tooling already produces today:

| Evidence | Location |
|---|---|
| `cargo xtask check` | Terminal/CI job log |
| Loom allocator test | Terminal/CI job log |
| `cargo xtask smoke-test --timeout 300` | Terminal/CI job log only (no dedicated `target/` artifact path yet) |
| `cargo xtask regression --timeout 90` | `target/regression/` and the CI `regression-artifacts` bundle on failure |
| Nightly stress sustaining evidence | `target/stress/` and the CI `stress-artifacts` bundle on nightly failure |

---

## Support Boundary

### What is supported in the headless/reference release

| Area | Supported scope |
|---|---|
| Boot target | QEMU x86_64 with OVMF (UEFI), serial-only or SDL GUI modes |
| Authentication | Local login with salted password hashes; `passwd`, `adduser`, `su` |
| Remote admin | SSH (default); telnet only with explicit opt-in build flag |
| Service management | `service list/status/start/stop/restart/enable/disable` via init PID 1 |
| Logging | syslogd via `/dev/log` Unix socket; `logger` CLI; `/var/log/messages` |
| Scheduling | crond with standard crontab format |
| Storage | ext2 root filesystem (VirtIO-blk), tmpfs |
| Build tooling | TCC (C compiler), `make`/`pdpmake`, `ar`, `install` |
| Rust std path | musl-linked cross-compiled binaries (`hello-rust`, `sysinfo-rust`, etc.) |
| Ports | `port install/remove/list` with bundled source and dependency resolution |
| Diagnostics | Crash diagnostics (Phase 43a), trace rings (Phase 43b), serial log |
| Shutdown/reboot | Orderly `shutdown` and `reboot` with orphan reaping and service stop |

### What is explicitly NOT supported (non-goals for Phase 53)

| Area | Status | When |
|---|---|---|
| GUI / display compositor / graphical session | Out of scope | Phase 56–57 |
| Mouse input or audio | Out of scope | Phase 56–57 |
| Broad hardware certification (bare-metal, non-QEMU) | Out of scope | Phase 55 |
| Large runtime ecosystems (Python, Node.js, JVM) | Out of scope | Post-1.0 (Phase 59–62) |
| Outbound HTTPS/TLS client tooling | Deferred | Post-1.0 |
| `git`, `gh`, GitHub integration | Deferred | Post-1.0 |
| DNS resolution and general outbound networking | Deferred | Post-1.0 |
| Package feeds / remote package repositories | Deferred | Post-1.0 |
| Dynamic linking / shared libraries | Deferred | Post-1.0 |
| Full POSIX compliance testing | Deferred | Post-1.0 |

The support boundary is intentionally narrow. Phase 58 (Release 1.0 Gate) builds
on exactly this bounded baseline rather than assuming broader coverage.

---

## Phase 53 / Phase 53a Closure Contract

Phase 53 defines the headless gates. Phase 53a modernizes the kernel memory
subsystem (per-CPU page cache, magazine slab allocator, SMP-scalable
allocation). These are related but have distinct closure rules:

| Decision | Defined by Phase 53 (now) | Satisfied after Phase 53a |
|---|---|---|
| Supported headless workflow | ✅ Defined above | — |
| Gate bundle (exact commands and checks) | ✅ Defined above | — |
| Support boundary and non-goals | ✅ Defined above | — |
| Operator workflow documentation | Tracks C–D | — |
| Automated gates pass on the allocator-sensitive baseline | — | ✅ Must pass after 53a lands |
| Nightly stress sustaining evidence stays green on the allocator-sensitive baseline | — | ✅ Must remain green after 53a lands |
| Phase 53 marked "Complete" | — | Only after 53a satisfies published gates |

**Closure rule:** Phase 53 may not be marked complete until the final-close
bundle in § Post-53a closure evidence passes on an image that includes the
Phase 53a allocator changes. The gate definitions are fixed now; the evidence is
produced after 53a.

This rule applies identically in the evaluation docs
(`docs/evaluation/roadmap/R06-hardening-and-operational-polish.md`) and in the
release gate (`docs/roadmap/58-release-1-0-gate.md`).

---

## Feature Scope

### Validation and release-gate discipline

Define the boot, login, service, storage, package, and recovery workflows that must pass before the project claims headless readiness. The goal is to make validation concrete, not rhetorical.

### Rust std and ports predictability

Treat the shipped Rust std pipeline and ports system as baseline infrastructure that now must behave predictably. This phase should remove obvious rough edges in install, build, and runtime expectations instead of leaving them as "later polish."

### Operator workflows and documentation

Make the service/logging/admin model understandable enough that a user can boot, inspect, recover, and shut down the system without tribal knowledge.

### Explicit support boundaries

Write down what the headless/reference system promises and what it still does not promise. That protects later hardware, GUI, and ecosystem work from being misread as release blockers too early.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Explicit validation gates | A headless release claim is meaningless without them |
| Predictable service/logging/admin workflow | Operators need one coherent story for running the system |
| Rust std and ports reliability for the supported workflow | These are already part of the shipped baseline |
| Honest support-boundary documentation | The project must distinguish shipped confidence from future ambition |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Security floor | Phase 48 fixes are complete and validated in the normal boot/admin flow (see § Supported Headless/Reference Workflow steps 1 and 7) | Pull missing hardening or smoke coverage into this phase |
| Service lifecycle | Phase 51 supervision and Phase 52 extraction behavior are stable enough for operator use (see § Supported Headless/Reference Workflow step 2) | Add missing restart, status, or recovery work |
| Tooling baseline | Phase 44 and 45 flows are reproducible enough for the release story (see § Supported Headless/Reference Workflow step 5) | Pull missing packaging or runtime cleanup into this phase |
| Validation story | All gates in § Gate Bundle pass on an image including Phase 53a allocator changes | Add the missing release-gate coverage instead of hand-waving it |

**Closure rule (repeated for emphasis):** Phase 53 is not complete until the
gate bundle passes on the post-53a allocator baseline. The gate definitions are
published now; the passing evidence is produced after Phase 53a lands. No
documentation may imply that Phase 53 is already complete before that evidence
exists.

## Important Components and How They Work

### Validation pipeline and release gates

The gate bundle (§ Gate Bundle) is part of the product. It names exact `cargo xtask` commands and manual operator checks that anchor the headless claim. The automated tier uses Phase 43c infrastructure (`smoke-test`, `regression`, `stress`); the manual tier covers service lifecycle, SSH login, storage, logs, shutdown, and failure recovery.

### Operator-visible system model

Services, logs, package behavior, and boot/shutdown flows together define whether the system is understandable enough to operate deliberately. The supported headless workflow (§ Supported Headless/Reference Workflow) is the single documented normal path through these subsystems.

### Support matrix and expectation management

Release quality is partly about saying no. The support boundary (§ Support Boundary) defines what m3OS supports in its headless/reference mode and what remains later work. Phase 58 builds on exactly this boundary.

## How This Builds on Earlier Phases

- Builds on Phase 43c by turning validation infrastructure into the exact gate bundle documented above.
- Builds on Phases 44 and 45 by treating Rust std support and ports as part of the real supported environment.
- Builds on Phases 46, 50, and 51 by turning the service model and extracted-service story into an operator-facing system.
- Uses Phase 53a as allocator-sensitive infrastructure that must satisfy the same published headless gates before the release claim closes (see § Phase 53 / Phase 53a Closure Contract).
- Depends on Phase 48 so the security floor is exercised in the normal boot/admin path, not bolted on as an afterthought.

## Implementation Outline

1. Define the supported headless/reference workflow, gate bundle, and support boundary (Track A — this document).
2. Turn the gate bundle into repeatable automation and evidence capture (Track B).
3. Audit the Rust std and ports flows for the release story and fix the rough edges that block routine use (Track C).
4. Harden service/logging/admin workflows into documented normal operations (Track D).
5. Update learning docs, subsystem docs, and evaluation docs to align with the new claim (Track E).
6. Collect closure evidence after Phase 53a and align version references (Track F).

## Learning Documentation Requirement

- Create `docs/53-headless-hardening.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the supported headless workflows, release gates, operator model, and which capabilities are intentionally out of scope for this milestone.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `README.md`, `docs/README.md`, `docs/roadmap/README.md`, `docs/43c-regression-stress-ci.md`, and `docs/45-ports-system.md`.
- Update `docs/evaluation/usability-roadmap.md`, `docs/evaluation/current-state.md`, and `docs/evaluation/roadmap/R06-hardening-and-operational-polish.md`.
- Update any setup or image documentation that describes the supported development or operator workflow.
- Keep `kernel/Cargo.toml` and any roadmap-facing version references aligned at `0.53.0`; the version tracks the roadmap phase number and does not by itself mark the phase complete before the post-53a evidence bundle passes.

## Acceptance Criteria

- The supported headless/reference workflow (§ above) is a single documented normal-operator path covering boot/login, service inspection/control, storage verification, log inspection, package/build basics, failure recovery, and clean shutdown/reboot.
- The gate bundle names exact `cargo xtask` commands and manual checks, not generic references to smoke/regression.
- The docs state where the Phase 48 security floor is exercised in the normal boot/admin path (steps 1 and 7).
- SSH is the documented default remote-admin path; telnet is documented as a non-default testing posture only.
- Broad outbound developer tooling (HTTPS clients, git, DNS) is explicitly deferred as a non-goal.
- GUI/local-session features, broad hardware certification, and large runtime ecosystems are explicitly out of scope.
- The support boundary is narrow enough to feed Phase 58 without reopening Phase 53 scope.
- The Phase 53 / Phase 53a closure contract states which decisions are defined now versus satisfied only after the published gates pass on the allocator-sensitive baseline.
- The post-53a closure bundle names the exact final-close commands and manual checks, records where regression/stress evidence lives, and states that smoke output remains stdout-only until the harness writes a dedicated artifact.
- Nightly `cargo xtask stress --test ssh-overlap --iterations 50 --timeout 90` is explicitly classified as sustaining evidence rather than a per-PR or final-close rerun.
- No documentation implies that Phase 53 is already complete before the gate bundle passes on the post-53a image.

## Companion Task List

- [Phase 53 Task List](./tasks/53-headless-hardening-tasks.md)

## How Real OS Implementations Differ

- Mature operating systems ship with far richer packaging, telemetry, and operator tooling than m3OS needs here.
- The key lesson to borrow is not feature count but discipline: release claims must map to validated workflows.
- m3OS should choose a narrow, supportable headless story rather than pretending to be a full server distribution.

## Deferred Until Later

- Broad outbound developer networking (HTTPS/TLS clients, DNS resolution, git remotes, GitHub tooling)
- GUI / display compositor / graphical session / local desktop
- Mouse input, audio output
- Large third-party runtime ecosystems (Python, Node.js, JVM)
- Broad hardware certification beyond QEMU x86_64 with OVMF
- Package feeds, remote package repositories, dynamic linking
- Full POSIX compliance testing
