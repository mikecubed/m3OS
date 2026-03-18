# Contracts: Kernel Boot Foundation

This is a bare-metal OS kernel — there are no HTTP APIs, REST endpoints, or RPC contracts. The "contracts" for Phase 1 are the interfaces between components.

## xtask CLI Contract

The `xtask` binary exposes three subcommands:

### `cargo xtask image`

**Input**: None (discovers kernel binary via Cargo workspace)
**Behavior**:
1. Compile the kernel for `x86_64-ostest` target in release mode
2. Use `DiskImageBuilder::new(kernel_path).create_uefi_image(uefi_path)` to produce raw UEFI image
3. Run `qemu-img convert -f raw -O vhdx -o subformat=dynamic <raw> <vhdx>` to produce Hyper-V image
**Output**:
- `target/x86_64-ostest/release/boot-uefi-ostest.img` (QEMU raw image)
- `target/x86_64-ostest/release/boot-uefi-ostest.vhdx` (Hyper-V image)
**Exit code**: 0 on success, non-zero on failure

### `cargo xtask run`

**Input**: None
**Behavior**:
1. Run `cargo xtask image` to build
2. Launch QEMU with: `qemu-system-x86_64 -bios <OVMF> -drive format=raw,file=<image> -serial stdio -display none`
**Output**: Serial output to stdout
**Exit code**: QEMU exit code

### `cargo xtask runner <kernel_binary>`

**Input**: Path to kernel binary (passed by Cargo runner)
**Behavior**: Same as `run` but uses the provided binary path directly
**Output**: Serial output to stdout
**Exit code**: QEMU exit code

## Kernel Serial Interface

### serial_print! / serial_println!

**Signature**: `serial_print!("{}", args...)` / `serial_println!("{}", args...)`
**Behavior**: Write formatted text to COM1 (0x3F8) via `uart_16550::SerialPort`
**Thread safety**: Protected by `spin::Mutex`
**Failure mode**: Silent (writes to port regardless of whether hardware responds)

### log crate backend

**Levels**: Trace, Debug, Info, Warn, Error
**Format**: `[LEVEL] message\n`
**Output**: Via `serial_println!`

## Kernel Entry Contract

**Symbol**: `_start` (created by `entry_point!` macro)
**Signature**: `fn kernel_main(boot_info: &'static mut BootInfo) -> !`
**Preconditions**: Bootloader has set up page tables, stack, and identity mapping
**Postconditions**: Serial initialized, hello message printed, CPU in HLT loop
**Panic behavior**: Print panic info to serial, enter HLT loop
