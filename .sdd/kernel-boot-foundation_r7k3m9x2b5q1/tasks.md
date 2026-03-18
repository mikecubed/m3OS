# Tasks: Kernel Boot Foundation

**Prerequisites**: plan.md ✅, spec.md ✅

**Tests**: Not requested in the specification. Test harness is a future phase (Phase 2+ per roadmap).

**Organization**: Tasks grouped by user story for independent implementation.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3, US4)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Cargo workspace, custom target, and build configuration

- [ ] T001 Create `kernel/Cargo.toml` with dependencies: `bootloader_api = "0.11.15"`, `uart_16550 = "0.4.0"`, `log = "0.4.29"`, `spin = "0.9.8"`, `x86_64 = "0.15.4"` in `/home/mihenderson/Projects/ostest/kernel/Cargo.toml`
- [ ] T002 Create custom target spec `/home/mihenderson/Projects/ostest/kernel/x86_64-ostest.json` with `disable-redzone: true`, `features: "-mmx,-sse,+soft-float"`, `panic-strategy: "abort"`, matching `docs/02-boot.md`
- [ ] T003 [P] Create `xtask/Cargo.toml` with dependencies: `bootloader = "0.11.15"` in `/home/mihenderson/Projects/ostest/xtask/Cargo.toml`
- [ ] T004 Update workspace root `/home/mihenderson/Projects/ostest/Cargo.toml` to include `kernel` and `xtask` members (workspace already exists, just verify members)
- [ ] T005 Verify `.cargo/config.toml` at `/home/mihenderson/Projects/ostest/.cargo/config.toml` has correct `build.target`, `build-std`, and runner config for `x86_64-ostest`

**Checkpoint**: Workspace compiles with `cargo check` (kernel as no_std freestanding, xtask as host binary)

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Serial output infrastructure that ALL user stories depend on

**⚠️ CRITICAL**: No user story work can begin until serial output works

- [ ] T006 Create `/home/mihenderson/Projects/ostest/kernel/src/serial.rs` — initialize `uart_16550::SerialPort` at COM1 (0x3F8), wrap in `spin::Mutex`, expose via `static` global
- [ ] T007 Implement `serial_print!` and `serial_println!` macros in `/home/mihenderson/Projects/ostest/kernel/src/serial.rs` using `core::fmt::Write` trait on the serial port
- [ ] T008 Implement `log::Log` trait on a zero-sized `SerialLogger` struct in `/home/mihenderson/Projects/ostest/kernel/src/serial.rs` — format as `[LEVEL] message`, register as global logger with `log::set_logger` and `log::set_max_level(LevelFilter::Trace)`
- [ ] T009 Create minimal `/home/mihenderson/Projects/ostest/kernel/src/main.rs` with `#![no_std]`, `#![no_main]`, `mod serial;`, `entry_point!(kernel_main)` using `bootloader_api`, and an empty `kernel_main` that calls `hlt_loop()`
- [ ] T010 Implement `hlt_loop()` function in `/home/mihenderson/Projects/ostest/kernel/src/main.rs` — loop that repeatedly calls `x86_64::instructions::hlt()`
- [ ] T011 Implement `#[panic_handler]` in `/home/mihenderson/Projects/ostest/kernel/src/main.rs` — print panic info (message, file, line) via `serial_println!` then call `hlt_loop()`

**Checkpoint**: Kernel compiles for `x86_64-ostest` target. Serial macros and panic handler are ready.

---

## Phase 3: User Story 1 — Boot and Print (Priority: P1) 🎯 MVP

**Goal**: `cargo xtask run` boots the kernel in QEMU and prints `[ostest] Hello from kernel!` to serial

**Independent Test**: Run `cargo xtask run` and observe `[ostest] Hello from kernel!` on stdout (serial)

### Implementation for User Story 1

- [ ] T012 [US1] Implement `xtask` subcommand parser in `/home/mihenderson/Projects/ostest/xtask/src/main.rs` — parse `image`, `run`, and `runner` from `std::env::args`, dispatch to handler functions
- [ ] T013 [US1] Implement `image` subcommand in `/home/mihenderson/Projects/ostest/xtask/src/main.rs` — compile kernel with `cargo build --release --target kernel/x86_64-ostest.json`, then use `bootloader::DiskImageBuilder::new(kernel_path).create_uefi_image(uefi_path)` to produce raw UEFI disk image
- [ ] T014 [US1] Implement `run` subcommand in `/home/mihenderson/Projects/ostest/xtask/src/main.rs` — call `image` then launch `qemu-system-x86_64` with `-bios <OVMF_path> -drive format=raw,file=<image> -serial stdio -display none -no-reboot`
- [ ] T015 [US1] Implement `runner` subcommand in `/home/mihenderson/Projects/ostest/xtask/src/main.rs` — accept kernel binary path from args, build UEFI image from it, launch QEMU (same as `run` but skip cargo build)
- [ ] T016 [US1] Add hello message and log output to `kernel_main` in `/home/mihenderson/Projects/ostest/kernel/src/main.rs` — call `serial::init()`, `serial_println!("[ostest] Hello from kernel!")`, `log::info!("Kernel initialized")`, then `hlt_loop()`

**Checkpoint**: `cargo xtask run` prints `[ostest] Hello from kernel!` to serial and halts cleanly. MVP complete.

---

## Phase 4: User Story 2 — Build Disk Image (Priority: P2)

**Goal**: `cargo xtask image` produces a UEFI-bootable disk image at a predictable output path

**Independent Test**: Run `cargo xtask image` and verify the output file exists and is non-empty

### Implementation for User Story 2

- [ ] T017 [US2] Add output path reporting to `image` subcommand in `/home/mihenderson/Projects/ostest/xtask/src/main.rs` — print the path of the produced disk image to stdout after successful build
- [ ] T018 [US2] Add error handling to `image` subcommand in `/home/mihenderson/Projects/ostest/xtask/src/main.rs` — handle missing kernel binary, failed bootloader build, and missing OVMF with clear error messages

**Checkpoint**: `cargo xtask image` produces a UEFI image and reports its path. Errors are user-friendly.

---

## Phase 5: User Story 3 — Hyper-V Disk Image (Priority: P2)

**Goal**: `cargo xtask image` also produces a VHDX image for Hyper-V Gen 2 VMs

**Independent Test**: Run `cargo xtask image` and verify a `.vhdx` file is produced alongside the raw image

### Implementation for User Story 3

- [ ] T019 [US3] Add VHDX conversion to `image` subcommand in `/home/mihenderson/Projects/ostest/xtask/src/main.rs` — after creating UEFI image, run `qemu-img convert -f raw -O vhdx -o subformat=dynamic <raw_image> <output.vhdx>` via `std::process::Command`
- [ ] T020 [US3] Handle `qemu-img` not found gracefully in `/home/mihenderson/Projects/ostest/xtask/src/main.rs` — print a warning ("VHDX image skipped: qemu-img not found") and continue without failing the build

**Checkpoint**: `cargo xtask image` produces both raw UEFI and VHDX images. Missing `qemu-img` degrades gracefully.

---

## Phase 6: User Story 4 — Panic Handler (Priority: P3)

**Goal**: Panics produce a human-readable message on serial and halt cleanly

**Independent Test**: Temporarily add `panic!("test panic")` in `kernel_main`, run `cargo xtask run`, observe formatted panic message on serial

### Implementation for User Story 4

- [ ] T021 [US4] Enhance `#[panic_handler]` in `/home/mihenderson/Projects/ostest/kernel/src/main.rs` — format panic output as `KERNEL PANIC at {file}:{line}\n  {message}` using `PanicInfo::location()` and `PanicInfo::message()`
- [ ] T022 [US4] Verify panic handler does not itself panic — ensure `serial_println!` does not allocate or use any fallible operations; use `core::fmt::Write` directly on the serial port if the mutex is poisoned/locked

**Checkpoint**: A triggered panic prints file, line, and message to serial, then halts. No double-panic.

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Documentation and cleanup

- [ ] T023 [P] Update `/home/mihenderson/Projects/ostest/docs/README.md` — change "Quick Start (future)" to working instructions now that `cargo xtask run` and `cargo xtask image` work
- [ ] T024 [P] Validate quickstart by following `/home/mihenderson/Projects/ostest/.sdd/kernel-boot-foundation_r7k3m9x2b5q1/quickstart.md` steps end-to-end on a clean build
- [ ] T025 Commit all Phase 1 Foundation files with descriptive commit message

**Checkpoint**: Documentation is accurate, build is clean, everything is committed.

---

## Dependencies & Execution Order

### Phase Dependencies

```
Phase 1: Setup ──────────────> Phase 2: Foundational ──────────> Phase 3: US1 (MVP)
                                                                      │
                                                                      ├──> Phase 4: US2
                                                                      ├──> Phase 5: US3
                                                                      └──> Phase 6: US4
                                                                              │
                                                                              v
                                                                      Phase 7: Polish
```

### User Story Dependencies

- **US1 (Boot and Print)**: Depends on Phase 2 completion. **No dependencies on other stories.**
- **US2 (Build Disk Image)**: Depends on US1 (image subcommand is implemented as part of US1; US2 adds polish)
- **US3 (Hyper-V Image)**: Depends on US2 (extends the image subcommand with VHDX conversion)
- **US4 (Panic Handler)**: Depends on Phase 2 only (panic handler uses serial macros). **Can run in parallel with US1.**

### Within Each User Story

- Foundational serial module must exist before any story
- xtask subcommands build sequentially (image → run → runner)
- Kernel changes (T016, T021) can parallel with xtask changes if serial module is ready

### Parallel Opportunities

- T001 and T003 can run in parallel (kernel/ and xtask/ Cargo.toml creation)
- T006, T007, T008 are sequential (serial port → macros → logger)
- T012–T015 are sequential (xtask subcommands depend on each other)
- US4 (T021–T022) can run in parallel with US2 and US3 after Phase 2 is done
- T023 and T024 (polish) can run in parallel

---

## Parallel Example: Setup Phase

```
# These can run in parallel:
T001: Create kernel/Cargo.toml
T003: Create xtask/Cargo.toml
```

## Parallel Example: After Foundational Phase

```
# US1 and US4 can proceed in parallel:
Stream A: T012 → T013 → T014 → T015 → T016  (US1: boot and print)
Stream B: T021 → T022                         (US4: panic handler)
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001–T005)
2. Complete Phase 2: Foundational (T006–T011)
3. Complete Phase 3: User Story 1 (T012–T016)
4. **STOP and VALIDATE**: Run `cargo xtask run` — see hello message on serial
5. This is a bootable, runnable kernel. MVP achieved.

### Incremental Delivery

1. Setup + Foundational → Kernel compiles
2. US1 → Kernel boots and prints to serial (MVP! 🎯)
3. US2 → Build pipeline polished with error handling
4. US3 → Hyper-V support added
5. US4 → Panic handling hardened
6. Polish → Docs updated, committed

### Note on Scope

This is Phase 1 of a multi-phase OS project. After this task list is complete, the next phases (Memory Management, Interrupts) build on this foundation. The kernel binary, xtask tooling, and serial output established here are used by every subsequent phase.

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Commit after each phase or logical group
- The kernel crate must always compile with `cargo build --target kernel/x86_64-ostest.json` — verify after each kernel change
- OVMF firmware path varies by system; xtask should check common paths or respect `OVMF_PATH` env var
