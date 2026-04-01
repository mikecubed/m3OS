# Phase 10 — Secure Boot Signing (Optional): Task List

**Status:** Complete
**Source Ref:** phase-10
**Depends on:** Phase 9 ✅ (optional)
**Goal:** Add UEFI Secure Boot signing to the build pipeline so the OS can boot on real hardware with Secure Boot enabled, using self-enrolled keys.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Key generation setup | — | ✅ Done |
| B | xtask sign implementation | A | ✅ Done |
| C | Validation | B | ✅ Done |
| D | Documentation | A, B, C | ✅ Done |

---

## Track A — Key Generation Setup

### A.1 — Write key generation script

**File:** `scripts/gen-secure-boot-keys.sh`
**Why it matters:** Signing requires a 4096-bit RSA key pair and self-signed certificate; a script ensures reproducible generation.

**Acceptance:**
- [x] Script generates `m3os.key` (private key) and `m3os.crt` (self-signed certificate) using `openssl req`
- [x] Certificate uses CN=`m3os Secure Boot Key` and is valid for 10 years
- [x] Both files are in `.gitignore` — private key is never committed

---

### A.2 — Document expected key file locations

**Why it matters:** The xtask sign command needs to find the key and cert at predictable paths.

**Acceptance:**
- [x] Expected output files and their locations relative to the repo root are documented

---

## Track B — xtask Sign Implementation

### B.1 — Add sign subcommand to xtask

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_sign`, `sign_efi`
**Why it matters:** Integrating signing into the build pipeline avoids manual sbsign invocations.

**Acceptance:**
- [x] `cargo xtask sign` (or `--sign` flag on `image`) accepts optional `--key` and `--cert` path arguments
- [x] Defaults to `m3os.key` and `m3os.crt` in the repo root

---

### B.2 — Run sbsign to produce signed EFI binary

**File:** `xtask/src/main.rs`
**Symbol:** `sign_efi`
**Why it matters:** The signed EFI binary is what UEFI firmware validates during Secure Boot.

**Acceptance:**
- [x] Invokes `sbsign --key <key> --cert <cert> --output <signed.efi> <unsigned.efi>` via `std::process::Command`
- [x] Fails with a clear error if `sbsign` is not found

---

### B.3 — Verify signature after signing

**File:** `xtask/src/main.rs`
**Symbol:** `sign_efi`
**Why it matters:** Catching a bad signature immediately prevents mysterious boot failures on real hardware.

**Acceptance:**
- [x] Runs `sbverify --cert <cert> <signed.efi>` after signing to confirm validity
- [x] Prints the signed EFI path and a MOK enrollment reminder on success

---

## Track C — Validation

### C.1 — Verify sbverify accepts the signed binary

**Why it matters:** Confirms the signing toolchain produced a valid signature.

**Acceptance:**
- [x] `sbverify --cert m3os.crt <signed-efi>` exits 0

---

### C.2 — Verify unsigned binary fails sbverify

**Why it matters:** Confirms sbverify is actually checking signatures, not just passing everything.

**Acceptance:**
- [x] The unsigned EFI binary fails `sbverify` against the cert (expected behavior)

---

### C.3 — Real hardware boot test with enrolled key (deferred)

**Why it matters:** The ultimate validation is booting on real Secure Boot hardware.

**Acceptance:**
- [ ] On a real machine with Secure Boot enabled: enroll the cert via MOK or UEFI db and confirm boot succeeds
- [ ] Temporarily disable the enrolled key and confirm the signed binary is rejected

> **Deferred:** Requires physical hardware with Secure Boot. Software signing and verification are validated; real hardware testing is tracked separately.

---

## Track D — Documentation

### D.1 — Document Secure Boot architecture and workflow

**File:** `docs/10-secure-boot.md`
**Why it matters:** The UEFI key hierarchy (PK/KEK/db/dbx) and enrollment paths are non-obvious and must be documented for contributors.

**Acceptance:**
- [x] Covers UEFI Secure Boot key hierarchy (PK / KEK / db / dbx)
- [x] Documents end-to-end workflow: `gen-secure-boot-keys.sh` then `cargo xtask sign`
- [x] Documents both enrollment paths: shim MOK (`mokutil --import`) and direct UEFI db (`efi-updatevar` / firmware setup)
- [x] Documents how to verify Secure Boot state (`mokutil --sb-state`, `dmesg | grep -i secure`)

---

### D.2 — Document shim and MOK concepts

**File:** `docs/10-secure-boot.md`
**Why it matters:** The distinction between shim's MOK list and the UEFI firmware db is a common source of confusion.

**Acceptance:**
- [x] Explains what shim is and that `mokutil` manages shim's MOK list, not the firmware db
- [x] Explains how distribution Secure Boot differs from personal key enrollment

---

### D.3 — Update roadmap to reflect Phase 10 status (deferred)

**Why it matters:** Roadmap accuracy depends on marking phases complete only after full validation.

**Acceptance:**
- [ ] Update `docs/roadmap/README.md` and `docs/08-roadmap.md` once real hardware validation passes

> **Deferred:** Blocked on real hardware boot test (C.3).

---

## Documentation Notes

- Phase 10 added Secure Boot signing to the xtask build pipeline, building on the UEFI boot from Phase 1 and the visible framebuffer from Phase 9.
- Software signing and verification are fully functional; real hardware enrollment testing is deferred.
- Key material (`.key`, `.crt`) is never committed to the repository.
