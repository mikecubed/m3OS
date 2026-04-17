# Phase 54 — Deep Serverization: Task List

**Status:** Complete
**Source Ref:** phase-54
**Depends on:** Phase 50 ✅, Phase 51 ✅, Phase 52 ✅, Phase 53 ✅
**Goal:** Move meaningful storage, namespace, and UDP policy out of ring 0 while
keeping the syscall ABI stable and validating degraded-mode behavior for the new
service boundaries.

## Track Layout

| Track | Focus | Status | Notes |
|---|---|---|---|
| A | Storage extraction | Complete | Read-only `/etc/...` rootfs reads now traverse `vfs_server`/`fat_server` for the migrated slice |
| B | VFS thinning | Complete | Metadata, access, `getdents`, and mount-policy flow through `vfs_server` with kernel fallback preserved |
| C | UDP policy extraction | Complete | UDP policy/state moves into `net_server` with kernel handle ownership preserved |
| D | Service integration | Complete | `init` degraded-mode rules and architecture docs reflect the extracted services honestly |
| E | Validation and closure | Complete | Regression, quality-gate, and docs closure finished; signal/IPC shutdown bug fixed |

## Track A — Storage extraction

### Task A1 — Route the first rootfs storage path through ring 3

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_open`, `open_resolved_path`
**Why it matters:** Phase 54 is not real unless at least one meaningful storage
path crosses a supervised ring-3 service boundary.

**Acceptance:**
- Read-only `/etc/...` opens can route through `vfs_server`
- bootstrap kernel fallback still works before the service is available
- DAC checks remain enforced on the migrated path

### Task A2 — Stand up the storage services

**File:** `userspace/fat_server/src/main.rs`, `xtask/src/main.rs`
**Symbol:** `program_main`, `populate_ext2_files`
**Why it matters:** The storage boundary must be backed by a real supervised
service, not just a design note.

**Acceptance:**
- `fat_server` is built, embedded, and configured as a service
- the ext2 data disk contains the service config needed for Phase 54 boot

## Track B — VFS thinning

### Task B1 — Move pathname and metadata policy outward

**File:** `userspace/vfs_server/src/main.rs`
**Symbol:** `handle_open`, `handle_read`, `handle_stat_path`, `handle_list_dir`
**Why it matters:** The kernel should keep handle/object mediation while userspace
owns the higher-level pathname and namespace policy for the migrated slice.

**Acceptance:**
- open/stat/access/getdents for the migrated rootfs slice route through `vfs_server`
- reply lengths and file-handle state stay bounded and explicit
- fallback behavior still works when the service is absent

### Task B2 — Preserve degraded-mode fallback

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `vfs_service_open`, `path_node_nofollow`
**Why it matters:** Service extraction is only operable if the kernel can fall
back safely when the service is missing or intentionally stopped.

**Acceptance:**
- failed routed opens can fall back to the kernel ext2/bootstrap path where documented
- stopping `vfs` does not make new rootfs opens permanently unusable

## Track C — UDP policy extraction

### Task C1 — Move UDP policy into `net_server`

**File:** `userspace/net_server/src/main.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** UDP service handlers and syscall routing for the migrated UDP path
**Why it matters:** Networking is too large a policy surface to leave untouched if
Phase 54 is supposed to prove real microkernel movement.

**Acceptance:**
- UDP bind/connect/send/recv policy routes through `net_server`
- kernel fd/socket handles remain stable for existing applications
- close-time teardown handles duplicate-close/reuse races correctly

### Task C2 — Add a regression probe for degraded UDP fallback

**File:** `userspace/udp-smoke/src/main.rs`, `xtask/src/main.rs`
**Symbol:** `main`, `serverization_fallback_steps`
**Why it matters:** The migrated UDP boundary needs a concrete operator-facing
check, not just compile-time evidence.

**Acceptance:**
- `/root/udp-smoke` exercises the migrated UDP path
- after stopping `net_udp`, the documented fallback path still succeeds

## Track D — Service integration

### Task D1 — Make restart/degraded rules explicit

**File:** `userspace/init/src/main.rs`, `xtask/src/main.rs`
**Symbol:** `note_extracted_service_degradation`, service config generation
**Why it matters:** Extracted core services need honest lifecycle semantics; silent
auto-restart would hide boundary failures instead of documenting them.

**Acceptance:**
- `vfs` and `net_udp` ship with `restart=never`
- `init` logs the degraded-mode contract for both services
- service configs are embedded in the shipped image

### Task D2 — Update architecture truth surfaces

**File:** `docs/appendix/architecture-and-syscalls.md`
**Symbol:** current-reality/service-boundary tables
**Why it matters:** The architecture docs must describe the shipped split, not the
pre-Phase-54 aspiration.

**Acceptance:**
- storage/VFS and UDP boundary tables mention the real ring-3 services
- the kernel-vs-userspace split is described as mechanism vs policy

## Track E — Validation and closure

### Task E1 — Prove degraded-mode behavior with regression coverage

**File:** `xtask/src/main.rs`
**Symbol:** `serverization_fallback_steps`, regression launch path
**Why it matters:** Phase 54 closure depends on evidence that the new service
boundaries behave correctly when the services are intentionally stopped.

**Acceptance:**
- regression provisions the ext2 data disk before QEMU launch
- `cargo xtask regression --test serverization-fallback` passes
- the regression demonstrates both rootfs and UDP degraded-mode fallback behavior

### Task E2 — Fix the IPC/signal shutdown contract exposed by validation

**File:** `kernel/src/process/mod.rs`, `kernel/src/ipc/endpoint.rs`, `kernel/src/task/scheduler.rs`
**Symbol:** `send_signal`, `cancel_task_wait`, `blocked_ipc_task_ids_for_pid`
**Why it matters:** Services blocked in IPC must not survive `SIGKILL` until a new
client request arrives; that would make extracted-service shutdown and fallback
semantics unreliable.

**Acceptance:**
- fatal signals wake tasks blocked in IPC wait states
- blocking `recv`/`reply` no longer prevents `service stop` from completing
- the regression passes because the kernel contract is fixed, not because the harness guesses better

### Task E3 — Land the phase-close docs and version bump

**File:** `docs/54-deep-serverization.md`, `docs/roadmap/54-deep-serverization.md`, `docs/roadmap/README.md`, `docs/roadmap/tasks/README.md`, `docs/evaluation/current-state.md`, `docs/evaluation/microkernel-path.md`, `docs/evaluation/roadmap/R07-deep-serverization.md`, `docs/README.md`, `kernel/Cargo.toml`
**Symbol:** n/a (phase-close documentation pass)
**Why it matters:** Phase 54 is only complete when the shipped system, the
evaluation docs, and the versioned milestone all say the same thing.

**Acceptance:**
- the learning doc exists and is linked from `docs/README.md`
- the roadmap phase and task indexes mark Phase 54 complete
- the evaluation docs no longer describe Phase 54 serverization as only future work
- the kernel version is bumped to `0.54.0`

## Documentation Notes

- Phase 54 is where storage/VFS and UDP policy stop being only future microkernel
  work and become shipped ring-3 services with explicit degraded-mode contracts.
- The most important validation fix was not a harness tweak but a kernel signal
  delivery change that wakes tasks blocked in IPC.
- Smoke-test coverage remains blocked only by the pre-existing TCC hello-world
  failure; the new Phase 54 regression path is green.
