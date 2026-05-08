# Findings: Phases 01–16 (Foundation through Network)

Audit date: 2026-05-07. Scope: design docs (`docs/roadmap/NN-slug.md`) and task docs
(`docs/roadmap/tasks/NN-slug-tasks.md`) for phases 01 through 16. No source code was
read; all findings are based on the docs as written.

---

## Phase 01 — Boot Foundation

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–D, items A.1–D.3).

**Deferred items:**
- "framebuffer output"
- "test harness integration beyond basic smoke testing"
- "real hardware boot support beyond the documented QEMU path"

**Documented shortcuts:**
- Production kernels support many boot environments, richer hardware discovery, multiple
  log sinks, and more defensive boot diagnostics; this phase keeps the boot path narrow
  and explicit.

**Acceptance gaps:** None. All four acceptance criteria are concrete and all task
checkboxes are checked.

**Cross-references to later remediation:** Framebuffer deferred to Phase 09.

**Red flags:** None.

---

## Phase 02 — Memory Basics

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–C, items A.1–C.2).

**Deferred items:**
- "reclaiming freed physical frames"
- "demand paging"
- "sophisticated virtual memory policies"

**Documented shortcuts:**
- Frame allocator is a bump allocator with no free list (explicitly noted as deferred).
- Fixed heap region backed by `linked_list_allocator` rather than a sophisticated slab
  or buddy allocator.
- Task A.2 explicitly notes: "Reclaiming freed physical frames is explicitly deferred
  to later phases."

**Acceptance gaps:** None. Criteria are concrete; all boxes checked.

**Cross-references to later remediation:** Frame reclaim deferred to Phase 17
(Memory Reclamation).

**Red flags:** None.

---

## Phase 03 — Interrupts

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–D, items A.1–D.2).

**Deferred items:**
- "APIC and SMP interrupt routing"
- "advanced driver interrupt models"
- "complex deferred work queues"

**Documented shortcuts:**
- Stays with the simpler PIC path rather than APIC; design doc explicitly states this
  is until "the reader understands exceptions, IRQ acknowledgment, and stack discipline."
- No deferred-work mechanism; interrupt handlers push to ring buffers and return.

**Acceptance gaps:** None.

**Cross-references to later remediation:** APIC-based routing addressed in Phase 15.

**Red flags:** None.

---

## Phase 04 — Tasking

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–D, items A.1–D.2).

**Deferred items:**
- "priorities and deadline scheduling"
- "sleep queues and timers beyond the basic tick"
- "SMP-aware run queues"

**Documented shortcuts:**
- Round-robin FIFO scheduler with no priority support; explicitly chosen for
  teachability.
- Timer-driven preemption implemented as a reschedule flag, not a proper quantum.

**Acceptance gaps:** None. "Scheduler behavior is simple enough to explain from logs"
is slightly qualitative but is paired with concrete observable criteria (interleaving
output, idle task visible).

**Cross-references to later remediation:** SMP-aware run queues in Phase 25;
deadline/priority scheduling deferred to phases beyond scope of this audit.

**Red flags:** None.

---

## Phase 05 — Userspace Entry

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–C, items A.1–C.3).

**Deferred items:**
- "dynamic ELF loading"
- "shared libraries"
- "full process lifecycle management"

**Documented shortcuts:**
- Only a single hardcoded userspace binary at this stage; dynamic ELF loading deferred
  to Phase 11.
- Production kernels have richer executable loading, memory permissions, and security
  policies; this phase uses a tiny single-purpose program.

**Acceptance gaps:** None.

**Cross-references to later remediation:** Full ELF loader in Phase 11; process
lifecycle in Phase 11.

**Red flags:** None.

---

## Phase 06 — IPC Core

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–D, items A.1–D.3).

**Deferred items:**
- "Large page-grant transfers"
- "IPC timeouts and cancellation"
- "Advanced scheduling policies around IPC"

**Documented shortcuts:**
- Capability table is a fixed 64-slot array (not dynamically sized).
- Message holds only 4 × u64 data words; real microkernels have more message registers.
- No formal verification; seL4-style correctness is explicitly noted as out of scope.

**Acceptance gaps:** None.

**Cross-references to later remediation:** Page-grant transfers referenced in Phase 16
(net_server shared memory). IPC completion addressed in Phase 50.

**Red flags:** None.

---

## Phase 07 — Core Servers

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–C).

**Deferred items:**
- "Automatic service restart policies"
- "Complex capability delegation tooling"
- "Dynamic driver loading"
- "Servers running in ring 3 (requires ELF loader from Phase 8+)"
- "String pointers via page grants for ring-3 IPC payloads"
- "Service deregistration"

**Documented shortcuts (explicitly stated in "Implementation Notes" section):**
- All servers (`init_task`, `console_server`, `kbd_server`) run as **ring-0 kernel
  threads**, not ring-3 processes. The design doc explicitly labels this a "deliberate
  scope decision" and explains why.
- String pointers in IPC payloads are kernel addresses — page grants needed for ring-3
  callers (not yet possible at this stage).
- Registry syscalls 9 and 10 are wired but unused (no ring-3 callers yet).
- kbd_server delivers IRQ1 notification and logs bits; scancode reading and forwarding
  key events to clients are explicitly labeled Phase 8+.

**Acceptance gaps (from the design doc's own status table):**
- "Keyboard events flow through `kbd_server`" — documented as **Partial**: "IRQ1 wakes
  `kbd_server` via notification and logs the notification bits; scancode reading and
  client forwarding are Phase 8+."
  - The phase is still marked Complete despite this criterion being only partially met.

**Cross-references to later remediation:** Ring-3 servers become possible after Phase
11 ELF loader. Scancode forwarding to clients is Phase 8+ work. Service restart
policies deferred to later phases (Phase 46 and Phase 51).

**Red flags:**
- The design doc itself marks one acceptance criterion as "Partial" ("Keyboard events
  flow through `kbd_server`") yet the phase is declared Complete. The partial state is
  transparently documented, but the status field does not reflect it.

---

## Phase 08 — Storage and VFS

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–C, items A.1–C.6).

**Deferred items:**
- "Writable filesystems"
- "Page cache and buffering"
- "Permissions and access control"

**Documented shortcuts:**
- VFS is a router, not a full filesystem implementation.
- FAT32 backend is read-only at this stage; write support deferred to Phase 13.
- No caching, buffering, or mutation in this phase.

**Acceptance gaps:** None. "The disk-image content used for demos is documented" is
slightly qualitative, but the task item (C.5) asks for the packaging process to be
documented, which is concrete.

**Cross-references to later remediation:** Write support in Phase 13. Permissions in a
later phase (38 or beyond).

**Red flags:** None.

---

## Phase 09 — Framebuffer and Shell

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–D, items A.1–D.6).

**Deferred items:**
- "Pipes and redirection"
- "Job control"
- "Full-screen applications"
- "Advanced graphics or windowing"

**Documented shortcuts:**
- Fixed bitmap font for text rendering (no vector or TrueType fonts).
- Shell is a minimal command interpreter with only built-in handlers; no fork+exec
  yet (full shell rewrite deferred to Phase 14).
- Console handles only basic ANSI-like sequences; full escape-code support deferred.

**Acceptance gaps:** None.

**Cross-references to later remediation:** Pipes, redirection, and job control in Phase
14. Advanced graphics in Phase 56.

**Red flags:** None.

---

## Phase 10 — Secure Boot Signing (Optional)

**Declared status:** Complete

**Task checkboxes:** Mixed — 2 items unchecked while the phase is declared Complete.

Unchecked items:
- **C.3** (Track C, Validation): "On a real machine with Secure Boot enabled: enroll
  the cert via MOK or UEFI db and confirm boot succeeds" — explicitly marked
  `[ ]` with the note: "Deferred: Requires physical hardware with Secure Boot. Software
  signing and verification are validated; real hardware testing is tracked separately."
- **D.3** (Track D, Documentation): "Update `docs/roadmap/README.md` once real hardware
  validation passes" — explicitly marked `[ ]` with the note: "Deferred: Blocked on
  real hardware boot test (C.3)."

**Deferred items:**
- "Key rotation and revocation."
- "Submitting to the Microsoft UEFI CA for public distribution."
- "Measured boot / TPM attestation (verifying the kernel hash against a TPM PCR)."
- "Signing individual kernel modules (not applicable to a monolithic or microkernel
  design where all trusted code is in the single EFI binary)."

**Documented shortcuts:**
- Real hardware boot test explicitly deferred; only software signing/verification
  (sbsign/sbverify) is validated.
- No shim chain; personal self-signed key enrollment only.

**Acceptance gaps:**
- The design doc acceptance criterion "The kernel boots on real hardware with Secure
  Boot enabled after enrolling the cert" is **not met** — it is blocked on physical
  hardware (C.3 unchecked). The phase is still marked Complete.
- The acceptance criterion "Booting the unsigned or untrusted image with Secure Boot
  on is rejected by firmware" is similarly unverified on real hardware.

**Cross-references to later remediation:** Task D.3 is blocked on C.3; both are
explicitly deferred.

**Red flags:**
- Two task checkboxes are unchecked (`[ ]`) while the phase is marked Complete.
- One acceptance criterion from the design doc — real hardware Secure Boot boot — is
  explicitly not satisfied. The doc notes this clearly but the phase status does not
  reflect the gap.

---

## Phase 11 — ELF Loader and Process Model

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–G, items A.1–G.5).

**Deferred items:**
- "copy-on-write page faults"
- "dynamic linking and `ld.so`"
- "process groups and sessions"
- "`clone` with shared address spaces (threads)"
- "`ptrace` and debugging support"

**Documented shortcuts:**
- Fork uses eager page table copying (no COW); explicitly noted and documented.
- Static linking only at this stage.
- Several Phase 11 items were deferred to Phase 12 and completed there (exception
  handler context switch, execve address space freeing, dead task reaping, SFMASK).
  These are flagged in Phase 12's Track A as "Phase 11 Deferred Cleanup."

**Acceptance gaps:** None. All criteria are concrete and checkboxes are checked.

**Cross-references to later remediation:** COW fork deferred to beyond Phase 16
scope. Dynamic linking deferred (Phase 31+). Threads via `clone` deferred (Phase 40).

**Red flags:** None. (The Phase 11 deferral items that landed in Phase 12 are
transparently documented in Phase 12's Track A.)

---

## Phase 12 — POSIX Compatibility Layer

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–G, items A.1–G.5).

**Deferred items:**
- "`futex` and pthreads"
- "`epoll` / `poll` / `select`"
- "signal delivery through the Linux signal ABI"
- "`/proc` filesystem entries"
- "dynamic linker support (`PT_INTERP`, `LD_LIBRARY_PATH`)"
- "`mprotect` and memory permission changes"

**Documented shortcuts:**
- `getcwd` and `chdir` implemented as initial stubs returning `/` (D.8: "initial stub
  returning `/`").
- `ioctl` implemented only as a TIOCGWINSZ stub (D.9).
- Non-anonymous `mmap` maps are rejected; only `MAP_ANONYMOUS` supported (D.5).
- Only ~40 musl-required syscalls implemented; full Linux ABI is hundreds of calls.

**Acceptance gaps:** None. The "real vs. stubbed syscalls" are explicitly documented in
task G.4.

**Cross-references to later remediation:** `futex`/pthreads in Phase 40; `epoll` in
Phase 37; signal ABI in Phase 19.

**Red flags:** None. Stubbed syscalls are transparently documented.

---

## Phase 13 — Writable Filesystem

**Declared status:** Complete

**Task checkboxes:** TASK DOC DOES NOT EXIST. The README entry reads:
`| 13 | Writable FS | ... | Complete | phase-13 | [Phase 13](./13-writable-fs.md) | *not yet created* |`
No file `docs/roadmap/tasks/13-writable-fs-tasks.md` exists in the repository.

**Deferred items (from design doc):**
- "page cache and write-back buffering"
- "journaling or copy-on-write crash recovery"
- "file permissions and ownership bits"
- "hard links and symbolic links"
- "`mmap` of file-backed pages"
- "extended attributes"

**Documented shortcuts:**
- FAT32 write path writes through immediately with no buffering; design doc explicitly
  accepts the corruption risk: "This phase writes through immediately and accepts the
  corruption risk, which is fine for a single-user development machine running in QEMU."
- tmpfs stores file data as kernel page lists with no disk involvement; `fsync` is a
  no-op on tmpfs.

**Acceptance gaps:**
- No task document exists, so there are no checkboxes to verify against the acceptance
  criteria. The five acceptance criteria in the design doc (tmpfs read/write round-trip,
  FAT32 file survives reboot, `unlink` frees pages, directory operations on both
  backends, FAT32 cluster allocation past EOF) cannot be cross-checked against any task
  completion record.

**Cross-references to later remediation:** Page cache/writeback in Phase 17 and
Phase 33. Journaling deferred. Permissions in Phase 27 and Phase 38.

**Red flags:**
- The task doc is documented in the README as "not yet created" — this is a
  documentation gap. The phase is marked Complete but has no corresponding task
  checklist, making it impossible to verify which acceptance criteria were actually
  satisfied from the roadmap docs alone.

---

## Phase 14 — Shell and Userspace Tools

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–J, items A.1–J.12).

**Deferred items (from task doc "Deferred Until Later" section):**
- "stderr redirection (`2>&1`, `2>file`)"
- "pipelines longer than two stages (`cmd1 | cmd2 | cmd3`)"
- "subshells (`$(...)` and backtick expansion)"
- "here-documents (`<<EOF`)"
- "`trap` built-in (custom signal handlers in shell scripts)"
- "shell scripting (loops, conditionals, functions)"
- "tab completion"
- "glob expansion (`*.txt`)"
- "moving the shell to a userspace ELF binary (currently remains a kernel task)"
- "per-process working directory (cwd is global)"
- "close-on-exec (`O_CLOEXEC` / `FD_CLOEXEC`)"

**Documented shortcuts:**
- Shell remains a **kernel task** (not a ring-3 userspace ELF) at the end of this
  phase; explicitly deferred.
- Per-process working directory is global (`cwd is global`); explicitly deferred.
- Pipe read and write use yield-loops (busy-wait) rather than proper blocking
  (B.4: "Pipe read blocks (yield-loop) if buffer empty"; B.5: "Pipe write blocks if
  buffer full"). This is a correctness/efficiency shortcut.
- `sys_nanosleep` is implemented as a yield-loop tick count (I.13: "yield-loop for tick
  count"), not a real timer-based sleep.
- Only two-stage pipelines supported; longer pipelines deferred.
- `O_CLOEXEC` / `FD_CLOEXEC` not implemented; explicitly deferred.

**Acceptance gaps:** None. All task checkboxes are checked and criteria are concrete.

**Cross-references to later remediation:** Shell moved to userspace ELF in Phase 20.
Per-process cwd likely addressed in Phase 38 or Phase 29. Busy-wait pipe and sleep
addressed in Phase 52c/57 series. `O_CLOEXEC` deferred to Phase 38 or 41.

**Red flags:**
- Pipe blocking is documented as a yield-loop (busy-wait) rather than proper task
  suspension. This is a correctness shortcut that affects all pipe-using programs, but
  it is transparently documented and all checkboxes are checked. Not a red flag per the
  audit criteria (it's a documented shortcut, not a hidden gap), but worth surfacing for
  the remediation tracking.

---

## Phase 15 — Hardware Discovery

**Declared status:** Complete

**Task checkboxes:** All checked (Tracks A–F, items A.1–F.8).

**Deferred items (from task doc "Deferred Until Later" section):**
- "ACPI AML interpreter and dynamic hardware events"
- "PCIe extended config space via MCFG (MMIO-based)"
- "MSI and MSI-X interrupt routing"
- "PCI device power management (D-states)"
- "ACPI S-states (sleep, hibernate)"
- "PCIe hotplug"
- "Application Processor (AP) startup (Phase 25: SMP)"
- "IOMMU / DMA remapping"
- "HPET as timer source"
- "PCI BAR MMIO mapping for specific device drivers (Phase 16: Network)"

**Documented shortcuts:**
- PCI enumeration uses legacy port-I/O (0xCF8/0xCFC) rather than PCIe MMIO config space
  (MCFG). Sufficient for a single QEMU target.
- LAPIC timer calibrated against the PIT; HPET not used.
- IRQ routing uses I/O APIC redirection table; MSI/MSI-X not implemented.
- Only static ACPI descriptor tables parsed; no AML bytecode interpreter.

**Acceptance gaps:** None. All criteria are concrete and checkboxes are checked.

**Cross-references to later remediation:** SMP AP startup in Phase 25. IOMMU in
Phase 55a. MSI/MSI-X deferred to phases beyond this audit scope. HPET remains deferred.

**Red flags:** None.

---

## Phase 16 — Network Stack

**Declared status:** Complete (design doc header)

**Task checkboxes:** The task doc (`16-network-tasks.md`) uses a **table format** (pipe
tables listing task IDs P16-T001 through P16-T073) rather than `[x]`/`[ ]` markdown
checkboxes. No status header is present in the task doc itself (the design doc carries
the "Complete" status). Individual task completion cannot be assessed from the task
doc format — there are no checkboxes, only descriptions.

**Deferred items (from task doc "Deferred Until Later" section):**
- "TCP retransmission timer and congestion control (CUBIC, BBR)"
- "IPv6"
- "DNS resolution"
- "TLS / DTLS"
- "`epoll` / `select` / `poll` for non-blocking socket I/O"
- "Multiple simultaneous TCP connections"
- "Checksum offload via virtio features"
- "DHCP client (static IP configuration only)"
- "Scatter-gather DMA"
- "VLAN tagging"
- "Zero-copy sendmsg/recvmsg"

**Documented shortcuts:**
- TCP has no retransmission timer and no congestion control in the first pass (design
  doc: "no retransmit timer in the first pass").
- Single connection at a time for TCP; multiple simultaneous connections deferred.
- Static IP configuration (10.0.2.15/24, gateway 10.0.2.2); no DHCP.
- UDP checksum may be zero (T036: "checksum may be zero for simplicity").
- net_server approach uses IPC for socket syscall routing — this adds latency compared
  to a kernel-native stack, but is architecturally consistent with the microkernel model.

**Acceptance gaps:**
- The task doc lacks `[x]`/`[ ]` checkboxes entirely. It is structured as a planning
  table rather than a completion checklist. The only testable acceptance items are in
  Track G (T065–T073) but they are described as "Acceptance:" prefixed text in a table
  cell, not as checked boxes.
- There is no "Status:" line in the task doc header. It is impossible to determine from
  the task doc alone whether any of the 73 tasks (T001–T073) were actually completed.

**Cross-references to later remediation:** TCP retransmit in Phase 23 (Socket API) or
later. `epoll`/`poll`/`select` in Phase 37. DHCP deferred. DNS deferred. TLS in Phase
43 (SSH server).

**Red flags:**
- The task doc uses a table format with no checkboxes and no status field. For a phase
  declared Complete, there is no documentary evidence in the task doc of which tasks
  were done. This is a structural gap: the task doc was written as a planning artifact
  and was never converted to a completion record (unlike every other phase 01–15 task
  doc which uses `[x]` checkboxes and explicit Status headers).

---

## Cross-Cutting Observations

1. **Phase 07** — One acceptance criterion ("Keyboard events flow through `kbd_server`")
   is explicitly labeled "Partial" in the design doc's own status table, yet the phase
   is declared Complete. Transparent, but the status field is inconsistent.

2. **Phase 10** — Two task checkboxes are unchecked (`[ ]`) and one acceptance criterion
   (real hardware Secure Boot boot) is explicitly unmet. The phase is declared Complete.
   The design doc is transparent about this, but the status field overstates completion.

3. **Phase 13** — No task doc exists. The README explicitly states "not yet created."
   Phase is marked Complete with no task-level evidence.

4. **Phase 16** — Task doc uses a table format with no checkboxes and no status header.
   Phase is marked Complete but no task-level completion record exists in the docs.

5. **Phases 01–06, 08–09, 11–12, 14–15** — Fully documented with all checkboxes
   checked. Deferred items are transparently listed. No red flags.
