# Research: Kernel Boot Foundation

## R1: bootloader crate API (DiskImageBuilder)

**Decision**: Use `bootloader` 0.11 with `DiskImageBuilder::new(kernel_path)` followed by `.create_uefi_image(output_path)`.

**Rationale**: The 0.11 API is the current stable API. `DiskImageBuilder::new` takes a `PathBuf` to the compiled kernel ELF, and `.create_uefi_image(&Path)` produces a GPT disk image with an EFI System Partition containing the bootloader and kernel. This is exactly what `xtask` needs.

**Alternatives considered**:
- `bootloader` 0.9 (older API, uses `bootimage` CLI tool) — rejected, 0.11 is current and uses a Rust API directly.
- Custom UEFI bootloader — rejected, far too complex for Phase 1.

## R2: bootloader_api entry point

**Decision**: Use `bootloader_api` 0.11 with `entry_point!(kernel_main)` macro. Function signature: `fn kernel_main(boot_info: &'static mut BootInfo) -> !`.

**Rationale**: The `entry_point!` macro creates the `_start` symbol and validates the function signature at compile time. Default `BootloaderConfig` is sufficient for Phase 1. Custom config (e.g., stack size) can be added later via `entry_point!(kernel_main, config = &CONFIG)`.

**Alternatives considered**: None — this is the only supported entry mechanism for `bootloader_api`.

## R3: Serial output approach

**Decision**: Use `uart_16550` crate wrapping COM1 at I/O port 0x3F8. Protect with `spin::Mutex` in a `lazy_static!` or `static` with `spin::Once`. Provide `serial_print!` and `serial_println!` macros using `core::fmt::Write`.

**Rationale**: `uart_16550` provides a safe `SerialPort` type with `fmt::Write` impl. The spinlock protects concurrent access (relevant once interrupts are added in Phase 3). QEMU maps `-serial stdio` to COM1 by default.

**Alternatives considered**:
- Raw port I/O via `x86_64::instructions::port` — rejected, `uart_16550` already wraps this correctly.
- `lazy_static!` crate — viable but `spin::Once` is simpler and avoids an extra dependency.

## R4: Log crate integration

**Decision**: Implement a minimal `log::Log` trait on a zero-sized struct that forwards to the serial macros. Set as global logger with `log::set_logger` + `log::set_max_level(LevelFilter::Trace)`.

**Rationale**: The `log` facade is already listed as a project dependency in `docs/08-roadmap.md`. A serial backend is the simplest implementation. Format: `[LEVEL] message`.

**Alternatives considered**: None — `log` is prescribed by the project architecture.

## R5: VHD/VHDX conversion for Hyper-V

**Decision**: Use `qemu-img convert -f raw -O vhdx -o subformat=dynamic <raw_image> <output.vhdx>` invoked from `xtask` via `std::process::Command`.

**Rationale**: No mature Rust crate exists for VHD/VHDX creation. `qemu-img` is the de facto standard, available on all platforms where QEMU is installed. Since QEMU is already a prerequisite, `qemu-img` is guaranteed to be available. The dynamic subformat keeps file size small.

**Alternatives considered**:
- Pure Rust VHD writer — rejected, no maintained crate; VHD format is well-documented but implementing it is out of scope.
- PowerShell `Convert-VHD` — rejected, Windows-only.
- Skip VHD and document manual `qemu-img` usage — viable fallback if automation proves problematic.

## R6: Custom target JSON

**Decision**: Place `x86_64-ostest.json` in `kernel/` directory, referenced by `.cargo/config.toml` as `kernel/x86_64-ostest.json`. Content matches `docs/02-boot.md` exactly.

**Rationale**: The target JSON is already specified in the project documentation. Key flags: `"disable-redzone": true`, `"features": "-mmx,-sse,+soft-float"`, `"panic-strategy": "abort"`.

**Alternatives considered**:
- Using `x86_64-unknown-none` built-in target — rejected, doesn't disable red zone or SIMD by default.

## R7: Panic handler design

**Decision**: `#[panic_handler]` function prints panic info (message, location) via `serial_println!` and enters an `hlt_loop()` that repeatedly executes `hlt` instruction.

**Rationale**: The `PanicInfo` type provides `.message()` and `.location()` methods. Writing to serial is the only output channel in Phase 1. The HLT loop prevents busy-waiting while ensuring the CPU doesn't resume execution.

**Alternatives considered**:
- QEMU debug exit on panic — useful for testing but inappropriate for the default panic handler (would kill QEMU on any panic, even during interactive debugging).
