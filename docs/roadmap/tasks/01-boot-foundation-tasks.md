# Phase 01 — Boot Foundation: Task List

**Status:** Complete
**Source Ref:** phase-01
**Depends on:** None
**Goal:** Establish the workspace layout, build tooling, serial output, and panic handling so the kernel can boot in QEMU and print diagnostic messages.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Workspace + target setup | — | ✅ Done |
| B | Kernel entry + serial logging | A | ✅ Done |
| C | Build tooling (xtask) | B | ✅ Done |
| D | Panic path + docs | C | ✅ Done |

---

## Track A — Workspace + Target Setup

### A.1 — Create or verify the workspace layout

**Files:**
- `Cargo.toml`
- `kernel/Cargo.toml`
- `xtask/Cargo.toml`

**Why it matters:** The workspace root must declare `kernel/` and `xtask/` so Cargo can build the OS and host tooling from a single tree.

**Acceptance:**
- [x] Workspace root lists `kernel` and `xtask` as members
- [x] Each crate compiles independently

### A.2 — Configure the OS build target and runner settings

**File:** `.cargo/config.toml`
**Why it matters:** The kernel must target `x86_64-unknown-none` with the red-zone disabled and SIMD off to avoid stack corruption and FPU state issues.

**Acceptance:**
- [x] Default target is `x86_64-unknown-none`
- [x] Runner delegates to `cargo xtask runner`

---

## Track B — Kernel Entry + Serial Logging

### B.1 — Implement a minimal kernel_main entry point

**File:** `kernel/src/main.rs`
**Symbol:** `kernel_main`
**Why it matters:** This is the first code that runs after the bootloader hands off control; it must reach a stable halt loop.

**Acceptance:**
- [x] `entry_point!(kernel_main)` macro registers the entry
- [x] Function enters a halt loop after initialization

### B.2 — Add serial initialization and print macros

**File:** `kernel/src/serial.rs`
**Symbols:** `init`, `serial_print!`, `serial_println!`
**Why it matters:** Serial output is the primary debug channel in headless QEMU; without it there is no visibility into boot progress.

**Acceptance:**
- [x] `serial::init()` configures the UART
- [x] `serial_print!` and `serial_println!` macros write to the serial port

### B.3 — Install a logger backend that writes through serial

**File:** `kernel/src/serial.rs`
**Symbols:** `SerialLogger`, `init_logger`
**Why it matters:** The `log` crate facade lets all kernel code use `log::info!()` etc. without coupling to serial directly.

**Acceptance:**
- [x] `SerialLogger` implements `log::Log`
- [x] `init_logger()` registers the global logger

---

## Track C — Build Tooling (xtask)

### C.1 — Implement cargo xtask image

**File:** `xtask/src/main.rs`
**Why it matters:** Produces a bootable disk image that QEMU (or real hardware) can load via UEFI.

**Acceptance:**
- [x] `cargo xtask image` builds the kernel and produces a bootable image artifact

### C.2 — Implement cargo xtask run

**File:** `xtask/src/main.rs`
**Why it matters:** Provides a one-command workflow to build and launch the OS in QEMU with serial output.

**Acceptance:**
- [x] `cargo xtask run` builds the image and launches QEMU with the expected firmware and serial configuration

---

## Track D — Panic Path + Docs

### D.1 — Add a readable panic handler

**File:** `kernel/src/main.rs`
**Symbol:** `panic`
**Why it matters:** Early boot failures must produce actionable output rather than a silent hang.

**Acceptance:**
- [x] `#[panic_handler]` prints the panic message to serial
- [x] Machine halts cleanly after the panic message

### D.2 — Confirm boot and image output

**Why it matters:** End-to-end validation that the entire boot pipeline works.

**Acceptance:**
- [x] `cargo xtask run` boots and prints a clear startup message
- [x] `cargo xtask image` produces the expected artifact
- [x] Intentional panic produces useful output and the machine halts

### D.3 — Document the boot flow and logging strategy

**Why it matters:** Future contributors need to understand how the build, boot, serial, and panic paths connect.

**Acceptance:**
- [x] Boot flow from `xtask` to `kernel_main` is documented
- [x] Serial logging and panic strategy is documented
- [x] A note explains how mature kernels support more boot modes, logging sinks, and diagnostics

---

## Documentation Notes

- This is the first phase; no changes relative to a previous phase.
- Establishes the foundational build and debug infrastructure used by all subsequent phases.
