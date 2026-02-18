# Quickstart: Kernel Boot Foundation

## Prerequisites

- **Rust nightly** toolchain (for `build-std` and custom target)
- **QEMU** with `qemu-system-x86_64` and `qemu-img` in PATH
- **OVMF** UEFI firmware (typically at `/usr/share/OVMF/OVMF_CODE.fd` on Linux, or use the `ovmf-prebuilt` crate)

## Build & Run

```bash
# Build the kernel and create disk images (UEFI raw + VHDX)
cargo xtask image

# Build and launch in QEMU with serial output
cargo xtask run
```

## Expected Output

```
[ostest] Hello from kernel!
[INFO] Kernel initialized
```

QEMU will remain running (kernel is in HLT loop). Press `Ctrl+A, X` to exit QEMU.

## Hyper-V

After `cargo xtask image`, the VHDX image is at:
```
target/x86_64-ostest/release/boot-uefi-ostest.vhdx
```

1. Create a Hyper-V Gen 2 VM
2. Disable Secure Boot in VM settings
3. Attach the VHDX as the boot disk
4. Start the VM

Note: Serial output is not available on Hyper-V Gen 2 by default. The kernel will boot but the hello message won't be visible until Phase 8 (framebuffer).

## Project Layout

```
kernel/src/main.rs    — kernel entry point, panic handler, hlt_loop
kernel/src/serial.rs  — serial port init, macros, log backend
xtask/src/main.rs     — build system: image, run, runner subcommands
```

## Troubleshooting

| Problem | Solution |
|---|---|
| `error: no matching package named bootloader_api` | Ensure Rust nightly is active: `rustup default nightly` |
| QEMU: `Could not load OVMF` | Install OVMF: `sudo apt install ovmf` or set `OVMF_PATH` env var |
| Triple fault on boot | Verify `x86_64-ostest.json` matches `docs/02-boot.md` target spec |
| No serial output | Ensure QEMU is launched with `-serial stdio` (xtask does this automatically) |
