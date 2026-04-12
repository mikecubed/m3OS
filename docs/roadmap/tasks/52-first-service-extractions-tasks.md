# Phase 52 — First Service Extractions: Task List

**Status:** In Progress
**Source Ref:** phase-52
**Depends on:** Phase 50 (IPC Completion) ✅, Phase 51 (Service Model Maturity) ✅
**Goal:** Move the first visible console and input services into supervised ring-3 processes without faking the boundary, while keeping the remaining kernel-side transition work explicit and measurable.

> **Recreated note:** The original `docs/roadmap/tasks/52-first-service-extractions-tasks.md`
> file is missing from repository history. This replacement was recreated on
> 2026-04-12 from `docs/52-first-service-extractions.md`,
> `docs/roadmap/52-first-service-extractions.md`, the linked roadmap index
> entries, and the current Phase 52 implementation in this tree.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Console service extraction and transitional ownership split | None | In Progress |
| B | Keyboard service extraction and stdin feeder bridge | None | Complete |
| C | Build, ramdisk, and service-manager wiring for extracted services | A, B | Complete |
| D | Restart validation, measurements, and doc closure for Phase 52 | A, B, C | In Progress |

---

## Track A — Console Service Extraction and Transitional Ownership

### A.1 — Stand up a ring-3 console service endpoint

**File:** `userspace/console_server/src/main.rs`
**Symbol:** `program_main`
**Why it matters:** Phase 52 is only meaningful if the console becomes a real userspace service with its own endpoint and registration path instead of remaining a kernel-only concept.

**Acceptance:**
- [x] `console_server` creates an endpoint with `create_endpoint()`
- [x] `console_server` registers itself as `"console"` with `ipc_register_service`
- [x] `console_server` enters a blocking IPC server loop instead of exiting after startup

### A.2 — Preserve console output while the handoff is still hybrid

**Files:**
- `userspace/console_server/src/main.rs`
- `kernel/src/main.rs`

**Symbol:** `server_loop_stdout`, `console_server_task`
**Why it matters:** The current tree still needs login, shell, and fallback output to remain visible while the userspace `console_server` is being brought up; the task doc needs to capture that this handoff is not finished yet.

**Acceptance:**
- [x] `server_loop_stdout` accepts `CONSOLE_WRITE` bulk-data IPC requests and echoes the payload to stdout
- [x] `init_task` still spawns the kernel `console_server_task` so direct `sys_write(STDOUT)` output remains visible during boot and shell use
- [ ] Direct console writes flow exclusively through the `"console"` ring-3 service without the kernel fallback path

---

## Track B — Keyboard Extraction and Stdin Feeder Bridge

### B.1 — Move keyboard IRQ service ownership to ring 3

**File:** `userspace/kbd_server/src/main.rs`
**Symbol:** `program_main`
**Why it matters:** This is the first completed hardware-facing service extraction in Phase 52: IRQ-driven keyboard delivery no longer depends on a kernel-resident `kbd_server_task`.

**Acceptance:**
- [x] `kbd_server` creates and registers the `"kbd"` endpoint in userspace
- [x] `kbd_server` binds itself to IRQ1 with `create_irq_notification(1)`
- [x] `KBD_READ` replies return scancodes after polling `read_kbd_scancode()` and blocking on the notification when the buffer is empty

### B.2 — Bridge keyboard IPC back into the existing stdin path

**Files:**
- `userspace/stdin_feeder/src/main.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs`

**Symbol:** `program_main`, `sys_push_raw_input`
**Why it matters:** Existing userspace still reads stdin through the traditional TTY path, so Phase 52 needs a bridge from the new keyboard IPC service back into the shared kernel line discipline.

**Acceptance:**
- [x] `stdin_feeder` retries lookup of the `"kbd"` service until it becomes available
- [x] Scancodes are translated to bytes or VT100 escape sequences and forwarded with `push_raw_input`
- [x] The kernel `sys_push_raw_input` path remains the single line-discipline path for canonical editing, echo, signals, and ICRNL

### B.3 — Remove the ring-0 keyboard server task from kernel bootstrap

**File:** `kernel/src/main.rs`
**Symbol:** `init_task`
**Why it matters:** The kernel-side extraction claim is only honest if `init_task` no longer pre-registers or spawns the old keyboard service in ring 0.

**Acceptance:**
- [x] `init_task` no longer creates or registers a kernel-owned `"kbd"` endpoint
- [x] `init_task` no longer spawns a ring-0 `kbd_server_task`
- [x] The remaining serial feeder path is clearly separate from the keyboard extraction path

---

## Track C — Build, Ramdisk, and Service-Manager Wiring

### C.1 — Build and embed the extracted service binaries

**Files:**
- `xtask/src/main.rs`
- `kernel/src/fs/ramdisk.rs`

**Symbol:** `build_userspace_bins`, `BIN_ENTRIES`
**Why it matters:** A service extraction is incomplete if the binaries are not part of the normal build and boot image; this is the plumbing that makes the ring-3 services real at boot time.

**Acceptance:**
- [x] `build_userspace_bins` builds `console_server`, `kbd_server`, and `stdin_feeder`
- [x] The generated initrd staging path includes all three binaries during normal builds
- [x] `BIN_ENTRIES` embeds all three services into the boot ramdisk

### C.2 — Wire the extracted services into init's managed service set

**Files:**
- `xtask/src/main.rs`
- `userspace/init/src/main.rs`

**Symbol:** `populate_ext2_files`, `KNOWN_CONFIGS`
**Why it matters:** Phase 52 depends on the Phase 51 service manager for restartability, so the extracted services must be part of the normal managed boot flow rather than ad hoc startup logic.

**Acceptance:**
- [x] `populate_ext2_files` writes `console.conf`, `kbd.conf`, and `stdin_feeder.conf` into `/etc/services.d/`
- [x] The generated service definitions express the intended dependency chain (`kbd` depends on `console`; `stdin_feeder` depends on `console,kbd`)
- [x] `KNOWN_CONFIGS` includes the three Phase 52 service configs so init can still load them without directory scanning support

---

## Track D — Restart Validation, Measurements, and Documentation Closure

### D.1 — Align the learning and evaluation docs with the hybrid implementation

**Files:**
- `docs/52-first-service-extractions.md`
- `docs/07-core-servers.md`
- `docs/09-framebuffer-and-shell.md`
- `docs/evaluation/roadmap/R05-first-service-extractions.md`

**Symbol:** `## What Moved to Userspace`, `## Phase 52 Update: Console and Keyboard Extracted to Ring 3`, `## Phase 52 Update: Framebuffer Rendering Extraction (In Progress)`, `## Implementation Progress (Phase 52)`
**Why it matters:** The docs have to teach the real current architecture: keyboard extraction is live, stdin bridging is live, and console extraction is still a hybrid transition rather than a finished full userspace takeover.

**Acceptance:**
- [x] The learning doc explains what stayed in the kernel and what moved to userspace
- [x] The supporting docs call out the transitional console state instead of claiming a completed kernel removal
- [x] The roadmap and learning-doc indexes link to the Phase 52 docs and this task list

### D.2 — Measure and prove restartable extracted-service behavior before phase closure

**File:** `docs/52-first-service-extractions.md`
**Symbol:** `## Boundary Measurements`
**Why it matters:** Phase 52 is still marked In Progress because the project has not yet closed the loop on restart/reconnect proof and the promised boundary measurements.

**Acceptance:**
- [ ] The boundary-measurement table is filled with real latency and recovery numbers
- [ ] Focused validation shows that `console_server`, `kbd_server`, and `stdin_feeder` can be restarted without requiring a machine reboot
- [ ] Phase 52 can be moved from In Progress to Complete only after the restart proof and measurements are written down

---

## Documentation Notes

- This file is a reconstructed replacement for a missing Phase 52 task doc, not a verbatim recovery.
- The checkboxes reflect the current HEAD implementation state inferred from the surviving Phase 52 docs and code, not an exact historical snapshot of the lost original.
- Phase 52a, 52b, 52c, and 52d remain the authoritative follow-on task docs for the reliability, hardening, architecture-evolution, and closure work that Phase 52 exposed.
