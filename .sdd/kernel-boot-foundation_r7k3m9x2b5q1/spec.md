# Feature Specification: Kernel Boot Foundation

**Created**: 2026-02-18
**Status**: Draft
**Input**: Phase 1 Foundation — boot the kernel via UEFI using the bootloader crate, print a hello message to serial via uart_16550, and halt cleanly. Must use the no_std x86_64 target defined in docs/02-boot.md.

## User Scenarios & Testing

### User Story 1 — Boot and Print (Priority: P1)

As an OS developer, I want to run `cargo xtask run` and see a "Hello from kernel!" message appear on the serial console, so that I know the kernel has booted successfully via UEFI and basic I/O works.

**Why this priority**: This is the absolute foundation — without a working boot path and serial output, no further kernel development is possible. Every subsequent phase depends on this.

**Independent Test**: Can be fully tested by running `cargo xtask run` and observing `[ostest] Hello from kernel!` on serial output (QEMU `-serial stdio`). Delivers proof that the entire boot chain works end-to-end.

**Acceptance Scenarios**:

1. **Given** the project is built with `cargo xtask image`, **When** QEMU launches the resulting disk image, **Then** the kernel entry point is reached without a crash or triple fault.
2. **Given** the kernel has booted, **When** `kernel_main` executes, **Then** the message `[ostest] Hello from kernel!` appears on serial output.
3. **Given** the hello message has been printed, **When** the kernel has no more work to do, **Then** the system halts cleanly (enters an HLT loop) without crashing.

---

### User Story 2 — Build Disk Image (Priority: P2)

As an OS developer, I want to run `cargo xtask image` to produce a bootable UEFI disk image from the kernel binary, so that I have a repeatable build pipeline before adding more functionality.

**Why this priority**: The build tooling must exist before the kernel can be tested, but it delivers less visible value than seeing the kernel actually run.

**Independent Test**: Run `cargo xtask image` and verify a disk image file is produced at the expected output path. The image can be inspected or booted manually in QEMU.

**Acceptance Scenarios**:

1. **Given** the workspace is set up with `kernel/` and `xtask/` crates, **When** `cargo xtask image` is run, **Then** a UEFI-bootable disk image is produced.
2. **Given** the disk image exists, **When** it is launched in QEMU with UEFI firmware, **Then** QEMU boots to the kernel entry point without errors.

---

### User Story 3 — Hyper-V Disk Image (Priority: P2)

As an OS developer, I want `cargo xtask image` to also produce a VHD/VHDX disk image suitable for Hyper-V Gen 2 VMs, so that I can boot and test the kernel on Hyper-V in addition to QEMU.

**Why this priority**: Hyper-V support broadens the testing surface and enables developers on Windows/Hyper-V to participate without installing QEMU, but it is not needed for the primary QEMU-based development workflow.

**Independent Test**: Run `cargo xtask image` and verify a `.vhdx` file is produced alongside the raw disk image. Create a Hyper-V Gen 2 VM using the VHD and confirm UEFI boot reaches the kernel entry point.

**Acceptance Scenarios**:

1. **Given** `cargo xtask image` has been run, **When** the build completes, **Then** a VHD/VHDX disk image is produced in addition to the QEMU raw image.
2. **Given** the VHD/VHDX image exists, **When** it is attached to a Hyper-V Gen 2 VM configured for UEFI boot, **Then** the VM boots to the kernel entry point without errors.
3. **Given** serial is not available on Hyper-V, **When** the kernel boots, **Then** the kernel does not crash due to serial port writes (writes are silently dropped if no hardware responds).

---

### User Story 4 — Panic Handler (Priority: P3)

As an OS developer, I want the kernel to print a meaningful error message to serial and halt when a panic occurs, so that I can diagnose failures during development.

**Why this priority**: Graceful panic handling is essential for debugging but is secondary to the boot path itself working.

**Independent Test**: Trigger a deliberate panic (e.g., `panic!("test")`) in `kernel_main` and observe the formatted panic message on serial output, followed by a clean halt.

**Acceptance Scenarios**:

1. **Given** the kernel is running, **When** a `panic!()` is triggered, **Then** a human-readable panic message including the file name and line number is printed to serial.
2. **Given** a panic has occurred, **When** the panic handler completes output, **Then** the system halts (enters an HLT loop) and does not reboot or triple-fault.

---

### Edge Cases

- What happens if serial hardware is not available (e.g., QEMU launched without `-serial`, or Hyper-V Gen 2 without COM port)? The kernel should not crash; writes to the serial port should be no-ops or silently dropped.
- What happens if the bootloader fails to pass valid `BootInfo`? The kernel entry point receives a well-typed struct from the `bootloader_api` crate; if the bootloader itself fails, QEMU will not reach `kernel_main` (pre-kernel failure).
- What happens if the kernel binary is too large for the bootloader to load? The build will succeed but QEMU will fail to boot. This is acceptable for Phase 1 as the kernel is minimal.

## Requirements

### Functional Requirements

- **FR-001**: The project MUST define a Cargo workspace with `kernel/` and `xtask/` as members.
- **FR-002**: The kernel crate MUST be a freestanding `no_std` binary using the custom `x86_64-ostest` target with red zone disabled, SIMD disabled, and panic strategy set to abort, as defined in `docs/02-boot.md`.
- **FR-003**: The kernel MUST use the `bootloader_api` crate to define an entry point that receives `BootInfo`.
- **FR-004**: The `xtask` crate MUST use the `bootloader` crate's builder API to produce a UEFI-bootable disk image from the compiled kernel binary.
- **FR-005**: The `xtask` crate MUST provide subcommands `image` (build disk image), `run` (build + launch QEMU), and `runner` (QEMU launcher used by cargo runner).
- **FR-005a**: The `xtask image` command MUST also produce a VHD/VHDX disk image suitable for booting in Hyper-V Gen 2 VMs.
- **FR-006**: The kernel MUST initialize a serial port using the `uart_16550` crate and print `[ostest] Hello from kernel!` to serial on boot.
- **FR-007**: The kernel MUST provide `serial_print!` and `serial_println!` macros for formatted serial output.
- **FR-008**: The kernel MUST integrate the `log` crate with a serial backend so that `log::info!()`, `log::warn!()`, etc. write to serial.
- **FR-009**: The kernel MUST define a panic handler that prints the panic info (message, file, line) to serial and enters an HLT loop.
- **FR-010**: After printing the hello message, the kernel MUST enter an idle HLT loop that halts the CPU until the next interrupt, preventing busy-waiting.

### Key Entities

- **Kernel Binary**: The `no_std` freestanding executable compiled for the `x86_64-ostest` target, entered via the `bootloader_api` entry point macro.
- **BootInfo**: Read-only structure passed by the bootloader containing memory regions, framebuffer info, and physical memory offset. Used during initialization only.
- **Serial Port**: The 16550 UART at the standard COM1 I/O port, wrapped by `uart_16550`, providing formatted text output for debugging and logging.
- **Disk Image**: The UEFI-bootable image produced by `xtask` bundling the bootloader and kernel binary together.

## Success Criteria

### Measurable Outcomes

- **SC-001**: Running the build command produces a bootable disk image in under 2 minutes on a typical development machine.
- **SC-002**: The hello message appears on serial output within 5 seconds of QEMU launch.
- **SC-003**: The system remains stable in the HLT loop for at least 60 seconds without crashing, triple-faulting, or rebooting.
- **SC-004**: A triggered panic produces a human-readable message on serial containing the panic source location (file and line number).
- **SC-005**: The `log` crate's `info!` macro produces visible output on serial, confirming the logging backend is functional.

## Assumptions

- QEMU is installed on the development machine and supports UEFI boot via OVMF firmware.
- For Hyper-V testing: a Windows host with Hyper-V enabled and ability to create Gen 2 VMs. `qemu-img` or equivalent tool available for VHD/VHDX conversion.
- The Rust nightly toolchain is available (required for `build-std`, `custom_test_frameworks`, and the custom target JSON).
- The standard COM1 serial port address (0x3F8) is used, which QEMU emulates by default. Hyper-V may not expose COM1; the kernel must not crash in that case.
- The `bootloader` crate v0.11+ is used, which provides both the `bootloader_api` (kernel-side) and `bootloader` (builder-side) crates.
