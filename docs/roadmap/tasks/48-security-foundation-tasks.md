# Phase 48 — Security Foundation: Task List

**Status:** Planned
**Source Ref:** phase-48
**Depends on:** Phase 27 (User Accounts) ✅, Phase 30 (Telnet Server) ✅, Phase 42 (Crypto Primitives) ✅, Phase 43 (SSH) ✅, Phase 46 (System Services) ✅
**Goal:** Close the trust-floor gaps in identity transitions, entropy, password
storage, and boot defaults so that the system's multi-user and network-facing
claims are backed by real enforcement rather than trusted-demo assumptions.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Evaluation gate and trust-floor audit | — | Planned |
| B | Kernel-enforced credential transitions | A | Planned |
| C | Entropy pipeline and `getrandom()` hardening | A | Planned |
| D | Password hashing upgrade | C | Planned |
| E | Default credential removal and provisioning | D | Planned |
| F | Telnet opt-in and service default policy | A | Planned |
| G | Account file update hardening | — | Planned |
| H | Smoke and regression testing | B, C, D, E, F, G | Planned |
| I | Documentation, versioning, and roadmap integration | H | Planned |

---

## Track A — Evaluation Gate and Trust-Floor Audit

Audit the current trust-floor failures and map each to a concrete fix before
implementation begins. This track closes the evaluation gate defined in the
phase design doc.

### A.1 — Audit current setuid/setgid enforcement

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `userspace/login/src/main.rs`
- `userspace/su/src/main.rs`

**Symbol:** `sys_linux_setuid`, `sys_linux_setgid`, `sys_linux_setreuid`, `sys_linux_setregid`
**Why it matters:** `sys_linux_setuid` and `sys_linux_setgid` currently set
credentials unconditionally with no privilege check, meaning any unprivileged
process can become root. This audit must produce the exact list of call sites
and the privilege rules that will replace the unconditional behavior.

**Acceptance:**
- [ ] Written audit note lists every kernel function and userspace call site that performs credential transitions
- [ ] Audit identifies which transitions must be root-only, which must match saved UID, and which are currently unguarded
- [ ] Audit confirms whether `setreuid`/`setregid` partial checks are sufficient or need tightening

### A.2 — Audit current entropy pipeline

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `seed_pseudorandom_state`, `fill_pseudorandom_bytes`, `sys_getrandom`
**Why it matters:** The current PRNG uses a single TSC read as its sole seed
and applies an xorshift-multiply mixer with no reseeding or hardware entropy
accumulation. SSH keys, password salts, and all crypto-lib operations
depend on this path producing unpredictable output.

**Acceptance:**
- [ ] Audit documents the current seed source (TSC only), mixing algorithm (xorshift64), and output path
- [ ] Audit confirms whether RDRAND/RDSEED is available on the target QEMU CPU model
- [ ] Audit identifies every userspace consumer of `getrandom()`: crypto-lib, sshd, genkey, passwd, adduser

### A.3 — Audit password hashing and default credentials

**Files:**
- `userspace/syscall-lib/src/sha256.rs`
- `xtask/src/main.rs`

**Symbol:** `hash_password`, `populate_ext2_files`
**Why it matters:** Passwords are currently hashed with a single SHA-256
iteration using the username as salt. The image ships with hardcoded root and
user accounts whose passwords match the usernames. Both must be replaced
before the system can claim any credential security.

**Acceptance:**
- [ ] Audit lists the exact hash format (`$sha256$<hex_salt>$<hex_hash>`), iteration count (1), and salt derivation (username bytes)
- [ ] Audit identifies the hardcoded credentials in `populate_ext2_files` at `xtask/src/main.rs:4064`
- [ ] Audit identifies every consumer of `verify_password` and `hash_password` across the tree

### A.4 — Audit service defaults and telnet exposure

**Files:**
- `userspace/init/src/main.rs`
- `kernel/initrd/etc/services.d/telnetd.conf`
- `xtask/src/main.rs`

**Symbol:** `KNOWN_CONFIGS`
**Why it matters:** Telnet is currently in the default service list and
auto-starts on every boot with `restart=always`. A plaintext remote shell in
the default boot path invalidates the system's security story regardless of
how strong the other credentials are.

**Acceptance:**
- [ ] Audit enumerates every service config file written to the ext2 image by xtask
- [ ] Audit confirms which services start unconditionally at boot via init's `KNOWN_CONFIGS` array
- [ ] Audit documents the current telnetd restart policy and network exposure

---

## Track B — Kernel-Enforced Credential Transitions

Replace unconditional credential syscalls with privilege-checked enforcement
so that the UID/GID model becomes a real security boundary.

### B.1 — Write failing tests for credential enforcement

**Files:**
- `kernel-core/src/lib.rs`
- `kernel-core/tests/credential_enforcement.rs`

**Symbol:** `test_setuid_nonroot_denied`, `test_setgid_nonroot_denied`
**Why it matters:** TDD requires failing tests before implementation. These
tests define the expected behavior: non-root processes must not be able to
escalate to arbitrary UIDs or GIDs.

**Acceptance:**
- [ ] Host-side test asserts that a simulated non-root process calling setuid(0) is denied with `EPERM`
- [ ] Host-side test asserts that root (euid 0) calling setuid to any UID succeeds
- [ ] Host-side test asserts that a non-root process can only setuid back to its real UID
- [ ] All tests fail before Track B.2 implementation

### B.2 — Enforce privilege checks in `sys_linux_setuid` and `sys_linux_setgid`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_setuid`, `sys_linux_setgid`
**Why it matters:** These two functions are the core credential transition
path. Without enforcement here, the entire UID/GID model is advisory.

**Acceptance:**
- [ ] `sys_linux_setuid`: if `euid == 0`, sets both `uid` and `euid` to the requested value; if `euid != 0`, only allows setting `euid` back to the real `uid`; otherwise returns `NEG_EPERM`
- [ ] `sys_linux_setgid`: mirrors `sys_linux_setuid` logic for `gid`/`egid`
- [ ] Existing login and su flows continue to work because they run as root (euid 0) when calling setuid/setgid
- [ ] Track B.1 host-side tests pass

### B.3 — Tighten `sys_linux_setreuid` and `sys_linux_setregid`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_setreuid`, `sys_linux_setregid`
**Why it matters:** These functions already have partial checks but should
match the POSIX rules closely enough that no escalation path exists through
the reuid/regid variants either.

**Acceptance:**
- [ ] `sys_linux_setreuid` allows ruid change only if `euid == 0` or `new_ruid` matches current real or effective uid
- [ ] `sys_linux_setreuid` allows euid change only if `euid == 0` or `new_euid` matches current real, effective, or saved uid
- [ ] `sys_linux_setregid` mirrors the same logic for GID
- [ ] A QEMU integration test (`cargo xtask test --test setuid_enforcement`) confirms that an unprivileged process cannot escalate

### B.4 — Update doc comment for the Phase 27 setuid trust model

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_setuid` (doc comment at line 2657)
**Why it matters:** The existing doc comment documents the old trust model
("password check in userspace provides the security boundary"). It must be
updated to reflect kernel enforcement so future contributors do not
accidentally revert to the old model.

**Acceptance:**
- [ ] Doc comment on `sys_linux_setuid` explains the new enforcement rules
- [ ] Doc comment on `sys_linux_setgid` matches
- [ ] No reference to "unrestricted" or "unconditional" remains in these doc comments

---

## Track C — Entropy Pipeline and `getrandom()` Hardening

Upgrade the kernel PRNG to use hardware entropy when available and document
the quality contract.

### C.1 — Write failing tests for RDRAND-backed entropy

**File:** `kernel-core/src/lib.rs`
**Symbol:** `test_csprng_output_not_constant`
**Why it matters:** The current PRNG can produce predictable output because
TSC values are low-entropy on fast boot. A test that detects constant or
trivially patterned output catches regressions in the entropy path.

**Acceptance:**
- [ ] Host-side test generates two 32-byte buffers from the PRNG with different seeds and asserts they differ
- [ ] Host-side test asserts that a zero seed does not produce all-zero output
- [ ] Tests fail or are trivially satisfiable before Track C.2 changes

### C.2 — Add RDRAND/RDSEED support to the kernel entropy path

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `seed_pseudorandom_state`, `fill_pseudorandom_bytes`
**Why it matters:** RDRAND provides hardware-backed entropy on all modern
x86_64 CPUs (including QEMU's default CPU model). Mixing RDRAND output into
the seed makes the PRNG output materially stronger than TSC-only seeding.

**Acceptance:**
- [ ] `seed_pseudorandom_state` checks CPUID for RDRAND availability and uses it as the primary seed source
- [ ] TSC remains as a fallback if RDRAND is unavailable
- [ ] The seed mixes at least 64 bits of RDRAND output with TSC to hedge against RDRAND-only failure modes
- [ ] `fill_pseudorandom_bytes` re-seeds from RDRAND every 256 bytes to limit the damage of state compromise
- [ ] The fallback constant `0xDEAD_BEEF_CAFE_BABE` is only reachable if both RDRAND and TSC return zero

### C.3 — Extract PRNG into `kernel-core` for host testing

**Files:**
- `kernel-core/src/prng.rs`
- `kernel/src/arch/x86_64/syscall.rs`

**Symbol:** `Prng`, `fill_pseudorandom_bytes`
**Why it matters:** Moving the pure PRNG mixing logic into `kernel-core`
makes it testable on the host via `cargo test -p kernel-core` without
requiring QEMU, following the project convention for pure-logic code.

**Acceptance:**
- [ ] `kernel-core/src/prng.rs` contains the xorshift-multiply mixer and a `Prng` struct with `fill_bytes(&mut self, out: &mut [u8])`
- [ ] `kernel/src/arch/x86_64/syscall.rs` imports the `Prng` struct and only handles the hardware-specific seeding
- [ ] Existing `sys_getrandom` behavior is unchanged
- [ ] `cargo test -p kernel-core` exercises the PRNG mixing logic

### C.4 — Update `crypto-lib` entropy documentation

**File:** `userspace/crypto-lib/src/random.rs`
**Symbol:** `csprng_init`
**Why it matters:** The current doc comment warns that getrandom output is
"not cryptographically secure." After Track C.2, this warning should be
updated to reflect the improved entropy source and its remaining limitations.

**Acceptance:**
- [ ] Doc comment on `csprng_init` describes the new RDRAND-backed seed path
- [ ] Comment retains an honest caveat about the PRNG not being audited for production use
- [ ] No stale reference to "TSC-seeded PRNG" remains in `random.rs`

---

## Track D — Password Hashing Upgrade

Replace single-iteration SHA-256 with an iterated scheme that adds a
meaningful work factor.

### D.1 — Implement iterated SHA-256 password hashing

**File:** `userspace/syscall-lib/src/sha256.rs`
**Symbol:** `hash_password`, `verify_password`
**Why it matters:** A single SHA-256 iteration is effectively free to brute
force. An iterated scheme (e.g., 10,000 rounds of HMAC-SHA-256 or simple
repeated hashing) adds a meaningful work factor using only the SHA-256
primitive already available in the codebase.

**Acceptance:**
- [ ] `hash_password` applies at least 10,000 SHA-256 iterations over the salt+password input
- [ ] New shadow format encodes the iteration count: `$sha256i$<rounds>$<hex_salt>$<hex_hash>`
- [ ] `verify_password` handles both old `$sha256$` format (for migration) and new `$sha256i$` format
- [ ] Constant-time comparison is preserved in the new path

### D.2 — Generate cryptographically random salts

**File:** `userspace/syscall-lib/src/sha256.rs`
**Symbol:** `hash_password`
**Why it matters:** The current salt is the username encoded as hex, which
is deterministic and public. Using `getrandom()` output as salt prevents
rainbow-table attacks and ensures different hashes for identical passwords
across accounts.

**Acceptance:**
- [ ] `passwd` and `adduser` generate a 16-byte random salt via `syscall_lib::getrandom()` instead of using the username
- [ ] The salt is encoded as hex in the shadow entry
- [ ] Old username-derived salts continue to verify via the legacy `$sha256$` format path

### D.3 — Update passwd and adduser to use the new hash format

**Files:**
- `userspace/passwd/src/main.rs`
- `userspace/adduser/src/main.rs`

**Symbol:** `hash_password` call sites
**Why it matters:** Both programs must switch to the new iterated format
with random salts so that all newly created or changed passwords benefit
from the stronger scheme.

**Acceptance:**
- [ ] `passwd` generates new hashes in `$sha256i$` format with random salt
- [ ] `adduser` generates new hashes in `$sha256i$` format with random salt
- [ ] `login` and `su` correctly verify both old and new hash formats without code changes (handled by `verify_password`)

---

## Track E — Default Credential Removal and Provisioning

Remove baked-in passwords from the image build and define the replacement
provisioning flow.

### E.1 — Remove hardcoded passwords from xtask image build

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ext2_files`
**Why it matters:** The current image build writes deterministic
password hashes for root ("root") and user ("user") at
`xtask/src/main.rs:4064`. These shared secrets are public knowledge and
invalidate the entire login story.

**Acceptance:**
- [ ] `populate_ext2_files` writes shadow entries with a locked-account marker (e.g., `!` or `*` in the hash field) instead of valid password hashes
- [ ] The root and user accounts still exist in `/etc/passwd` with correct UIDs and shells
- [ ] The image boots successfully with locked accounts (no login possible until passwords are set)

### E.2 — Implement first-boot password setup in login

**File:** `userspace/login/src/main.rs`
**Symbol:** `main` (login flow)
**Why it matters:** With locked default accounts, the system needs a way to
set initial passwords on first boot. The simplest approach is to have login
detect a locked account and prompt for a new password before proceeding.

**Acceptance:**
- [ ] When login detects a shadow entry with `!` or `*` as the hash, it prompts "Set password for <username>:" instead of "Password:"
- [ ] The new password is hashed with the iterated scheme from Track D and written to `/etc/shadow`
- [ ] After setting the password, login proceeds with normal authentication
- [ ] Second and subsequent logins use normal password verification

### E.3 — Document the provisioning flow

**File:** `docs/roadmap/48-security-foundation.md`
**Symbol:** (provisioning documentation)
**Why it matters:** The first-boot provisioning flow must be documented so
that users and contributors understand how initial credentials work and
what changed from the old baked-in password model.

**Acceptance:**
- [ ] Phase design doc or learning doc explains the locked-account marker and first-boot password setup
- [ ] Documentation covers how to add new accounts after first boot (adduser still works normally)
- [ ] No reference to "default password is root/user" remains in any project documentation

---

## Track F — Telnet Opt-In and Service Default Policy

Make the default image safe by removing telnet from the default boot path.

### F.1 — Remove telnetd from default service configs

**Files:**
- `xtask/src/main.rs`
- `kernel/initrd/etc/services.d/telnetd.conf`

**Symbol:** `populate_ext2_files`, `telnetd_conf`
**Why it matters:** A plaintext remote shell cannot be in the default boot
path. Telnet must be available but not started by default, requiring
explicit operator action to enable it.

**Acceptance:**
- [ ] `populate_ext2_files` no longer writes `telnetd.conf` to the ext2 image by default
- [ ] `kernel/initrd/etc/services.d/telnetd.conf` remains in the repo as a reference but is not installed into the image
- [ ] Init's `KNOWN_CONFIGS` array can still reference telnetd.conf; the service simply does not start because the file is absent on disk

### F.2 — Add an opt-in mechanism for telnet

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ext2_files`
**Why it matters:** Operators who need telnet for debugging must be able to
enable it deliberately. A build-time flag or a documented manual step keeps
telnet available without making it a default risk.

**Acceptance:**
- [ ] `cargo xtask image --enable-telnet` (or equivalent flag) writes `telnetd.conf` to the image
- [ ] Without the flag, telnetd.conf is not present and telnet does not start
- [ ] The xtask help text documents the flag

### F.3 — Document the service default policy

**File:** `docs/roadmap/48-security-foundation.md`
**Symbol:** (service defaults documentation)
**Why it matters:** The decision to remove telnet from defaults and the
mechanism for re-enabling it must be documented so that operators and
contributors understand the security rationale.

**Acceptance:**
- [ ] Documentation explains which services start by default (sshd, syslogd, crond) and why
- [ ] Documentation explains how to enable telnetd for debugging
- [ ] Documentation explains why telnet was removed from defaults (plaintext credentials on the wire)

---

## Track G — Account File Update Hardening

Improve the atomicity and robustness of shadow/passwd file updates.

### G.1 — Use single-write pattern in adduser

**File:** `userspace/adduser/src/main.rs`
**Symbol:** `main` (file write sections)
**Why it matters:** Adduser currently appends to `/etc/passwd` and
`/etc/shadow` using multiple sequential `write()` calls per file, which
could produce partial entries if interrupted. Building the full entry in a
buffer and writing it in a single call matches the pattern already used by
`passwd`.

**Acceptance:**
- [ ] Adduser builds the complete `/etc/passwd` append entry in a buffer before writing
- [ ] Adduser builds the complete `/etc/shadow` append entry in a buffer before writing
- [ ] Adduser builds the complete `/etc/group` append entry in a buffer before writing
- [ ] Each file gets a single `write()` call for the new entry instead of multiple sequential writes

### G.2 — Add fsync after shadow file writes

**Files:**
- `userspace/passwd/src/main.rs`
- `userspace/adduser/src/main.rs`

**Symbol:** `fsync` (syscall wrapper)
**Why it matters:** Neither passwd nor adduser calls `fsync()` after writing
credential files. On a system with persistent storage, an interrupted write
without fsync could leave the shadow file in an inconsistent state on disk.

**Acceptance:**
- [ ] `syscall-lib` exposes an `fsync(fd)` wrapper if one does not already exist
- [ ] `passwd` calls `fsync()` on the shadow file descriptor before closing it
- [ ] `adduser` calls `fsync()` on each credential file descriptor before closing it
- [ ] The kernel `sys_fsync` syscall is implemented or confirmed to exist

---

## Track H — Smoke and Regression Testing

Add targeted test coverage for the hardened defaults.

### H.1 — QEMU test for credential enforcement

**File:** `kernel/tests/setuid_enforcement.rs`
**Symbol:** `test_setuid_enforcement`
**Why it matters:** A QEMU-based integration test proves that the kernel
actually denies credential escalation in a real boot environment, not just
in host-side unit tests.

**Acceptance:**
- [ ] Test binary forks a child, drops to a non-root UID, attempts `setuid(0)`, and asserts `EPERM`
- [ ] Test passes via `cargo xtask test --test setuid_enforcement`
- [ ] Test is included in the standard `cargo xtask test` suite

### H.2 — QEMU test for boot with locked accounts

**File:** `kernel/tests/boot_locked_accounts.rs`
**Symbol:** `test_boot_locked_accounts`
**Why it matters:** The system must boot cleanly even when all accounts are
locked, confirming that the first-boot provisioning path works and that no
service depends on being able to authenticate with default credentials.

**Acceptance:**
- [ ] Test boots the system with locked accounts (no valid password hashes)
- [ ] Test confirms init starts, services launch, and the login prompt appears
- [ ] Test passes via `cargo xtask test --test boot_locked_accounts`

### H.3 — QEMU test for telnet absence in default boot

**File:** `kernel/tests/telnet_default_off.rs`
**Symbol:** `test_telnet_not_running`
**Why it matters:** Regression coverage ensures telnet does not reappear in
the default boot path if someone accidentally re-adds the config file.

**Acceptance:**
- [ ] Test boots the default image and confirms no process named telnetd is running
- [ ] Test passes via `cargo xtask test --test telnet_default_off`

### H.4 — Validate existing quality gates pass

**Symbol:** `cargo xtask check`, `cargo xtask test`
**Why it matters:** All changes in this phase must pass the existing
clippy, rustfmt, kernel-core host tests, and QEMU test suite without
regressions.

**Acceptance:**
- [ ] `cargo xtask check` passes with zero warnings
- [ ] `cargo test -p kernel-core` passes all host-side tests including new PRNG and credential tests
- [ ] `cargo xtask test` passes all existing QEMU tests plus the new ones from this track

---

## Track I — Documentation, Versioning, and Roadmap Integration

### I.1 — Create the Phase 48 learning doc

**File:** `docs/48-security-foundation.md`
**Symbol:** (aligned learning doc)
**Why it matters:** The phase design doc requires a learning doc that
explains why the old trust model was insufficient, how credential
transitions and entropy work now, and how this phase differs from later
sandboxing or isolation work.

**Acceptance:**
- [ ] Learning doc follows the **Template: aligned legacy learning doc** from `docs/appendix/doc-templates.md`
- [ ] Explains the old trust model (unconditional setuid, TSC-only entropy, baked-in credentials, default telnet)
- [ ] Explains the new enforcement model for each area
- [ ] Scoped to Phase 48 only; does not cover later sandboxing or privilege separation phases
- [ ] Includes Key Files table with exact paths

### I.2 — Update `docs/27-user-accounts.md`

**File:** `docs/27-user-accounts.md`
**Symbol:** (Known Limitations section)
**Why it matters:** The Phase 27 learning doc lists "setuid/setgid syscalls
are unconditional" and "salt is username bytes" as known limitations. These
must be updated to reflect that Phase 48 has closed those gaps.

**Acceptance:**
- [ ] Known Limitations section notes that Phase 48 added kernel enforcement for setuid/setgid
- [ ] Known Limitations section notes that Phase 48 replaced username-derived salts with random salts
- [ ] Password Hashing section documents the new iterated format alongside the old one
- [ ] No stale claims about unconditional credential transitions remain

### I.3 — Update `docs/evaluation/security-review.md`

**File:** `docs/evaluation/security-review.md`
**Symbol:** (security review findings)
**Why it matters:** The security review document must reflect the shipped
behavior after Phase 48, not the old trust-floor failures.

**Acceptance:**
- [ ] Review notes that setuid/setgid are now kernel-enforced
- [ ] Review notes the improved entropy source
- [ ] Review notes the removal of default credentials and telnet from defaults
- [ ] Remaining gaps (no sandboxing, no privilege separation beyond UID checks) are honestly documented

### I.4 — Update roadmap docs and cross-links

**Files:**
- `docs/roadmap/48-security-foundation.md`
- `docs/roadmap/README.md`
- `docs/roadmap/tasks/README.md`

**Symbol:** (Companion Task List, roadmap summary row)
**Why it matters:** The phase design doc's Companion Task List section must
link to this task doc, and the roadmap README must show the correct status
and task link.

**Acceptance:**
- [ ] `docs/roadmap/48-security-foundation.md` Companion Task List section links to `./tasks/48-security-foundation-tasks.md`
- [ ] `docs/roadmap/README.md` Phase 48 row shows `[Tasks](./tasks/48-security-foundation-tasks.md)` instead of "Deferred until implementation planning"
- [ ] `docs/roadmap/tasks/README.md` lists Phase 48 if a tasks index exists
- [ ] Status transitions update appropriately when implementation begins and lands

### I.5 — Update README files

**Files:**
- `README.md`
- `docs/README.md`
- `docs/roadmap/README.md`

**Symbol:** (project description, documentation index)
**Why it matters:** The root README and docs index must reflect the new
security baseline so that readers understand the system's current trust
properties.

**Acceptance:**
- [ ] `docs/README.md` adds Phase 48 learning doc to the Phase-Aligned Learning Docs table
- [ ] Root `README.md` mentions kernel-enforced identity transitions in the project description if appropriate
- [ ] No README claims unconditional setuid or baked-in credentials after the phase lands
- [ ] No impacted README is left stale

### I.6 — Bump kernel version to 0.48.0

**Files:**
- `kernel/Cargo.toml`
- `userspace/syscall-lib/Cargo.toml`

**Symbol:** `version` field
**Why it matters:** Every phase must bump the kernel version to match the
phase number, and any modified crate must have its version updated.

**Acceptance:**
- [ ] `kernel/Cargo.toml` version is `0.48.0`
- [ ] `userspace/syscall-lib/Cargo.toml` version is bumped (it gains new hash format and fsync support)
- [ ] Any other modified crate Cargo.toml files have their versions incremented
- [ ] No modified crate or related document is left with stale version metadata after the phase lands

---

## Documentation Notes

- Phase 48 replaces the Phase 27 trust model where setuid/setgid were unconditional and password checks in userspace were the sole security boundary.
- The entropy upgrade replaces the TSC-only xorshift PRNG with RDRAND-seeded mixing, affecting all downstream consumers: crypto-lib, sshd, genkey, passwd, adduser.
- The password hash format changes from `$sha256$` (single iteration, username salt) to `$sha256i$` (iterated, random salt) with backward-compatible verification.
- Telnet moves from default-on to opt-in, changing the image build workflow for operators who need plaintext remote access for debugging.
- Default credentials are replaced by a locked-account-and-first-boot-setup model, changing the out-of-box experience for new images.
