# Versioning Reform — Single Workspace Version (Spec)

**Status:** Specified (executes in a follow-on PR)
**Source Ref:** phase-98
**Track:** C
**Summary:** Replace 114 divergent per-phase per-crate versions with a single `[workspace.package] version = "0.98.0"` so phase branches touch zero version lines and can no longer conflict on Cargo metadata.

---

## Background and Motivation

Every workspace member currently carries an independent version encoded with the phase number in which it was introduced or last changed (`kernel = "0.96.0"`, `kernel-core = "0.53.0"`, USB drivers at `"0.92.0"`, etc.). Because phases develop on feature branches and land in parallel, any two branches that touch a crate owned by different phases both edit the same `version = "..."` line — a guaranteed, semantically-meaningless merge conflict.

None of these crates are published to crates.io. Cargo version fields exist solely to satisfy `[package]` grammar. A single workspace version eliminates all of that bookkeeping overhead with a three-line root edit plus a mechanical per-member substitution.

---

## 1. Target Root Block

Add the following block to `/home/mikecubed/projects/ostest/Cargo.toml` immediately before the existing `[workspace.dependencies]` block (currently line 252). The root Cargo.toml currently has **no** `[workspace.package]` section.

```toml
[workspace.package]
version = "0.98.0"
edition = "2024"
```

Nothing else changes in the root Cargo.toml. The existing `[workspace]`, `[profile.*]`, and `[workspace.dependencies]` blocks are untouched.

---

## 2. Mass-Conversion Rule

### Scope

The conversion applies to **114 workspace member Cargo.toml files**: all files listed in the `members` array of `/home/mikecubed/projects/ostest/Cargo.toml` (lines 2–235), which excludes:

- `sunset-local/Cargo.toml` — vendored, not a workspace member (see Section 3)
- `userspace/calc-rust/Cargo.toml` — present in the tree but **not listed in `members`**; it retains its own version

Ground truth: `git grep -lE '^\[package\]' -- '**/Cargo.toml' | grep -v 'sunset-local\|calc-rust'` returns exactly 114 files as of phase-98.

### Transformation per member

For every member Cargo.toml, apply two line-level substitutions:

| Before | After |
|---|---|
| `version = "…"` (standalone line at column 0, inside `[package]`) | `version.workspace = true` |
| `edition = "…"` (standalone line at column 0, inside `[package]`) | `edition.workspace = true` |

**Why inline dependency version specs are untouched:** Lines of the form `foo = { version = "1", ... }` are TOML inline tables in `[dependencies]` or `[workspace.dependencies]`. They do not start with `^version =` at column 0; they appear mid-line after a key name. A regex anchored to `^version = ` (caret + `version = `, no leading content) does not match them. The substitution is therefore safe to apply as a column-0–anchored replacement.

**Verified precondition:** All 114 member Cargo.toml files already use `edition = "2024"`. The check `git grep -lE '^\[package\]' -- '**/Cargo.toml' | grep -v 'sunset-local\|calc-rust' | xargs -I{} sh -c 'grep -qE "^edition" {} || echo {}'` returns no output, confirming no member is missing an edition line. The `edition.workspace = true` substitution can therefore be applied universally across all 114 files without first checking for presence.

### Candidate script approach

The following `sed` invocation processes one member at a time. Run it over all 114 files:

```bash
# Generate the file list
git grep -lE '^\[package\]' -- '**/Cargo.toml' \
  | grep -v 'sunset-local\|calc-rust' \
  > /tmp/member-cargo-toml-list.txt

# Apply both substitutions in place
while IFS= read -r f; do
  sed -i \
    -e 's/^version = ".*"$/version.workspace = true/' \
    -e 's/^edition = ".*"$/edition.workspace = true/' \
    "$f"
done < /tmp/member-cargo-toml-list.txt
```

**This script must be reviewed before execution.** Specifically:

1. Confirm the sed substitution does not inadvertently match a `version = ` line inside a `[dependencies]` block that happens to sit at column 0. Such a line would only appear if a dependency was declared as a bare `version = "..."` key on its own line — non-standard TOML. A pre-flight `git grep -nE '^\[dependencies\]' -A 20 -- '**/Cargo.toml' | grep '^version = '` confirms there are no such lines.
2. After running, diff every changed file to confirm the substitution touched only the `[package]` block.
3. Run `cargo check -p kernel` (the default member) immediately after to catch any malformed TOML before committing.

---

## 3. Exclusion: `sunset-local`

`sunset-local/` is a **vendored copy** of the Sunset SSH library. Its `Cargo.toml` (`sunset-local/Cargo.toml`) carries:

```
edition = "2021"
version = "0.4.0"
```

It is **not listed in the workspace `members` array**. It is referenced only in `[workspace.dependencies]` at root Cargo.toml line 263:

```toml
sunset = { path = "sunset-local", default-features = false }
```

Because `sunset-local` is a vendored third-party crate with edition 2021 (predating the project's edition 2024 baseline) and a separate version history, it must remain on its own standalone version and edition. Do not add `version.workspace = true` or `edition.workspace = true` to any file under `sunset-local/`. The Cargo workspace resolver will not apply `[workspace.package]` to a non-member path dependency.

---

## 4. Behavior Change: Boot Banner / `uname` Version String

`env!("CARGO_PKG_VERSION")` is evaluated at compile time from the `version` field of the enclosing crate's resolved `[package]`. After the reform, every workspace member's resolved version is `0.98.0`.

The kernel crate (`kernel/Cargo.toml`) currently carries `version = "0.96.0"`. After the reform, `env!("CARGO_PKG_VERSION")` inside the kernel returns `"0.98.0"`. Wherever the kernel embeds this string — the boot banner, the `uname` release field, any `m3ctl version` output — the reported value changes from `0.96.0` to `0.98.0`.

**This is the one intended behavior change.** No source file edits are required to produce it; it follows automatically from the workspace version resolution.

Note: `AGENTS.md` line 7 currently reads `kernel **v0.97.0**` while `kernel/Cargo.toml` line 3 reads `version = "0.96.0"`. There is a pre-existing drift of one phase between the prose header and the Cargo field. The versioning reform PR must reconcile this: set the workspace version to `0.98.0`, and update the `AGENTS.md` header to `kernel **v0.98.0**` as part of the same commit.

---

## 5. `AGENTS.md` Version-Bump Policy Rewrite

### Current text (to replace)

Located in `/home/mikecubed/projects/ostest/AGENTS.md`, the maintenance-policy block currently reads (line 26):

> When a phase lands, the only edits permitted in this file are: bump the kernel version above, and add a bullet to the capability inventory **only if it introduces a new capability class** (not for changes within an existing one). Prefer rewriting an existing bullet over adding prose. If a section starts listing internal symbols or per-change detail, move it to `docs/` and link instead.

The phrase "**bump the kernel version above**" is the version-bump policy being replaced.

### Replacement text

Replace that sentence only, keeping the rest of the bullet intact. The revised policy block reads:

> When a phase lands, the only edits permitted in this file are: update the `kernel **vX.Y.Z**` header line only when an OS release version is cut (the phase number lives in `docs/roadmap/`, a `phase-NN` git tag, and the commit message; **do NOT bump Cargo versions per phase** — the single `[workspace.package]` version in the root `Cargo.toml` is an OS release version bumped only at a deliberate release step, not per phase), and add a bullet to the capability inventory **only if it introduces a new capability class** (not for changes within an existing one). Prefer rewriting an existing bullet over adding prose. If a section starts listing internal symbols or per-change detail, move it to `docs/` and link instead.

This replacement is in-place: the surrounding paragraph ("Maintenance policy for this file — keep it small") and its other sentences are unchanged.

---

## 6. Verification Step for the Follow-on PR

The follow-on PR is complete when **both** of the following pass:

### Step A — `cargo xtask check`

```bash
cargo xtask check
```

Must exit 0: clippy (`-D warnings`), rustfmt, and all host-side unit tests pass. This verifies that `version.workspace = true` / `edition.workspace = true` are syntactically valid and that no member's code broke under the inherited `edition = "2024"` (which all members already use, so no behavioral change is expected).

### Step B — Version grep audit

```bash
git grep -nE '^version = "' -- '**/Cargo.toml'
```

After the reform, this command must return **exactly two entries**:

1. The root `[workspace.package]` block — `Cargo.toml:N:version = "0.98.0"` (the single source of truth, where N is its line number after insertion)
2. `sunset-local/Cargo.toml:16:version = "0.4.0"` (the excluded vendored library)

`userspace/calc-rust/Cargo.toml` is not a workspace member and will still carry its own `version = "0.1.0"` line; it is an acceptable third entry and should be listed as such in the PR description. It is not converted because `version.workspace = true` is only valid in workspace members.

The expected output shape (order may vary by filesystem):

```
Cargo.toml:<line>:version = "0.98.0"
sunset-local/Cargo.toml:16:version = "0.4.0"
userspace/calc-rust/Cargo.toml:5:version = "0.1.0"
```

Any line beyond these three indicates an unconverted member and must be fixed before the PR merges.

---

## Summary of Changes in the Follow-on PR

| File | Change |
|---|---|
| `Cargo.toml` | Add `[workspace.package]` block with `version = "0.98.0"` and `edition = "2024"` |
| `AGENTS.md` | Update header from `v0.96.0`/`v0.97.0` to `v0.98.0`; replace version-bump policy sentence |
| 114 × `<member>/Cargo.toml` | Replace `version = "…"` with `version.workspace = true`; replace `edition = "…"` with `edition.workspace = true` |
| `sunset-local/Cargo.toml` | No change |
| `userspace/calc-rust/Cargo.toml` | No change (non-member) |
