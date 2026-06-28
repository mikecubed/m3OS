# AGENTS.md Slimming (Spec)

**Status:** Executed in PR #270 (folded into Phase 98 — see the Execution Note below)
**Source Ref:** phase-98
**Track:** D
**Summary:** Specification (and now execution record) for cutting `AGENTS.md` by relocating the regression-gate table to `docs/appendix/regression-gates.md`, collapsing the capability-inventory bullets, and removing stale/duplicated content — with zero operational info lost.

## Execution Note (PR #270)

Executed as part of Phase 98 (not deferred). As-landed: `AGENTS.md` went **82,868 B → 22,313 B** (~73%, leaner than the ~28–30 KB estimate because the capability bullets + stale blocks were larger than estimated); `docs/appendix/regression-gates.md` was created with **54** `## <gate>` sections carrying every gate description verbatim; the lean inline table retained all **39** top-level gate→env-var rows; `.github/copilot-instructions.md` became a 533-byte thin pointer; the header was bumped to `v0.98.0` and the version-bump policy rewritten for the single workspace version (Track C). Relocation integrity verified: every lean-table gate has a `regression-gates.md` section, and the sub-gate env-vars `M3OS_CARGO_REGRESSION` / `M3OS_USB_ETH_REGRESSION` remain documented inside their parent gate sections.

---

## 1. Current Measurements

| File | Bytes | Lines |
|---|---|---|
| `AGENTS.md` | **82,868** | 309 |
| `.github/copilot-instructions.md` | 7,992 | ~200 |

The regression-gate table (pre-push hook table, lines 96–136 of the current file) is the dominant cost: 41 table rows where each cell is a 200–600-word phase diary. The capability-inventory bullets (lines 11–22) add another ~12 KB, including the ~9 KB "Package management" saga.

---

## 2. The Lean Inline Gate Table

This is the produced artifact. The follow-on PR replaces the current 41-row verbose table with the table below verbatim. The intro sentence before the table in AGENTS.md becomes:

> These opt-in gates run on `pre-push` when their env var is set. Full per-gate descriptions live in [`docs/appendix/regression-gates.md`](docs/appendix/regression-gates.md).

**Replacement table** (paste in place of the current `| Gate | Env var |` table and its trailing `tls-smoke`/`dns-smoke` note paragraph):

| Gate | Env var | One-line purpose |
|---|---|---|
| `ssh-e1000-banner-check` | `M3OS_E1000_REGRESSION=1` | e1000 NIC initializes; SSH server banner answers on TCP/22. |
| `doom-audio-smoke` | `M3OS_DOOM_AUDIO_REGRESSION=1` | DOOM plays non-silent PCM through the ac97/hda `audio_server` mixer. |
| `termios-smoke` | `M3OS_TERMIOS_REGRESSION=1` | PTY/termios/line-discipline correct (raw mode, SIGWINCH, ICANON). |
| `tui-app-smoke` | `M3OS_TUI_APP_REGRESSION=1` | ncurses TUI apps (htop, tmux, less) render correctly over the terminal. |
| `doom-concurrent-smoke` | `M3OS_DOOM_CONCURRENT_REGRESSION=1` | DOOM runs concurrently with other ring-3 processes; no scheduler starvation. |
| `tiling-smoke` | `M3OS_TILING_REGRESSION=1` | `display_server` tiling layout and multi-window compositor work correctly. |
| `htop-render-probe` | `M3OS_HTOP_REGRESSION=1` | htop process list renders visible rows on the QMP/PPM framebuffer dump. |
| `xhci-bringup-smoke` + `xhci-enum-smoke` + `usb-smoke` + `usb-report-smoke` + `usb-hotplug-smoke` + `usb-storage-smoke` + `usb-hub-smoke` + `usb-mount-smoke` + `usb-unmount-smoke` + `usb-storage-dual-smoke` + `usb-multi-controller-smoke` + `usb-eth-smoke` | `M3OS_USB_REGRESSION=1` | Full xHCI USB suite: HID (Boot+Report), mass-storage, hub, hot-plug, multi-controller, USB-Ethernet. |
| `tls-smoke` PASS (not SKIP) | `M3OS_TLS_REGRESSION=1` | PT_TLS/pthread smoke PASSED (not skipped); musl cross-compiler was present at build. |
| `dns-smoke` PASS (not SKIP) | `M3OS_DNS_REGRESSION=1` | DNS/recvmsg smoke PASSED (not skipped); musl cross-compiler was present at build. |
| `multi-nic-smoke` | `M3OS_MULTI_NIC_REGRESSION=1` | e1000 + e1000e + igb NICs all initialize and register in the multi-NIC registry. |
| `ure-smoke` | `M3OS_URE_REGRESSION=1` | RTL8156 USB-Ethernet dongle brings up via `ure` driver on real silicon; skip without dongle. |
| `hda-smoke` | `M3OS_HDA_REGRESSION=1` | Intel HD Audio codec + `hda-duplex` drives a non-silent WAV output stream. |
| `usb-audio-smoke` | `M3OS_USB_AUDIO_REGRESSION=1` | USB UAC speaker receives PCM over isochronous OUT TRBs; captured WAV is non-silent. |
| `wifi-smoke` | `M3OS_WIFI_REGRESSION=1` | mt792x Wi-Fi driver loads; radio path VFIO-validated only (always skip-with-reason in CI). |
| `ahci-smoke` + `ahci-root-smoke` + `ahci-rw-smoke` + `ahci-persist-smoke` | `M3OS_AHCI_REGRESSION=1` | AHCI ring-3 suite: IDENTIFY/RW/flush, ext2 root mount, write round-trip, reboot-persistence. |
| `mitigations-status-smoke` | `M3OS_MITIGATIONS_REGRESSION=1` | `m3ctl mitigations status` reports correct Spectre-v2/retpoline/KPTI posture at boot. |
| `pkgcache-hit-check` | `M3OS_PKGCACHE_REGRESSION=1` | Second port build hits `.m3pkg` cache with zero compiler invocations. |
| `pkg-smoke` | `M3OS_PKG_REGRESSION=1` | In-OS `pkg` manager install/list/verify/upgrade/remove + transitive dependency solver works. |
| `git-local-smoke` | `M3OS_GIT_REGRESSION=1` | Static git + pkg solver runs local init/commit/branch/merge workflow in-OS. |
| `git-ssh-smoke` | `M3OS_GIT_SSH_REGRESSION=1` | dropbear SSH client installs and runs in-OS; live clone/mismatch-reject is opt-in. |
| `git-https-smoke` | `M3OS_GIT_HTTPS_REGRESSION=1` | mbedTLS+curl+git chain installs; TLS cert verify + live HTTPS clone is opt-in. |
| `python-smoke` | `M3OS_PYTHON_REGRESSION=1` | Static CPython 3.12 installs; stdlib imports, sha256, and file I/O run in-OS. |
| `coreutils-smoke` | `M3OS_COREUTILS_REGRESSION=1` | uutils/coreutils 0.9.0 multicall installs; GNU-compat battery + inode-identity passes. |
| `clang-smoke` | `M3OS_CLANG_REGRESSION=1` | Clang 18 + lld installs; compiles + links C and C++ natively in-OS. |
| `rustc-smoke` | `M3OS_RUST_REGRESSION=1` | Dynamic musl rustc 1.96.0 installs; `rustc hello.rs` compiles and runs (KVM-gated). |
| `go-runtime-smoke` | `M3OS_GO_REGRESSION=1` | Static Go 1.24 runtime starts, spawns goroutine, completes HTTP GET over TCP stack. |
| `gh-smoke` | `M3OS_GH_REGRESSION=1` | Static `gh` 2.82.1 runs in-OS; authenticated `gh pr list` / `gh issue create` opt-in. |
| `node-smoke` | `M3OS_NODE_REGRESSION=1` | Jitless Node 22 installs; local JS runtime + HTTP GET over in-kernel TCP always-on. |
| `userspace-simd-smoke` | `M3OS_SIMD_REGRESSION=1` | AES-NI + SSE binary runs fault-free in ring-3; kernel ELF confirmed to contain no XMM. |
| `pku-smoke` | `M3OS_PKU_REGRESSION=1` | PKU alloc/deny-fault/sigframe/W^X-v2 matrix passes; SKIPs on a no-PKU CPU. |
| `kstack-overflow-smoke` | `M3OS_KSTACK_OVERFLOW_REGRESSION=1` | Kernel-stack overflow kills the offending child via SIGSEGV; parent keeps running. |
| `smp-smoke` | `M3OS_SMP_REGRESSION=1` | 256 futex-heavy async ops complete across 4 cores; no TLB-shootdown panics or lost wakeups. |
| `node-jit-smoke` | `M3OS_NODE_JIT_REGRESSION=1` | JIT Node: V8 TurboFan + WASM execute under W^X v2 PKU guard (requires KVM + PKU CPU). |
| `claude-smoke` | `M3OS_CLAUDE_REGRESSION=1` | claude-code 2.1.112 installs (DEPS=node), CLI runs; TUI render arm requires KVM + JIT node. |
| `vfs-throughput-smoke` | `M3OS_VFS_THROUGHPUT_REGRESSION=1` | 8 MiB VFS write+read IPC-call count stays under coalescing-path regression ceilings. |
| `vfs-bulkio-smoke` | `M3OS_VFS_BULKIO_REGRESSION=1` | mbedtls install read/write block-call deltas stay under thresholds after coalescing. |
| `ipv6-smoke` | `M3OS_IPV6_REGRESSION=1` | IPv6 link-local, NDP Neighbor Advertisement, AF_INET6 sockets, ICMPv6, TCP/UDP all pass. |
| `dynamic-hello-smoke` (+ `dynamic-python-smoke` opt-in) | `M3OS_DYNAMIC_C_REGRESSION=1` | Dynamic C binary loads via PT_INTERP + libc.so + dlopen; TLS and thread-fault arms pass. |

The `tls-smoke`/`dns-smoke` PASS-not-SKIP note (currently the paragraph at lines 138–143) is absorbed into the purpose column above; remove the standalone paragraph.

---

## 3. Relocation Target: `docs/appendix/regression-gates.md`

### 3.1 File to create (in follow-on PR)

`docs/appendix/regression-gates.md` — **do not create this file in the slimming PR itself before the table is replaced**; create it in the same commit that replaces the AGENTS.md table.

### 3.2 Header

```markdown
# Regression Gates — Full Descriptions

Each section below corresponds to one named gate in the `AGENTS.md` pre-push
gate table. The lean table in [`AGENTS.md`](../../AGENTS.md) gives the
env var and a one-line purpose; this file gives the full description verbatim.

Gates are ordered identically to the AGENTS.md lean table.
```

### 3.3 Section convention

One `## <gate-name>` section per named gate. Where a single env var covers a bundle of named gates (the USB suite, AHCI suite), each constituent gate gets its own `##` section in document order, grouped under a `### Bundle: M3OS_<FOO>_REGRESSION=1` sub-header for visual grouping. Example:

```markdown
### Bundle: M3OS_USB_REGRESSION=1

## xhci-bringup-smoke

<verbatim description extracted from the AGENTS.md table cell>

## xhci-enum-smoke

...
```

### 3.4 Named-gate inventory

The follow-on PR extracts **54 named gates** into sections, in this order:

**Standalone (one gate per env var):** `ssh-e1000-banner-check`, `doom-audio-smoke`, `termios-smoke`, `tui-app-smoke`, `doom-concurrent-smoke`, `tiling-smoke`, `htop-render-probe`

**USB bundle (`M3OS_USB_REGRESSION`):** `xhci-bringup-smoke`, `xhci-enum-smoke`, `usb-smoke`, `usb-report-smoke`, `usb-hotplug-smoke`, `usb-storage-smoke`, `usb-hub-smoke`, `usb-mount-smoke`, `usb-unmount-smoke`, `usb-storage-dual-smoke`, `usb-multi-controller-smoke`, `usb-eth-smoke`

**Standalone:** `tls-smoke`, `dns-smoke`, `multi-nic-smoke`, `ure-smoke`, `hda-smoke`, `usb-audio-smoke`, `wifi-smoke`

**AHCI bundle (`M3OS_AHCI_REGRESSION`):** `ahci-smoke`, `ahci-root-smoke`, `ahci-rw-smoke`, `ahci-persist-smoke`

**Standalone:** `mitigations-status-smoke`, `pkgcache-hit-check`, `pkg-smoke`, `git-local-smoke`, `git-ssh-smoke`, `git-https-smoke`, `python-smoke`, `coreutils-smoke`, `clang-smoke`, `rustc-smoke`, `go-runtime-smoke`, `gh-smoke`, `node-smoke`, `userspace-simd-smoke`, `pku-smoke`, `kstack-overflow-smoke`, `smp-smoke`, `node-jit-smoke`, `claude-smoke`, `vfs-throughput-smoke`, `vfs-bulkio-smoke`, `ipv6-smoke`

**Dynamic-C bundle (`M3OS_DYNAMIC_C_REGRESSION`):** `dynamic-hello-smoke`, `dynamic-python-smoke`

### 3.5 Content of each section

Each section's body is the **verbatim** text extracted from the current AGENTS.md gate-table cell for that gate (with any table-formatting characters removed). No rewording. For bundled gates, extract the portion of the composite cell that describes that specific gate; the prose boundaries are the per-gate introductory parentheticals in the existing cell.

---

## 4. Capability-Inventory Collapse

### 4.1 Rule

Each capability-class bullet in the `## Project Overview` block collapses to **one line** per class: the class name (bold) + a single sentence of ≤25 words + a pointer to `docs/roadmap/README.md` for detail. Phase references and implementation notes are removed from AGENTS.md; they live in the phase docs.

### 4.2 Before/After Example — "Package management" bullet

**Before** (current line 17, ~3,800 characters):

```
- **Package management**: content-addressed prebuilt-package substrate — a relocatable `.m3pkg` format + portable content key (`pkg-format`, host-tested), an `xtask` seal-after-install / resolve-before-build pkgcache (`target/pkgcache/`, strip-before-seal, zero-rebuild gate), and an offline in-OS `pkg install`/`remove`/`upgrade`/`list`/`verify` installer with a transitive dependency solver ... [continues for ~800 words covering git, Python, Clang/LLVM, Go, Node.js, Claude Code, uutils/coreutils, rustc]
```

**After** (one line):

```
- **Package management**: content-addressed `.m3pkg` substrate + `pkg` in-OS installer with transitive dependency solver; cross-compiled ports include git, Python, Clang, Go, Node.js, Claude Code, coreutils, rustc. See `docs/roadmap/README.md`.
```

### 4.3 Full class list to collapse

Apply the same one-line treatment to every bullet in the capability inventory. Current bullets (lines 11–22):

- **Userspace** — keep one line (already short; add pointer to codebase-map)
- **Networking & storage** — collapse to one line; remove per-driver detail
- **Wireless** — keep one line (already concise)
- **IOMMU substrate** — keep one line (already concise)
- **Ring-3 driver hosting** — keep one line (already concise)
- **USB host stack** — collapse to one line; remove per-class driver detail and phase annotations
- **Package management** — see before/after above
- **Graphical stack** — keep one line (already concise)
- **Audio** — keep one line (already concise)
- **Terminal** — keep one line (already concise)
- **Dynamic linking & a real `libc.so`** — collapse to one line
- **CPU hardening** — collapse to one line; remove per-mitigation detail

The "Phase history is NOT maintained here" note and the "keep it small" maintenance policy paragraph (lines 24–26) are **kept verbatim** — they are the policy statement, not prose to cut.

---

## 5. Version-Bump Policy and Drift Reconciliation

### 5.1 Drift: current state

| Location | Value |
|---|---|
| `AGENTS.md` line 7 (kernel version header) | `v0.97.0` |
| `kernel/Cargo.toml` `version` field | `0.96.0` |

The kernel crate (`kernel/Cargo.toml`) reports `0.96.0`; the AGENTS.md header claims `v0.97.0`. These are out of sync.

### 5.2 Resolution in the follow-on PR

After the Track C versioning reform (`docs/appendix/versioning-reform.md`) adopts a single `[workspace.package] version = "0.98.0"`, all crates report `0.98.0`. The AGENTS.md kernel version header becomes:

```
**m3OS** (technical name: `m3os`) is a bootable microkernel OS in Rust: x86_64, UEFI boot, kernel **v0.98.0**.
```

The slimming PR executes after Track C's follow-on PR. If executed before, set the header to `v0.97.0` temporarily (matching what a clean build actually reports from the kernel crate's bumped version) and note the pending Track C reconciliation in a comment.

### 5.3 Version-bump policy rewrite

The "keep it small" maintenance-policy paragraph (line 26) currently says "bump the kernel version above." After Track C, the policy line becomes:

> When a phase lands, bump the single workspace version in `[workspace.package]` in the root `Cargo.toml` and update the version in the header above. All other version lines in the tree are `version.workspace = true` and update automatically.

See `docs/appendix/versioning-reform.md` for the full migration spec.

---

## 6. Stale Content Flagged for Removal

The following three blocks are removed in the follow-on PR. None carries operational info not available elsewhere.

### 6.1 Phase-annotated `cargo xtask check` comment

**Current line 39** (truncated for readability):

```
cargo xtask check        # clippy (-D warnings) + rustfmt + host tests for kernel-core
                         # (incl. storage::{ahci,ata}, spectre, kpti), passwd,
                         # driver_runtime, ... pkg-format (Phase 85a .m3pkg pack/unpack/
                         # verify + content-key), pkg (Phase 85a installed-file DB),
                         # xtask (Phase 85a Portfile parser + package_key + pkgcache
                         # seal/resolve) + the Phase 84 retpoline objdump indirect-branch gate
```

The phase annotations and crate list are maintenance-sensitive (they go stale every phase) and are not needed for a developer to run `cargo xtask check`.

**Replacement:**

```bash
cargo xtask check        # clippy (-D warnings) + rustfmt + all host-side unit tests
```

### 6.2 Stale ASCII architecture diagram

**Current lines 149–168:**

```
Ring 0 (kernel/):                Ring 3 (userspace/):
  - Frame allocator                - init (PID 1 daemon)
  - Page table manager             - sh0 (built-in shell)
  - Scheduler (SMP-aware)          - coreutils (cat, ls, grep, etc.)
  - IPC engine + capabilities      - ping (ICMP network tool)
  - IDT / APIC / interrupt router  - edit (text editor)
                                   - login, su, passwd, adduser
                                   - id, whoami
                                   - ion shell (external)
  - Syscall gate
  - VFS + FAT32 + tmpfs
  - Network stack (IPv4/TCP/UDP)
  - Unix domain sockets (AF_UNIX)
  - VirtIO drivers (blk, net)
  - ACPI / PCI enumeration
  - Framebuffer console
  - TTY + signal handling
  - SMP (multi-core boot + IPI)
```

**Stale claims:**
- `VFS + FAT32 + tmpfs` — the root filesystem is ext2 served by the ring-3 `vfs_server`, not FAT32. FAT32 was the early boot format; it is not the runtime root.
- `Network stack (IPv4/TCP/UDP)` — the stack is now dual-stack IPv4/IPv6 (Phase 91: ICMPv6, NDP, SLAAC, DHCPv6, `AF_INET6`). The IPv4-only claim is wrong.

**Action:** remove the ASCII diagram entirely. The prose sentence immediately before it ("Microkernel: ring 0 kernel handles memory management, scheduling, IPC, interrupt routing, and device drivers. Userspace processes run in ring 3...") is sufficient; the diagram's detail lives in `docs/appendix/codebase-map.md`.

### 6.3 Duplicated doc-template rules block

**Current lines 293–309** (`### Documentation templates — all docs must conform`):

```
All roadmap docs must follow the templates in `docs/appendix/doc-templates.md`. When creating
or updating docs, use the matching template:

| Doc type | Template section | Required fields |
|---|---|---|
| Phase design doc | ... | ... |
| Phase task doc  | ... | ... |
| Roadmap README row | ... | ... |

Rules:
- Never create a task doc without all template sections populated.
- Never create a design doc missing Status, Source Ref, Depends on, or Builds on.
...
```

This block is a copy of the content already canonically defined in `docs/appendix/doc-templates.md`. Maintaining two copies causes drift. **Action:** replace the entire block with a one-line pointer:

```markdown
### Documentation templates

All roadmap and appendix docs must follow the templates in
[`docs/appendix/doc-templates.md`](docs/appendix/doc-templates.md).
```

---

## 7. `.github/copilot-instructions.md` Thin-Pointer Spec

### 7.1 Current state

`.github/copilot-instructions.md` is **7,992 bytes** and duplicates content from AGENTS.md with a stale "toy OS" framing. Specifically it contains:

- Its own "Build & Run Commands" section with the same `cargo xtask image/run/test` commands, but missing `cargo xtask run-gui`, `cargo xtask check`, `cargo xtask fmt --fix`, and the `--fresh` / `--timeout` flags.
- Its own "Critical target flags" section (the `disable-redzone`, `-mmx,-sse`, `panic-strategy` explanation) — verbatim from AGENTS.md but missing the Phase 86f nuance that userspace (`x86_64-m3os.json`) does enable SSE/AES-NI while only the kernel stays soft-float.
- Its own "Architecture" and "Adding a new kernel crate" guidance, describing a much earlier project state (it calls m3OS a "toy bootable operating system" and "educational but aims for a functional userspace shell" — both are now outdated given the full networking, graphical, and toolchain stack).

The copilot-instructions framing predates Phase ~60 and no longer matches the project's actual scope or conventions.

### 7.2 Required replacement

**Replace the entire content of `.github/copilot-instructions.md`** with:

```markdown
# Copilot Instructions — m³OS

This repository uses `AGENTS.md` as the canonical source for project
guidance, conventions, and build instructions. Read `AGENTS.md` before
making any changes.

Key entry points:
- **Build & run:** `cargo xtask run` / `cargo xtask check` — see `AGENTS.md` § Build & Run
- **Architecture:** see `docs/appendix/codebase-map.md`
- **Conventions:** see `AGENTS.md` § Critical Conventions
- **Regression gates:** see `docs/appendix/regression-gates.md`
- **Phase docs:** see `docs/roadmap/README.md`
```

This eliminates the duplicated stale content and ensures any update to AGENTS.md automatically becomes the source of truth for both Claude Code and GitHub Copilot.

---

## 8. Target Size Summary

| Metric | Before | After |
|---|---|---|
| `AGENTS.md` bytes | 82,868 | ~28,000–30,000 |
| `AGENTS.md` lines | 309 | ~130–140 |
| Gate table rows | 39 (verbose; ~60 KB) | 39 (lean; ~3.5 KB) |
| Capability inventory | 12 bullets, ~12 KB | 12 bullets, ~1.4 KB |
| `docs/appendix/regression-gates.md` | (does not exist) | ~55 KB (54 gate sections) |
| `.github/copilot-instructions.md` | 7,992 bytes | ~300 bytes |
| Operational info lost | — | **none** (gate→env-var mapping stays inline) |

The ~63% reduction (~37 K tokens → ~13 K tokens per session) comes almost entirely from relocating the gate-table diaries to `regression-gates.md`. The Gate→env-var mapping and one-line purpose remain inline so a developer never needs to open `regression-gates.md` to know which env var to set; the full description is one click away when needed.

---

## 9. Execution Checklist (for the follow-on PR)

The follow-on PR must perform these edits atomically (one commit is ideal):

1. Create `docs/appendix/regression-gates.md` with the 54-section structure (section 3).
2. Replace AGENTS.md lines 96–143 (the gate table + tls/dns note) with the lean table from section 2, preceded by the intro sentence pointing to `regression-gates.md`.
3. Collapse the capability-inventory bullets (lines 11–22) per section 4.
4. Update the kernel version header (line 7) per section 5.2.
5. Rewrite the version-bump policy sentence in the maintenance-policy paragraph (line 26) per section 5.3.
6. Replace the `cargo xtask check` comment (line 39) per section 6.1.
7. Remove the ASCII architecture diagram block (lines 149–168) per section 6.2.
8. Replace the doc-template rules block (lines 293–309) with the one-liner per section 6.3.
9. Overwrite `.github/copilot-instructions.md` per section 7.2.
10. Verify: `wc -c AGENTS.md` reports ≤ 30,000 bytes.
11. Verify: every gate name and env var from the original table appears in `regression-gates.md`.
