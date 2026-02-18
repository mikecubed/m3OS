# Implementation Plan: Kernel Boot Foundation

**Plan**: `kernel-boot-foundation_r7k3m9x2b5q1` | **Date**: 2026-02-18 | **Spec**: `spec.md`

## Summary

Implement Phase 1 of ostest: a bootable Rust microkernel that boots via UEFI using the `bootloader` crate, prints `[ostest] Hello from kernel!` to serial via `uart_16550`, integrates the `log` crate with a serial backend, provides a panic handler, and halts cleanly. The `xtask` build tool produces both a raw UEFI disk image (for QEMU) and a VHD/VHDX image (for Hyper-V). The kernel is a freestanding `no_std` binary compiled for the custom `x86_64-ostest` target.

## Technical Context

**Language/Version**: Rust nightly (requires `build-std`, `custom_test_frameworks`)
**Primary Dependencies**:
  - `bootloader_api` 0.11.15 (kernel-side: entry point macro, `BootInfo` type)
  - `bootloader` 0.11.15 (xtask-side: `DiskImageBuilder` for image creation)
  - `uart_16550` 0.4.0 (serial port driver)
  - `log` 0.4.29 (logging facade)
  - `spin` 0.9.8 (spinlock mutex for `no_std`)
  - `x86_64` 0.15.4 (HLT instruction, port I/O)
**Storage**: N/A (no filesystem in Phase 1)
**Testing**: QEMU-based via `cargo xtask test` (not yet implemented in Phase 1, just the foundation)
**Target Platform**: x86_64 bare-metal, custom target `x86_64-ostest.json` (no_std, no redzone, no SIMD, panic=abort)
**Project Type**: Cargo workspace with two members (`kernel/`, `xtask/`)
**Performance Goals**: Boot to hello message in <5 seconds in QEMU
**Constraints**: `no_std` everywhere in kernel; `unsafe` only at hardware boundaries
**Scale/Scope**: Minimal — ~200 lines of kernel code, ~100 lines of xtask code

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- ✅ Two workspace members (`kernel/`, `xtask/`) — `kernel` is the OS binary, `xtask` is the host build tool. No unnecessary projects.
- ✅ No abstractions beyond what's needed: serial output module, panic handler, entry point. No premature patterns.
- ✅ Custom target JSON matches `docs/02-boot.md` specification exactly.
- ✅ `xtask` is a standard host binary (`std`) — does not share the kernel's `no_std` target.

## Project Structure

```text
ostest/
├── .cargo/
│   └── config.toml              # custom target, build-std, runner
├── Cargo.toml                   # workspace root
├── kernel/
│   ├── Cargo.toml               # depends on bootloader_api, uart_16550, log, spin, x86_64
│   ├── x86_64-ostest.json       # custom target spec
│   └── src/
│       ├── main.rs              # entry_point!, kernel_main, panic handler, hlt_loop
│       └── serial.rs            # SerialPort init, serial_print!/serial_println! macros, log backend
├── xtask/
│   ├── Cargo.toml               # depends on bootloader (builder API)
│   └── src/
│       └── main.rs              # subcommands: image, run, runner
└── docs/                        # existing design documentation
```

**Structure Decision**: Two-crate workspace as prescribed by `docs/02-boot.md`. The kernel is a freestanding binary; xtask is a host tool. No shared library crate needed in Phase 1.
