# Phase 48 - Security Foundation

**Status:** Complete
**Source Ref:** phase-48
**Depends on:** Phase 27 (User Accounts) ✅, Phase 30 (Telnet Server) ✅, Phase 42 (Crypto Primitives) ✅, Phase 43 (SSH) ✅, Phase 46 (System Services) ✅
**Builds on:** Turns the already-shipped login, SSH, crypto, and service-management baseline into a system that can make a credible headless security claim instead of relying on trusted-demo assumptions
**Primary Components:** kernel/src/arch/x86_64/syscall.rs, userspace/login, userspace/passwd, userspace/init, userspace/sshd, userspace/crypto-lib, xtask/src/main.rs

## Milestone Goal

m3OS closes the trust-floor gaps that currently block any honest claim of "safe enough for serious headless use." When this phase lands, identity transitions, password storage, entropy, and default remote-exposure behavior all match the project's intended multi-user and network-facing story.

## Why This Phase Exists

Phase 46 makes m3OS feel much more like a real operating environment, which raises the cost of leaving obvious security shortcuts in place. Today the system already has user accounts, persistent storage, remote access, and daemon management. That means the remaining trust failures are no longer harmless early-project shortcuts; they directly invalidate the system's security claims.

This phase exists to repair the security floor before deeper microkernel work, release planning, or broader ecosystem work can be taken seriously.

## Learning Goals

- Understand the difference between interesting security mechanisms and a credible threat model.
- Learn how credential transitions, password storage, and boot defaults interact to define the real trust boundary.
- See why entropy quality matters for SSH keys, salts, tokens, and any later TLS-heavy workflow.
- Understand why service management increases the importance of safe defaults instead of reducing it.

## Feature Scope

### Kernel-enforced identity transitions

Replace any remaining "userspace says this credential change is okay" behavior with explicit kernel-side checks for `setuid`, `setgid`, and related transitions. The root boundary must become real again before any multi-user or daemon-isolation story can be trusted.

### Entropy and `getrandom()` correctness

Upgrade the current randomness path to a documented, materially stronger entropy pipeline. The phase should define where the seed material comes from, how it is mixed, and what quality guarantees m3OS is willing to make for SSH, salts, and later HTTPS/client tooling.

### Secure remote-access defaults

Make the default image safe by policy, not just by documentation. Telnet must stop being part of the default exposure story, and image-time or first-boot credential handling must no longer rely on baked-in shared secrets.

### Password storage and account hygiene

Replace the current weak password-hash story with a slow, salted design that better reflects modern expectations. The phase should also harden account-file update behavior enough that credential changes are not obviously fragile or lossy.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Kernel-enforced `setuid` / `setgid` rules | Without this, the UID/GID model is not real |
| Stronger `getrandom()` backing | SSH, salts, and later HTTPS tooling all depend on it |
| Default telnet removal or opt-in gating | A plaintext remote shell cannot stay in the default boot path |
| Removal of baked-in default credentials | Shared default secrets invalidate the whole login story |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Account model baseline | Phase 27 account semantics, passwd/shadow flow, and permission checks are understood and current | Pull any missing account-file cleanup or ownership rules into this phase |
| Crypto baseline | Phase 42 primitives and Phase 43 SSH usage are documented against the actual current tree | Add missing key-generation or salt-handling cleanup needed for the stronger entropy path |
| Service-default baseline | Phase 46 boot/service defaults are enumerated and reproducible | Add explicit image/default-service configuration work instead of leaving it implicit |
| Validation baseline | There is a repeatable smoke path for boot, login, SSH, password changes, and shutdown | Add targeted smoke/regression coverage required to prove the trust-floor fixes |

## Important Components and How They Work

### Credential enforcement in the syscall layer

The kernel syscall path is where credential transitions stop being policy fiction and become actual security boundaries. This phase should make privilege-changing syscalls validate the caller's current authority rather than trusting earlier userspace decisions.

### Entropy pipeline and random data contract

The randomness path should be treated as a system component, not a helper. It needs a seed source, a mixing strategy, a userspace contract (`getrandom()`, `/dev/urandom` if introduced), and clear documentation about what quality is promised.

### Image and service default policy

The combination of init defaults, account provisioning, and remote-access configuration determines whether a fresh image is safe to boot. This phase should make those defaults explicit and reviewable instead of scattering them across setup scripts and initrd content.

## How This Builds on Earlier Phases

- Extends Phase 27 by turning the account model into an actual enforced trust boundary.
- Builds on Phase 42 by making cryptographic primitives depend on a stronger entropy source.
- Builds on Phase 43 by ensuring SSH is not undermined by weak keys, salts, or image defaults.
- Builds on Phase 46 by hardening the service/defaults baseline that now shapes the whole headless system story.

## Implementation Outline

1. Audit the current trust-floor failures and map each one to a concrete kernel, userspace, or image-default fix.
2. Implement kernel-side credential transition enforcement and remove any remaining trust shortcuts.
3. Replace the current weak entropy path with a documented stronger CSPRNG/seed pipeline.
4. Remove baked-in default credentials and define the replacement provisioning or first-boot flow.
5. Make telnet opt-in only and align init/service defaults with the new exposure policy.
6. Upgrade password hashing and account-file update semantics.
7. Add or extend smoke/regression coverage for login, SSH, password changes, and boot defaults.
8. Update documentation, release framing, and version references for the new trust floor.

## Learning Documentation Requirement

- Create `docs/48-security-foundation.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain why the old trust model was insufficient, how credential transitions and entropy work now, which files own the final policy, and how this phase differs from later sandboxing or isolation work.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `README.md`, `docs/README.md`, and `docs/roadmap/README.md` to describe the new security baseline honestly.
- Update `docs/27-user-accounts.md`, `docs/42-crypto-primitives.md`, `docs/43-ssh-server.md`, and `docs/evaluation/security-review.md` to reflect the shipped behavior.
- If new boot/default-service behavior changes setup or image expectations, update `setup.sh`, image notes, and any relevant initrd documentation.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.48.0`.

## Acceptance Criteria

- Unprivileged processes can no longer escalate via unconditional `setuid` or `setgid`.
- `getrandom()` is backed by a documented stronger entropy source than the current time-seeded fallback.
- The default boot path does not expose telnet unless explicitly enabled.
- Images no longer ship with baked-in default credentials.
- Password hashes use a slow salted scheme, or a clearly documented transitional replacement that materially improves the current state.
- Boot/login/SSH smoke coverage exists for the hardened defaults and passes in the normal validation path.

## Companion Task List

- [Phase 48 task list](./tasks/48-security-foundation-tasks.md)

## How Real OS Implementations Differ

- Linux and BSD systems have much deeper credential, entropy, and hardening stacks than m3OS needs initially.
- Production systems also layer service users, secret rotation, audit trails, and more advanced account policy on top of the basics.
- m3OS should copy the discipline of safe defaults and explicit trust boundaries, not the full operational complexity of a mature distribution.

## Deferred Until Later

- Full privilege separation across all network services
- Advanced key-management, rotation, and audit infrastructure
- Rich multi-factor or hardware-backed authentication flows
- General sandboxing beyond the repaired trust floor
