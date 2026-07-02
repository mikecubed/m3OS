# Handoff — Phase 107: Networked & Signed Package Distribution

**Date:** 2026-07-02 (living doc — update on every session working this phase)
**Branch:** `feat/phase-107-networked-signed-packages` (off `main` at `05a2c6d4`)
**State:** COMPLETE (in-tree). All four tracks landed + green:
`pkg-net-smoke` PASSES at default `-smp 8` (33 steps), offline `pkg-smoke`
unregressed, `cargo xtask check` clean. Committed + PR'd. The only
remaining work is OWNER-side and off-repo: create the public `m3os-pkgs`
repo + `M3OS_PKG_SIGNING_KEY_HEX` secret (runbook:
`docs/appendix/m3os-pkgs/README.md`), then validate the opt-in
`M3OS_PKG_NET=1` live-HTTPS arm.
**Charter:** `docs/roadmap/107-networked-signed-packages.md` (tasks doc:
`docs/roadmap/tasks/107-networked-signed-packages-tasks.md`)

## Where things stand

Recently merged to `main`:

- **PR #272** (Phase 100 software) and **PR #273** (Phase 101 QEMU-side:
  AML interpreter/namespace/`_CRS` in `kernel-core/src/acpi/`, kernel SCI
  path + `SYS_ACPI_*` syscalls, ring-3 `acpid`, `acpi-smoke` gate). Phase
  101's remaining work is HW arms + `Notify()` routing + real
  `RegionSpace` backend — see `docs/handoffs/next-dell-session.md`.

This session implemented Phase 107 (all CI-side; nothing needs hardware):

### Track A — signed index (`pkg-format`)
- `pkg-format/src/lib.rs` gained `pub mod index`: `IndexEntry`,
  `serialize_index` (name-sorted, deterministic — the signed bytes ARE the
  serialized text), `parse_index` (forward-compat unknown tags, hex/size
  validation, `MAX_INDEX_ENTRIES` guard). Host tests: round-trip,
  determinism, malformed-reject, unknown-tag skip.

### Track B — networked `pkg` verbs (`userspace/pkg`)
- `pkg update [url]` — fetches `<base>/index.m3idx` + `.sig` from
  `/etc/pkg/repos.conf` (or the explicit URL override — used by the smoke
  tamper arm), ed25519-verifies against `/etc/pkg/keys/m3os-pkgs.pub`,
  **fail-closed** (bad sig/parse → previous cached index kept), caches to
  `/var/lib/pkg/index.m3idx`.
- `pkg install <name>` — a local `/usr/pkg/<name>.m3pkg` takes the Phase
  85a offline path **unchanged** (pkg-smoke contract); otherwise the
  networked branch: index `D:` fields → the existing `topo_install_order`,
  fetch each missing `<key>.m3pkg` via spawned `curl` (fork/execve/waitpid
  — **no TLS linked into pkg**), SHA-256 vs the signed index `C:` (hard
  reject on mismatch, poisoned file unlinked), `.meta` sidecar staged from
  the index, then the unchanged `install_one`.
- Lib helpers (host-tested): `parse_repos_conf`, `index_dep_map`.
- New dep: `crypto-lib` (verify half only).

### Track C — publish side
- `cargo xtask repo-index [--out <dir>] [--gen-key <path>]`
  (`xtask/src/port_build.rs`): walks `target/pkgcache/` over
  `BUILDABLE_PORTS`, refuses corrupt blobs, emits + signs `index.m3idx`
  (env `M3OS_PKG_SIGNING_KEY` = path to 32-raw-byte or 64-hex seed;
  absent → unsigned + warning), self-verifies (parse + ed25519), stages
  `<key>.m3pkg` blobs. xtask gained a `crypto-lib` dep.
- **Key material (IMPORTANT):** official keypair generated this session.
  - public: committed at `keys/m3os-pkgs.pub`, staged into every image at
    `/etc/pkg/keys/m3os-pkgs.pub` (see `populate_ext2_files`).
  - private seed: `~/.m3os/m3os-pkgs-signing-key` (0600, NOT committed).
    **User action:** add its hex line as Actions secret
    `M3OS_PKG_SIGNING_KEY_HEX` in the future `m3os-pkgs` repo.
  - pubkey hex: `41e06f0db31fffef59697dc591017ea4bed676a8cdc1a403cce3913fa9a98ccd`
- Workflow template + owner setup runbook:
  `docs/appendix/m3os-pkgs/{build-and-publish.yml,README.md}`.
  **User actions remaining:** create the public `m3os-pkgs` repo, add the
  secret, `gh release create repo-x86_64`, copy the workflow in.
- Default repo URL staged in `/etc/pkg/repos.conf`:
  `https://github.com/mikecubed/m3os-pkgs/releases/download/repo-x86_64`.

### Track D — validation
- Host tests all green: `pkg-format` (20 incl. index), `pkg` lib (20 incl.
  repos-conf + solver-from-index-`D:`), xtask (182 incl. index-level
  sign→tamper→reject + signing-seed loading).
- `pkg-net-smoke` gate (`cmd_pkg_net_smoke`, exit code 96): per-run
  keypair; synthetic `nettest`→`netdep` packages; host HTTP thread serves
  `/good` (real index), `/tampered` (flipped index byte + real sig),
  `/bad` (validly-signed index whose `C:` ≠ served blob) at
  `10.0.2.100:80` via SLIRP `guestfwd`; image built with the
  `M3OS_PKG_TEST_PUBKEY` / `M3OS_PKG_TEST_REPO_URL` staging overrides
  (see `populate_ext2_files`). In-guest sequence: `pkg install curl`
  (bundled transport) → `pkg update` (verify, 2 pkgs) → tampered update
  rejected → `pkg install nettest` (dep-first fetch+check+install, payload
  `cat` readable) → bad-blob install rejected pre-extraction.
  Precondition: curl/mbedtls/zlib/ca-certificates in `target/pkgcache`.
- Wired: `M3OS_PKG_NET_REGRESSION=1` pre-push block + `AGENTS.md` row.
  Live-HTTPS arm is opt-in `M3OS_PKG_NET=1`, skip-with-reason until the
  real repo exists.

## RESUME HERE

1. **`pkg-net-smoke` debugging in progress.** Findings so far (runs 1–4;
   full serial via `M3OS_SERIAL_LOG`, logs in the session scratchpad):
   - Boot, autologin, `pkg install curl` (+deps) all PASS.
   - Image staging verified byte-exact by mounting the built `disk.img`
     partition with debugfs: `/etc/pkg/repos.conf` has the SLIRP test URL,
     `/etc/pkg/keys/m3os-pkgs.pub` matches the session key.
   - **Transport is fine**: a shell-run
     `curl -sS -v -o /tmp/idx.probe http://10.0.2.100:80/good/index.m3idx`
     connects and gets `HTTP/1.1 200` instantly (guestfwd + host HTTP
     thread both good).
   - **pkg's spawn reaches execve**: kernel logs
     `elf: mapped pid=48 binary=/usr/local/bin/curl` for the pkg-spawned
     child — then `pkg update` produces no further output; curl's
     `--connect-timeout 20` never surfaces an exit either.
   - waitpid theory RULED OUT by reading the kernel: `sys_waitpid` has a
     1 s deadline backstop (Phase 57e Bug #13) that re-scans every second,
     so an already-zombie child cannot hang it.
   - **Run 5 bisected a real curl-on-m3OS hang**: a SHELL-run curl with
     `--connect-timeout 20 --max-time 120` hangs PRE-CONNECT (host server
     logged no GET), while plain `-sS -v` works instantly. Explanation:
     with no threaded/c-ares resolver, curl arms the alarm()/SIGALRM +
     sigsetjmp timeout path whenever a timeout flag is set — that
     machinery wedges on m3OS. **Do not pass timeout flags to the
     in-guest curl** (fetch_url no longer does; the smoke has a comment
     pinning this). This also explains runs 3–5's pkg hangs, but NOT
     runs 1–2 (which used plain argv) — the remaining delta there is
     envp: pkg passed only PATH where the shell passes PATH/HOME/TERM;
     `fetch_url` now mirrors the shell's envp (HOME=/root, TERM).
   - **Run 6 killed the spawn-seam theory**: probe 2 — run from the
     SHELL with plain `-fsSL -o /var/lib/pkg/b.probe` — ALSO hung
     pre-connect (host server logged only probe 1's GET). Caveat learned:
     a Wait on an `echo MARKER` output false-matches the tty INPUT echo —
     use `curl -w 'P2DONE %{http_code}'` markers instead (only printed on
     completion).
   - **Run 7 confirmed a second-connection wedge**: a SECOND
     byte-identical `-sS -v` probe hangs. Crucially curl printed
     `Connected to 10.0.2.100` for BOTH attempts while the host server
     received only ONE GET — the guest TCP stack believes the handshake
     completed but connection #2's request bytes never arrive host-side.
     First connection of a boot always works.
   - **Run 8 (control, `-smp 1`) ALSO fails** — connection #2 wedges
     single-core too, so SMP is exonerated (user directive stands
     regardless: gates must run multi-core; the `-smp 1` pin was removed).
     Ephemeral-port tuple reuse also ruled out by reading the allocator
     (`tick_count() as u16 | 0x8000` — collisions need same-tick
     connects; ours are ~10 s apart).
   - **Converged root cause: libslirp `guestfwd` wires only the FIRST
     flow to a `tcp:` target.** Later guest connections complete their
     handshake against slirp itself (curl prints Connected) but are never
     dialed through to the host — an established black hole. Fits every
     observation, and explains why node/go gates (one connection per
     boot) never hit it. NOTE for future gates: do not use guestfwd for
     anything that connects twice.
   - **Fix (run 9)**: no guestfwd — the host server binds a FIXED port
     (`127.0.0.1:18923`) and the guest fetches from
     `http://10.0.2.2:18923/...` (the SLIRP host alias, ordinary
     outbound TCP, no per-flow plumbing). Multi-connection immediately
     worked at `-smp 8` (6 GETs/boot).
   - Three residual bugs after the transport fix, all found+fixed:
     (1) **em-dash sentinels** — the harness assembles serial chunks with
     lossy UTF-8, so a chunk boundary can split a multi-byte char and the
     pattern never matches. All wait-matched `pkg` output is now pure
     ASCII (house rule: keep sentinels ASCII).
     (2) **fail-prefix own-goal** — `"pkg install: netdep: fetch"`
     matched pkg's own `fetching N bytes` progress line; prefixes
     tightened to `fetch failed`.
     (3) **blob fetches ignored the index's source base** — install
     fetched `U:` paths against repos.conf even when the index came from
     a `pkg update <url>` override (gate's /bad arm 404'd off /good).
     Fixed properly: a successful update records its base in
     `/var/lib/pkg/index.src`; installs prefer it, repos.conf bases
     remain fallback mirrors.
   - **RESOLVED — run 12 PASSED** (all 31 steps, 48 s, `-smp 8`): index
     verify, tamper reject, dep-solved fetch+SHA-check install, payload
     readable, bad-blob reject. Probes 1–2 remain in the gate as
     transport/multi-connection regression sentinels.
2. Then: `cargo xtask fmt --fix && cargo xtask check`, re-run
   `pkg-net-smoke` + `pkg-smoke` (offline non-regression: the
   `cmd_install` early-branch must not disturb bundled installs).
3. Docs: README roadmap row 107 + tasks-doc track table/checkboxes to
   match what actually landed (mirror the Phase 101 honesty style:
   deferred items stay unchecked with notes).
4. Commit (suggest: one commit, all tracks), push, PR to `main`.
5. After merge (user actions): create `m3os-pkgs` repo per
   `docs/appendix/m3os-pkgs/README.md`, then validate the live arm
   (`pkg update` against real GitHub) and record it.

## Facts that will save the next session time

- The smoke's Send steps need `&'static str` — guest-side URLs are the
  fixed `http://10.0.2.100:80/...` (guestfwd maps to the dynamic host
  port), which is why they can be static.
- `SmokeStep::Wait` matches are **non-consuming**; ordered waits on
  distinct patterns are fine, exact-duplicate patterns would re-match.
- `pkg`'s DB "artifact key" is the whole-blob SHA-256 (`install_one`),
  which is exactly the index `C:`/`K:` value — content-addressing lines up
  end to end.
- `env::set_var` needs `unsafe` (Rust 2024); the gate sets/unsets the two
  staging overrides around `create_data_disk` only.
- `debugfs` staging: new dirs need explicit `mkdir` lines
  (`etc/pkg`, `etc/pkg/keys` added).
- Ports metadata: `port_deps()` is a hard-coded Rust table in
  `port_build.rs` (NOT the Portfile `DEPS=`); `repo-index` uses it for
  `D:` — keep them in sync when adding ports.
