# Phase 53 — Headless Hardening: Task List

**Status:** Complete
**Source Ref:** phase-53
**Depends on:** Phase 43c (Regression and Stress) ✅, Phase 44 (Rust Cross-Compilation) ✅, Phase 45 (Ports System) ✅, Phase 46 (System Services) ✅, Phase 48 (Security Foundation) ✅, Phase 51 (Service Model Maturity), Phase 52d (Kernel Completion and Roadmap Alignment) ✅, Phase 53a (Kernel Memory Modernization)
**Goal:** Turn the shipped services, Rust std path, ports flow, and validation harness into a defensible headless/reference operating story with explicit support boundaries, repeatable operator workflows, and evidence-backed release gates.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Headless contract, support boundary, and gate definition | — | Complete |
| B | Release-gate automation and evidence capture | A | Complete |
| C | Rust std and ports workflow predictability | A | Complete |
| D | Operator lifecycle, logging, storage, and recovery workflows | A | Complete |
| E | Learning docs, subsystem docs, and evaluation alignment | A, B, C, D | Complete |
| F | Closure evidence, Phase 53a gate satisfaction, and version alignment | B, C, D, E | Complete |

---

## Track A — Headless Contract, Support Boundary, and Gate Definition

Turn the Phase 53 promise into a narrow, operator-visible contract instead of a
general hardening wish list.

### A.1 — Define the supported headless/reference workflow and gate bundle

**Files:**
- `docs/roadmap/53-headless-hardening.md`
- `README.md`
- `docs/evaluation/usability-roadmap.md`

**Symbol:** headless support matrix, validation-gate bundle, operator workflow sections
**Why it matters:** Phase 53 only becomes meaningful when the project can name the exact workflows it supports and the exact gates that back that claim.

**Acceptance:**
- [ ] The supported headless flow explicitly covers boot/login, service inspection, storage, log inspection, package or install basics, failure recovery, and clean shutdown/reboot
- [ ] The gate bundle names the exact automation and manual checks required for the headless claim instead of relying on generic references to "smoke" or "regression"
- [ ] The published workflow states where the Phase 48 security floor is exercised and validated in the normal boot/admin path
- [ ] The definition of the gate bundle is published early enough that later allocator-sensitive work in Phase 53a has a fixed target
- [ ] The supported workflow is written as a normal operator path rather than as scattered implementation notes

### A.2 — Record explicit support boundaries and remote/outbound non-goals

**Files:**
- `docs/roadmap/53-headless-hardening.md`
- `docs/evaluation/usability-roadmap.md`
- `docs/roadmap/58-release-1-0-gate.md`

**Symbol:** support matrix, non-goals, Stage 1 headless boundary
**Why it matters:** A defensible headless/reference release must say what is in scope and what remains later work, especially around remote administration, outbound tooling, and post-1.0 ecosystem growth.

**Acceptance:**
- [ ] The supported remote-administration path is documented consistently (for example, SSH-first defaults and any non-default telnet/testing posture)
- [ ] Broad outbound developer tooling and network-client workflows are either named as supported or explicitly deferred instead of being left ambiguous
- [ ] GUI/local-session features, broad hardware certification, and large runtime ecosystems remain explicitly out of scope for Phase 53
- [ ] The support boundary is narrow enough to feed Phase 58's release decision without reopening Phase 53 scope

### A.3 — Publish the closure contract between Phase 53 and Phase 53a

**Files:**
- `docs/roadmap/53-headless-hardening.md`
- `docs/evaluation/roadmap/R06-hardening-and-operational-polish.md`
- `docs/roadmap/58-release-1-0-gate.md`

**Symbol:** evaluation gate, planning note, release-gate boundary
**Why it matters:** Phase 53 defines the headless gates, but it should not be marked closed on documentation alone while allocator-sensitive infrastructure still has to satisfy those same gates.

**Acceptance:**
- [ ] Phase 53 docs state which release-gate decisions are defined now versus satisfied only after Phase 53a work is complete
- [ ] Evaluation and release docs use the same closure rule for the headless/reference claim
- [ ] No documentation implies that Phase 53 is complete before the published gates pass on the allocator-sensitive baseline

---

## Track B — Release-Gate Automation and Evidence Capture

Make the published headless gate bundle line up with the actual automation and
artifacts used to prove it.

### B.1 — Align smoke coverage with the published headless workflows

**Files:**
- `xtask/src/main.rs`
- `docs/43c-regression-stress-ci.md`

**Symbol:** `smoke_test_script`, `cmd_smoke_test`, `run_smoke_script`
**Why it matters:** The smoke test is the broadest end-to-end headless gate. If it does not match the workflows the docs claim to support, the release story is rhetorical instead of evidence-backed.

**Acceptance:**
- [ ] Smoke covers the boot/login, service inspection, storage, log inspection, package/build basics, and shutdown/reboot steps that the Phase 53 support matrix treats as required
- [ ] Smoke-step labels and failure output are specific enough to distinguish harness breakage from guest/runtime failure
- [ ] The published docs describe the same smoke path and timeout expectations that xtask actually enforces
- [ ] Any supported workflow intentionally excluded from smoke is explicitly assigned to regression or manual-gate coverage

### B.2 — Register targeted regressions for operator-critical failure paths

**Files:**
- `xtask/src/main.rs`
- `userspace/fork-test/src/main.rs`
- `userspace/pty-test/src/main.rs`
- `userspace/unix-socket-test/src/main.rs`

**Symbol:** `regression_tests`, guest test `main` entry points
**Why it matters:** A headless release claim needs more than one boot demo. Service lifecycle, PTY/login behavior, IPC-backed admin paths, and other operator-critical flows need targeted regressions that survive routine development churn.

**Acceptance:**
- [ ] `regression_tests()` covers the workflows the support matrix treats as load-bearing instead of only older subsystem-specific race tests
- [ ] Each operator-critical regression prints unambiguous pass/fail markers and leaves enough serial output to diagnose why it failed
- [ ] Updated or new guest-side tests distinguish harness/setup failure from guest/runtime failure
- [ ] The regression bundle stays scoped to the published headless promise rather than silently absorbing unrelated future-phase coverage

### B.3 — Keep CI and release docs on the same gate bundle

**Files:**
- `.github/workflows/pr.yml`
- `.github/workflows/build.yml`
- `.github/workflows/nightly-stress.yml`
- `docs/43c-regression-stress-ci.md`
- `docs/roadmap/53-headless-hardening.md`

**Symbol:** workflow steps, CI tiers, gate bundle sections
**Why it matters:** If the release docs, local workflow, and CI jobs disagree about which checks are required, Phase 53 cannot make a trustworthy headless readiness claim.

**Acceptance:**
- [ ] PR and main-branch workflows run the exact commands the Phase 53 docs call required headless gates, or the docs label any extra/manual-only gates explicitly
- [ ] Nightly or opt-in stress coverage is documented as sustaining evidence rather than silently becoming a mandatory every-run prerequisite
- [ ] Gate artifact locations (serial logs, trace dumps, failure directories) are documented in one place
- [ ] No documentation claims a required release gate that CI never exercises without calling it manual

### B.4 — Validate the security floor in the normal headless login/admin flow

**Files:**
- `xtask/src/main.rs`
- `userspace/login/src/main.rs`
- `userspace/su/src/main.rs`
- `userspace/passwd/src/main.rs`
- `docs/roadmap/53-headless-hardening.md`

**Symbol:** `smoke_test_script`, `_start`, `su_main`
**Why it matters:** Phase 53 explicitly depends on Phase 48's security floor being validated in the normal boot/admin path. That cannot stay an implicit assumption if the headless claim includes remote administration and multi-user operation.

**Acceptance:**
- [ ] The published headless gates exercise the shipped login and credential-management path rather than assuming Phase 48 remains valid implicitly
- [ ] The support matrix names which admin actions require root and how privilege failures surface during the supported workflow
- [ ] Credential-handling and remote-admin defaults are documented consistently with the Phase 48 security posture
- [ ] Security-floor validation is treated as a required closure gate rather than an optional follow-up

---

## Track C — Rust std and Ports Workflow Predictability

Treat the shipped Rust std and ports work as baseline infrastructure that must
behave predictably enough for routine headless use.

### C.1 — Make the musl Rust workflow an explicit supported path

**Files:**
- `xtask/src/main.rs`
- `docs/44-rust-cross-compilation.md`
- `README.md`

**Symbol:** `build_musl_rust_bins`, `build_userspace_bins`, Rust std workflow sections
**Why it matters:** The Phase 44 pipeline is already part of the shipped environment. Phase 53 must turn it into a boring, documented workflow instead of an advanced-path curiosity.

**Acceptance:**
- [ ] Host prerequisites and skip/failure behavior for the musl Rust path are documented with actionable messages
- [ ] The supported Rust std binaries or validation samples are named explicitly instead of being described vaguely
- [ ] At least one Rust std workflow is exercised by a published release gate or clearly marked as manual validation
- [ ] The docs distinguish the baseline supported Rust std path from broader post-1.0 ecosystem ambitions

### C.2 — Make ports fetch/build/install behavior deterministic enough for the release story

**Files:**
- `ports/port.sh`
- `xtask/src/main.rs`
- `docs/45-ports-system.md`

**Symbol:** `cmd_install`, `resolve_deps`, `generate_manifest`, `populate_ports_tree`
**Why it matters:** A headless/reference system that ships a ports tree still feels fragile if fetch failures, dependency handling, or install state behave unpredictably.

**Acceptance:**
- [ ] Build-time source-fetch failures are surfaced clearly and never look like silent success
- [ ] The supported ports workflow names which ports and dependency paths are part of the Phase 53 headless claim
- [ ] Install and removal state remain observable through manifests or equivalent tracking surfaces documented for operators
- [ ] Repeated image-build or ports-preparation runs are predictable enough that the published workflow can be repeated without surprise drift

### C.3 — Align the supported image-building story with the documented headless baseline

**Files:**
- `xtask/src/main.rs`
- `README.md`
- `docs/evaluation/current-state.md`

**Symbol:** `populate_ext2_files`, `build_musl_bins`, `build_musl_rust_bins`
**Why it matters:** Phase 53 is partly about turning the current QEMU-first developer workflow into a routine, repeatable baseline rather than a collection of one-off host assumptions.

**Acceptance:**
- [ ] The docs name the supported image/build path (`cargo xtask run`, `cargo xtask run --fresh`, or documented equivalent) and its prerequisites explicitly
- [ ] Generated artifacts stay in generated paths rather than dirtying checked-in assets as part of the normal workflow
- [ ] Optional host-tool dependencies are called out explicitly instead of being implicit parts of the release story
- [ ] Build/image documentation matches the actual xtask pipeline used by the published headless gates

---

## Track D — Operator Lifecycle, Logging, and Recovery Workflows

Make the shipped service model, logs, and admin commands coherent enough that an
operator can manage the system deliberately.

### D.1 — Make service state inspection and control part of the normal path

**Files:**
- `userspace/init/src/main.rs`
- `userspace/coreutils-rs/src/service.rs`
- `docs/46-system-services.md`
- `docs/roadmap/51-service-model-maturity.md`

**Symbol:** `ServiceManager`, `cmd_list`, `cmd_status`, `main`
**Why it matters:** A headless/reference system needs one documented way to inspect, restart, stop, and reason about services. If the normal path is still ad hoc, the service model is not trustworthy enough for a release claim.

**Acceptance:**
- [ ] The supported operator workflow explains how to list services, inspect one service, restart it, stop it, and understand dependency-related failures
- [ ] `service` command output and docs use the same state vocabulary and restart semantics
- [ ] Disabled, failed, and restarting services are distinguishable enough for routine headless operations
- [ ] The documented workflow uses the shipped service model rather than manual PID hunting as the expected recovery path

### D.2 — Make logging and failure diagnosis coherent across init, syslog, and admin tools

**Files:**
- `userspace/init/src/main.rs`
- `userspace/syslogd/src/main.rs`
- `userspace/coreutils-rs/src/logger.rs`
- `userspace/coreutils-rs/src/dmesg.rs`
- `docs/46-system-services.md`

**Symbol:** `/dev/log`, `DEV_LOG`, `main`
**Why it matters:** Operators cannot trust a headless system if service failures, kernel diagnostics, and syslog output live in disconnected or undocumented places.

**Acceptance:**
- [ ] The documented log path explains how service logs, syslog output, and kernel diagnostics are inspected during normal operation and after failure
- [ ] Headless recovery guidance points to one coherent evidence trail instead of multiple undocumented buffers
- [ ] Failure of `/dev/log` or missing syslog connectivity is surfaced explicitly enough for operators and gate tooling
- [ ] The log-inspection steps used in automation and docs are consistent with each other

### D.3 — Treat shutdown and reboot as first-class supported workflows

**Files:**
- `userspace/init/src/main.rs`
- `userspace/coreutils-rs/src/shutdown.rs`
- `userspace/coreutils-rs/src/reboot_cmd.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs`

**Symbol:** `shutdown_services`, `main`, `sys_reboot`
**Why it matters:** Clean shutdown and reboot are part of the release promise for a managed headless system, not optional cleanup steps.

**Acceptance:**
- [ ] The published headless workflow includes clean shutdown and reboot rather than assuming crash-or-poweroff endings
- [ ] Service drain order, stop timeouts, and reboot semantics are documented against the shipped implementation
- [ ] At least one published gate or explicit closure checklist exercises shutdown/reboot from the supported userspace commands
- [ ] Recovery guidance explains what evidence remains available after a failed or partial shutdown

### D.4 — Make the storage workflow part of the supported headless path

**Files:**
- `userspace/coreutils-rs/src/mount.rs`
- `userspace/coreutils-rs/src/df.rs`
- `userspace/coreutils-rs/src/stat_cmd.rs`
- `docs/24-persistent-storage.md`
- `docs/45-ports-system.md`

**Symbol:** `main`
**Why it matters:** Phase 53's release story explicitly includes storage. Operators need a documented way to verify that the writable data path backing packages, logs, and user data is healthy and recoverable.

**Acceptance:**
- [ ] The supported headless workflow explains how to verify writable storage, inspect capacity, and distinguish ramdisk/image/data-disk expectations
- [ ] At least one published gate or explicit manual checklist covers the normal storage path used by packages, logs, or persisted user data
- [ ] Recovery guidance names what to inspect when storage-backed workflows fail
- [ ] Storage expectations match the shipped filesystem/image model rather than assuming later hardware or packaging features

---

## Track E — Learning Docs, Subsystem Docs, and Evaluation Alignment

Make the documentation surface tell one consistent story about the supported
headless/reference baseline.

### E.1 — Create the aligned Phase 53 learning doc

**Files:**
- `docs/53-headless-hardening.md`
- `docs/appendix/doc-templates.md`
- `docs/README.md`

**Symbol:** aligned learning-doc sections
**Why it matters:** Phase 53 requires a learner-facing explanation of the supported headless workflows, release gates, operator model, and explicit non-goals.

**Acceptance:**
- [ ] `docs/53-headless-hardening.md` uses the aligned learning-doc template from `docs/appendix/doc-templates.md`
- [ ] The learning doc explains the supported headless workflows, validation gates, operator model, and intentional non-goals
- [ ] The learning doc links to the Phase 53 roadmap doc and the Phase 53 task doc
- [ ] `docs/README.md` links the learning doc when Phase 53 lands

### E.2 — Align top-level and subsystem docs with the headless claim

**Files:**
- `README.md`
- `docs/43c-regression-stress-ci.md`
- `docs/45-ports-system.md`
- `docs/46-system-services.md`

**Symbol:** overview, validation, ports, and operator workflow sections
**Why it matters:** The headless claim is only trustworthy if top-level docs and subsystem docs describe the same workflows, gates, and support boundaries.

**Acceptance:**
- [ ] Top-level and subsystem docs describe the same supported headless workflow and validation bundle
- [ ] Rust std and ports docs explain their supported role in the release story instead of only their implementation mechanics
- [ ] Service/logging docs describe the same operator path used by the headless learning doc
- [ ] No doc keeps framing Phase 53 outcomes as future work once the headless/reference baseline is published

### E.3 — Align evaluation docs with Phase 53 outcomes and remaining gaps

**Files:**
- `docs/evaluation/usability-roadmap.md`
- `docs/evaluation/current-state.md`
- `docs/evaluation/roadmap/R06-hardening-and-operational-polish.md`

**Symbol:** Stage 1 readiness, current-state summary, R06 acceptance criteria
**Why it matters:** The evaluation docs are part of how the project judges readiness. They must reflect the same Phase 53 support boundary instead of silently broadening it.

**Acceptance:**
- [ ] Stage 1/headless readiness criteria reflect the actual Phase 53 support matrix and gate bundle
- [ ] Evaluation docs still call out later-scope items explicitly instead of letting them silently become Phase 53 blockers
- [ ] Current-state text differentiates shipped headless confidence from future GUI, hardware, and ecosystem work
- [ ] R06 and Phase 53 use consistent language about what counts as a trustworthy headless/reference baseline

---

## Track F — Closure Evidence, Phase 53a Gate Satisfaction, and Version Alignment

Close the phase only when the published headless gates and docs all line up.

### F.1 — Close Phase 53 only on the same gates it publishes

**Files:**
- `docs/roadmap/53-headless-hardening.md`
- `docs/roadmap/README.md`
- `xtask/src/main.rs`
- `.github/workflows/pr.yml`
- `.github/workflows/build.yml`
- `docs/43c-regression-stress-ci.md`

**Symbol:** evaluation gate, acceptance criteria, gate bundle sections, workflow steps
**Why it matters:** A headless-hardening milestone that closes on narrative alone would undermine the exact release discipline Phase 53 is meant to introduce.

**Acceptance:**
- [ ] Phase 53 acceptance criteria reference the same commands and manual checks the learning and evaluation docs publish, including `cargo xtask check`, `cargo xtask smoke-test`, and `cargo xtask regression`
- [ ] The closure bundle states whether a targeted stress command (for example `cargo xtask stress --test ssh-overlap --iterations 50`) is required final evidence or sustaining/nightly evidence
- [ ] The post-53a rerun step records where evidence lives, including regression and stress artifacts under `target/regression/` and `target/stress/`, and documents the smoke-test evidence path if it remains stdout-only
- [ ] Phase 53 stays incomplete until the published gates pass on the allocator-sensitive baseline after Phase 53a lands
- [ ] The roadmap summary row, milestone notes, and closure evidence all point to the same gate bundle
- [ ] Manual-only closure items are listed explicitly instead of being implied

### F.2 — Align version references and phase-close documentation when the phase lands

**Files:**
- `kernel/Cargo.toml`
- `README.md`
- `docs/README.md`
- `docs/roadmap/README.md`

**Symbol:** `version`, release references, milestone summary row
**Why it matters:** Phase 53's public story should stay as disciplined in versioning and release language as it is in validation.

**Acceptance:**
- [ ] `kernel/Cargo.toml` and release/version references move to `0.53.0` only when the published headless gates are satisfied
- [ ] Phase-close documentation states whether the result is a headless/reference milestone instead of implying a broader 1.0 or GUI claim
- [ ] Release/version language stays consistent across top-level docs and roadmap summaries
- [ ] Later-phase ambitions remain framed as Phase 54+/58+ work rather than being implied by the version bump

---

## Documentation Notes

- Phase 53 is a convergence and release-discipline phase, not a license to add
  unrelated new subsystems. Prefer tightening the supported story over broadening
  scope.
- Define the headless gate bundle early, but do not call the phase complete until
  the same bundle passes after allocator-sensitive work in Phase 53a.
- Keep the Phase 53 claim narrower than the later Phase 58 release decision:
  this phase establishes an honest headless/reference baseline, not the full 1.0
  promise.
- Prefer exact validated workflows and explicit non-goals over broad language like
  "server-ready" or "production-ready."

## Parallel Implementation Summary

**Merged tracks:** A (contract/support boundary), B (smoke/regression/CI gates), C (Rust std + ports baseline), D (service/logging/storage workflows), E (learning/subsystem/evaluation docs), F (closure evidence, `su`/`passwd` security-floor fixes, and run-harness alignment).

**Retained/abandoned tracks:** None retained; no track was abandoned unresolved.

**Validation run:**
- `cargo xtask check`
- `cargo +nightly test -p xtask --target x86_64-unknown-linux-gnu`
- `cargo test -p passwd --target x86_64-unknown-linux-gnu`
- `RUSTFLAGS='--cfg loom' cargo test -p kernel-core --target x86_64-unknown-linux-gnu --test allocator_loom`
- `cargo xtask smoke-test --timeout 300`
- `cargo xtask regression --timeout 90`
- Manual closure checks across fresh `cargo xtask run --fresh` boots: service lifecycle, storage round-trip, log pipeline, SSH login/failure-recovery, `passwd user`, `su root`, reboot, and shutdown

**Unresolved follow-ups:** None in Phase 53. Nightly `cargo xtask stress --test ssh-overlap --iterations 50 --timeout 90` remains sustaining evidence rather than a final-close rerun requirement.

**Workflow outcome measures:** discovery-reuse=yes; rescue-attempts=3 (Track D rescue lane, Track E rescue lane, final manual-harness narrowing); abandonment-events=0; re-review-loops=A:0, B:1, C:1, D:1, E:1, F:0.

## Final Readiness Report

**Review surface:** Local stable branch diff `main...feat/53-headless-hardening` (`main...HEAD` in the integration worktree)

**Structured checker:** `code-review` agent (report-only, whole integrated diff)

**Current state:** Done

**Verification checklist:**
- CI state gate — **PASS**: the branch has no published PR/remote CI surface pending; the required local gate bundle settled successfully (`cargo xtask check`, loom, smoke-test, regression, targeted host tests, targeted xtask tests)
- Review state gate — **PASS**: no published PR exists, so there are no unresolved PR review threads on the stable review surface
- Diff integrity gate — **PASS**: the final diff matches the established Phase 53 task doc and the user-confirmed branch intent to complete `docs/roadmap/tasks/53-headless-hardening-tasks.md` end-to-end on `feat/53-headless-hardening`

**Blockers:** None

**Fix-now items:** None

**Follow-ups:** None before review. Nightly `cargo xtask stress --test ssh-overlap --iterations 50 --timeout 90` remains sustaining evidence after merge rather than a blocking closeout item.

**Skipped checks:** No scout pass was launched because the integrated diff shape, affected modules, and validation commands were already established in-session. There is no remote PR-only check-run surface to inspect because the branch has not been published yet.

**Unresolved questions:** None

**Next action:** Open the branch for human review / PR publication against `main`

**Verdict:** ready for review

**Readiness workflow outcome measures:** discovery-reuse=yes; rescue-attempts=0; final-gate-result=ready.
