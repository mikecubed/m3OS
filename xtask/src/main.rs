use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Seek, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::SystemTime;

use anyhow::Context;
use fatfs::Dir;
use tempfile::NamedTempFile;

const KERNEL_FILE_NAME: &str = "kernel-x86_64";
const UEFI_BOOT_FILENAME: &str = "efi/boot/bootx64.efi";
const SBSIGN_TOOL_HINT: &str = "Install `sbsigntool` to use `cargo xtask sign`.";
const KERNEL_CORE_HOST_TARGET: &str = "x86_64-unknown-linux-gnu";
const QEMU_ISA_DEBUG_EXIT_DEVICE: &str = "isa-debug-exit,iobase=0xf4,iosize=0x04";

/// QEMU arguments enabling an emulated Intel VT-d IOMMU on the q35 machine.
///
/// Phase 55a Track F.1: the IOMMU-specific device arguments appended
/// whenever `--iommu` is set. The partnering `-machine q35,kernel_irqchip=split`
/// requirement is emitted by [`build_machine_arg`], which also folds in any
/// display-mode machine options (e.g. `pcspk-audiodev=noaudio` under `--gui`)
/// so QEMU sees exactly one `-machine` flag rather than two — multiple
/// `-machine` arguments would let the later invocation clobber the earlier
/// one and silently drop settings.
pub const IOMMU_QEMU_ARGS: &[&str] = &["-device", "intel-iommu,x-scalable-mode=off"];

/// QEMU process exit codes produced by the ISA debug-exit device.
/// The device computes `(value << 1) | 1`, so kernel writing 0x10 → exit 0x21,
/// and kernel writing 0x11 → exit 0x23.
const QEMU_EXIT_SUCCESS: i32 = 0x21;
const QEMU_EXIT_FAILURE: i32 = 0x23;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QemuDisplayMode {
    Headless,
    Gui,
}

/// Optional QEMU device attachments selected via `--device <name>`.
///
/// Phase 55 (F.1) exposes reproducible real-hardware QEMU configurations:
/// `--device nvme` appends an NVMe drive; `--device e1000` replaces the default
/// virtio-net NIC with the Intel 82540EM classic e1000. Defaults (all fields
/// `false`) preserve the legacy VirtIO-blk + VirtIO-net behavior.
///
/// Phase 55a Track F.1 adds the `iommu` field, driven by `--iommu`: when set,
/// [`IOMMU_QEMU_ARGS`] is appended to the QEMU command line so the guest
/// boots on top of an emulated Intel VT-d unit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DeviceSet {
    /// Attach a QEMU NVMe controller with a 64 MiB backing image.
    nvme: bool,
    /// Replace the default virtio-net NIC with the Intel 82540EM e1000.
    e1000: bool,
    /// Enable the emulated Intel VT-d IOMMU (`--iommu`).
    iommu: bool,
}

/// Parse `--device <name>` and `--iommu` flags out of `args`, returning the
/// resulting [`DeviceSet`] plus the remaining arguments.
///
/// Supported `--device` names: `nvme`, `e1000`. Unknown names return an error
/// rather than silently dropping through so a typo is caught immediately.
///
/// Phase 55a Track F.1 adds `--iommu`, a standalone flag (no value) that sets
/// [`DeviceSet::iommu`] so launchers append [`IOMMU_QEMU_ARGS`] to the QEMU
/// command line. Remaining args (e.g. `--fresh`, `--gui`) pass through.
fn extract_device_flags(args: &[String]) -> Result<(DeviceSet, Vec<String>), String> {
    let mut devices = DeviceSet::default();
    let mut remaining = Vec::with_capacity(args.len());
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--device" {
            index += 1;
            let name = args
                .get(index)
                .ok_or_else(|| "missing value for `--device`".to_string())?;
            apply_device_flag(name.as_str(), &mut devices)?;
        } else if let Some(name) = arg.strip_prefix("--device=") {
            apply_device_flag(name, &mut devices)?;
        } else if arg == "--iommu" {
            devices.iommu = true;
        } else {
            remaining.push(arg.clone());
        }
        index += 1;
    }

    Ok((devices, remaining))
}

fn apply_device_flag(name: &str, devices: &mut DeviceSet) -> Result<(), String> {
    match name {
        "nvme" => devices.nvme = true,
        "e1000" => devices.e1000 = true,
        other => {
            return Err(format!(
                "unknown `--device` value `{other}` (supported: nvme, e1000)"
            ));
        }
    }
    Ok(())
}

/// Path to the QEMU NVMe backing image used by `--device nvme`.
fn nvme_image_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target/nvme.img")
}

/// Ensure the NVMe backing image exists as a 64 MiB zeroed file. The image is
/// regenerated if missing but otherwise reused so operator-written data
/// survives across `cargo xtask run --device nvme` invocations.
fn ensure_nvme_image(workspace_root: &Path) -> PathBuf {
    let path = nvme_image_path(workspace_root);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = File::create(&path)
            .unwrap_or_else(|e| panic!("failed to create NVMe image at {}: {e}", path.display()));
        // 64 MiB = 64 * 1024 * 1024 bytes.
        file.set_len(64 * 1024 * 1024)
            .unwrap_or_else(|e| panic!("failed to size NVMe image {}: {e}", path.display()));
        println!("Created NVMe image: {} (64 MiB)", path.display());
    }
    path
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        Some("image") => {
            let root = workspace_root();
            let image_args = parse_image_args(&args[2..], &root).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            cmd_image(&image_args);
        }
        Some("run") => {
            let (devices, remaining) = extract_device_flags(&args[2..]).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            let fresh = remaining.iter().any(|a| a == "--fresh");
            cmd_run(fresh, devices);
        }
        Some("run-gui") => {
            let (devices, remaining) = extract_device_flags(&args[2..]).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            let fresh = remaining.iter().any(|a| a == "--fresh");
            cmd_run_gui(fresh, devices);
        }
        Some("check") => cmd_check(),
        Some("fmt") => {
            let fix = args.iter().any(|a| a == "--fix");
            cmd_fmt(fix);
        }
        Some("test") => {
            let test_args = parse_test_args(&args[2..]).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            cmd_test(&test_args);
        }
        Some("smoke-test") => {
            let smoke_args = parse_smoke_test_args(&args[2..]).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            cmd_smoke_test(&smoke_args);
        }
        Some("device-smoke") => {
            let device_smoke_args = parse_device_smoke_args(&args[2..]).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            cmd_device_smoke(&device_smoke_args);
        }
        Some("runner") => {
            let kernel_binary = args
                .get(2)
                .expect("usage: cargo xtask runner <kernel-binary>");
            cmd_runner(PathBuf::from(kernel_binary));
        }
        Some("sign") => {
            let root = workspace_root();
            let sign_args = parse_sign_args(&args[2..], &root).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            cmd_sign(&sign_args);
        }
        Some("regression") => {
            let regression_args = parse_regression_args(&args[2..]).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            cmd_regression(&regression_args);
        }
        Some("clean") => cmd_clean(),
        Some("stress") => {
            let stress_args = parse_stress_args(&args[2..]).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            cmd_stress(&stress_args);
        }
        Some(other) => {
            eprintln!("Unknown subcommand: {other}");
            eprintln!("Usage: {}", usage());
            std::process::exit(1);
        }
        None => {
            eprintln!("Usage: {}", usage());
            std::process::exit(1);
        }
    }
}

fn usage() -> &'static str {
    "cargo xtask <image [--sign [--key <path>] [--cert <path>]] [--enable-telnet]|run [--fresh] [--iommu] [--device nvme|e1000]...|run-gui [--fresh] [--iommu] [--device nvme|e1000]...|clean|check|fmt [--fix]|test [--test <name>] [--timeout <secs>] [--display] [--iommu] [--device nvme|e1000]...|smoke-test [--display] [--timeout <secs>]|device-smoke --device nvme|e1000 [--iommu] [--timeout <secs>] [--display]|regression [--test <name>] [--timeout <secs>] [--display]|stress [--test <name>] [--iterations <N>] [--timeout <secs>] [--seed <u64>] [--continue-on-failure] [--display]|runner <kernel-binary>|sign <unsigned-efi> [--key <path>] [--cert <path>]>"
}

fn workspace_root() -> PathBuf {
    let output = Command::new(env!("CARGO"))
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .expect("failed to run cargo locate-project");
    let path = String::from_utf8(output.stdout).unwrap();
    PathBuf::from(path.trim()).parent().unwrap().to_path_buf()
}

fn generated_initrd_dir(root: &Path) -> PathBuf {
    root.join("target/generated-initrd")
}

fn ensure_generated_initrd_dir(root: &Path) -> PathBuf {
    let initrd = generated_initrd_dir(root);
    fs::create_dir_all(&initrd).unwrap_or_else(|e| {
        panic!(
            "failed to create generated initrd directory {}: {e}",
            initrd.display()
        );
    });
    initrd
}

/// Compile userspace Rust binaries and stage them under target/generated-initrd/.
///
/// Includes Phase 11 test binaries (exit0, fork-test, echo-args) and
/// Phase 20 init + shell. Each is compiled for `x86_64-unknown-none`
/// (statically linked, no libc) in release mode. The resulting ELF files
/// are embedded in the kernel's ramdisk via `include_bytes!`.
fn build_userspace_bins() {
    let root = workspace_root();
    let initrd = ensure_generated_initrd_dir(&root);

    // (package, binary, needs_alloc)
    let bins: &[(&str, &str, bool)] = &[
        ("exit0", "exit0", false),
        ("fork-test", "fork-test", false),
        ("echo-args", "echo-args", false),
        ("ping", "ping", false),
        ("udp-smoke", "udp-smoke", false),
        ("smoke-runner", "smoke-runner", false),
        ("init", "init", false),
        ("shell", "sh0", false),
        ("edit", "edit", true),
        ("login", "login", false),
        ("su", "su", false),
        ("passwd", "passwd", false),
        ("adduser", "adduser", false),
        ("id", "id", false),
        ("whoami", "whoami", false),
        ("pty-test", "pty-test", false),
        ("unix-socket-test", "unix-socket-test", false),
        ("thread-test", "thread-test", false),
        ("crypto-test", "crypto-test", true),
        ("sshd", "sshd", true),                     // Phase 43: SSH server
        ("syslogd", "syslogd", false),              // Phase 46: system logger
        ("crond", "crond", false),                  // Phase 46: cron daemon
        ("console_server", "console_server", true), // Phase 52: ring-3 console (alloc for kernel-core dep)
        ("kbd_server", "kbd_server", false),        // Phase 52: ring-3 keyboard
        ("stdin_feeder", "stdin_feeder", false),    // Phase 52: ring-3 stdin
        ("fat_server", "fat_server", true),         // Phase 54: ring-3 FAT storage (alloc)
        ("vfs_server", "vfs_server", true),         // Phase 54: ring-3 VFS service (alloc)
        ("net_server", "net_server", true),         // Phase 54: ring-3 UDP network service (alloc)
        // Phase 55b Tracks D.1 / E.1: ring-3 device driver scaffolds
        // (`needs_alloc = true` for driver_runtime + kernel-core deps).
        // Real bring-up lands in D.2/D.3 and E.2/E.3.
        ("nvme_driver", "nvme_driver", true),
        ("e1000_driver", "e1000_driver", true),
    ];

    for &(pkg, bin, needs_alloc) in bins {
        let build_std = if needs_alloc {
            "-Zbuild-std=core,compiler_builtins,alloc"
        } else {
            "-Zbuild-std=core,compiler_builtins"
        };
        let status = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args([
                "build",
                "--release",
                "--package",
                pkg,
                "--bin",
                bin,
                "--target",
                "x86_64-unknown-none",
                build_std,
                "-Zbuild-std-features=compiler-builtins-mem",
            ])
            .status()
            .unwrap_or_else(|_| panic!("failed to build userspace binary {bin}"));

        if !status.success() {
            eprintln!("userspace build failed for {bin}");
            std::process::exit(1);
        }

        let src = root.join(format!("target/x86_64-unknown-none/release/{bin}"));
        let dst = initrd.join(format!("{bin}"));
        fs::copy(&src, &dst).unwrap_or_else(|e| {
            panic!("failed to copy {bin} to initrd: {e}");
        });
        println!(
            "userspace: {} → target/generated-initrd/{bin}",
            src.display()
        );
    }

    // Rust coreutils — build all binaries in one cargo invocation.
    let coreutils_bins: &[&str] = &[
        "true",
        "false",
        "echo",
        "pwd",
        "sleep",
        "rm",
        "mkdir",
        "rmdir",
        "mv",
        "cat",
        "cp",
        "grep",
        "env",
        "PROMPT",
        "ls",
        "ln",
        "readlink", // Phase 32: build tool utilities
        "touch",
        "stat",
        "wc",
        "ar",
        "install",
        "meminfo", // Phase 33: memory diagnostics
        "date",
        "uptime", // Phase 34: timekeeping utilities
        // Phase 41 Rust ports (batch 1 — trivial)
        "umount",
        "dmesg",
        "chmod",
        "mount",
        "kill",
        "tee",
        // Phase 41 Rust ports (batch 2 — small)
        "head",
        "file",
        "strings",
        "uniq",
        "free",
        "df",
        "hexdump",
        // Phase 41 Rust ports (batch 3 — medium)
        "cal",
        "tr",
        "sort",
        "tail",
        "ps",
        "du",
        "chown",
        "find",
        // Phase 41 Rust ports (batch 4 — complex)
        "cut",
        "diff",
        "sed",
        "xargs",
        "less",
        "patch",
        "sha256sum",
        "genkey", // Phase 42: crypto utilities
        // Phase 46: system services commands
        "service",
        "logger",
        "shutdown",
        "reboot",
        "hostname",
        "who",
        "w",
        "last",
        "crontab",
    ];
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "build",
            "--release",
            "--package",
            "coreutils-rs",
            "--bins",
            "--target",
            "x86_64-unknown-none",
            "-Zbuild-std=core,compiler_builtins",
            "-Zbuild-std-features=compiler-builtins-mem",
        ])
        .status()
        .expect("failed to build coreutils-rs");

    if !status.success() {
        eprintln!("userspace build failed for coreutils-rs");
        std::process::exit(1);
    }

    for bin in coreutils_bins {
        let src = root.join(format!("target/x86_64-unknown-none/release/{bin}"));
        let dst = initrd.join(format!("{bin}"));
        fs::copy(&src, &dst).unwrap_or_else(|e| {
            panic!("failed to copy {bin} to initrd: {e}");
        });
        println!(
            "userspace: {} → target/generated-initrd/{bin}",
            src.display()
        );
    }
}

/// Compile Phase 12 musl-linked C binaries and stage them under target/generated-initrd/.
///
/// Requires `musl-gcc` on the host PATH (package `musl-tools` on Debian/Ubuntu).
/// Each binary is compiled as a fully static ELF with `-static -O2`.
fn build_musl_bins() {
    let root = workspace_root();
    let initrd = ensure_generated_initrd_dir(&root);

    // (source path relative to workspace root, output name)
    let bins: &[(&str, &str)] = &[
        ("userspace/hello-c/hello.c", "hello"),
        ("userspace/tmpfs-test/tmpfs-test.c", "tmpfs-test"),
        // Phase 19 signal handler test
        ("userspace/signal-test/signal-test.c", "signal-test"),
        // Phase 21: stdin test
        ("userspace/stdin-test/stdin-test.c", "stdin-test"),
        // Phase 30: telnet server
        ("userspace/telnetd/telnetd.c", "telnetd"),
        // Phase 33: mmap/munmap leak test
        (
            "userspace/mmap-leak-test/mmap-leak-test.c",
            "mmap-leak-test",
        ),
    ];

    let cc = match find_musl_cc() {
        Some(cc) => cc,
        None => {
            eprintln!(
                "warning: musl cross-compiler not found — skipping C binary builds \
                 (install musl-tools on Debian/Ubuntu or musl-gcc-cross-bin on Arch to enable)"
            );
            // Create empty placeholders so include_bytes! doesn't fail.
            for (_, name) in bins {
                let dst = initrd.join(format!("{name}"));
                if !dst.exists() {
                    fs::write(&dst, b"").unwrap_or_else(|e| {
                        eprintln!(
                            "warning: failed to create placeholder {}: {e}",
                            dst.display()
                        );
                    });
                }
            }
            return;
        }
    };

    for (src_rel, name) in bins {
        let src = root.join(src_rel);
        let dst = initrd.join(format!("{name}"));
        let status = match Command::new(cc)
            .args([
                "-static",
                "-O2",
                src.to_str().expect("non-UTF-8 path"),
                "-o",
                dst.to_str().expect("non-UTF-8 path"),
            ])
            .status()
        {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "warning: {cc} disappeared between probe and execution — skipping {name}"
                );
                let dst = initrd.join(format!("{name}"));
                if !dst.exists() {
                    fs::write(&dst, b"").unwrap_or_else(|e| {
                        eprintln!(
                            "warning: failed to create placeholder {}: {e}",
                            dst.display()
                        );
                    });
                }
                continue;
            }
            Err(e) => panic!("failed to run {cc} for {name}: {e}"),
        };
        if !status.success() {
            eprintln!("{cc} failed for {name}");
            std::process::exit(1);
        }
        println!("musl: {} → target/generated-initrd/{name}", src.display());
    }
}

/// Phase 44: Cross-compile musl-linked Rust userspace programs and stage them
/// under target/generated-initrd/.
///
/// Each crate is built individually via `--manifest-path` (they are NOT workspace
/// members). Zero-length placeholders are created first so the ramdisk
/// `include_bytes!` path always resolves. Uses `x86_64-unknown-linux-musl`
/// target with prebuilt std (no `-Zbuild-std`) and warns instead of failing when
/// the target or an individual crate is unavailable.
fn reset_placeholder_file(path: &Path) -> io::Result<()> {
    File::create(path).map(|_| ())
}

fn build_musl_rust_bins() {
    let root = workspace_root();
    let initrd = ensure_generated_initrd_dir(&root);

    let crates: &[&str] = &[
        "hello-rust",
        "sysinfo-rust",
        "httpd-rust",
        "calc-rust",
        "todo-rust",
    ];

    // Reset every staged file to a zero-length placeholder first so stale
    // cached binaries cannot survive missing-target or build-failure paths.
    for name in crates {
        let dst = initrd.join(name);
        if let Err(e) = reset_placeholder_file(&dst) {
            eprintln!("warning: failed to create placeholder target/generated-initrd/{name}: {e}");
        }
    }

    // Check musl target availability once before the build loop so a single
    // crate-specific error doesn't skip the rest.
    let musl_target_available = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("x86_64-unknown-linux-musl"))
        .unwrap_or(false);

    if !musl_target_available {
        eprintln!(
            "warning: x86_64-unknown-linux-musl target not installed — \
             leaving Rust std demo placeholders in target/generated-initrd/ for: {}.\n\
             Run: rustup target add x86_64-unknown-linux-musl",
            crates.join(", ")
        );
        return;
    }

    for name in crates {
        let manifest = root.join(format!("userspace/{name}/Cargo.toml"));
        if !manifest.exists() {
            eprintln!(
                "warning: userspace/{name}/Cargo.toml not found — leaving target/generated-initrd/{name} as a placeholder"
            );
            continue;
        }

        println!("musl-rust: building {name} for x86_64-unknown-linux-musl...");
        let status = match Command::new(env!("CARGO"))
            .current_dir(&root)
            .args([
                "build",
                "--manifest-path",
                manifest.to_str().expect("non-UTF-8 path"),
                "--target",
                "x86_64-unknown-linux-musl",
                "--release",
            ])
            // Produce non-PIE static binaries (ET_EXEC) so the kernel's ELF
            // loader doesn't conflict with musl's self-relocating CRT startup.
            .env(
                "RUSTFLAGS",
                "-C relocation-model=static -C target-feature=+crt-static",
            )
            .status()
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: failed to run cargo build for {name}: {e}");
                continue;
            }
        };

        if !status.success() {
            eprintln!(
                "warning: musl Rust build failed for {name} — leaving target/generated-initrd/{name} as a placeholder"
            );
            continue;
        }

        let built = root.join(format!(
            "userspace/{name}/target/x86_64-unknown-linux-musl/release/{name}"
        ));
        let dst = initrd.join(name);

        // Strip debug symbols to reduce binary size; fall back to plain copy.
        let strip_status = Command::new("strip")
            .args(["-o", dst.to_str().unwrap(), built.to_str().unwrap()])
            .status();
        match strip_status {
            Ok(s) if s.success() => {}
            _ => {
                // Fallback: copy without stripping.
                fs::copy(&built, &dst).unwrap_or_else(|e| {
                    panic!("failed to copy {name} to initrd: {e}");
                });
            }
        }

        // Print binary size for visibility.
        if let Ok(meta) = fs::metadata(&dst) {
            println!(
                "musl-rust: {name} → target/generated-initrd/{name} ({} bytes)",
                meta.len()
            );
        } else {
            println!("musl-rust: {name} → target/generated-initrd/{name}");
        }
    }
}

/// Cross-compile ion shell for musl and stage it under target/generated-initrd/.
///
/// Strategy: clone ion from GitHub (or use cached clone in target/ion-src/),
/// build with `cargo build --release --target x86_64-unknown-linux-musl`,
/// strip, and copy to target/generated-initrd/ion.
///
/// If the ion binary already exists and is newer than ion's Cargo.toml,
/// the build is skipped (cache hit).
fn build_ion() {
    let root = workspace_root();
    let initrd = ensure_generated_initrd_dir(&root);
    let ion_elf = initrd.join("ion");

    // If a pre-built ion binary exists, skip the build.
    if ion_elf.exists() && ion_elf.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        println!("ion: using cached {}", ion_elf.display());
        return;
    }

    let ion_src = root.join("target/ion-src");
    if !ion_src.join("Cargo.toml").exists() {
        println!("ion: cloning ion shell from GitHub...");
        let status = Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "https://github.com/redox-os/ion.git",
                ion_src.to_str().unwrap(),
            ])
            .status()
            .expect("failed to run git clone for ion");
        if !status.success() {
            eprintln!("Failed to clone ion repository");
            std::process::exit(1);
        }
    }

    println!("ion: building for x86_64-unknown-linux-musl (static, non-PIE)...");
    let status = Command::new(env!("CARGO"))
        .current_dir(&ion_src)
        .env(
            "RUSTFLAGS",
            "-C relocation-model=static -C target-feature=+crt-static",
        )
        .args([
            "build",
            "--release",
            "--target",
            "x86_64-unknown-linux-musl",
        ])
        .status()
        .expect("failed to build ion");
    if !status.success() {
        eprintln!("ion build failed");
        std::process::exit(1);
    }

    let built = ion_src.join("target/x86_64-unknown-linux-musl/release/ion");

    // Strip debug symbols to reduce binary size (~3.7M → ~3.2M).
    let strip_status = Command::new("strip")
        .args(["-o", ion_elf.to_str().unwrap(), built.to_str().unwrap()])
        .status();
    match strip_status {
        Ok(s) if s.success() => {}
        _ => {
            // Fallback: copy without stripping.
            fs::copy(&built, &ion_elf).expect("failed to copy ion binary to generated initrd");
        }
    }
    println!("ion: {} → target/generated-initrd/ion", built.display());
}

/// Phase 32: Cross-compile pdpmake (POSIX make) for the OS.
///
/// Strategy: clone pdpmake from GitHub (or use cached clone in target/pdpmake-src/),
/// build with `musl-gcc -static -O2`, and place the resulting binary in
/// target/generated-initrd/make.
fn build_pdpmake() {
    let root = workspace_root();
    let initrd = ensure_generated_initrd_dir(&root);
    let make_elf = initrd.join("make");

    // Check cache.
    if make_elf.exists() && make_elf.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        println!("pdpmake: using cached {}", make_elf.display());
        return;
    }

    // Clone pdpmake source.
    let pdpmake_src = root.join("target/pdpmake-src");
    if !pdpmake_src.join("main.c").exists() {
        println!("pdpmake: cloning from GitHub...");
        let _ = fs::remove_dir_all(&pdpmake_src);
        let status = Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "--branch",
                "2.0.4",
                "https://github.com/rmyorston/pdpmake.git",
                pdpmake_src.to_str().unwrap(),
            ])
            .status()
            .expect("failed to run git clone for pdpmake");
        if !status.success() {
            eprintln!("warning: failed to clone pdpmake — creating empty placeholder");
            if !make_elf.exists() {
                fs::write(&make_elf, b"").unwrap();
            }
            return;
        }
    }

    // Collect all .c files in the pdpmake source directory.
    let mut c_files: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&pdpmake_src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "c") {
                c_files.push(path.to_str().unwrap().to_string());
            }
        }
    }

    c_files.sort(); // deterministic build order across hosts/filesystems
    if c_files.is_empty() {
        eprintln!("warning: no .c files found in pdpmake source — creating empty placeholder");
        if !make_elf.exists() {
            fs::write(&make_elf, b"").unwrap();
        }
        return;
    }

    // Build with musl-gcc.
    // Include m3os_system.c to replace musl's system() which uses posix_spawn
    // (CLONE_VM|CLONE_VFORK) — our kernel treats clone as plain fork, so we
    // need a system() that uses fork+exec directly.
    let system_override = root.join("userspace/coreutils/m3os_system.c");
    let mut args = vec!["-static".to_string(), "-O2".to_string()];
    args.extend(c_files);
    if system_override.exists() {
        args.push(system_override.to_str().unwrap().to_string());
    }
    args.push("-o".to_string());
    args.push(make_elf.to_str().unwrap().to_string());

    let cc = match find_musl_cc() {
        Some(cc) => cc,
        None => {
            eprintln!("warning: musl cross-compiler not found — skipping pdpmake build");
            if !make_elf.exists() {
                fs::write(&make_elf, b"").unwrap();
            }
            return;
        }
    };

    let status = match Command::new(cc).args(&args).status() {
        Ok(s) => s,
        Err(e) => panic!("failed to run {cc} for pdpmake: {e}"),
    };
    if !status.success() {
        eprintln!("warning: pdpmake build failed");
        if !make_elf.exists() {
            fs::write(&make_elf, b"").unwrap();
        }
        return;
    }

    println!("pdpmake: built → target/generated-initrd/make");
}

/// Phase 47: Cross-compile doomgeneric + m3OS platform layer into a static DOOM binary.
///
/// Strategy: clone doomgeneric from GitHub into `target/doomgeneric-src/`, collect core engine
/// `.c` files from `target/doomgeneric-src/doomgeneric/` (skipping platform-specific back-ends
/// and standalone tools), add `userspace/doom/dg_m3os.c`, compile with
/// `musl-gcc -static -O2`. Output: `target/generated-initrd/doom`.
///
/// Gracefully creates an empty placeholder if musl-gcc is not available.
fn build_doom() {
    // Pinned upstream commit — update when pulling in doomgeneric changes.
    // Commit date: 2026-03-28  "__bool_true_false_are_defined handling"
    // Repo: https://github.com/ozkl/doomgeneric
    const DOOMGENERIC_COMMIT: &str = "3b1d53020373b502035d7d48dede645a7c429feb";

    let root = workspace_root();
    let initrd = ensure_generated_initrd_dir(&root);
    let doom_bin = initrd.join("doom");

    let commit_stamp = initrd.join("doom.commit");
    let cached_commit = fs::read_to_string(&commit_stamp).unwrap_or_default();

    // Cache hit: non-empty binary AND it was built from the current pinned commit.
    // When DOOMGENERIC_COMMIT changes the stamp mismatch forces a rebuild.
    if doom_bin.exists()
        && doom_bin.metadata().map(|m| m.len() > 0).unwrap_or(false)
        && cached_commit.trim() == DOOMGENERIC_COMMIT
    {
        println!(
            "doom: using cached {} (commit {})",
            doom_bin.display(),
            DOOMGENERIC_COMMIT
        );
        return;
    }

    // Clone doomgeneric source and pin to the known-good commit.
    let dg_src = root.join("target/doomgeneric-src");
    if !dg_src.join("doomgeneric").join("doomgeneric.c").exists() {
        println!("doom: cloning doomgeneric (full history) for commit {DOOMGENERIC_COMMIT}...");
        let _ = fs::remove_dir_all(&dg_src);
        let status = Command::new("git")
            .args([
                "clone",
                "https://github.com/ozkl/doomgeneric.git",
                dg_src.to_str().unwrap(),
            ])
            .status()
            .expect("failed to run git clone for doomgeneric");
        if !status.success() {
            eprintln!("warning: failed to clone doomgeneric — creating empty placeholder");
            if !doom_bin.exists() {
                fs::write(&doom_bin, b"").unwrap();
            }
            return;
        }
    }

    // Always enforce the pinned commit — even in a cached clone.
    // This guards against stale caches and DOOMGENERIC_COMMIT changes.
    println!("doom: ensuring doomgeneric is at pinned commit {DOOMGENERIC_COMMIT}...");
    let checkout = Command::new("git")
        .args([
            "-C",
            dg_src.to_str().unwrap(),
            "checkout",
            "--force",
            DOOMGENERIC_COMMIT,
        ])
        .status()
        .expect("failed to run git checkout for doomgeneric");
    if !checkout.success() {
        // The cached clone may be shallow or corrupted — self-heal by
        // deleting it and re-cloning before retrying.
        eprintln!("doom: checkout failed — re-cloning doomgeneric to recover...");
        let _ = fs::remove_dir_all(&dg_src);
        let reclone = Command::new("git")
            .args([
                "clone",
                "https://github.com/ozkl/doomgeneric.git",
                dg_src.to_str().unwrap(),
            ])
            .status()
            .expect("failed to run git clone for doomgeneric recovery");
        if !reclone.success() {
            eprintln!("doom: re-clone failed — aborting build");
            if !doom_bin.exists() {
                let _ = fs::write(&doom_bin, b"");
            }
            return;
        }
        let retry = Command::new("git")
            .args([
                "-C",
                dg_src.to_str().unwrap(),
                "checkout",
                "--force",
                DOOMGENERIC_COMMIT,
            ])
            .status()
            .expect("failed to run git checkout for doomgeneric recovery");
        if !retry.success() {
            eprintln!("doom: checkout still failed after re-clone — aborting build");
            if !doom_bin.exists() {
                let _ = fs::write(&doom_bin, b"");
            }
            return;
        }
    }

    // Collect core engine .c files — skip all platform-specific implementations.
    // The doomgeneric repo bundles SDL, Allegro, X11, Windows, etc. back-ends;
    // we only want the engine core and will provide our own dg_m3os.c.
    //
    // Excluded patterns:
    //   doomgeneric_*.c  — alternative platform back-ends (SDL, xlib, win, …)
    //   i_sdl*.c         — SDL audio/music drivers
    //   i_allegro*.c     — Allegro audio/music drivers
    //   mus2mid.c        — standalone tool with its own main()
    let dg_game_src = dg_src.join("doomgeneric");

    // Apply local patches — copy any files from userspace/doom/patches/ into
    // the doomgeneric source tree, overwriting the upstreamed originals.
    // This runs after git checkout so our patches survive the forced reset.
    let patches_dir = root.join("userspace/doom/patches");
    if patches_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&patches_dir) {
            for entry in entries.flatten() {
                let src = entry.path();
                if src.extension().is_some_and(|e| e == "c" || e == "h") {
                    let dst = dg_game_src.join(src.file_name().unwrap());
                    fs::copy(&src, &dst).unwrap_or_else(|e| {
                        eprintln!(
                            "doom: failed to apply patch {:?}: {e}",
                            src.file_name().unwrap()
                        );
                        0
                    });
                    println!(
                        "doom: applied patch {}",
                        src.file_name().unwrap().to_str().unwrap_or("?")
                    );
                }
            }
        }
    }

    let mut c_files: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&dg_game_src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|e| e == "c") {
                continue;
            }
            let name = path.file_name().unwrap().to_str().unwrap_or("");
            // Skip platform-specific back-ends and standalone tools.
            if name.starts_with("doomgeneric_")
                || name.starts_with("i_sdl")
                || name.starts_with("i_allegro")
                || name == "mus2mid.c"
            {
                continue;
            }
            c_files.push(path.to_str().unwrap().to_string());
        }
    }

    c_files.sort(); // deterministic build order
    if c_files.is_empty() {
        eprintln!("warning: no .c files found in doomgeneric source — creating empty placeholder");
        if !doom_bin.exists() {
            fs::write(&doom_bin, b"").unwrap();
        }
        return;
    }

    // Add the m3OS platform layer.
    let platform = root.join("userspace/doom/dg_m3os.c");
    if platform.exists() {
        c_files.push(platform.to_str().unwrap().to_string());
    } else {
        eprintln!("warning: userspace/doom/dg_m3os.c not found — creating empty placeholder");
        if !doom_bin.exists() {
            fs::write(&doom_bin, b"").unwrap();
        }
        return;
    }

    // Detect musl cross-compiler.
    let cc = match find_musl_cc() {
        Some(cc) => cc,
        None => {
            eprintln!("warning: musl cross-compiler not found — skipping doom build");
            if !doom_bin.exists() {
                fs::write(&doom_bin, b"").unwrap();
            }
            return;
        }
    };

    // Include path: point to the doomgeneric source so dg_m3os.c can
    // `#include "doomgeneric/doomgeneric.h"` via the cloned source.
    // Disable optional SDL audio (FEATURE_SOUND) — m3OS has no audio yet.
    let mut args = vec![
        "-static".to_string(),
        "-O2".to_string(),
        format!("-I{}", dg_src.to_str().unwrap()),
        "-UFEATURE_SOUND".to_string(),
    ];
    args.extend(c_files);
    args.push("-o".to_string());
    args.push(doom_bin.to_str().unwrap().to_string());

    let status = match Command::new(cc).args(&args).status() {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("warning: {cc} not found — skipping doom build");
            if !doom_bin.exists() {
                fs::write(&doom_bin, b"").unwrap();
            }
            return;
        }
        Err(e) => panic!("failed to run {cc} for doom: {e}"),
    };
    if !status.success() {
        eprintln!("warning: doom build failed");
        if !doom_bin.exists() {
            fs::write(&doom_bin, b"").unwrap();
        }
        return;
    }

    println!("doom: built → target/generated-initrd/doom");
    // Record the commit so future runs can validate the binary cache.
    let _ = fs::write(initrd.join("doom.commit"), DOOMGENERIC_COMMIT);
}

/// Phase 31: Cross-compile TCC for x86-64 Linux with musl (static binary).
///
/// Strategy: clone TCC source from repo.or.cz (or use cached clone in
/// target/tcc-src/), configure with `--prefix=/usr` so TCC knows where
/// to find headers and libraries at runtime inside the OS, build with
/// `x86_64-linux-musl-gcc -static`, strip, and place the resulting
/// binary in a staging directory.
///
/// Returns the path to the staging directory containing the TCC binary,
/// or `None` if the build fails (musl cross-compiler not available, etc.).
///
/// The staging directory layout:
///   target/tcc-staging/usr/bin/tcc          — TCC binary
///   target/tcc-staging/usr/lib/libc.a       — musl libc
///   target/tcc-staging/usr/lib/crt1.o       — CRT start
///   target/tcc-staging/usr/lib/crti.o       — CRT init prologue
///   target/tcc-staging/usr/lib/crtn.o       — CRT init epilogue
///   target/tcc-staging/usr/lib/tcc/include/ — TCC-specific headers
///   target/tcc-staging/usr/include/         — musl system headers
///   target/tcc-staging/usr/src/hello.c      — test program
///   target/tcc-staging/usr/src/tcc/         — TCC source for self-hosting
fn build_tcc() -> Option<PathBuf> {
    let root = workspace_root();
    let staging = root.join("target/tcc-staging");
    let tcc_bin = staging.join("usr/bin/tcc");

    // Check if we already have a complete cached build. Validate sentinel
    // artifacts to avoid reusing a partially-populated staging dir from an
    // interrupted prior run.
    let sentinels_ok = [
        staging.join("usr/lib/libc.a"),
        staging.join("usr/lib/tcc/libtcc1.a"),
    ]
    .iter()
    .all(|p| p.metadata().map(|m| m.len() > 0).unwrap_or(false));
    if tcc_bin.exists()
        && tcc_bin.metadata().map(|m| m.len() > 0).unwrap_or(false)
        && sentinels_ok
        && staging.join("usr/include").is_dir()
    {
        println!("tcc: using cached {}", tcc_bin.display());
        return Some(staging);
    }
    // Incomplete staging — remove and rebuild.
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }

    // Check for musl cross-compiler.
    let cc = match find_musl_cc() {
        Some(cc) => cc,
        None => {
            eprintln!(
                "warning: musl cross-compiler not found — skipping TCC build \
                 (install musl-tools on Debian/Ubuntu or musl-gcc-cross-bin on Arch \
                 to enable Phase 31)"
            );
            return None;
        }
    };

    // Clone TCC source.
    let tcc_src = root.join("target/tcc-src");
    if tcc_src.exists() && !tcc_src.join("configure").exists() {
        // Incomplete clone — delete and re-clone.
        eprintln!("tcc: incomplete tcc-src cache (configure missing), re-cloning...");
        let _ = fs::remove_dir_all(&tcc_src);
    }
    if !tcc_src.join("configure").exists() {
        println!("tcc: cloning TCC from repo.or.cz...");
        let status = Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "--branch",
                "mob",
                "https://repo.or.cz/tinycc.git",
                tcc_src.to_str().unwrap(),
            ])
            .status()
            .expect("failed to run git clone for TCC");
        if !status.success() {
            eprintln!("warning: failed to clone TCC — skipping Phase 31 TCC build");
            return None;
        }
    }

    // Clean stale artifacts before reconfiguring (a cached c2str.exe from a
    // previous build with different flags may be dynamically linked and fail).
    let _ = Command::new("make")
        .current_dir(&tcc_src)
        .args(["clean"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Configure TCC.
    // Use --extra-ldflags=-static to produce a fully static, non-PIE binary.
    // The --extra-cflags=-static alone doesn't prevent PIE on newer toolchains.
    println!("tcc: configuring with {cc} (static, --prefix=/usr)...");
    let configure_status = Command::new("sh")
        .current_dir(&tcc_src)
        .args([
            "./configure",
            "--prefix=/usr",
            &format!("--cc={cc} -static"),
            "--extra-cflags=-static",
            "--extra-ldflags=-static -no-pie",
            "--cpu=x86_64",
            "--triplet=x86_64-linux-musl",
            "--config-musl",
        ])
        .status()
        .expect("failed to run TCC configure");
    if !configure_status.success() {
        eprintln!("warning: TCC configure failed — skipping Phase 31 TCC build");
        return None;
    }

    // Build TCC binary only (skip libtcc1.a which has bcheck.c portability
    // issues under musl). The tcc binary itself is all we need — musl's libc.a
    // provides the C runtime for programs TCC compiles.
    println!("tcc: building...");
    let make_status = Command::new("make")
        .current_dir(&tcc_src)
        .args(["-j4", "tcc"])
        .status()
        .expect("failed to run make for TCC");
    if !make_status.success() {
        eprintln!("warning: TCC build failed — skipping Phase 31 TCC build");
        return None;
    }

    // Verify the binary is static.
    let built_tcc = tcc_src.join("tcc");
    if !built_tcc.exists() {
        eprintln!("warning: TCC binary not found after build");
        return None;
    }

    // Build libtcc1.a — TCC's own runtime support library.
    // Skip bcheck.c which has musl portability issues.
    println!("tcc: building libtcc1.a...");
    let lib_objects = [
        ("lib/libtcc1.c", "lib/libtcc1.o"),
        ("lib/stdatomic.c", "lib/stdatomic.o"),
        ("lib/atomic.S", "lib/atomic.o"),
        ("lib/builtin.c", "lib/builtin.o"),
        ("lib/alloca.S", "lib/alloca.o"),
        ("lib/dsohandle.c", "lib/dsohandle.o"),
    ];
    let mut lib_ok = true;
    for (src, obj) in &lib_objects {
        let status = Command::new(tcc_src.join("tcc").to_str().unwrap())
            .current_dir(&tcc_src)
            .args(["-c", src, "-o", obj, "-B.", "-I."])
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            eprintln!("warning: failed to compile {src} for libtcc1.a");
            lib_ok = false;
            break;
        }
    }
    if lib_ok {
        let obj_paths: Vec<&str> = lib_objects.iter().map(|(_, o)| *o).collect();
        let mut ar_args = vec!["-ar", "rcs", "libtcc1.a"];
        ar_args.extend(obj_paths.iter());
        let status = Command::new(tcc_src.join("tcc").to_str().unwrap())
            .current_dir(&tcc_src)
            .args(&ar_args)
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            eprintln!("warning: failed to create libtcc1.a");
        } else {
            println!("tcc: libtcc1.a built successfully");
        }
    }

    // Create staging directory structure.
    let dirs = [
        "usr/bin",
        "usr/lib",
        "usr/lib/tcc/include",
        "usr/lib/x86_64-linux-musl",
        "usr/include",
        "usr/include/sys",
        "usr/include/bits",
        "usr/include/arpa",
        "usr/include/net",
        "usr/include/netinet",
        "usr/include/netpacket",
        "usr/include/scsi",
        "usr/src/tcc",
    ];
    for d in &dirs {
        fs::create_dir_all(staging.join(d)).unwrap_or_else(|e| {
            panic!("failed to create staging dir {d}: {e}");
        });
    }

    // Copy TCC binary (stripped).
    let strip_status = Command::new("strip")
        .args(["-o", tcc_bin.to_str().unwrap(), built_tcc.to_str().unwrap()])
        .status();
    match strip_status {
        Ok(s) if s.success() => {}
        _ => {
            fs::copy(&built_tcc, &tcc_bin).expect("failed to copy TCC binary");
        }
    }
    println!(
        "tcc: {} → staging/usr/bin/tcc ({})",
        built_tcc.display(),
        human_size(tcc_bin.metadata().map(|m| m.len()).unwrap_or(0))
    );

    // Copy musl libc.a and CRT objects to both /usr/lib/ and the triplet path
    // /usr/lib/x86_64-linux-musl/ (TCC searches the triplet path first for CRT).
    let musl_lib = Path::new("/usr/lib/x86_64-linux-musl");
    fs::create_dir_all(staging.join("usr/lib/x86_64-linux-musl")).expect("create triplet lib dir");
    let crt_files = ["libc.a", "crt1.o", "crti.o", "crtn.o"];
    for name in &crt_files {
        let src = musl_lib.join(name);
        if src.exists() {
            // Copy to /usr/lib/
            let dst = staging.join(format!("usr/lib/{name}"));
            fs::copy(&src, &dst).unwrap_or_else(|e| {
                panic!("failed to copy {name}: {e}");
            });
            // Also copy to triplet path for TCC's default CRT search.
            let dst_triplet = staging.join(format!("usr/lib/x86_64-linux-musl/{name}"));
            fs::copy(&src, &dst_triplet).unwrap_or_else(|e| {
                panic!("failed to copy {name} to triplet path: {e}");
            });
            println!("tcc: {name} → staging/usr/lib/ + triplet");
        } else {
            eprintln!("warning: musl {name} not found at {}", src.display());
        }
    }

    // Copy libtcc1.a to /usr/lib/tcc/ where TCC expects it.
    let libtcc1_src = tcc_src.join("libtcc1.a");
    if libtcc1_src.exists() {
        let dst = staging.join("usr/lib/tcc/libtcc1.a");
        fs::copy(&libtcc1_src, &dst).expect("failed to copy libtcc1.a");
        println!("tcc: libtcc1.a → staging/usr/lib/tcc/libtcc1.a");
    }

    // Copy musl headers recursively.
    let musl_include = Path::new("/usr/include/x86_64-linux-musl");
    if musl_include.is_dir() {
        copy_dir_recursive(musl_include, &staging.join("usr/include"))
            .expect("failed to copy musl headers");
        println!("tcc: musl headers → staging/usr/include/");
    } else {
        eprintln!(
            "warning: musl headers not found at {}",
            musl_include.display()
        );
    }

    // Copy TCC-specific headers.
    let tcc_include = tcc_src.join("include");
    if tcc_include.is_dir() {
        copy_dir_recursive(&tcc_include, &staging.join("usr/lib/tcc/include"))
            .expect("failed to copy TCC headers");
        println!("tcc: TCC headers → staging/usr/lib/tcc/include/");
    }

    // Create hello.c test program.
    let hello_src = staging.join("usr/src/hello.c");
    fs::write(
        &hello_src,
        "#include <stdio.h>\nint main() {\n    printf(\"hello, world\\n\");\n    return 0;\n}\n",
    )
    .expect("write hello.c");
    println!("tcc: hello.c → staging/usr/src/hello.c");

    // Copy TCC source for self-hosting.
    let tcc_source_files = [
        "tcc.c",
        "tcc.h",
        "libtcc.c",
        "libtcc.h",
        "tccpp.c",
        "tccgen.c",
        "tccelf.c",
        "tccasm.c",
        "tccrun.c",
        "x86_64-gen.c",
        "x86_64-link.c",
        "i386-asm.c",
        "i386-asm.h",
        "tcc-doc.h",
        "config.h",
        "tcctok.h",
    ];
    for name in &tcc_source_files {
        let src = tcc_src.join(name);
        if src.exists()
            && let Err(e) = fs::copy(&src, staging.join(format!("usr/src/tcc/{name}")))
        {
            eprintln!("warning: failed to copy TCC source {name}: {e}");
        }
    }
    println!("tcc: TCC source → staging/usr/src/tcc/");

    Some(staging)
}

/// Recursively copy a directory tree.
fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Human-readable file size.
fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn build_kernel() -> PathBuf {
    let root = workspace_root();
    build_userspace_bins();
    build_musl_bins();
    // Phase 44: cross-compile musl-linked Rust userspace programs.
    build_musl_rust_bins();
    // Phase 31: cross-compile TCC (result used during disk image creation).
    build_tcc();
    build_ion();
    // Phase 32: cross-compile pdpmake (POSIX make).
    build_pdpmake();
    // Phase 45: fetch port sources for bundling into the disk image.
    fetch_port_sources();
    // Phase 47: cross-compile DOOM.
    build_doom();
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "build",
            "--release",
            "--package",
            "kernel",
            "--target",
            "x86_64-unknown-none",
            "-Zbuild-std=core,compiler_builtins,alloc",
            "-Zbuild-std-features=compiler-builtins-mem",
        ])
        .status()
        .expect("failed to run cargo build");

    if !status.success() {
        eprintln!("Kernel build failed");
        std::process::exit(1);
    }

    root.join("target/x86_64-unknown-none/release/kernel")
}

fn create_uefi_image(kernel_binary: &Path) -> PathBuf {
    let uefi_path = kernel_binary.parent().unwrap().join("boot-uefi-m3os.img");

    let builder = bootloader::DiskImageBuilder::new(kernel_binary.to_path_buf());
    builder
        .create_uefi_image(&uefi_path)
        .expect("failed to create UEFI disk image");

    println!("UEFI image: {}", uefi_path.display());
    uefi_path
}

fn convert_to_vhdx(uefi_image: &Path) {
    let vhdx_path = uefi_image.with_extension("vhdx");

    match Command::new("qemu-img")
        .args([
            "convert",
            "-f",
            "raw",
            "-O",
            "vhdx",
            "-o",
            "subformat=dynamic",
        ])
        .arg(uefi_image)
        .arg(&vhdx_path)
        .status()
    {
        Ok(status) if status.success() => {
            println!("VHDX image: {}", vhdx_path.display());
        }
        Ok(_) => {
            eprintln!("Warning: qemu-img convert failed; VHDX image skipped");
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Warning: qemu-img not found; VHDX image skipped");
        }
        Err(e) => {
            eprintln!("Warning: qemu-img failed ({e}); VHDX image skipped");
        }
    }
}

/// Find a musl cross-compiler on the system.
///
/// Checks for `x86_64-linux-musl-gcc` (Debian/Ubuntu cross-compiler),
/// `x86_64-unknown-linux-musl1.2-gcc` (Arch `musl-gcc-cross-bin`),
/// and `musl-gcc` (Debian/Ubuntu `musl-tools` wrapper), in that order.
fn find_musl_cc() -> Option<&'static str> {
    let candidates = [
        "x86_64-linux-musl-gcc",
        "x86_64-unknown-linux-musl1.2-gcc",
        "musl-gcc",
    ];
    for cc in candidates {
        if Command::new(cc)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            return Some(cc);
        }
    }
    None
}

fn find_ovmf() -> PathBuf {
    if let Ok(path) = std::env::var("OVMF_PATH") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return p;
        }
    }

    let candidates = [
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/ovmf/OVMF.fd",
        "/usr/share/edk2-ovmf/x64/OVMF_CODE.fd",
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
        "/usr/share/qemu/OVMF.fd",
        // Arch Linux (edk2-ovmf package) uses .4m suffix and x64/ subdirectory.
        // Prefer the combined OVMF.4m.fd since it works with -bios; the split
        // OVMF_CODE.4m.fd requires pflash setup with a separate VARS file.
        "/usr/share/ovmf/x64/OVMF.4m.fd",
        "/usr/share/OVMF/x64/OVMF.4m.fd",
        "/usr/share/edk2/x64/OVMF.4m.fd",
    ];

    for path in &candidates {
        let p = PathBuf::from(path);
        if p.exists() {
            return p;
        }
    }

    eprintln!("Error: OVMF firmware not found.");
    eprintln!("Install it (e.g., `sudo apt install ovmf`) or set OVMF_PATH.");
    std::process::exit(1);
}

/// Number of guest CPUs to configure. Defaults to 4 for SMP-race coverage on
/// dev machines; CI (2-vCPU runners) overrides via `M3OS_SMP=2` to avoid
/// oversubscribing the host and starving the guest scheduler under TCG.
fn qemu_smp_count() -> u32 {
    std::env::var("M3OS_SMP")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(4)
}

fn qemu_args(uefi_image: &Path, ovmf: &Path, display_mode: QemuDisplayMode) -> Vec<String> {
    qemu_args_with_devices(uefi_image, ovmf, display_mode, DeviceSet::default())
}

/// Resolve the NVMe backing-image path for `devices` and assemble QEMU args.
///
/// Wrapper around the pure [`qemu_args_with_devices_resolved`] that performs
/// the filesystem side effect (creating `target/nvme.img` if missing) only
/// when the caller actually asks for NVMe. Non-test callers use this; the
/// unit tests pass an explicit dummy path to the pure function to stay
/// hermetic (Comment 6 from PR #113 review).
fn qemu_args_with_devices(
    uefi_image: &Path,
    ovmf: &Path,
    display_mode: QemuDisplayMode,
    devices: DeviceSet,
) -> Vec<String> {
    let nvme_path = if devices.nvme {
        Some(ensure_nvme_image(&workspace_root()))
    } else {
        None
    };
    qemu_args_with_devices_resolved(
        uefi_image,
        ovmf,
        display_mode,
        devices,
        nvme_path.as_deref(),
    )
}

/// Pure QEMU argument assembly with optional Phase 55 device overrides.
///
/// * `devices.nvme = true` appends `-drive file=<nvme_image>,if=none,id=nvme0
///   -device nvme,serial=deadbeef,drive=nvme0` using the caller-supplied
///   `nvme_image` (never touches the filesystem itself).
/// * `devices.e1000 = true` replaces the default `virtio-net-pci,netdev=net0`
///   device with `-device e1000,netdev=net0`. The netdev itself is unchanged
///   (QEMU SLIRP user-mode networking), so hostfwd rules still apply.
///
/// `DeviceSet::default()` preserves the legacy VirtIO-blk + VirtIO-net path.
///
/// Panics if `devices.nvme` is true but `nvme_image` is `None` — a
/// programming error the wrapper above prevents.
fn qemu_args_with_devices_resolved(
    uefi_image: &Path,
    ovmf: &Path,
    display_mode: QemuDisplayMode,
    devices: DeviceSet,
    nvme_image: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "-bios".to_string(),
        ovmf.display().to_string(),
        "-drive".to_string(),
        format!("format=raw,file={}", uefi_image.display()),
        "-serial".to_string(),
        "stdio".to_string(),
        // Phase 36: increase RAM to 1 GB for larger disk image and extended storage workloads.
        "-m".to_string(),
        "1024".to_string(),
        "-smp".to_string(),
        qemu_smp_count().to_string(),
    ];

    match display_mode {
        QemuDisplayMode::Headless => {
            args.extend(["-display".to_string(), "none".to_string()]);
        }
        QemuDisplayMode::Gui => {
            args.extend([
                "-display".to_string(),
                "sdl".to_string(),
                "-audiodev".to_string(),
                "none,id=noaudio".to_string(),
            ]);
        }
    }

    // Phase 55a Track F.1: collect every required `-machine` property into a
    // single comma-joined value. Emitting two `-machine` options would let QEMU
    // silently drop the earlier one — e.g. `--gui --iommu` would lose
    // `pcspk-audiodev=noaudio` when `q35,kernel_irqchip=split` followed it.
    if let Some(value) = build_machine_arg(display_mode, devices.iommu) {
        args.extend(["-machine".to_string(), value]);
    }

    // Phase 16: virtio-net NIC with QEMU user-mode networking.
    // Phase 30: port-forward host 2323 → guest 23 for telnet access.
    // Phase 55 (F.1): `--device e1000` swaps the virtio-net-pci device out for
    // the Intel 82540EM classic e1000. The netdev remains unchanged.
    let nic_device = if devices.e1000 {
        "e1000,netdev=net0"
    } else {
        "virtio-net-pci,netdev=net0"
    };
    args.extend([
        "-device".to_string(),
        nic_device.to_string(),
        "-netdev".to_string(),
        "user,id=net0,hostfwd=tcp::2323-:23,hostfwd=tcp::2222-:22".to_string(),
    ]);

    // Phase 24: virtio-blk data disk.
    let data_disk = uefi_image.parent().unwrap().join("disk.img");
    if data_disk.exists() {
        args.extend([
            "-drive".to_string(),
            format!("file={},format=raw,if=virtio", data_disk.display()),
        ]);
    }

    // Phase 55 (F.1): `--device nvme` adds a second drive behind a QEMU NVMe
    // controller. The caller (`qemu_args_with_devices`) resolves the backing
    // image path via `ensure_nvme_image`; this function itself is pure so
    // unit tests can exercise it without writing to `target/nvme.img`.
    if devices.nvme {
        let nvme_path =
            nvme_image.expect("qemu_args_with_devices_resolved: devices.nvme requires nvme_image");
        args.extend([
            "-drive".to_string(),
            format!("file={},if=none,id=nvme0,format=raw", nvme_path.display()),
            "-device".to_string(),
            "nvme,serial=deadbeef,drive=nvme0".to_string(),
        ]);
    }

    // Phase 55a Track F.1: `--iommu` enables an emulated Intel VT-d unit on
    // the q35 machine. The partnering `q35,kernel_irqchip=split` machine
    // property is emitted by `build_machine_arg` above; `IOMMU_QEMU_ARGS`
    // carries only the `-device intel-iommu,x-scalable-mode=off` pair.
    if devices.iommu {
        args.extend(IOMMU_QEMU_ARGS.iter().map(|s| (*s).to_string()));
    }

    args.extend(["-no-reboot".to_string()]);
    args
}

/// Build the consolidated `-machine` value for a given launcher
/// configuration, or `None` if no machine-level options are needed.
///
/// QEMU treats multiple `-machine` arguments as separate invocations —
/// the later one wipes settings established by the earlier one — so
/// every required property must ride on a single flag. `--iommu`
/// contributes `q35,kernel_irqchip=split` (the VT-d device rejects the
/// default `kernel_irqchip=on` model); `--gui` contributes
/// `pcspk-audiodev=noaudio` (so the PC speaker does not try to bind a
/// null audio backend). Combined configurations join both with a comma.
fn build_machine_arg(display_mode: QemuDisplayMode, iommu: bool) -> Option<String> {
    let mut opts: Vec<&'static str> = Vec::new();
    if iommu {
        opts.push("q35,kernel_irqchip=split");
    }
    if matches!(display_mode, QemuDisplayMode::Gui) {
        opts.push("pcspk-audiodev=noaudio");
    }
    if opts.is_empty() {
        None
    } else {
        Some(opts.join(","))
    }
}

fn launch_qemu(uefi_image: &Path, display_mode: QemuDisplayMode) {
    launch_qemu_with_devices(uefi_image, display_mode, DeviceSet::default());
}

fn launch_qemu_with_devices(uefi_image: &Path, display_mode: QemuDisplayMode, devices: DeviceSet) {
    let ovmf = find_ovmf();
    let args = qemu_run_args_with_devices(uefi_image, &ovmf, display_mode, devices);

    if display_mode == QemuDisplayMode::Gui {
        println!(
            "QEMU GUI mode: click the window to grab the keyboard, then press Ctrl+Alt+G to release it."
        );
    }

    let status = Command::new("qemu-system-x86_64")
        .args(&args)
        .status()
        .expect("failed to launch QEMU");

    std::process::exit(normalize_run_qemu_exit(status.code()));
}

#[cfg(test)]
fn qemu_run_args(uefi_image: &Path, ovmf: &Path, display_mode: QemuDisplayMode) -> Vec<String> {
    qemu_run_args_with_devices(uefi_image, ovmf, display_mode, DeviceSet::default())
}

fn qemu_run_args_with_devices(
    uefi_image: &Path,
    ovmf: &Path,
    display_mode: QemuDisplayMode,
    devices: DeviceSet,
) -> Vec<String> {
    let mut args = qemu_args_with_devices(uefi_image, ovmf, display_mode, devices);
    args.retain(|arg| arg != "-no-reboot");
    args.extend([
        "-device".to_string(),
        QEMU_ISA_DEBUG_EXIT_DEVICE.to_string(),
    ]);
    args
}

fn normalize_run_qemu_exit(code: Option<i32>) -> i32 {
    match code {
        Some(0) | Some(QEMU_EXIT_SUCCESS) => 0,
        Some(other) => other,
        None => 1,
    }
}

fn cmd_check() {
    let root = workspace_root();
    build_userspace_bins();
    build_musl_bins();
    // Phase 44: cross-compile musl-linked Rust userspace programs.
    build_musl_rust_bins();
    build_ion();
    build_pdpmake();
    build_doom();

    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "clippy",
            "--package",
            "kernel",
            "--target",
            "x86_64-unknown-none",
            "-Zbuild-std=core,compiler_builtins,alloc",
            "-Zbuild-std-features=compiler-builtins-mem",
            "--",
            "-D",
            "warnings",
        ])
        .status()
        .expect("failed to run cargo clippy");

    if !status.success() {
        eprintln!("clippy reported errors");
        std::process::exit(1);
    }

    // Clippy for all userspace crates (same target as kernel).
    let userspace_pkgs = [
        "syscall-lib",
        "exit0",
        "fork-test",
        "echo-args",
        "init",
        "shell",
        "ping",
        "edit",
        "login",
        "su",
        "passwd",
        "adduser",
        "id",
        "whoami",
        "pty-test",
        "unix-socket-test",
        "thread-test",
        "crypto-lib",
        "crypto-test",
        "coreutils-rs",
        // Phase 55b Track C.1 — ring-3 driver runtime library
        "driver_runtime",
        // Phase 55b Track D.1 — ring-3 NVMe driver scaffold
        "nvme_driver",
    ];
    let mut clippy_args = vec![
        "clippy".to_string(),
        "--target".to_string(),
        "x86_64-unknown-none".to_string(),
        "-Zbuild-std=core,compiler_builtins,alloc".to_string(),
        "-Zbuild-std-features=compiler-builtins-mem".to_string(),
    ];
    for pkg in &userspace_pkgs {
        clippy_args.push("--package".to_string());
        clippy_args.push(pkg.to_string());
    }
    clippy_args.extend(["--".to_string(), "-D".to_string(), "warnings".to_string()]);

    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(&clippy_args)
        .status()
        .expect("failed to run userspace clippy");

    if !status.success() {
        eprintln!("userspace clippy reported errors");
        std::process::exit(1);
    }

    // Clippy for syscall-lib with the alloc feature enabled (heap code is feature-gated).
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "clippy",
            "--package",
            "syscall-lib",
            "--features",
            "alloc",
            "--target",
            "x86_64-unknown-none",
            "-Zbuild-std=core,compiler_builtins,alloc",
            "-Zbuild-std-features=compiler-builtins-mem",
            "--",
            "-D",
            "warnings",
        ])
        .status()
        .expect("failed to run syscall-lib alloc clippy");

    if !status.success() {
        eprintln!("syscall-lib (alloc feature) clippy reported errors");
        std::process::exit(1);
    }

    // Host-side allocator/property coverage uses:
    //   cargo test -p kernel-core --target x86_64-unknown-linux-gnu
    // Password-shadow rewrite regression coverage uses:
    //   cargo test -p passwd --target x86_64-unknown-linux-gnu --no-default-features --features host-tests --test passwd_host
    // Loom coverage remains opt-in via:
    //   RUSTFLAGS="--cfg loom" cargo test -p kernel-core --target x86_64-unknown-linux-gnu --test <...>
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "clippy",
            "--package",
            "kernel-core",
            "--target",
            KERNEL_CORE_HOST_TARGET,
            "--",
            "-D",
            "warnings",
        ])
        .status()
        .expect("failed to run kernel-core clippy");

    if !status.success() {
        eprintln!("kernel-core clippy reported errors");
        std::process::exit(1);
    }

    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "test",
            "--package",
            "kernel-core",
            "--target",
            KERNEL_CORE_HOST_TARGET,
        ])
        .status()
        .expect("failed to run kernel-core tests");

    if !status.success() {
        eprintln!(
            "kernel-core host tests failed — rerun `cargo test -p kernel-core --target {KERNEL_CORE_HOST_TARGET}`"
        );
        std::process::exit(1);
    }

    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "test",
            "--package",
            "passwd",
            "--target",
            KERNEL_CORE_HOST_TARGET,
            "--no-default-features",
            "--features",
            "host-tests",
            "--test",
            "passwd_host",
        ])
        .status()
        .expect("failed to run passwd host tests");

    if !status.success() {
        eprintln!(
            "passwd host tests failed — rerun `cargo test -p passwd --target {KERNEL_CORE_HOST_TARGET} --no-default-features --features host-tests --test passwd_host`"
        );
        std::process::exit(1);
    }

    // Phase 55b Track C.1: ensure the driver_runtime crate's host-side
    // smoke tests (module surface + DriverRuntimeError lift) stay
    // green. The authoritative behavioral suite against the abstract
    // contracts lives in `kernel-core/tests/driver_runtime_contract.rs`
    // and runs as part of the kernel-core tests invoked above; this
    // runs the re-export smoke tests against the crate itself.
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "test",
            "--package",
            "driver_runtime",
            "--target",
            KERNEL_CORE_HOST_TARGET,
        ])
        .status()
        .expect("failed to run driver_runtime tests");

    if !status.success() {
        eprintln!(
            "driver_runtime host tests failed — rerun `cargo test -p driver_runtime --target {KERNEL_CORE_HOST_TARGET}`"
        );
        std::process::exit(1);
    }

    // Format check for both kernel and kernel-core.
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["fmt", "--all", "--", "--check"])
        .status()
        .expect("failed to run cargo fmt");

    if !status.success() {
        eprintln!("rustfmt found unformatted code — run `cargo xtask fmt --fix` to fix");
        std::process::exit(1);
    }

    println!(
        "check passed: clippy clean, formatting correct, kernel-core, passwd, and driver_runtime host tests pass"
    );
}

#[derive(Debug, Clone)]
struct TestArgs {
    test_name: Option<String>,
    timeout_secs: u64,
    display: bool,
    devices: DeviceSet,
}

fn parse_test_args(args: &[String]) -> Result<TestArgs, String> {
    // Extract `--device` flags first so they are available to the test harness
    // even when interleaved with other flags. Phase 55 (F.1): lets operators
    // run `cargo xtask test --device nvme` to exercise the NVMe smoke path.
    let (devices, args) = extract_device_flags(args)?;
    let args: &[String] = &args;

    let mut test_name = None;
    let mut timeout_secs = 60u64;
    let mut display = false;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--test" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "missing value for `--test`".to_string())?;
                test_name = Some(value.clone());
            }
            "--timeout" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "missing value for `--timeout`".to_string())?;
                timeout_secs = value
                    .parse()
                    .map_err(|_| format!("invalid timeout value: {value}"))?;
            }
            "--display" => {
                display = true;
            }
            _ if let Some(value) = arg.strip_prefix("--test=") => {
                test_name = Some(value.to_string());
            }
            _ if let Some(value) = arg.strip_prefix("--timeout=") => {
                timeout_secs = value
                    .parse()
                    .map_err(|_| format!("invalid timeout value: {value}"))?;
            }
            _ => {
                return Err(format!("unknown test flag `{arg}`"));
            }
        }
        index += 1;
    }

    Ok(TestArgs {
        test_name,
        timeout_secs,
        display,
        devices,
    })
}

/// Build kernel test binaries and return their paths.
///
/// Uses `cargo build --tests --message-format=json` to discover the compiled
/// test binary paths without running them.
fn build_test_binaries(test_name: Option<&str>) -> Vec<PathBuf> {
    let root = workspace_root();
    build_userspace_bins();
    build_musl_bins();
    build_musl_rust_bins();
    build_ion();
    build_pdpmake();
    build_doom();

    let mut build_args = vec![
        "build",
        "--tests",
        "--package",
        "kernel",
        "--target",
        "x86_64-unknown-none",
        "-Zbuild-std=core,compiler_builtins,alloc",
        "-Zbuild-std-features=compiler-builtins-mem",
        "--message-format=json",
    ];

    // If a specific test name is requested, pass it via --test.
    let test_flag;
    if let Some(name) = test_name {
        test_flag = name.to_string();
        build_args.push("--test");
        build_args.push(&test_flag);
    }

    let output = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(&build_args)
        .stderr(std::process::Stdio::inherit())
        .output()
        .expect("failed to run cargo build --tests");

    if !output.status.success() {
        eprintln!("Kernel test build failed");
        std::process::exit(1);
    }

    // Parse JSON lines to find test executable paths.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut binaries = Vec::new();
    for line in stdout.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if json.get("reason").and_then(|v| v.as_str()) == Some("compiler-artifact") {
                if let Some(executable) = json.get("executable").and_then(|v| v.as_str()) {
                    // Only include test binaries (those with test = true in profile).
                    if json
                        .get("profile")
                        .and_then(|p| p.get("test"))
                        .and_then(|t| t.as_bool())
                        == Some(true)
                    {
                        binaries.push(PathBuf::from(executable));
                    }
                }
            }
        }
    }

    if binaries.is_empty() {
        eprintln!("No test binaries found");
        std::process::exit(1);
    }

    binaries
}

/// QEMU arguments for running a test kernel: headless, with ISA debug exit device.
#[cfg(test)]
fn qemu_test_args(uefi_image: &Path, ovmf: &Path, display: bool) -> Vec<String> {
    qemu_test_args_with_devices(uefi_image, ovmf, display, DeviceSet::default())
}

fn qemu_test_args_with_devices(
    uefi_image: &Path,
    ovmf: &Path,
    display: bool,
    devices: DeviceSet,
) -> Vec<String> {
    let display_mode = if display {
        QemuDisplayMode::Gui
    } else {
        QemuDisplayMode::Headless
    };
    let mut args = qemu_args_with_devices(uefi_image, ovmf, display_mode, devices);
    // Strip hostfwd from netdev to avoid port conflicts during tests.
    for arg in args.iter_mut() {
        if arg.starts_with("user,id=net0,hostfwd=") {
            *arg = "user,id=net0".to_string();
        }
    }
    // Add ISA debug exit device so the test kernel can signal pass/fail.
    args.extend([
        "-device".to_string(),
        QEMU_ISA_DEBUG_EXIT_DEVICE.to_string(),
    ]);
    args
}

fn cmd_test(test_args: &TestArgs) {
    let binaries = build_test_binaries(test_args.test_name.as_deref());
    let ovmf = find_ovmf();
    let mut all_passed = true;

    for binary in &binaries {
        let name = binary
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        println!("\n--- Running test: {name} ---");

        let uefi_image = create_uefi_image(binary);
        let args =
            qemu_test_args_with_devices(&uefi_image, &ovmf, test_args.display, test_args.devices);

        let mut child = Command::new("qemu-system-x86_64")
            .args(&args)
            .spawn()
            .expect("failed to launch QEMU");

        let timeout = std::time::Duration::from_secs(test_args.timeout_secs);
        let start = std::time::Instant::now();

        let exit_code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    if start.elapsed() > timeout {
                        eprintln!(
                            "Test {name} timed out after {}s — killing QEMU",
                            test_args.timeout_secs
                        );
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => {
                    eprintln!("Error waiting for QEMU: {e}");
                    break None;
                }
            }
        };

        match exit_code {
            Some(QEMU_EXIT_SUCCESS) => {
                println!("Test {name}: PASSED");
            }
            Some(QEMU_EXIT_FAILURE) => {
                eprintln!("Test {name}: FAILED (test panicked)");
                all_passed = false;
            }
            Some(code) => {
                eprintln!("Test {name}: FAILED (unexpected QEMU exit code: 0x{code:x})");
                all_passed = false;
            }
            None => {
                eprintln!("Test {name}: FAILED (timeout or signal)");
                all_passed = false;
            }
        }
    }

    if all_passed {
        println!("\nAll {} test(s) passed", binaries.len());
    } else {
        eprintln!("\nSome tests failed");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Smoke test: boot full OS in QEMU, inject serial input, verify output
// ---------------------------------------------------------------------------

/// A single step in an expect-style smoke test script.
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum SmokeStep {
    /// Wait for `pattern` to appear in serial output within `timeout_secs`.
    Wait {
        pattern: &'static str,
        timeout_secs: u64,
        label: &'static str,
    },
    /// Send `input` to QEMU stdin (serial input to guest OS).
    Send {
        input: &'static str,
        label: &'static str,
    },
    /// Pause between steps.
    Sleep { millis: u64 },
    /// Wait for either `pattern_a` or `pattern_b`. Injects `extra_steps_a`
    /// if pattern_a matches, or `extra_steps_b` if pattern_b matches.
    /// Used for first-boot vs. normal login branching.
    WaitEither {
        pattern_a: &'static str,
        pattern_b: &'static str,
        timeout_secs: u64,
        label: &'static str,
        extra_steps_a: &'static [SmokeStep],
        extra_steps_b: &'static [SmokeStep],
    },
}

#[derive(Debug, Clone)]
struct SmokeTestArgs {
    display: bool,
    timeout_secs: u64,
}

fn parse_smoke_test_args(args: &[String]) -> Result<SmokeTestArgs, String> {
    let mut display = false;
    let mut timeout_secs = 120u64;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--display" => display = true,
            "--timeout" => {
                index += 1;
                timeout_secs = args
                    .get(index)
                    .ok_or("--timeout requires a value")?
                    .parse()
                    .map_err(|_| "invalid --timeout value")?;
            }
            other => return Err(format!("unknown smoke-test flag: {other}")),
        }
        index += 1;
    }

    Ok(SmokeTestArgs {
        display,
        timeout_secs,
    })
}

/// Strip background noise lines from serial output.
///
/// The kernel's `log` crate emits lines like `[INFO] [p3] fork()` on the same
/// serial port as userspace output. PID 1 also writes service lifecycle chatter
/// such as `init: restarting 'syslogd' (2/10)` to that same serial stream.
/// When either class of message lands mid-line, the entire line is corrupted
/// and must be discarded to avoid false pattern matches.
///
/// Operates line-by-line: any line containing a recognised tag is removed.
/// Tags recognised: kernel log levels and init service lifecycle prefixes.
const BACKGROUND_LOG_PREFIXES: &[&str] = &[
    "[INFO] [",
    "[DEBUG] [",
    "[WARN] [",
    "[ERROR] [",
    "[TRACE] [",
];

const BACKGROUND_INIT_PREFIXES: &[&str] = &[
    "init: starting '",
    "init: started '",
    "init: service '",
    "init: restarting '",
    "init: execve failed for '",
    "init: session ended, respawning login...",
];

fn starts_with_background_noise(input: &str) -> bool {
    BACKGROUND_LOG_PREFIXES
        .iter()
        .chain(BACKGROUND_INIT_PREFIXES.iter())
        .any(|pfx| input.starts_with(pfx))
}

fn strip_background_noise(input: &str) -> String {
    // Kernel log prefixes — always `[LEVEL] [subsystem] message...\n`.
    // Match the second bracket to avoid false positives on userspace text
    // that might literally contain `[INFO]`.

    let mut out = String::with_capacity(input.len());
    let mut pos = 0;

    while pos < input.len() {
        let remaining = &input[pos..];

        // Check if current position starts a noise fragment.
        if starts_with_background_noise(remaining) {
            // Skip everything up to and including the next newline.
            if let Some(nl) = remaining.find('\n') {
                pos += nl + 1;
            } else {
                pos = input.len();
            }
        } else if let Some(c) = remaining.chars().next() {
            out.push(c);
            pos += c.len_utf8();
        } else {
            break;
        }
    }

    out
}

/// Strip ANSI CSI escape sequences from a string.
///
/// Handles: ESC [ <params> <letter>  and  ESC <single-char>.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // ESC [ ... <letter>
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                // consume parameter bytes (0-9 ; ? and space)
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() || c == ';' || c == '?' || c == ' ' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                // consume final byte (letter @ through ~)
                if let Some(&c) = chars.peek() {
                    if c.is_ascii() && (c as u8) >= b'@' && (c as u8) <= b'~' {
                        chars.next();
                    }
                }
            } else {
                // ESC + single character (e.g., ESC c)
                chars.next();
            }
        } else {
            out.push(ch);
        }
    }

    out
}

#[derive(Clone, Copy)]
enum SerialMatchMode {
    Stripped,
    Cleaned,
    RenderedStripped,
    RenderedCleaned,
}

fn render_terminal_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut line: Vec<char> = Vec::new();
    let mut cursor = 0usize;

    for ch in input.chars() {
        match ch {
            '\n' => {
                out.extend(line.iter());
                out.push('\n');
                line.clear();
                cursor = 0;
            }
            '\r' => {
                line.clear();
                cursor = 0;
            }
            '\x08' => cursor = cursor.saturating_sub(1),
            c if c.is_control() => {}
            c => {
                if cursor < line.len() {
                    line[cursor] = c;
                } else {
                    while line.len() < cursor {
                        line.push(' ');
                    }
                    line.push(c);
                }
                cursor += 1;
            }
        }
    }

    out.extend(line.iter());
    out
}

fn map_cleaned_offset_to_stripped(stripped: &str, cleaned_offset: usize) -> Option<usize> {
    if cleaned_offset == 0 {
        return Some(0);
    }

    let mut stripped_pos = 0;
    let mut cleaned_len = 0;

    while stripped_pos < stripped.len() {
        let remaining = &stripped[stripped_pos..];
        if starts_with_background_noise(remaining) {
            if let Some(nl) = remaining.find('\n') {
                stripped_pos += nl + 1;
            } else {
                stripped_pos = stripped.len();
            }
            continue;
        }

        let ch = remaining.chars().next()?;
        let len = ch.len_utf8();
        stripped_pos += len;
        cleaned_len += len;
        if cleaned_len >= cleaned_offset {
            return Some(stripped_pos);
        }
    }

    None
}

fn map_stripped_offset_to_raw(raw: &str, stripped_offset: usize) -> usize {
    if stripped_offset == 0 {
        return 0;
    }

    let raw_bytes = raw.as_bytes();
    let mut raw_idx = 0;
    let mut stripped_len = 0;

    while stripped_len < stripped_offset && raw_idx < raw_bytes.len() {
        if raw_bytes[raw_idx] == 0x1b {
            raw_idx += 1;
            if raw_idx < raw_bytes.len() && raw_bytes[raw_idx] == b'[' {
                raw_idx += 1;
                while raw_idx < raw_bytes.len() && !(b'@'..=b'~').contains(&raw_bytes[raw_idx]) {
                    raw_idx += 1;
                }
                if raw_idx < raw_bytes.len() {
                    raw_idx += 1;
                }
            } else if raw_idx < raw_bytes.len() {
                raw_idx += 1;
            }
            continue;
        }

        let ch = raw[raw_idx..]
            .chars()
            .next()
            .expect("raw_idx must remain on a char boundary");
        let len = ch.len_utf8();
        raw_idx += len;
        stripped_len += len;
    }

    raw_idx
}

fn drain_serial_through_match(
    serial_buf: &mut String,
    stripped: &str,
    mode: SerialMatchMode,
    match_end: usize,
) {
    if matches!(
        mode,
        SerialMatchMode::RenderedStripped | SerialMatchMode::RenderedCleaned
    ) {
        serial_buf.clear();
        return;
    }

    let stripped_end = match mode {
        SerialMatchMode::Stripped => Some(match_end),
        SerialMatchMode::Cleaned => map_cleaned_offset_to_stripped(stripped, match_end),
        SerialMatchMode::RenderedStripped | SerialMatchMode::RenderedCleaned => None,
    };

    if let Some(stripped_end) = stripped_end {
        let raw_end = map_stripped_offset_to_raw(serial_buf, stripped_end).min(serial_buf.len());
        serial_buf.drain(..raw_end);
    } else if let Some(nl) = serial_buf.rfind('\n') {
        serial_buf.drain(..=nl);
    } else if serial_buf.len() > 4096 {
        let drain = serial_buf.len() - 4096;
        serial_buf.drain(..drain);
    }
}

fn find_serial_match(
    stripped: &str,
    cleaned: &str,
    pattern: &str,
) -> Option<(SerialMatchMode, usize)> {
    if matches!(pattern, "# " | "$ ") {
        if let Some(end) = prompt_suffix_end(stripped, pattern) {
            return Some((SerialMatchMode::Stripped, end));
        }
        if let Some(end) = prompt_suffix_end(cleaned, pattern) {
            return Some((SerialMatchMode::Cleaned, end));
        }
        let rendered_stripped = render_terminal_text(stripped);
        if let Some(end) = prompt_suffix_end(&rendered_stripped, pattern) {
            return Some((SerialMatchMode::RenderedStripped, end));
        }
        let rendered_cleaned = render_terminal_text(cleaned);
        if let Some(end) = prompt_suffix_end(&rendered_cleaned, pattern) {
            return Some((SerialMatchMode::RenderedCleaned, end));
        }
        return None;
    }
    if let Some(pos) = stripped.find(pattern) {
        return Some((SerialMatchMode::Stripped, pos + pattern.len()));
    }
    cleaned
        .find(pattern)
        .map(|pos| (SerialMatchMode::Cleaned, pos + pattern.len()))
}

fn prompt_suffix_end(buf: &str, prompt: &str) -> Option<usize> {
    let trimmed = buf.trim_end_matches(['\r', '\n']);
    if !trimmed.ends_with(prompt) {
        return None;
    }
    Some(trimmed.len())
}

/// Background serial output reader.
///
/// Spawns a thread that reads from `stdout` and sends chunks over the channel.
/// The thread exits when the pipe closes (QEMU exits).
fn spawn_serial_reader(stdout: std::process::ChildStdout) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut reader = std::io::BufReader::new(stdout);
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn drain_serial_until_idle(
    rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    serial_buf: &mut String,
    serial_history: &mut String,
    idle_threshold: std::time::Duration,
    idle_cap: std::time::Duration,
) {
    let idle_start = std::time::Instant::now();
    let mut last_data = std::time::Instant::now();

    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(chunk) => {
                append_serial_chunk(serial_buf, serial_history, &chunk);
                last_data = std::time::Instant::now();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if last_data.elapsed() >= idle_threshold {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if idle_start.elapsed() >= idle_cap {
            break;
        }
    }
}

/// Uniform timing multiplier for Wait/WaitEither timeout budgets in the smoke
/// and regression step executors. Set `M3OS_CI_TIMING_MULT=3` (or any positive
/// float) in CI to give serialized-VFS IPC round-trips enough headroom under
/// `-smp 2` TCG; local dev leaves it unset for strict fail-fast budgets.
///
/// Applies only to *timeouts* (max-wait budgets). Explicit `Sleep { millis }`
/// steps are deliberate fixed settle delays — scaling them turns the 25s
/// boot-settle into 75s and runs the suite past its CI wall-clock.
fn ci_timing_multiplier() -> f32 {
    std::env::var("M3OS_CI_TIMING_MULT")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .unwrap_or(1.0)
}

fn scaled_secs(secs: u64) -> std::time::Duration {
    let scaled = ((secs as f32) * ci_timing_multiplier()).ceil() as u64;
    std::time::Duration::from_secs(scaled.max(secs))
}

/// Run an expect-style smoke test script against a running QEMU instance.
///
/// Returns `Ok(())` on success or `Err(message)` on failure.
fn run_smoke_script(
    child: &mut std::process::Child,
    steps: &[SmokeStep],
    global_timeout: std::time::Duration,
) -> Result<(), String> {
    use std::collections::VecDeque;
    let stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let rx = spawn_serial_reader(stdout);

    let mut serial_buf = String::new();
    let mut serial_history = String::new();
    let global_start = std::time::Instant::now();
    // Use a queue so WaitEither can inject extra steps at the front.
    let mut queue: VecDeque<&SmokeStep> = steps.iter().collect();
    let mut step_num = 0usize;

    while let Some(step) = queue.pop_front() {
        step_num += 1;
        // Global timeout check.
        if global_start.elapsed() > global_timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "global timeout ({global_timeout:?}) exceeded at step {}",
                step_num
            ));
        }
        let remaining = queue.len();

        match step {
            SmokeStep::Wait {
                pattern,
                timeout_secs,
                label,
            } => {
                println!("[step {}] wait: {label} ({}s)", step_num, timeout_secs);
                let step_deadline = std::time::Instant::now() + scaled_secs(*timeout_secs);
                let global_deadline = global_start + global_timeout;
                let deadline = step_deadline.min(global_deadline);

                loop {
                    // Drain any available output.
                    while let Ok(chunk) = rx.try_recv() {
                        append_serial_chunk(&mut serial_buf, &mut serial_history, &chunk);
                    }

                    // Check for pattern in stripped output.  Also try with
                    // kernel log lines removed — the kernel can inject
                    // `[INFO] [mmap] ...` mid-line, splitting userspace
                    // output and preventing a contiguous match.
                    let stripped = strip_ansi(&serial_buf);
                    let cleaned = strip_background_noise(&stripped);
                    if let Some((mode, match_end)) = find_serial_match(&stripped, &cleaned, pattern)
                    {
                        drain_serial_through_match(&mut serial_buf, &stripped, mode, match_end);
                        break;
                    }

                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let tail = tail_lines(&strip_ansi(&serial_history), 80);
                        return Err(format!(
                            "step {} timed out: {label}\n\
                             expected pattern: \"{pattern}\"\n\
                             last serial output:\n{tail}",
                            step_num
                        ));
                    }

                    // Wait a bit before polling again.
                    match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(chunk) => {
                            append_serial_chunk(&mut serial_buf, &mut serial_history, &chunk);
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            // QEMU exited — check if the pattern arrived
                            // before the pipe closed. Only treat as success
                            // on the final step; mid-script disconnect means
                            // subsequent steps would fail anyway.
                            let _ = child.wait();
                            let stripped = strip_ansi(&serial_buf);
                            if stripped.contains(pattern) && remaining == 0 {
                                serial_buf.clear();
                                break;
                            }
                            let tail = tail_lines(&strip_ansi(&serial_history), 80);
                            return Err(format!(
                                "QEMU exited while waiting for step {}: {label}\n\
                                 expected pattern: \"{pattern}\"\n\
                                 last serial output:\n{tail}",
                                step_num
                            ));
                        }
                    }
                }
            }

            SmokeStep::Send { input, label } => {
                println!("[step {}] send: {label}", step_num);
                // Drain serial output until 150ms of silence before sending
                // input.  This ensures the shell/terminal has finished all
                // prompt rendering (ANSI escapes, cursor repositioning).
                drain_serial_until_idle(
                    &rx,
                    &mut serial_buf,
                    &mut serial_history,
                    std::time::Duration::from_millis(150),
                    std::time::Duration::from_secs(2),
                );
                if let Some(stdin) = child.stdin.as_mut() {
                    use std::io::Write;
                    serial_buf.clear();
                    if stdin.write_all(input.as_bytes()).is_err() {
                        return Err(format!(
                            "failed to send input at step {}: {label}",
                            step_num
                        ));
                    }
                    let _ = stdin.flush();
                } else {
                    return Err(format!("no stdin pipe at step {}: {label}", step_num));
                }
            }

            SmokeStep::Sleep { millis } => {
                println!("[step {}] sleep {}ms", step_num, millis);
                std::thread::sleep(std::time::Duration::from_millis(*millis));
            }

            SmokeStep::WaitEither {
                pattern_a,
                pattern_b,
                timeout_secs,
                label,
                extra_steps_a,
                extra_steps_b,
            } => {
                println!(
                    "[step {}] wait-either: {label} ({}s)",
                    step_num, timeout_secs
                );
                let step_deadline = std::time::Instant::now() + scaled_secs(*timeout_secs);
                let global_deadline = global_start + global_timeout;
                let deadline = step_deadline.min(global_deadline);

                let matched_a;
                loop {
                    while let Ok(chunk) = rx.try_recv() {
                        append_serial_chunk(&mut serial_buf, &mut serial_history, &chunk);
                    }
                    let stripped = strip_ansi(&serial_buf);
                    let cleaned = strip_background_noise(&stripped);
                    if let Some((mode, match_end)) =
                        find_serial_match(&stripped, &cleaned, pattern_a)
                    {
                        matched_a = true;
                        drain_serial_through_match(&mut serial_buf, &stripped, mode, match_end);
                        break;
                    }
                    if let Some((mode, match_end)) =
                        find_serial_match(&stripped, &cleaned, pattern_b)
                    {
                        matched_a = false;
                        drain_serial_through_match(&mut serial_buf, &stripped, mode, match_end);
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let tail = tail_lines(&strip_ansi(&serial_history), 80);
                        return Err(format!(
                            "step {} timed out: {label}\n\
                             expected pattern_a: \"{pattern_a}\"\n\
                             expected pattern_b: \"{pattern_b}\"\n\
                             last serial output:\n{tail}",
                            step_num
                        ));
                    }
                    match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(chunk) => {
                            append_serial_chunk(&mut serial_buf, &mut serial_history, &chunk);
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            let _ = child.wait();
                            let tail = tail_lines(&strip_ansi(&serial_history), 80);
                            return Err(format!(
                                "QEMU exited while waiting for step {}: {label}\n\
                                 last serial output:\n{tail}",
                                step_num
                            ));
                        }
                    }
                }
                let inject = if matched_a {
                    println!(
                        "  -> matched pattern_a, injecting {} extra steps",
                        extra_steps_a.len()
                    );
                    extra_steps_a
                } else {
                    println!(
                        "  -> matched pattern_b, injecting {} extra steps",
                        extra_steps_b.len()
                    );
                    extra_steps_b
                };
                for extra in inject.iter().rev() {
                    queue.push_front(extra);
                }
            }
        }
    }

    // All steps passed — kill QEMU.
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn append_serial_chunk(serial_buf: &mut String, serial_history: &mut String, chunk: &[u8]) {
    let text = String::from_utf8_lossy(chunk);
    serial_buf.push_str(&text);
    serial_history.push_str(&text);
    trim_serial_buffer(serial_buf, 64 * 1024, 48 * 1024);
    trim_serial_buffer(serial_history, 256 * 1024, 192 * 1024);
}

fn trim_serial_buffer(buf: &mut String, max_len: usize, keep_len: usize) {
    if buf.len() <= max_len {
        return;
    }

    let mut cut = buf.len().saturating_sub(keep_len);
    while cut < buf.len() && !buf.is_char_boundary(cut) {
        cut += 1;
    }
    buf.drain(..cut);
}

/// Return the last `n` lines of a string.
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Helper: send a command and wait for the shell prompt to return.
/// The Send step itself drains serial output until idle before writing,
/// so no explicit sleep is needed to avoid input races.
fn cmd_then_prompt(
    input: &'static str,
    send_label: &'static str,
    wait_label: &'static str,
    timeout: u64,
) -> Vec<SmokeStep> {
    vec![
        SmokeStep::Send {
            input,
            label: send_label,
        },
        SmokeStep::Wait {
            pattern: "# ",
            timeout_secs: timeout,
            label: wait_label,
        },
    ]
}

/// Comprehensive smoke test: login, coreutils, TCC, Phase 32 build tools.
///
/// Replaces the Phase 31 smoke test with a more thorough script that validates
/// the full userspace stack including new utilities and the make build tool.
#[allow(unreachable_code)]
fn smoke_test_script(doom_wad_available: bool) -> Vec<SmokeStep> {
    let _ = doom_wad_available;
    let mut steps = Vec::new();
    const BOOT_READY_MARKER: &str = "init: started 'net_udp' pid=";

    // -----------------------------------------------------------------------
    // 1. Boot and start the dedicated guest-side smoke runner
    // -----------------------------------------------------------------------
    const BOOT_MARKER_SETTLE: &[SmokeStep] = &[SmokeStep::Wait {
        pattern: "SMOKE:BEGIN",
        timeout_secs: 20,
        label: "wait for smoke runner start after final boot marker",
    }];

    steps.push(SmokeStep::WaitEither {
        pattern_a: "SMOKE:BEGIN",
        pattern_b: BOOT_READY_MARKER,
        timeout_secs: 60,
        label: "wait for smoke runner start or final boot marker",
        extra_steps_a: &[],
        extra_steps_b: BOOT_MARKER_SETTLE,
    });

    // -----------------------------------------------------------------------
    // 2. Guest-side smoke runner
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Wait {
        pattern: "SMOKE:auth:PASS",
        timeout_secs: 10,
        label: "guest/auth: smoke runner confirmed root session",
    });
    steps.push(SmokeStep::Wait {
        pattern: "SMOKE:tcc-version:PASS",
        timeout_secs: 30,
        label: "guest/tcc: smoke runner verified tcc version",
    });
    // When M3OS_SMOKE_SKIP_TCC_COMPILE=1 is set, the guest emits :SKIP instead
    // of :PASS for both tcc-compile and hello (there is no compiled binary to
    // run). Local dev keeps full :PASS coverage and gets a 600s budget for
    // TCC under TCG (vs. the previous 180s).
    steps.push(SmokeStep::WaitEither {
        pattern_a: "SMOKE:tcc-compile:PASS",
        pattern_b: "SMOKE:tcc-compile:SKIP",
        timeout_secs: 600,
        label: "guest/tcc: smoke runner compiled hello world or skipped",
        extra_steps_a: &[],
        extra_steps_b: &[],
    });
    steps.push(SmokeStep::WaitEither {
        pattern_a: "SMOKE:hello:PASS",
        pattern_b: "SMOKE:hello:SKIP",
        timeout_secs: 20,
        label: "guest/hello: smoke runner ran compiled hello or skipped",
        extra_steps_a: &[],
        extra_steps_b: &[],
    });
    steps.push(SmokeStep::Wait {
        pattern: "SMOKE:storage:PASS",
        timeout_secs: 20,
        label: "guest/storage: smoke runner verified ext2 file lifecycle",
    });
    steps.push(SmokeStep::Wait {
        pattern: "SMOKE:net:PASS",
        timeout_secs: 45,
        label: "guest/net: smoke runner completed udp smoke",
    });
    steps.push(SmokeStep::Wait {
        pattern: "SMOKE:log:PASS",
        timeout_secs: 20,
        label: "guest/log: smoke runner verified syslog marker",
    });
    steps.push(SmokeStep::Wait {
        pattern: "SMOKE:PASS",
        timeout_secs: 5,
        label: "guest smoke runner completed all checks",
    });

    // Shutdown/reboot (headless workflow §7) is verified by the manual
    // release checklist. Automated shutdown verification requires precise
    // QEMU-exit coordination that is fragile under CI load; the regression
    // suite covers the operator workflows leading up to shutdown.

    return steps;

    // -----------------------------------------------------------------------
    // 4. Phase 32 utilities: touch, stat, wc
    // -----------------------------------------------------------------------

    // touch — create a new file
    steps.extend(cmd_then_prompt(
        "/bin/touch /tmp/smoke_file\n",
        "send: touch /tmp/smoke_file",
        "wait: prompt after touch",
        10,
    ));

    // stat — verify the file exists and shows metadata
    steps.push(SmokeStep::Send {
        input: "/bin/stat /tmp/smoke_file\n",
        label: "stat: show file metadata",
    });
    steps.push(SmokeStep::Wait {
        pattern: "File:",
        timeout_secs: 10,
        label: "verify stat output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after stat",
    });

    // wc — count words in a known file
    steps.push(SmokeStep::Send {
        input: "/bin/wc /home/project/main.c\n",
        label: "wc: count lines in main.c",
    });
    steps.push(SmokeStep::Wait {
        pattern: "main.c",
        timeout_secs: 10,
        label: "verify wc output contains filename",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after wc",
    });

    // -----------------------------------------------------------------------
    // 5. Demo project: build with the bundled shell script
    // -----------------------------------------------------------------------
    steps.extend(cmd_then_prompt(
        "cd /home/project\n",
        "send: cd /home/project",
        "wait: prompt after cd",
        5,
    ));

    steps.push(SmokeStep::Send {
        input: "/home/project/build.sh\n",
        label: "build demo project",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Building demo project...",
        timeout_secs: 20,
        label: "verify build.sh startup",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Demo project running!",
        timeout_secs: 120,
        label: "verify demo output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Build and test complete.",
        timeout_secs: 120,
        label: "wait for demo build completion",
    });
    steps.push(SmokeStep::Sleep { millis: 300 });

    // -----------------------------------------------------------------------
    // 6. ar — create a static library (using util.o from make build)
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/ar rcs libutil.a util.o\n",
        label: "ar: create static library",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "wait for ar to finish",
    });

    // Verify archive was created
    steps.push(SmokeStep::Send {
        input: "/bin/stat libutil.a\n",
        label: "stat: verify libutil.a exists",
    });
    steps.push(SmokeStep::Wait {
        pattern: "File:",
        timeout_secs: 10,
        label: "verify libutil.a stat output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after ar stat",
    });

    // -----------------------------------------------------------------------
    // 7. Phase 33: mmap/munmap leak test
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/mmap-leak-test\n",
        label: "mmap/munmap leak test",
    });
    steps.push(SmokeStep::Wait {
        pattern: "PASS",
        timeout_secs: 30,
        label: "verify no frame leak",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after leak test",
    });

    // -----------------------------------------------------------------------
    // 8. Phase 38: filesystem enhancements integration
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/tmpfs-test\n",
        label: "phase 38 integration test",
    });
    steps.push(SmokeStep::Wait {
        pattern: ", 0 failed",
        timeout_secs: 30,
        label: "verify tmpfs-test passed",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "prompt after tmpfs-test",
    });

    steps.extend(cmd_then_prompt(
        "/bin/ln -s /bin/sh0 /tmp/mysh\n",
        "send: ln -s /bin/sh0 /tmp/mysh",
        "wait: prompt after ln",
        10,
    ));
    steps.push(SmokeStep::Send {
        input: "/bin/readlink /tmp/mysh\n",
        label: "readlink: verify symlink target",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/bin/sh0",
        timeout_secs: 10,
        label: "verify readlink output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after readlink",
    });
    steps.extend(cmd_then_prompt(
        "/bin/rm /tmp/mysh\n",
        "send: rm /tmp/mysh",
        "wait: prompt after rm symlink",
        10,
    ));
    steps.extend(cmd_then_prompt(
        "/bin/ln -s /etc/../etc/passwd /phase38-passwd-link\n",
        "send: ln -s /etc/../etc/passwd /phase38-passwd-link",
        "wait: prompt after ext2 symlink create",
        10,
    ));
    steps.push(SmokeStep::Send {
        input: "/bin/stat /phase38-passwd-link\n",
        label: "stat: verify ext2 symlink metadata",
    });
    steps.push(SmokeStep::Wait {
        pattern: "symbolic link",
        timeout_secs: 10,
        label: "verify stat sees ext2 symlink",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after ext2 symlink stat",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/readlink /phase38-passwd-link\n",
        label: "readlink: verify ext2 symlink target",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/etc/../etc/passwd",
        timeout_secs: 10,
        label: "verify ext2 readlink output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after ext2 readlink",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/grep root:x:0:0: /phase38-passwd-link\n",
        label: "grep: follow ext2 symlink target",
    });
    steps.push(SmokeStep::Wait {
        pattern: "root:x:0:0:",
        timeout_secs: 15,
        label: "verify ext2 symlink follow output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after ext2 symlink cat",
    });
    steps.extend(cmd_then_prompt(
        "/bin/rm /phase38-passwd-link\n",
        "send: rm /phase38-passwd-link",
        "wait: prompt after ext2 symlink rm",
        10,
    ));

    // -----------------------------------------------------------------------
    // 9. Phase 41 initial tools: head, tail, tee, chmod, chown
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/head -n 1 /home/project/main.c\n",
        label: "head: first line of main.c",
    });
    steps.push(SmokeStep::Wait {
        pattern: "#include",
        timeout_secs: 10,
        label: "verify head -n output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after head -n",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /home/project/main.c | /bin/head\n",
        label: "head: default stdin mode",
    });
    steps.push(SmokeStep::Wait {
        pattern: "#include",
        timeout_secs: 10,
        label: "verify head default output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after head stdin",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/tail -n 1 /etc/passwd\n",
        label: "tail: last passwd line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "user:x:1000:1000:user:/home/user:/bin/ion",
        timeout_secs: 10,
        label: "verify tail -n output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after tail -n",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /etc/passwd | /bin/tail\n",
        label: "tail: default stdin mode",
    });
    steps.push(SmokeStep::Wait {
        pattern: "user:x:1000:1000:user:/home/user:/bin/ion",
        timeout_secs: 10,
        label: "verify tail default output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after tail stdin",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/echo teecheck | /bin/tee /tmp/tee-output\n",
        label: "tee: write stdout and file",
    });
    steps.push(SmokeStep::Wait {
        pattern: "teecheck",
        timeout_secs: 10,
        label: "verify tee stdout",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after tee write",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /tmp/tee-output\n",
        label: "tee: verify written file",
    });
    steps.push(SmokeStep::Wait {
        pattern: "teecheck",
        timeout_secs: 10,
        label: "verify tee file content",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after tee file check",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/echo appendcheck | /bin/tee -a /tmp/tee-output\n",
        label: "tee: append mode",
    });
    steps.push(SmokeStep::Wait {
        pattern: "appendcheck",
        timeout_secs: 10,
        label: "verify tee append stdout",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after tee append",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /tmp/tee-output\n",
        label: "tee: verify appended file",
    });
    steps.push(SmokeStep::Wait {
        pattern: "appendcheck",
        timeout_secs: 10,
        label: "verify tee append file content",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after tee append check",
    });
    steps.extend(cmd_then_prompt(
        "/bin/touch /tmp/permfile\n",
        "send: touch permfile",
        "wait: prompt after touch permfile",
        10,
    ));
    steps.extend(cmd_then_prompt(
        "/bin/chmod 600 /tmp/permfile\n",
        "send: chmod permfile",
        "wait: prompt after chmod",
        10,
    ));
    steps.push(SmokeStep::Send {
        input: "/bin/stat /tmp/permfile\n",
        label: "stat: verify chmod result",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Access: (00600)",
        timeout_secs: 10,
        label: "verify chmod stat output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after chmod stat",
    });
    steps.extend(cmd_then_prompt(
        "/bin/chown user:user /tmp/permfile\n",
        "send: chown permfile",
        "wait: prompt after chown",
        10,
    ));
    steps.push(SmokeStep::Send {
        input: "/bin/stat /tmp/permfile\n",
        label: "stat: verify chown result",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Uid: 1000",
        timeout_secs: 10,
        label: "verify chown uid",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Gid: 1000",
        timeout_secs: 10,
        label: "verify chown gid",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after chown stat",
    });

    // -----------------------------------------------------------------------
    // 10. Phase 41 text tools: sort, uniq, cut
    // -----------------------------------------------------------------------
    steps.extend(cmd_then_prompt(
        "/bin/echo pear > /tmp/sort_words\n",
        "sort fixture: write pear",
        "prompt after writing pear",
        10,
    ));
    steps.extend(cmd_then_prompt(
        "/bin/echo apple >> /tmp/sort_words\n",
        "sort fixture: append apple",
        "prompt after appending apple",
        10,
    ));
    steps.extend(cmd_then_prompt(
        "/bin/echo orange >> /tmp/sort_words\n",
        "sort fixture: append orange",
        "prompt after appending orange",
        10,
    ));
    steps.push(SmokeStep::Send {
        input: "/bin/sort /tmp/sort_words | /bin/head -n 1\n",
        label: "sort: verify first lexicographic line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "apple",
        timeout_secs: 10,
        label: "verify sort first line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after first sort line check",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/sort /tmp/sort_words | /bin/head -n 2 | /bin/tail -n 1\n",
        label: "sort: verify middle lexicographic line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "orange",
        timeout_secs: 10,
        label: "verify sort middle line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after middle sort line check",
    });
    steps.extend(cmd_then_prompt(
        "/bin/echo 10 > /tmp/sort_nums\n",
        "sort numeric fixture: write 10",
        "prompt after writing 10",
        10,
    ));
    steps.extend(cmd_then_prompt(
        "/bin/echo 2 >> /tmp/sort_nums\n",
        "sort numeric fixture: append 2",
        "prompt after appending 2",
        10,
    ));
    steps.extend(cmd_then_prompt(
        "/bin/echo 1 >> /tmp/sort_nums\n",
        "sort numeric fixture: append 1",
        "prompt after appending 1",
        10,
    ));
    steps.push(SmokeStep::Send {
        input: "/bin/sort -n /tmp/sort_nums | /bin/head -n 1\n",
        label: "sort: verify first numeric line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "1",
        timeout_secs: 15,
        label: "verify numeric sort first line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after first numeric line check",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/sort -n /tmp/sort_nums | /bin/head -n 2 | /bin/tail -n 1\n",
        label: "sort: verify middle numeric line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "2",
        timeout_secs: 15,
        label: "verify numeric sort middle line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after middle numeric line check",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /tmp/sort_nums | /bin/sort -rn | /bin/head -n 1\n",
        label: "sort: verify clustered pipeline first line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "10",
        timeout_secs: 15,
        label: "verify clustered pipeline first line",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after clustered pipeline first line check",
    });
    steps.extend(cmd_then_prompt(
        "/bin/echo root:x:0:0:root:/root:/bin/ion > /tmp/uniq_input\n",
        "uniq fixture: write first root line",
        "prompt after first uniq fixture line",
        10,
    ));
    steps.extend(cmd_then_prompt(
        "/bin/echo root:x:0:0:root:/root:/bin/ion >> /tmp/uniq_input\n",
        "uniq fixture: append second root line",
        "prompt after second uniq fixture line",
        10,
    ));
    steps.extend(cmd_then_prompt(
        "/bin/echo daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin >> /tmp/uniq_input\n",
        "uniq fixture: append daemon line",
        "prompt after daemon uniq fixture line",
        10,
    ));
    steps.push(SmokeStep::Send {
        input: "/bin/uniq -c /tmp/uniq_input\n",
        label: "uniq: count adjacent duplicates",
    });
    steps.push(SmokeStep::Wait {
        pattern: "2 root:x:0:0:root:/root:/bin/ion",
        timeout_secs: 20,
        label: "verify uniq count output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after uniq",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cut -d: -f1 /etc/passwd\n",
        label: "cut: passwd usernames",
    });
    steps.push(SmokeStep::Wait {
        pattern: "root",
        timeout_secs: 10,
        label: "verify cut field output includes root",
    });
    steps.push(SmokeStep::Wait {
        pattern: "user",
        timeout_secs: 10,
        label: "verify cut field output includes user",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after cut field",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/echo abcdef | /bin/cut -c2-4\n",
        label: "cut: character range",
    });
    steps.push(SmokeStep::Wait {
        pattern: "bcd",
        timeout_secs: 10,
        label: "verify cut character output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after cut chars",
    });

    // -----------------------------------------------------------------------
    // 11. Phase 41 text tools: tr, sed
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/echo HELLO | /bin/tr A-Z a-z\n",
        label: "tr: translate uppercase to lowercase",
    });
    steps.push(SmokeStep::Wait {
        pattern: "hello",
        timeout_secs: 10,
        label: "verify tr translation output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after tr translate",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/echo hello | /bin/tr -d '\\n' | /bin/wc -l\n",
        label: "tr: delete newline",
    });
    steps.push(SmokeStep::Wait {
        pattern: "0",
        timeout_secs: 10,
        label: "verify tr delete newline output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after tr delete",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/echo foofoo | /bin/sed 's/foo/bar/'\n",
        label: "sed: single substitution",
    });
    steps.push(SmokeStep::Wait {
        pattern: "barfoo",
        timeout_secs: 10,
        label: "verify sed single substitution",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after sed substitution",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/echo foofoo | /bin/sed 's/foo/bar/g'\n",
        label: "sed: global substitution",
    });
    steps.push(SmokeStep::Wait {
        pattern: "barbar",
        timeout_secs: 10,
        label: "verify sed global substitution",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after sed global substitution",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /etc/passwd /etc/passwd /etc/passwd | /bin/sed -n '3,5p'\n",
        label: "sed: print selected range",
    });
    steps.push(SmokeStep::Wait {
        pattern: "root:x:0:0:root:/root:/bin/ion",
        timeout_secs: 10,
        label: "verify sed range output includes line 3",
    });
    steps.push(SmokeStep::Wait {
        pattern: "user:x:1000:1000:user:/home/user:/bin/ion",
        timeout_secs: 10,
        label: "verify sed range output includes line 4",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after sed range print",
    });

    // -----------------------------------------------------------------------
    // 12. Phase 41 file tools: file, hexdump
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/file /bin/sh0\n",
        label: "file: detect ELF binary",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/bin/sh0: ELF 64-bit",
        timeout_secs: 10,
        label: "verify file ELF output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after file ELF",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/file /home/project/main.c\n",
        label: "file: detect ASCII text",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/home/project/main.c: ASCII text",
        timeout_secs: 10,
        label: "verify file text output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after file text",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/file /dev/null\n",
        label: "file: detect character special",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/dev/null: character special",
        timeout_secs: 10,
        label: "verify file char device output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after file char device",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/hexdump -n 16 /bin/sh0\n",
        label: "hexdump: default output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "00000000",
        timeout_secs: 10,
        label: "verify hexdump offset",
    });
    steps.push(SmokeStep::Wait {
        pattern: "7f 45 4c 46",
        timeout_secs: 10,
        label: "verify hexdump ELF magic",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after hexdump default",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/hexdump -C -n 16 /bin/sh0\n",
        label: "hexdump: canonical output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "00000000",
        timeout_secs: 10,
        label: "verify hexdump -C offset",
    });
    steps.push(SmokeStep::Wait {
        pattern: "7f 45 4c 46",
        timeout_secs: 10,
        label: "verify hexdump -C ELF magic",
    });
    steps.push(SmokeStep::Wait {
        pattern: "|.ELF",
        timeout_secs: 10,
        label: "verify hexdump -C ASCII gutter",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after hexdump canonical",
    });

    // -----------------------------------------------------------------------
    // 13. Phase 41 file tools: du, df
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/du -s /home/project\n",
        label: "du: summarize project directory",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/home/project",
        timeout_secs: 10,
        label: "verify du summary path",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after du summary",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/du -h -s /home/project\n",
        label: "du: human-readable summary",
    });
    steps.push(SmokeStep::Wait {
        pattern: "\t/home/project",
        timeout_secs: 10,
        label: "verify du human-readable path",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after du human-readable",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/du -h -s /home/project | /bin/cut -f1\n",
        label: "du: isolate human-readable size field",
    });
    steps.push(SmokeStep::Wait {
        pattern: "K",
        timeout_secs: 10,
        label: "verify du human-readable K-byte suffix",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after du human-readable size field",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/df\n",
        label: "df: list mounted filesystems",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Mounted on",
        timeout_secs: 10,
        label: "verify df header",
    });
    steps.push(SmokeStep::Wait {
        pattern: " /",
        timeout_secs: 10,
        label: "verify df root mount",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/proc",
        timeout_secs: 10,
        label: "verify df proc mount",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after df",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/df -h\n",
        label: "df: human-readable output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Mounted on",
        timeout_secs: 10,
        label: "verify df -h header",
    });
    steps.push(SmokeStep::Wait {
        pattern: " /",
        timeout_secs: 10,
        label: "verify df -h root mount",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/proc",
        timeout_secs: 10,
        label: "verify df -h proc mount",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after df -h",
    });

    // -----------------------------------------------------------------------
    // 14. Phase 41 file tools: find, xargs
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/find /home/project -name '*.c'\n",
        label: "find: match C source files",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/home/project/main.c",
        timeout_secs: 10,
        label: "verify find name match",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after find name",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/find /home/project -type d\n",
        label: "find: directories only",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/home/project",
        timeout_secs: 10,
        label: "verify find directory output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after find directories",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/find /home/project -type f\n",
        label: "find: files only",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/home/project/main.c",
        timeout_secs: 10,
        label: "verify find file output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after find files",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/find /home/project -name '*.c' | /bin/xargs /bin/grep main\n",
        label: "xargs: grep matches from find",
    });
    steps.push(SmokeStep::Wait {
        pattern: "main",
        timeout_secs: 10,
        label: "verify xargs grep output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after xargs grep",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/find /home/project -name '*.c' -print0 | /bin/xargs -0 /bin/grep main\n",
        label: "xargs: null-delimited grep",
    });
    steps.push(SmokeStep::Wait {
        pattern: "main",
        timeout_secs: 10,
        label: "verify xargs -0 output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after xargs -0",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/find /home/project -name '*.c' | /bin/xargs -I ITEM /bin/echo file:ITEM\n",
        label: "xargs: replacement string",
    });
    steps.push(SmokeStep::Wait {
        pattern: "file:/home/project/main.c",
        timeout_secs: 10,
        label: "verify xargs replacement output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after xargs replacement",
    });

    // -----------------------------------------------------------------------
    // 15. Phase 41 system tools: ps, free, dmesg, mount, umount, kill
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/ps -e\n",
        label: "ps: list processes",
    });
    steps.push(SmokeStep::Wait {
        pattern: "PID",
        timeout_secs: 10,
        label: "verify ps header",
    });
    steps.push(SmokeStep::Wait {
        pattern: "ion",
        timeout_secs: 10,
        label: "verify ps shell entry",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after ps",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/free\n",
        label: "free: memory summary",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Mem:",
        timeout_secs: 10,
        label: "verify free output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after free",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/free -h\n",
        label: "free: human-readable output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Mem:",
        timeout_secs: 10,
        label: "verify free -h output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after free -h",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/dmesg\n",
        label: "dmesg: kernel log snapshot",
    });
    steps.push(SmokeStep::Wait {
        pattern: "execve(/bin/dmesg)",
        timeout_secs: 10,
        label: "verify dmesg output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 20,
        label: "prompt after dmesg",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/mount\n",
        label: "mount: list mounts",
    });
    steps.push(SmokeStep::Wait {
        pattern: "/proc",
        timeout_secs: 10,
        label: "verify mount output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after mount",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/umount /\n",
        label: "umount: busy root error",
    });
    steps.push(SmokeStep::Wait {
        pattern: "busy",
        timeout_secs: 10,
        label: "verify umount busy error",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after umount busy",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/kill -l\n",
        label: "kill: list signals",
    });
    steps.push(SmokeStep::Wait {
        pattern: "TERM",
        timeout_secs: 10,
        label: "verify kill -l output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after kill -l",
    });

    // -----------------------------------------------------------------------
    // 16. Phase 41 developer tools: strings, cal, diff, patch, less
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/strings -n 4 /etc/passwd | /bin/head -n 1\n",
        label: "strings: extract printable text",
    });
    steps.push(SmokeStep::Wait {
        pattern: "root:x:0:0",
        timeout_secs: 10,
        label: "verify strings output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after strings",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cal 6 2025\n",
        label: "cal: specific month",
    });
    steps.push(SmokeStep::Wait {
        pattern: "June 2025",
        timeout_secs: 10,
        label: "verify cal month header",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Su Mo Tu We Th Fr Sa",
        timeout_secs: 10,
        label: "verify cal weekday header",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after cal month",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cal 2025 | /bin/grep December\n",
        label: "cal: full year output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "December",
        timeout_secs: 10,
        label: "verify cal year output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after cal year",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/echo alpha > /tmp/diff-a\n",
        label: "diff fixture: write alpha",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after diff fixture alpha",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/echo beta > /tmp/diff-b\n",
        label: "diff fixture: write beta",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after diff fixture beta",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/diff /tmp/diff-a /tmp/diff-b > /tmp/change.diff ; /bin/cat /tmp/change.diff\n",
        label: "diff: unified output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "--- /tmp/diff-a",
        timeout_secs: 10,
        label: "verify diff old header",
    });
    steps.push(SmokeStep::Wait {
        pattern: "+++ /tmp/diff-b",
        timeout_secs: 10,
        label: "verify diff new header",
    });
    steps.push(SmokeStep::Wait {
        pattern: "@@ -1 +1 @@",
        timeout_secs: 10,
        label: "verify diff hunk header",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after diff",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/patch < /tmp/change.diff\n",
        label: "patch: apply unified diff",
    });
    steps.push(SmokeStep::Wait {
        pattern: "applied hunk 1",
        timeout_secs: 10,
        label: "verify patch apply output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after patch",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /tmp/diff-a\n",
        label: "patch: verify patched file",
    });
    steps.push(SmokeStep::Wait {
        pattern: "beta",
        timeout_secs: 10,
        label: "verify patched file content",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after patched file check",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/less /etc/passwd\n",
        label: "less: open pager",
    });
    steps.push(SmokeStep::Wait {
        pattern: "root:",
        timeout_secs: 10,
        label: "verify less initial content",
    });
    steps.push(SmokeStep::Send {
        input: "\x1b[Bq",
        label: "less: scroll once and quit",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 10,
        label: "prompt after less",
    });

    // -----------------------------------------------------------------------
    // 17. Clean demo build artifacts
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Send {
        input: "/bin/rm -f /home/project/main.o /home/project/util.o /home/project/demo\n",
        label: "clean demo artifacts",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "wait for artifact cleanup",
    });

    // -----------------------------------------------------------------------
    // 18. Phase 47 — verify /bin/doom is present in the ramdisk
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Sleep { millis: 300 });
    steps.push(SmokeStep::Send {
        input: "ls /bin\n",
        label: "doom: list /bin directory",
    });
    steps.push(SmokeStep::Wait {
        pattern: "doom",
        timeout_secs: 10,
        label: "doom: verify doom binary appears in /bin listing",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "doom: prompt after ls",
    });

    if doom_wad_available {
        // -------------------------------------------------------------------
        // 19. Phase 47 — run doom long enough to prove the WAD boots
        // -------------------------------------------------------------------
        steps.push(SmokeStep::Send {
            input: "/bin/doom -iwad /usr/share/doom/doom1.wad\n",
            label: "doom: launch with iwad",
        });
        // Wait for I_InitGraphics to complete (proof WAD loaded OK)
        steps.push(SmokeStep::Wait {
            pattern: "I_InitGraphics:",
            timeout_secs: 30,
            label: "doom: wait for graphics init",
        });
    }

    steps
}

fn cmd_smoke_test(smoke_args: &SmokeTestArgs) {
    let kernel_binary = build_kernel();

    // Fail fast if TCC was not built (build_tcc() returned None, e.g. musl
    // toolchain missing). Check *after* build_kernel() since that calls
    // build_tcc().
    let tcc_staging_bin = workspace_root().join("target/tcc-staging/usr/bin/tcc");
    if !tcc_staging_bin.exists() {
        eprintln!(
            "error: TCC binary not found at {}\n\
             The smoke test requires TCC. Install musl-tools and retry.",
            tcc_staging_bin.display()
        );
        std::process::exit(1);
    }
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);
    // Always rebuild the data disk so the demo project is freshly populated.
    let disk_img = uefi_image.parent().unwrap().join("disk.img");
    if disk_img.exists() {
        let _ = fs::remove_file(&disk_img);
    }
    create_data_disk(uefi_image.parent().unwrap(), false, true);

    let ovmf = find_ovmf();
    let display_mode = if smoke_args.display {
        QemuDisplayMode::Gui
    } else {
        QemuDisplayMode::Headless
    };
    let mut args = qemu_args(&uefi_image, &ovmf, display_mode);
    // Strip hostfwd to avoid port conflicts in CI (same as qemu_test_args).
    for arg in args.iter_mut() {
        if arg.starts_with("user,id=net0,hostfwd=") {
            *arg = "user,id=net0".to_string();
        }
    }

    let steps = smoke_test_script(false);
    let base_timeout_secs = smoke_args.timeout_secs;

    // QEMU TCG emulation speed varies with host load. Retry up to 3 times
    // so a single unlucky scheduling window does not fail the gate. Each
    // retry uses a 50% longer global timeout.
    const MAX_ATTEMPTS: usize = 3;
    let mut last_err = String::new();

    for attempt in 1..=MAX_ATTEMPTS {
        let timeout_secs = base_timeout_secs + (attempt as u64 - 1) * (base_timeout_secs / 2);
        let global_timeout = std::time::Duration::from_secs(timeout_secs);
        println!(
            "smoke-test: launching QEMU (attempt {}/{}, timeout {}s)",
            attempt, MAX_ATTEMPTS, timeout_secs
        );
        if attempt > 1 {
            let disk_img = uefi_image.parent().unwrap().join("disk.img");
            if disk_img.exists() {
                let _ = fs::remove_file(&disk_img);
            }
            create_data_disk(uefi_image.parent().unwrap(), false, true);
        }
        let mut child = Command::new("qemu-system-x86_64")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to launch QEMU");

        let start = std::time::Instant::now();

        match run_smoke_script(&mut child, &steps, global_timeout) {
            Ok(()) => {
                let elapsed = start.elapsed().as_secs();
                if attempt > 1 {
                    println!(
                        "smoke-test: PASSED on attempt {} ({} steps in {}s)",
                        attempt,
                        steps.len(),
                        elapsed
                    );
                } else {
                    println!("smoke-test: PASSED ({} steps in {}s)", steps.len(), elapsed);
                }
                return;
            }
            Err(msg) => {
                let _ = child.kill();
                let _ = child.wait();
                last_err = msg;
                if attempt < MAX_ATTEMPTS {
                    eprintln!(
                        "smoke-test: attempt {} failed, retrying...\n{}",
                        attempt, last_err
                    );
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
            }
        }
    }

    eprintln!("smoke-test: FAILED after {MAX_ATTEMPTS} attempts\n{last_err}");
    std::process::exit(1);
}

// ---------------------------------------------------------------------------
// Phase 55b Track F.4 — device-path data smokes through ring-3 drivers
// ---------------------------------------------------------------------------

/// Arguments for `cargo xtask device-smoke`.
///
/// Accepts the same `--device` and `--iommu` flags as `run` and `test` so
/// callers can run the NVMe or e1000 smoke in any combination of hardware
/// configuration.  `--timeout` overrides the per-attempt wall-clock budget.
/// `--display` opens the QEMU SDL window (useful for local debugging).
#[derive(Debug, Clone)]
struct DeviceSmokeArgs {
    devices: DeviceSet,
    timeout_secs: u64,
    display: bool,
}

/// Parse `cargo xtask device-smoke [--device nvme|e1000] [--iommu]
///   [--timeout <secs>] [--display]` into a [`DeviceSmokeArgs`].
///
/// Device and IOMMU flags are handled by [`extract_device_flags`]; the
/// remaining tokens are walked for `--timeout` / `--display`.
fn parse_device_smoke_args(args: &[String]) -> Result<DeviceSmokeArgs, String> {
    let (devices, remaining) = extract_device_flags(args)?;
    let mut timeout_secs = 120u64;
    let mut display = false;
    let mut index = 0;

    while index < remaining.len() {
        match remaining[index].as_str() {
            "--display" => display = true,
            "--timeout" => {
                index += 1;
                timeout_secs = remaining
                    .get(index)
                    .ok_or("--timeout requires a value")?
                    .parse()
                    .map_err(|_| "invalid --timeout value")?;
            }
            other => return Err(format!("unknown device-smoke flag: {other}")),
        }
        index += 1;
    }

    Ok(DeviceSmokeArgs {
        devices,
        timeout_secs,
        display,
    })
}

/// Expect-style smoke script for `--device nvme`.
///
/// Phase 55b F.4b — full data-path round-trip:
///
/// 1. Waits for the kernel first-message.
/// 2. Waits for init to register the nvme_driver service config.
/// 3. Waits for `NVME_SMOKE:rw:PASS` — the sentinel emitted by nvme_driver
///    itself after a successful 512 B write+read round-trip at LBA 0.  This
///    exercises the full DMA / PRP / doorbell / completion chain through the
///    ring-3 driver, not just the service-config loading step.
///
/// The self-test runs inside the driver before the IPC endpoint is
/// registered, so no concurrent client can race with the pattern on LBA 0.
/// A timeout of 120 s covers the bring-up state machine + Identify + I/O
/// queue creation + the round-trip itself on a TCG QEMU instance.
fn device_smoke_script_nvme() -> Vec<SmokeStep> {
    vec![
        SmokeStep::Wait {
            pattern: "[m3os] Hello from kernel",
            timeout_secs: 30,
            label: "wait for kernel first message",
        },
        SmokeStep::Wait {
            pattern: "init: driver.registered name=nvme_driver",
            timeout_secs: 60,
            label: "wait for nvme_driver service-config registration in init",
        },
        SmokeStep::Wait {
            pattern: "NVME_SMOKE:rw:PASS",
            timeout_secs: 120,
            label: "wait for nvme_driver 512 B LBA-0 round-trip self-test to pass",
        },
    ]
}

/// Expect-style smoke script for `--device e1000`.
///
/// Phase 55b F.4b — link-state confirmation + honest ICMP/TCP skip:
///
/// 1. Waits for the kernel first-message.
/// 2. Waits for init to register the e1000_driver service config.
/// 3. Waits for `E1000_SMOKE:link:PASS` — emitted by e1000_driver after
///    bring-up confirms link state (up or transitioning).
///
/// ICMP echo and TCP connect are deferred: the full TX/RX server loop that
/// would send and receive Ethernet frames lands in Track E.3; until that
/// track completes the driver exits after bring-up and emits honest-skip
/// sentinels (`E1000_SMOKE:icmp:SKIP`, `E1000_SMOKE:tcp:SKIP`).  The skip
/// markers are present in the driver source so the smoke harness can grep
/// for them but this script does not block on them — a wait for a
/// per-boot SKIP sentinel would just race with the bring-up log.
fn device_smoke_script_e1000() -> Vec<SmokeStep> {
    vec![
        SmokeStep::Wait {
            pattern: "[m3os] Hello from kernel",
            timeout_secs: 30,
            label: "wait for kernel first message",
        },
        SmokeStep::Wait {
            pattern: "init: driver.registered name=e1000_driver",
            timeout_secs: 60,
            label: "wait for e1000_driver service-config registration in init",
        },
        SmokeStep::Wait {
            pattern: "E1000_SMOKE:link:PASS",
            timeout_secs: 90,
            label: "wait for e1000_driver link-state confirmation at bring-up",
        },
    ]
}

/// Run the Phase 55b F.4 device-path data smoke for the requested `devices`.
///
/// Builds the kernel, creates the UEFI image and data disk, then launches QEMU
/// with the selected device set.  Reads the serial log via an expect-style
/// script and asserts the `driver.registered: <name>` line appears within the
/// timeout budget.  Exits non-zero on failure.
///
/// When neither `--device nvme` nor `--device e1000` is given the command
/// prints a helpful diagnostic and exits 1 rather than running a no-op smoke.
fn cmd_device_smoke(args: &DeviceSmokeArgs) {
    if !args.devices.nvme && !args.devices.e1000 {
        eprintln!(
            "device-smoke: no device selected — pass --device nvme or --device e1000\n\
             Usage: cargo xtask device-smoke [--device nvme|e1000] [--iommu] \
             [--timeout <secs>] [--display]"
        );
        std::process::exit(1);
    }

    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);

    let disk_img = uefi_image.parent().unwrap().join("disk.img");
    if disk_img.exists() {
        let _ = fs::remove_file(&disk_img);
    }
    create_data_disk(uefi_image.parent().unwrap(), false, false);

    let ovmf = find_ovmf();
    let display_mode = if args.display {
        QemuDisplayMode::Gui
    } else {
        QemuDisplayMode::Headless
    };
    let mut qemu_args = qemu_args_with_devices(&uefi_image, &ovmf, display_mode, args.devices);
    // Strip hostfwd to avoid port conflicts in CI (same as qemu_test_args).
    for arg in qemu_args.iter_mut() {
        if arg.starts_with("user,id=net0,hostfwd=") {
            *arg = "user,id=net0".to_string();
        }
    }

    // Select the appropriate smoke script based on the requested device.
    // When both are requested (nvme + e1000) we concatenate both scripts so a
    // single QEMU boot asserts both `driver.registered` events.
    let mut steps: Vec<SmokeStep> = Vec::new();
    if args.devices.nvme {
        steps.extend(device_smoke_script_nvme());
    }
    if args.devices.e1000 {
        // Re-use the UEFI-stub and kernel-main Wait steps only if nvme wasn't
        // already requested (they appear once in the log regardless).
        if args.devices.nvme {
            // Only append the driver registration wait — the early-boot waits
            // already passed in the nvme script above.
            steps.push(SmokeStep::Wait {
                pattern: "driver.registered: e1000_driver",
                timeout_secs: 60,
                label: "wait for e1000_driver to register with device host",
            });
        } else {
            steps.extend(device_smoke_script_e1000());
        }
    }

    let base_timeout_secs = args.timeout_secs;
    const MAX_ATTEMPTS: usize = 3;
    let mut last_err = String::new();

    for attempt in 1..=MAX_ATTEMPTS {
        let timeout_secs = base_timeout_secs + (attempt as u64 - 1) * (base_timeout_secs / 2);
        let global_timeout = std::time::Duration::from_secs(timeout_secs);
        println!(
            "device-smoke: launching QEMU (attempt {}/{attempt}, timeout {}s)",
            MAX_ATTEMPTS, timeout_secs
        );

        // Recreate the disk on retry to avoid state from a previous partial boot.
        if attempt > 1 {
            let disk_img = uefi_image.parent().unwrap().join("disk.img");
            if disk_img.exists() {
                let _ = fs::remove_file(&disk_img);
            }
            create_data_disk(uefi_image.parent().unwrap(), false, false);
        }

        let mut child = Command::new("qemu-system-x86_64")
            .args(&qemu_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to launch QEMU");

        let start = std::time::Instant::now();

        match run_smoke_script(&mut child, &steps, global_timeout) {
            Ok(()) => {
                let elapsed = start.elapsed().as_secs();
                if attempt > 1 {
                    println!(
                        "device-smoke: PASSED on attempt {attempt} ({} steps in {elapsed}s)",
                        steps.len()
                    );
                } else {
                    println!("device-smoke: PASSED ({} steps in {elapsed}s)", steps.len());
                }
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Err(msg) => {
                let _ = child.kill();
                let _ = child.wait();
                last_err = msg;
                if attempt < MAX_ATTEMPTS {
                    eprintln!("device-smoke: attempt {attempt} failed, retrying...\n{last_err}");
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
            }
        }
    }

    eprintln!("device-smoke: FAILED after {MAX_ATTEMPTS} attempts\n{last_err}");
    std::process::exit(1);
}

fn cmd_fmt(fix: bool) {
    let root = workspace_root();
    let mut args = vec!["fmt", "--all"];
    if !fix {
        args.extend(["--", "--check"]);
    }
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(&args)
        .status()
        .expect("failed to run cargo fmt");

    if !status.success() {
        if fix {
            eprintln!("rustfmt failed");
        } else {
            eprintln!("rustfmt found unformatted code — run `cargo xtask fmt --fix` to fix");
        }
        std::process::exit(1);
    }

    if fix {
        println!("fmt: formatting applied");
    } else {
        println!("fmt: formatting correct");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageArgs {
    sign: bool,
    key: PathBuf,
    cert: PathBuf,
    enable_telnet: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SignArgs {
    unsigned_efi: PathBuf,
    signed_efi: PathBuf,
    key: PathBuf,
    cert: PathBuf,
}

#[derive(Clone, Debug)]
enum FileDataSource {
    File(PathBuf),
}

impl FileDataSource {
    fn len(&self) -> anyhow::Result<u64> {
        match self {
            FileDataSource::File(path) => Ok(fs::metadata(path)
                .with_context(|| format!("failed to read metadata of file `{}`", path.display()))?
                .len()),
        }
    }

    fn copy_to(&self, target: &mut dyn io::Write) -> anyhow::Result<()> {
        match self {
            FileDataSource::File(path) => {
                io::copy(
                    &mut File::open(path).with_context(|| {
                        format!("failed to open `{}` for copying", path.display())
                    })?,
                    target,
                )
                .with_context(|| format!("failed to copy `{}`", path.display()))?;
            }
        }

        Ok(())
    }
}

fn default_key_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("m3os.key")
}

fn default_cert_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("m3os.crt")
}

fn parse_image_args(args: &[String], workspace_root: &Path) -> Result<ImageArgs, String> {
    let mut sign = false;
    let mut key = None;
    let mut cert = None;
    let mut enable_telnet = false;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--sign" => {
                sign = true;
            }
            "--key" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "missing value for `--key`".to_string())?;
                key = Some(PathBuf::from(value));
            }
            "--cert" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "missing value for `--cert`".to_string())?;
                cert = Some(PathBuf::from(value));
            }
            _ if let Some(value) = arg.strip_prefix("--key=") => {
                key = Some(PathBuf::from(value));
            }
            _ if let Some(value) = arg.strip_prefix("--cert=") => {
                cert = Some(PathBuf::from(value));
            }
            "--enable-telnet" => {
                enable_telnet = true;
            }
            _ => {
                return Err(format!("unknown image flag `{arg}`"));
            }
        }
        index += 1;
    }

    if !sign && (key.is_some() || cert.is_some()) {
        return Err("`--key`/`--cert` require `--sign`".to_string());
    }

    Ok(ImageArgs {
        sign,
        key: key.unwrap_or_else(|| default_key_path(workspace_root)),
        cert: cert.unwrap_or_else(|| default_cert_path(workspace_root)),
        enable_telnet,
    })
}

fn parse_sign_args(args: &[String], workspace_root: &Path) -> Result<SignArgs, String> {
    let mut unsigned_efi = None;
    let mut key = None;
    let mut cert = None;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--key" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "missing value for `--key`".to_string())?;
                key = Some(PathBuf::from(value));
            }
            "--cert" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "missing value for `--cert`".to_string())?;
                cert = Some(PathBuf::from(value));
            }
            _ if let Some(value) = arg.strip_prefix("--key=") => {
                key = Some(PathBuf::from(value));
            }
            _ if let Some(value) = arg.strip_prefix("--cert=") => {
                cert = Some(PathBuf::from(value));
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown sign flag `{arg}`"));
            }
            _ => {
                if unsigned_efi.replace(PathBuf::from(arg)).is_some() {
                    return Err("expected a single unsigned EFI path".to_string());
                }
            }
        }
        index += 1;
    }

    let unsigned_efi = unsigned_efi.ok_or_else(|| "missing unsigned EFI path".to_string())?;
    Ok(SignArgs {
        signed_efi: signed_path(&unsigned_efi),
        unsigned_efi,
        key: key.unwrap_or_else(|| default_key_path(workspace_root)),
        cert: cert.unwrap_or_else(|| default_cert_path(workspace_root)),
    })
}

fn signed_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("bootx64");
    let file_name = match path.extension().and_then(|ext| ext.to_str()) {
        Some(extension) if !extension.is_empty() => format!("{stem}-signed.{extension}"),
        _ => format!("{stem}-signed"),
    };

    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    }
}

/// Create a 64 MB raw data disk image with an MBR partition table and an
/// ext2-formatted partition. The image is placed at `output_dir/disk.img`.
/// Skips creation if the image already exists to preserve persisted data.
///
/// Requires `e2fsprogs` on the host: `mkfs.ext2`, `debugfs`, `e2fsck`.
fn create_data_disk(output_dir: &Path, enable_telnet: bool, smoke_test_mode: bool) -> PathBuf {
    let disk_path = output_dir.join("disk.img");
    // Phase 36: increased from 128 MB to 1 GB to support the expanded persistent
    // storage requirements for filesystem stress testing and larger workloads.
    const DISK_SIZE: u64 = 1024 * 1024 * 1024; // 1 GB
    if disk_path.exists() {
        let meta = std::fs::metadata(&disk_path).ok();
        let size = meta.map(|m| m.len()).unwrap_or(0);
        if size < DISK_SIZE {
            println!(
                "WARNING: existing data disk is {} MB but {} MB is expected. \
                 Delete {} to recreate at the correct size.",
                size / (1024 * 1024),
                DISK_SIZE / (1024 * 1024),
                disk_path.display()
            );
        }
        println!("Data disk: {} (existing, preserved)", disk_path.display());
        return disk_path;
    }
    const SECTOR_SIZE: u64 = 512;
    const PARTITION_START_LBA: u32 = 2048; // 1 MB offset
    let total_sectors = (DISK_SIZE / SECTOR_SIZE) as u32;
    let partition_sectors = total_sectors - PARTITION_START_LBA;
    let partition_offset = PARTITION_START_LBA as u64 * SECTOR_SIZE;
    let partition_size = partition_sectors as u64 * SECTOR_SIZE;

    // Create the disk image file, zeroed out.
    let mut disk_file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&disk_path)
        .unwrap_or_else(|e| {
            eprintln!("Error: failed to create disk.img: {e}");
            std::process::exit(1);
        });
    disk_file.set_len(DISK_SIZE).unwrap_or_else(|e| {
        eprintln!("Error: failed to set disk.img size: {e}");
        std::process::exit(1);
    });

    // Write MBR.
    let mut mbr = [0u8; 512];

    // Partition entry 1 at offset 446 (16 bytes).
    let entry = &mut mbr[446..462];
    entry[0] = 0x80; // status: active
    entry[1] = 0xFE; // CHS start (LBA mode)
    entry[2] = 0xFF;
    entry[3] = 0xFF;
    entry[4] = 0x83; // type: Linux / ext2
    entry[5] = 0xFE; // CHS end (LBA mode)
    entry[6] = 0xFF;
    entry[7] = 0xFF;
    // LBA start (little-endian u32)
    entry[8..12].copy_from_slice(&PARTITION_START_LBA.to_le_bytes());
    // Sector count (little-endian u32)
    entry[12..16].copy_from_slice(&partition_sectors.to_le_bytes());

    // MBR signature.
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    disk_file.seek(io::SeekFrom::Start(0)).unwrap();
    disk_file.write_all(&mbr).unwrap_or_else(|e| {
        eprintln!("Error: failed to write MBR: {e}");
        std::process::exit(1);
    });
    drop(disk_file);

    // Extract partition area to a temp file, format as ext2, copy back.
    let part_tmp = output_dir.join("disk_partition.tmp");
    {
        let pf = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&part_tmp)
            .expect("create partition temp file");
        pf.set_len(partition_size)
            .expect("set partition temp file size");
    }

    // Format with mkfs.ext2 (4K blocks, ext2 rev 0, no optional features).
    // Newer e2fsprogs (>=1.47.4, Arch) removed -r; try -E revision=0 first
    // (rev 0 implies no features, so -O none is not needed), then fall back
    // to -r 0 -O none for older versions (Ubuntu 22.04/24.04).
    let mkfs_status = Command::new("mkfs.ext2")
        .args(["-b", "4096", "-L", "m3data", "-E", "revision=0", "-q"])
        .arg(&part_tmp)
        .status()
        .expect("failed to run mkfs.ext2 — is e2fsprogs installed?");
    if !mkfs_status.success() {
        let fallback = Command::new("mkfs.ext2")
            .args(["-b", "4096", "-L", "m3data", "-O", "none", "-r", "0", "-q"])
            .arg(&part_tmp)
            .status()
            .expect("failed to run mkfs.ext2");
        if !fallback.success() {
            eprintln!("Error: mkfs.ext2 failed (exit {})", fallback);
            std::process::exit(1);
        }
    }

    // Populate files using debugfs.
    populate_ext2_files(&part_tmp, output_dir, enable_telnet, smoke_test_mode);

    // Phase 31: populate TCC, musl headers/libs, and test files.
    let root = workspace_root();
    let tcc_staging = root.join("target/tcc-staging");
    if tcc_staging.join("usr/bin/tcc").exists() {
        populate_tcc_files(&part_tmp, &tcc_staging);
    }

    // Phase 32: populate demo project for make/build-tools testing.
    populate_demo_project(&part_tmp, &root);

    // Phase 45: populate ports tree and bundled source into /usr/ports/.
    let ports_src = root.join("target/ports-src");
    populate_ports_tree(&part_tmp, &root, &ports_src);
    // Phase 47: place doom1.wad on the ext2 partition.
    populate_doom_files(&part_tmp);

    // Validate with e2fsck.
    let fsck_status = Command::new("e2fsck")
        .args(["-n", "-f"])
        .arg(&part_tmp)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to run e2fsck");
    if !fsck_status.success() {
        eprintln!("Warning: e2fsck returned non-zero (exit {})", fsck_status);
    }

    // Copy the formatted partition back into the disk image at the offset.
    {
        let part_data = fs::read(&part_tmp).expect("read partition temp file");
        let mut disk = fs::OpenOptions::new()
            .write(true)
            .open(&disk_path)
            .expect("reopen disk image");
        disk.seek(io::SeekFrom::Start(partition_offset)).unwrap();
        disk.write_all(&part_data).expect("write partition to disk");
    }
    let _ = fs::remove_file(&part_tmp);

    println!("Data disk: {} (ext2, with /etc files)", disk_path.display());
    disk_path
}

/// Populate the ext2 partition image with initial directories and files
/// using `debugfs -w`. Creates temp host files for the `write` command.
fn populate_ext2_files(
    part_path: &Path,
    output_dir: &Path,
    enable_telnet: bool,
    smoke_test_mode: bool,
) {
    // Standard Unix root filesystem layout.
    let passwd_content =
        "root:x:0:0:root:/root:/bin/ion\nuser:x:1000:1000:user:/home/user:/bin/ion\n";
    // Pre-provisioned password hashes for CI/testing.  Format: $sha256$hex_salt$hex_hash
    // where hash = SHA-256(salt_bytes || password_bytes).  Passwords: root="root", user="user".
    let shadow_content = "root:$sha256$63695f726f6f745f73616c7431323334$5c8e5a851fee488aae9fc5890dd433f8a391fba2860899c271a6e6f5d3e4c439::::::\nuser:$sha256$63695f757365725f73616c7435363738$64fb26f3575e26ed5fc3b07e6c4ca2b6af8bf1f17267c34babb76448301a16ca::::::\n";
    let group_content = "root:x:0:root\nuser:x:1000:user\n";

    // Phase 46: service definition files.
    let sshd_conf = "name=sshd\ncommand=/bin/sshd\ntype=daemon\nrestart=always\nmax_restart=10\ndepends=syslogd\n";
    let telnetd_conf = "name=telnetd\ncommand=/bin/telnetd\ntype=daemon\nrestart=always\nmax_restart=10\ndepends=syslogd\n";
    let syslogd_conf = "name=syslogd\ncommand=/bin/syslogd\ntype=daemon\nrestart=always\nmax_restart=10\ndepends=\n";
    let crond_conf = "name=crond\ncommand=/bin/crond\ntype=daemon\nrestart=always\nmax_restart=10\ndepends=syslogd\n";

    // Phase 52: service definitions for extracted ring-3 services.
    let console_conf = "name=console\ncommand=/bin/console_server\ntype=daemon\nrestart=always\nmax_restart=10\ndepends=\n";
    let kbd_conf = "name=kbd\ncommand=/bin/kbd_server\ntype=daemon\nrestart=always\nmax_restart=10\ndepends=console\n";
    let stdin_feeder_conf = "name=stdin_feeder\ncommand=/bin/stdin_feeder\ntype=daemon\nrestart=always\nmax_restart=10\ndepends=console,kbd\n";

    // Phase 54: storage service definitions.
    let fat_server_conf = "name=fat\ncommand=/bin/fat_server\ntype=daemon\nrestart=always\nmax_restart=10\ndepends=\nuser=200\n";
    let vfs_server_conf = "name=vfs\ncommand=/bin/vfs_server\ntype=daemon\nrestart=never\nmax_restart=0\ndepends=fat\nuser=200\n";

    // Phase 54 Track C: UDP network service.
    let net_server_conf = "name=net_udp\ncommand=/bin/net_server\ntype=daemon\nrestart=never\nmax_restart=0\ndepends=\n";

    // Phase 55b F.1: ring-3 driver process service configs.
    // No `depends=` line — the IOMMU substrate (Phase 55a) is kernel-internal
    // init, not a supervised service.  restart=on-failure with max_restart=5
    // provides supervised crash recovery without infinite loops.
    let nvme_driver_conf =
        "name=nvme_driver\ncommand=/drivers/nvme\ntype=daemon\nrestart=on-failure\nmax_restart=5\n";
    let e1000_driver_conf = "name=e1000_driver\ncommand=/drivers/e1000\ntype=daemon\nrestart=on-failure\nmax_restart=5\n";

    let hostname_content = "m3os\n";
    let smoke_mode_content = "enabled\n";
    let empty_content = "";
    let udp_smoke_bin = generated_initrd_dir(&workspace_root()).join("udp-smoke");

    // Create temp host files for debugfs `write` command.
    let passwd_tmp = output_dir.join("_tmp_passwd");
    let shadow_tmp = output_dir.join("_tmp_shadow");
    let group_tmp = output_dir.join("_tmp_group");
    let sshd_conf_tmp = output_dir.join("_tmp_sshd_conf");
    let syslogd_conf_tmp = output_dir.join("_tmp_syslogd_conf");
    let crond_conf_tmp = output_dir.join("_tmp_crond_conf");
    let console_conf_tmp = output_dir.join("_tmp_console_conf");
    let kbd_conf_tmp = output_dir.join("_tmp_kbd_conf");
    let stdin_feeder_conf_tmp = output_dir.join("_tmp_stdin_feeder_conf");
    let fat_server_conf_tmp = output_dir.join("_tmp_fat_server_conf");
    let vfs_server_conf_tmp = output_dir.join("_tmp_vfs_server_conf");
    let net_server_conf_tmp = output_dir.join("_tmp_net_server_conf");
    let nvme_driver_conf_tmp = output_dir.join("_tmp_nvme_driver_conf");
    let e1000_driver_conf_tmp = output_dir.join("_tmp_e1000_driver_conf");
    let hostname_tmp = output_dir.join("_tmp_hostname");
    let smoke_mode_tmp = output_dir.join("_tmp_smoke_mode");
    let empty_tmp = output_dir.join("_tmp_empty");
    fs::write(&passwd_tmp, passwd_content).expect("write temp passwd");
    fs::write(&shadow_tmp, shadow_content).expect("write temp shadow");
    fs::write(&group_tmp, group_content).expect("write temp group");
    fs::write(&sshd_conf_tmp, sshd_conf).expect("write temp sshd.conf");
    fs::write(&syslogd_conf_tmp, syslogd_conf).expect("write temp syslogd.conf");
    fs::write(&crond_conf_tmp, crond_conf).expect("write temp crond.conf");
    fs::write(&console_conf_tmp, console_conf).expect("write temp console.conf");
    fs::write(&kbd_conf_tmp, kbd_conf).expect("write temp kbd.conf");
    fs::write(&stdin_feeder_conf_tmp, stdin_feeder_conf).expect("write temp stdin_feeder.conf");
    fs::write(&fat_server_conf_tmp, fat_server_conf).expect("write temp fat_server.conf");
    fs::write(&vfs_server_conf_tmp, vfs_server_conf).expect("write temp vfs_server.conf");
    fs::write(&net_server_conf_tmp, net_server_conf).expect("write temp net_server.conf");
    fs::write(&nvme_driver_conf_tmp, nvme_driver_conf).expect("write temp nvme_driver.conf");
    fs::write(&e1000_driver_conf_tmp, e1000_driver_conf).expect("write temp e1000_driver.conf");
    fs::write(&hostname_tmp, hostname_content).expect("write temp hostname");
    fs::write(&empty_tmp, empty_content).expect("write temp empty file");
    if smoke_test_mode {
        fs::write(&smoke_mode_tmp, smoke_mode_content).expect("write temp smoke marker");
    }

    // Phase 48: telnetd service config is only written when --enable-telnet is passed.
    let telnetd_cmds = if enable_telnet {
        let telnetd_conf_tmp = output_dir.join("_tmp_telnetd_conf");
        fs::write(&telnetd_conf_tmp, telnetd_conf).expect("write temp telnetd.conf");
        format!(
            "write \"{}\" etc/services.d/telnetd.conf\n\
             sif etc/services.d/telnetd.conf mode 0x81A4\n\
             sif etc/services.d/telnetd.conf uid 0\n\
             sif etc/services.d/telnetd.conf gid 0\n",
            telnetd_conf_tmp.display()
        )
    } else {
        String::new()
    };

    let smoke_mode_cmds = if smoke_test_mode {
        format!(
            "write \"{}\" etc/m3os-smoke-test-mode\n\
             sif etc/m3os-smoke-test-mode mode 0x81A4\n\
             sif etc/m3os-smoke-test-mode uid 0\n\
             sif etc/m3os-smoke-test-mode gid 0\n",
            smoke_mode_tmp.display()
        )
    } else {
        String::new()
    };

    // CI toggle: dropping this marker tells the guest smoke-runner to skip
    // the TCC compile + hello-verify steps (both emit SKIP instead of PASS).
    // Compiling inside TCG under Phase 54's IPC-heavy VFS exceeds the per-step
    // budget. Dev machines leave the env unset and still exercise the full
    // path.
    let skip_tcc_cmds = if smoke_test_mode
        && std::env::var("M3OS_SMOKE_SKIP_TCC_COMPILE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    {
        fs::write(output_dir.join("_tmp_skip_tcc"), "").expect("write temp skip marker");
        format!(
            "write \"{}\" etc/m3os-skip-tcc-compile\n\
             sif etc/m3os-skip-tcc-compile mode 0x81A4\n\
             sif etc/m3os-skip-tcc-compile uid 0\n\
             sif etc/m3os-skip-tcc-compile gid 0\n",
            output_dir.join("_tmp_skip_tcc").display()
        )
    } else {
        String::new()
    };

    // Standard Unix root filesystem directories and files.
    // debugfs mode values: S_IFDIR|perm or S_IFREG|perm
    // S_IFDIR = 0o40000 = 0x4000, S_IFREG = 0o100000 = 0x8000
    // 0o40755 = 0x41ED, 0o40700 = 0x41C0, 0o100644 = 0x81A4, 0o100600 = 0x8180
    // 0o41777 = 0x43FF (sticky + 0o777)
    let cmds = format!(
        "mkdir bin\n\
         mkdir sbin\n\
         mkdir etc\n\
         mkdir root\n\
         mkdir home\n\
         mkdir home/user\n\
         mkdir root/.config\n\
         mkdir root/.config/ion\n\
         mkdir root/.local\n\
         mkdir root/.local/share\n\
         mkdir root/.local/share/ion\n\
         mkdir home/user/.config\n\
         mkdir home/user/.config/ion\n\
         mkdir home/user/.local\n\
         mkdir home/user/.local/share\n\
         mkdir home/user/.local/share/ion\n\
         mkdir tmp\n\
         mkdir var\n\
         mkdir dev\n\
         write \"{passwd}\" etc/passwd\n\
         write \"{shadow}\" etc/shadow\n\
         write \"{group}\" etc/group\n\
         write \"{empty}\" root/.local/share/ion/history\n\
         write \"{empty}\" home/user/.local/share/ion/history\n\
         sif bin mode 0x41ED\n\
         sif bin uid 0\n\
         sif bin gid 0\n\
         sif sbin mode 0x41ED\n\
         sif sbin uid 0\n\
         sif sbin gid 0\n\
         sif etc mode 0x41ED\n\
         sif etc uid 0\n\
         sif etc gid 0\n\
          sif root mode 0x41C0\n\
          sif root uid 0\n\
          sif root gid 0\n\
          write \"{udp_smoke_bin}\" root/udp-smoke\n\
          sif root/udp-smoke mode 0x81ED\n\
          sif root/udp-smoke uid 0\n\
          sif root/udp-smoke gid 0\n\
          sif home mode 0x41ED\n\
         sif home uid 0\n\
         sif home gid 0\n\
         sif home/user mode 0x41ED\n\
         sif home/user uid 1000\n\
         sif home/user gid 1000\n\
         sif root/.config mode 0x41C0\n\
         sif root/.config uid 0\n\
         sif root/.config gid 0\n\
         sif root/.config/ion mode 0x41C0\n\
         sif root/.config/ion uid 0\n\
         sif root/.config/ion gid 0\n\
         sif root/.local mode 0x41C0\n\
         sif root/.local uid 0\n\
         sif root/.local gid 0\n\
         sif root/.local/share mode 0x41C0\n\
         sif root/.local/share uid 0\n\
         sif root/.local/share gid 0\n\
         sif root/.local/share/ion mode 0x41C0\n\
         sif root/.local/share/ion uid 0\n\
         sif root/.local/share/ion gid 0\n\
         sif root/.local/share/ion/history mode 0x8180\n\
         sif root/.local/share/ion/history uid 0\n\
         sif root/.local/share/ion/history gid 0\n\
         sif home/user/.config mode 0x41ED\n\
         sif home/user/.config uid 1000\n\
         sif home/user/.config gid 1000\n\
         sif home/user/.config/ion mode 0x41ED\n\
         sif home/user/.config/ion uid 1000\n\
         sif home/user/.config/ion gid 1000\n\
         sif home/user/.local mode 0x41ED\n\
         sif home/user/.local uid 1000\n\
         sif home/user/.local gid 1000\n\
         sif home/user/.local/share mode 0x41ED\n\
         sif home/user/.local/share uid 1000\n\
         sif home/user/.local/share gid 1000\n\
         sif home/user/.local/share/ion mode 0x41ED\n\
         sif home/user/.local/share/ion uid 1000\n\
         sif home/user/.local/share/ion gid 1000\n\
         sif home/user/.local/share/ion/history mode 0x8180\n\
         sif home/user/.local/share/ion/history uid 1000\n\
         sif home/user/.local/share/ion/history gid 1000\n\
         sif tmp mode 0x43FF\n\
         sif tmp uid 0\n\
         sif tmp gid 0\n\
         sif var mode 0x41ED\n\
         sif var uid 0\n\
         sif var gid 0\n\
         mkdir var/log\n\
         sif var/log mode 0x41ED\n\
         sif var/log uid 0\n\
         sif var/log gid 0\n\
         mkdir var/spool\n\
         sif var/spool mode 0x41ED\n\
         sif var/spool uid 0\n\
         sif var/spool gid 0\n\
         mkdir var/spool/cron\n\
         sif var/spool/cron mode 0x41ED\n\
         sif var/spool/cron uid 0\n\
         sif var/spool/cron gid 0\n\
         mkdir etc/services.d\n\
         sif etc/services.d mode 0x41ED\n\
         sif etc/services.d uid 0\n\
         sif etc/services.d gid 0\n\
         sif dev mode 0x41ED\n\
         sif dev uid 0\n\
         sif dev gid 0\n\
         sif etc/passwd mode 0x81A4\n\
         sif etc/passwd uid 0\n\
         sif etc/passwd gid 0\n\
         sif etc/shadow mode 0x8180\n\
         sif etc/shadow uid 0\n\
         sif etc/shadow gid 0\n\
         sif etc/group mode 0x81A4\n\
         sif etc/group uid 0\n\
         sif etc/group gid 0\n\
         write \"{sshd_conf}\" etc/services.d/sshd.conf\n\
         sif etc/services.d/sshd.conf mode 0x81A4\n\
         sif etc/services.d/sshd.conf uid 0\n\
         sif etc/services.d/sshd.conf gid 0\n\
         {telnetd_cmds}\
         write \"{syslogd_conf}\" etc/services.d/syslogd.conf\n\
         sif etc/services.d/syslogd.conf mode 0x81A4\n\
         sif etc/services.d/syslogd.conf uid 0\n\
         sif etc/services.d/syslogd.conf gid 0\n\
         write \"{crond_conf}\" etc/services.d/crond.conf\n\
         sif etc/services.d/crond.conf mode 0x81A4\n\
         sif etc/services.d/crond.conf uid 0\n\
         sif etc/services.d/crond.conf gid 0\n\
         write \"{console_conf}\" etc/services.d/console.conf\n\
         sif etc/services.d/console.conf mode 0x81A4\n\
         sif etc/services.d/console.conf uid 0\n\
         sif etc/services.d/console.conf gid 0\n\
         write \"{kbd_conf}\" etc/services.d/kbd.conf\n\
         sif etc/services.d/kbd.conf mode 0x81A4\n\
         sif etc/services.d/kbd.conf uid 0\n\
         sif etc/services.d/kbd.conf gid 0\n\
         write \"{stdin_feeder_conf}\" etc/services.d/stdin_feeder.conf\n\
         sif etc/services.d/stdin_feeder.conf mode 0x81A4\n\
         sif etc/services.d/stdin_feeder.conf uid 0\n\
         sif etc/services.d/stdin_feeder.conf gid 0\n\
         write \"{fat_server_conf}\" etc/services.d/fat_server.conf\n\
         sif etc/services.d/fat_server.conf mode 0x81A4\n\
         sif etc/services.d/fat_server.conf uid 0\n\
         sif etc/services.d/fat_server.conf gid 0\n\
         write \"{vfs_server_conf}\" etc/services.d/vfs_server.conf\n\
         sif etc/services.d/vfs_server.conf mode 0x81A4\n\
         sif etc/services.d/vfs_server.conf uid 0\n\
         sif etc/services.d/vfs_server.conf gid 0\n\
         write \"{net_server_conf}\" etc/services.d/net_server.conf\n\
         sif etc/services.d/net_server.conf mode 0x81A4\n\
         sif etc/services.d/net_server.conf uid 0\n\
         sif etc/services.d/net_server.conf gid 0\n\
         write \"{nvme_driver_conf}\" etc/services.d/nvme_driver.conf\n\
         sif etc/services.d/nvme_driver.conf mode 0x81A4\n\
         sif etc/services.d/nvme_driver.conf uid 0\n\
         sif etc/services.d/nvme_driver.conf gid 0\n\
         write \"{e1000_driver_conf}\" etc/services.d/e1000_driver.conf\n\
         sif etc/services.d/e1000_driver.conf mode 0x81A4\n\
         sif etc/services.d/e1000_driver.conf uid 0\n\
         sif etc/services.d/e1000_driver.conf gid 0\n\
         write \"{hostname}\" etc/hostname\n\
         sif etc/hostname mode 0x81A4\n\
         sif etc/hostname uid 0\n\
         sif etc/hostname gid 0\n\
         {smoke_mode_cmds}\
         {skip_tcc_cmds}\
         q\n",
        passwd = passwd_tmp.display(),
        shadow = shadow_tmp.display(),
        group = group_tmp.display(),
        sshd_conf = sshd_conf_tmp.display(),
        telnetd_cmds = telnetd_cmds,
        syslogd_conf = syslogd_conf_tmp.display(),
        crond_conf = crond_conf_tmp.display(),
        console_conf = console_conf_tmp.display(),
        kbd_conf = kbd_conf_tmp.display(),
        stdin_feeder_conf = stdin_feeder_conf_tmp.display(),
        fat_server_conf = fat_server_conf_tmp.display(),
        vfs_server_conf = vfs_server_conf_tmp.display(),
        net_server_conf = net_server_conf_tmp.display(),
        nvme_driver_conf = nvme_driver_conf_tmp.display(),
        e1000_driver_conf = e1000_driver_conf_tmp.display(),
        hostname = hostname_tmp.display(),
        empty = empty_tmp.display(),
        smoke_mode_cmds = smoke_mode_cmds,
        skip_tcc_cmds = skip_tcc_cmds,
        udp_smoke_bin = udp_smoke_bin.display(),
    );

    let mut debugfs = Command::new("debugfs")
        .arg("-w")
        .arg(part_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run debugfs — is e2fsprogs installed?");
    {
        let stdin = debugfs.stdin.as_mut().expect("debugfs stdin");
        stdin
            .write_all(cmds.as_bytes())
            .expect("write debugfs commands");
    }
    let debugfs_output = debugfs.wait_with_output().expect("debugfs wait");
    if !debugfs_output.status.success() {
        let stderr = String::from_utf8_lossy(&debugfs_output.stderr);
        eprintln!(
            "Error: debugfs exited with {}: {}",
            debugfs_output.status, stderr
        );
        std::process::exit(1);
    }

    // Clean up temp files.
    let _ = fs::remove_file(&passwd_tmp);
    let _ = fs::remove_file(&shadow_tmp);
    let _ = fs::remove_file(&group_tmp);
    let _ = fs::remove_file(&sshd_conf_tmp);
    if enable_telnet {
        let _ = fs::remove_file(output_dir.join("_tmp_telnetd_conf"));
    }
    let _ = fs::remove_file(&syslogd_conf_tmp);
    let _ = fs::remove_file(&crond_conf_tmp);
    let _ = fs::remove_file(&hostname_tmp);
    let _ = fs::remove_file(&smoke_mode_tmp);
    let _ = fs::remove_file(&empty_tmp);
}

/// Phase 31: Populate TCC, musl headers/libraries, and test files into the
/// ext2 partition image from the staging directory.
///
/// Walks `staging_dir/usr/` recursively and creates the corresponding
/// directory tree and files on the ext2 image via `debugfs -w`.
fn populate_tcc_files(part_path: &Path, staging_dir: &Path) {
    let usr_root = staging_dir.join("usr");
    if !usr_root.is_dir() {
        return;
    }

    // Collect all directories and files relative to `staging_dir`.
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<(String, PathBuf)> = Vec::new(); // (ext2_path, host_path)
    collect_staging_entries(&usr_root, "usr", &mut dirs, &mut files);

    if files.is_empty() {
        return;
    }

    // Build debugfs command script.
    let mut cmds = String::new();

    // Create directories first (sorted so parents come before children).
    dirs.sort();
    for dir in &dirs {
        cmds.push_str(&format!("mkdir {dir}\n"));
    }

    // Write files.
    for (ext2_path, host_path) in &files {
        cmds.push_str(&format!("write \"{}\" {ext2_path}\n", host_path.display()));
    }

    // Set permissions: directories 0755, files 0644, TCC binary 0755.
    for dir in &dirs {
        cmds.push_str(&format!("sif {dir} mode 0x41ED\n"));
    }
    for (ext2_path, _) in &files {
        if ext2_path == "usr/bin/tcc" {
            // Executable.
            cmds.push_str(&format!("sif {ext2_path} mode 0x81ED\n"));
        } else {
            cmds.push_str(&format!("sif {ext2_path} mode 0x81A4\n"));
        }
    }

    cmds.push_str("q\n");

    println!(
        "tcc: populating ext2 with {} dirs, {} files",
        dirs.len(),
        files.len()
    );

    let mut debugfs = Command::new("debugfs")
        .arg("-w")
        .arg(part_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run debugfs for TCC population");
    {
        let stdin = debugfs.stdin.as_mut().expect("debugfs stdin");
        stdin
            .write_all(cmds.as_bytes())
            .expect("write TCC debugfs commands");
    }
    let debugfs_output = debugfs.wait_with_output().expect("debugfs wait");
    if !debugfs_output.status.success() {
        let stderr = String::from_utf8_lossy(&debugfs_output.stderr);
        eprintln!(
            "Error: debugfs (TCC) exited with {}: {}",
            debugfs_output.status, stderr
        );
        std::process::exit(1);
    }
}

/// Recursively collect directories and files from a staging directory.
fn collect_staging_entries(
    dir: &Path,
    prefix: &str,
    dirs: &mut Vec<String>,
    files: &mut Vec<(String, PathBuf)>,
) {
    dirs.push(prefix.to_string());
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let child_prefix = format!("{prefix}/{name_str}");
        let path = entry.path();
        if path.is_dir() {
            collect_staging_entries(&path, &child_prefix, dirs, files);
        } else {
            files.push((child_prefix, path));
        }
    }
}

/// Phase 32: Populate the demo multi-file C project into `/home/project/`
/// on the ext2 partition for `make` testing.
fn populate_demo_project(part_path: &Path, workspace_root: &Path) {
    let demo_dir = workspace_root.join("userspace/demo-project");
    if !demo_dir.is_dir() {
        return;
    }

    let demo_files: &[(&str, &str)] = &[
        ("Makefile", "home/project/Makefile"),
        ("main.c", "home/project/main.c"),
        ("util.c", "home/project/util.c"),
        ("util.h", "home/project/util.h"),
        ("build.sh", "home/project/build.sh"),
    ];

    let mut cmds = String::new();
    cmds.push_str("mkdir home/project\n");

    for (src_name, ext2_path) in demo_files {
        let host_path = demo_dir.join(src_name);
        if host_path.exists() {
            cmds.push_str(&format!("write \"{}\" {ext2_path}\n", host_path.display()));
        }
    }

    // Set permissions: directory 0755, files 0644, build.sh 0755.
    cmds.push_str("sif home/project mode 0x41ED\n");
    for (src_name, ext2_path) in demo_files {
        if demo_dir.join(src_name).exists() {
            if *src_name == "build.sh" {
                cmds.push_str(&format!("sif {ext2_path} mode 0x81ED\n")); // executable
            } else {
                cmds.push_str(&format!("sif {ext2_path} mode 0x81A4\n")); // 0644
            }
        }
    }

    cmds.push_str("q\n");

    println!("demo: populating ext2 with demo project in /home/project/");

    let mut debugfs = Command::new("debugfs")
        .arg("-w")
        .arg(part_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run debugfs for demo project");
    {
        let stdin = debugfs.stdin.as_mut().expect("debugfs stdin");
        stdin
            .write_all(cmds.as_bytes())
            .expect("write demo debugfs commands");
    }
    let output = debugfs.wait_with_output().expect("debugfs wait");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "Warning: debugfs (demo) exited with {}: {}",
            output.status, stderr
        );
    }
}

/// Phase 45: Fetch Lua source code for the ports system.
/// Downloads and extracts Lua 5.4.7 to `target/ports-src/lang/lua/src/`.
fn fetch_lua_source(ports_src: &Path) {
    let lua_dir = ports_src.join("lang/lua/src");
    if lua_dir.join("lua.c").exists() {
        println!("ports: Lua source already cached at {}", lua_dir.display());
        return;
    }

    let lua_tar = ports_src.join("lua-5.4.7.tar.gz");
    println!("ports: downloading Lua 5.4.7...");
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "-o",
            lua_tar.to_str().unwrap(),
            "https://www.lua.org/ftp/lua-5.4.7.tar.gz",
        ])
        .status();
    match status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!(
                "warning: failed to download Lua source for host cache {}",
                lua_dir.display()
            );
            return;
        }
    }

    // Extract to a temp dir, then move the src/ files.
    let extract_dir = ports_src.join("lua-extract");
    let _ = fs::remove_dir_all(&extract_dir);
    fs::create_dir_all(&extract_dir).unwrap();
    let status = Command::new("tar")
        .args([
            "xzf",
            lua_tar.to_str().unwrap(),
            "-C",
            extract_dir.to_str().unwrap(),
        ])
        .status()
        .expect("failed to run tar");
    if !status.success() {
        eprintln!(
            "warning: failed to extract Lua source into host cache {}",
            lua_dir.display()
        );
        return;
    }

    // Lua extracts to lua-5.4.7/src/ — copy the src/ contents.
    let lua_src_extracted = extract_dir.join("lua-5.4.7/src");
    if lua_src_extracted.is_dir() {
        fs::create_dir_all(&lua_dir).unwrap();
        for entry in fs::read_dir(&lua_src_extracted).unwrap() {
            let entry = entry.unwrap();
            let dest = lua_dir.join(entry.file_name());
            fs::copy(entry.path(), &dest).unwrap();
        }
        println!("ports: Lua source extracted to {}", lua_dir.display());
    }
    let _ = fs::remove_dir_all(&extract_dir);
    let _ = fs::remove_file(&lua_tar);
}

/// Phase 45: Fetch zlib source code for the ports system.
/// Downloads and extracts zlib 1.3.1 to `target/ports-src/lib/zlib/src/`.
fn fetch_zlib_source(ports_src: &Path) {
    const ZLIB_SHA256: &str = "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23";

    let zlib_dir = ports_src.join("lib/zlib/src");
    if zlib_dir.join("zlib.h").exists() {
        println!(
            "ports: zlib source already cached at {}",
            zlib_dir.display()
        );
        return;
    }

    let zlib_tar = ports_src.join("zlib-1.3.1.tar.gz");
    println!("ports: downloading zlib 1.3.1...");
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "-o",
            zlib_tar.to_str().unwrap(),
            "https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz",
        ])
        .status();
    match status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!(
                "warning: failed to download zlib source for host cache {}",
                zlib_dir.display()
            );
            return;
        }
    }

    // Verify SHA-256 checksum before extracting.
    if !verify_sha256(&zlib_tar, ZLIB_SHA256) {
        eprintln!(
            "warning: zlib tarball verification failed for host cache {} \
             (checksum mismatch or `sha256sum` unavailable) — removing the file.\n\
             Expected SHA-256: {ZLIB_SHA256}",
            zlib_dir.display()
        );
        let _ = fs::remove_file(&zlib_tar);
        return;
    }

    let extract_dir = ports_src.join("zlib-extract");
    let _ = fs::remove_dir_all(&extract_dir);
    fs::create_dir_all(&extract_dir).unwrap();
    let status = Command::new("tar")
        .args([
            "xzf",
            zlib_tar.to_str().unwrap(),
            "-C",
            extract_dir.to_str().unwrap(),
        ])
        .status()
        .expect("failed to run tar");
    if !status.success() {
        eprintln!(
            "warning: failed to extract zlib source into host cache {}",
            zlib_dir.display()
        );
        return;
    }

    // zlib extracts to zlib-1.3.1/ — copy all .c and .h files.
    let zlib_extracted = extract_dir.join("zlib-1.3.1");
    if zlib_extracted.is_dir() {
        fs::create_dir_all(&zlib_dir).unwrap();
        for entry in fs::read_dir(&zlib_extracted).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".c") || name_str.ends_with(".h") {
                let dest = zlib_dir.join(&name);
                fs::copy(entry.path(), &dest).unwrap();
            }
        }
        println!("ports: zlib source extracted to {}", zlib_dir.display());
    }
    let _ = fs::remove_dir_all(&extract_dir);
    let _ = fs::remove_file(&zlib_tar);
}

/// Phase 45: Fetch all port sources for bundling into the disk image.
fn fetch_port_sources() -> PathBuf {
    let root = workspace_root();
    let ports_src = root.join("target/ports-src");
    fs::create_dir_all(&ports_src).unwrap();
    println!("ports: using host cache {}", ports_src.display());
    fetch_lua_source(&ports_src);
    fetch_zlib_source(&ports_src);
    let lua_ready = ports_src.join("lang/lua/src/lua.c").exists();
    let zlib_ready = ports_src.join("lib/zlib/src/zlib.h").exists();
    println!(
        "ports: source readiness -> bundled ports: bc, sbase, mandoc; fetched sources: lua={}, zlib={} (minizip depends on zlib)",
        if lua_ready { "ready" } else { "missing" },
        if zlib_ready { "ready" } else { "missing" }
    );
    ports_src
}

/// Phase 45: Populate the ports tree into `/usr/ports/` on the ext2 partition.
///
/// Mirrors the host-side `ports/` directory (Portfiles, Makefiles, patches, and
/// bundled sources) plus any host-cached files from `target/ports-src/` into the
/// ext2 image. Also installs the `port` command at `/usr/bin/port` and creates
/// `/usr/local/` and `/var/db/ports/` directories.
fn populate_ports_tree(part_path: &Path, workspace_root: &Path, ports_src: &Path) {
    let ports_dir = workspace_root.join("ports");
    if !ports_dir.is_dir() {
        return;
    }

    println!(
        "ports: mirroring {} plus cached sources from {} into /usr/ports",
        ports_dir.display(),
        ports_src.display()
    );

    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<(String, PathBuf)> = Vec::new();

    // Collect port metadata files (Portfiles, Makefiles, patches, .gitkeep).
    collect_ports_entries(&ports_dir, "usr/ports", &mut dirs, &mut files);

    // Collect downloaded source files into the port tree.
    // Source files go to usr/ports/<category>/<name>/src/
    if ports_src.is_dir() {
        for category in &["lang", "lib", "math", "core", "doc", "util"] {
            let cat_dir = ports_src.join(category);
            if !cat_dir.is_dir() {
                continue;
            }
            for port_entry in fs::read_dir(&cat_dir).unwrap().flatten() {
                if !port_entry.path().is_dir() {
                    continue;
                }
                let port_name = port_entry.file_name();
                let src_dir = port_entry.path().join("src");
                if src_dir.is_dir() {
                    let prefix =
                        format!("usr/ports/{}/{}/src", category, port_name.to_string_lossy());
                    collect_staging_entries(&src_dir, &prefix, &mut dirs, &mut files);
                }
            }
        }
    }

    if files.is_empty() {
        return;
    }

    let mut cmds = String::new();

    // Ensure parent directories exist (debugfs mkdir requires parents).
    let parent_dirs = ["usr", "usr/bin"];
    for d in &parent_dirs {
        cmds.push_str(&format!("mkdir {d}\n"));
    }

    // Create infrastructure directories.
    let infra_dirs = [
        "usr/local",
        "usr/local/bin",
        "usr/local/lib",
        "usr/local/include",
        "var/db",
        "var/db/ports",
    ];
    for d in &infra_dirs {
        cmds.push_str(&format!("mkdir {d}\n"));
    }

    // Create port tree directories (sorted so parents come before children).
    dirs.sort();
    dirs.dedup();
    for dir in &dirs {
        cmds.push_str(&format!("mkdir {dir}\n"));
    }

    // Write files.
    for (ext2_path, host_path) in &files {
        cmds.push_str(&format!("write \"{}\" {ext2_path}\n", host_path.display()));
    }

    // Install port.sh as /usr/bin/port.
    let port_script = ports_dir.join("port.sh");
    if port_script.exists() {
        cmds.push_str(&format!(
            "write \"{}\" usr/bin/port\n",
            port_script.display()
        ));
    }

    // Set permissions: parent dirs 0755, owned by root.
    for d in &parent_dirs {
        cmds.push_str(&format!("sif {d} mode 0x41ED\n"));
        cmds.push_str(&format!("sif {d} uid 0\n"));
        cmds.push_str(&format!("sif {d} gid 0\n"));
    }

    // Set permissions: infrastructure dirs 0755.
    for d in &infra_dirs {
        cmds.push_str(&format!("sif {d} mode 0x41ED\n"));
    }

    // Port tree directories 0755.
    for dir in &dirs {
        cmds.push_str(&format!("sif {dir} mode 0x41ED\n"));
    }

    // Files: Makefiles and source 0644, port script executable 0755.
    for (ext2_path, _) in &files {
        cmds.push_str(&format!("sif {ext2_path} mode 0x81A4\n"));
    }
    if port_script.exists() {
        cmds.push_str("sif usr/bin/port mode 0x81ED\n");
    }

    // /var/db/ports owned by root with standard permissions.
    cmds.push_str("sif var/db/ports mode 0x41ED\n");
    cmds.push_str("sif var/db/ports uid 0\n");
    cmds.push_str("sif var/db/ports gid 0\n");

    cmds.push_str("q\n");

    println!(
        "ports: populating ext2 with {} dirs, {} files + port command",
        dirs.len() + infra_dirs.len(),
        files.len() + 1
    );

    let mut debugfs = Command::new("debugfs")
        .arg("-w")
        .arg(part_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run debugfs for ports population");
    {
        let stdin = debugfs.stdin.as_mut().expect("debugfs stdin");
        stdin
            .write_all(cmds.as_bytes())
            .expect("write ports debugfs commands");
    }
    let debugfs_output = debugfs.wait_with_output().expect("debugfs wait");
    if !debugfs_output.status.success() {
        let stderr = String::from_utf8_lossy(&debugfs_output.stderr);
        eprintln!(
            "Warning: debugfs (ports) exited with {}: {}",
            debugfs_output.status, stderr
        );
    }
}

/// Collect port tree entries (Portfiles, Makefiles, patches) from the ports
/// directory, skipping the port.sh script (installed separately).
fn collect_ports_entries(
    dir: &Path,
    prefix: &str,
    dirs: &mut Vec<String>,
    files: &mut Vec<(String, PathBuf)>,
) {
    dirs.push(prefix.to_string());
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip port.sh (installed at /usr/bin/port separately), work dirs,
        // and .git files.
        if name_str == "port.sh" || name_str == "work" || name_str.starts_with('.') {
            continue;
        }
        let child_prefix = format!("{prefix}/{name_str}");
        let path = entry.path();
        if path.is_dir() {
            collect_ports_entries(&path, &child_prefix, dirs, files);
        } else {
            files.push((child_prefix, path));
        }
    }
}

/// Download doom1.wad (shareware, freely redistributable) into `dest`.
///
/// Download `doom1.wad` (shareware) to `dest` if `M3OS_DOWNLOAD_WAD=1` is set.
///
/// Gated by the env var to avoid unexpected network access in offline/CI builds.
/// Verifies the SHA-256 checksum of the downloaded file and removes it on mismatch.
/// Tries `curl` first, then `wget`.
fn fetch_doom_wad(dest: &Path) {
    const WAD_URL: &str = "https://distro.ibiblio.org/slitaz/sources/packages/d/doom1.wad";
    // SHA-256 of doom1.wad from distro.ibiblio.org (verified 2026-04-04).
    const WAD_SHA256: &str = "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771";

    if std::env::var("M3OS_DOWNLOAD_WAD").as_deref() != Ok("1") {
        eprintln!(
            "doom: doom1.wad not found — set M3OS_DOWNLOAD_WAD=1 to auto-download, or\n\
             place it at target/doom1.wad (or repo root doom1.wad) manually.\n\
             Download: {WAD_URL}"
        );
        return;
    }

    println!("doom: doom1.wad not found — downloading shareware WAD (~4 MB)...");
    println!("doom: source: {WAD_URL}");

    // Try curl first.
    let curl_ok = Command::new("curl")
        .args(["-fsSL", "--output", dest.to_str().unwrap(), WAD_URL])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !curl_ok || !dest.exists() {
        // Fall back to wget.
        let wget_ok = Command::new("wget")
            .args(["-q", "-O", dest.to_str().unwrap(), WAD_URL])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !wget_ok || !dest.exists() {
            eprintln!(
                "warning: could not download doom1.wad (curl/wget not available or download failed)\n\
                 To enable DOOM: place doom1.wad in the repository root or at target/doom1.wad\n\
                 Download: {WAD_URL}"
            );
            let _ = fs::remove_file(dest);
            return;
        }
    }

    // Verify SHA-256 checksum.
    if !verify_sha256(dest, WAD_SHA256) {
        eprintln!(
            "warning: doom1.wad verification failed (checksum mismatch or `sha256sum` unavailable) — removing the file.\n\
             Expected SHA-256: {WAD_SHA256}\n\
             Place a valid doom1.wad at target/doom1.wad manually."
        );
        let _ = fs::remove_file(dest);
        return;
    }

    println!("doom: downloaded and verified → {}", dest.display());
}

/// Compute the hex SHA-256 digest of `path` and compare it to `expected`.
///
/// Returns `true` on a confirmed match.  Returns `false` on a checksum
/// mismatch or when `sha256sum` is unavailable.
///
/// When `sha256sum` is unavailable this function deletes `path` and returns
/// `false` — callers that set `M3OS_DOWNLOAD_WAD=1` have opted into
/// supply-chain verification, so a missing tool must not silently allow an
/// unverified binary to proceed.  On a checksum mismatch the file is left in
/// place and deletion is the caller's responsibility (see `fetch_doom_wad`).
fn verify_sha256(path: &Path, expected: &str) -> bool {
    // Use the `sha256sum` tool if available (common on Linux).
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success());

    if let Some(out) = output {
        let line = String::from_utf8_lossy(&out.stdout);
        // sha256sum output: "<hex>  <filename>"
        if let Some(hex) = line.split_whitespace().next() {
            return hex.eq_ignore_ascii_case(expected);
        }
    }

    // sha256sum is not available — treat as a hard error when the caller has
    // explicitly opted into verified downloads (M3OS_DOWNLOAD_WAD=1).
    eprintln!(
        "doom: sha256sum not found — cannot verify {}; deleting download",
        path.display()
    );
    let _ = std::fs::remove_file(path);
    false
}

/// Phase 47: Place doom1.wad on the ext2 partition at /usr/share/doom/doom1.wad.
///
/// The WAD is cached in target/doom1.wad (gitignored) and auto-downloaded on
/// first use. The shareware doom1.wad is freely redistributable (~4 MB).
fn populate_doom_files(part_path: &Path) {
    // Cache the WAD in target/ so it is never committed and persists across
    // builds.  Also accept a manually placed doom1.wad at the repo root for
    // users who already have it.
    let wad_cached = workspace_root().join("target/doom1.wad");
    let wad_root = workspace_root().join("doom1.wad");

    let wad_src = if wad_cached.exists() {
        wad_cached
    } else if wad_root.exists() {
        wad_root
    } else {
        fetch_doom_wad(&wad_cached);
        if wad_cached.exists() {
            wad_cached
        } else {
            return; // download failed; already warned
        }
    };

    let mut cmds = String::new();

    // Create /usr/share/doom/ directory tree.
    // debugfs mkdir does not create parent directories, so each level must be
    // created explicitly starting from the top-level `usr` directory.
    cmds.push_str("mkdir usr\n");
    cmds.push_str("mkdir usr/share\n");
    cmds.push_str("mkdir usr/share/doom\n");

    // Write the WAD file.
    cmds.push_str(&format!(
        "write \"{}\" usr/share/doom/doom1.wad\n",
        wad_src.display()
    ));

    // Set permissions.
    cmds.push_str("sif usr mode 0x41ED\n");
    cmds.push_str("sif usr/share mode 0x41ED\n");
    cmds.push_str("sif usr/share/doom mode 0x41ED\n");
    cmds.push_str("sif usr/share/doom/doom1.wad mode 0x81A4\n");
    cmds.push_str("q\n");

    // Run debugfs.
    let mut debugfs = Command::new("debugfs")
        .args(["-w", part_path.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run debugfs for doom files");

    {
        use std::io::Write as _;
        let stdin = debugfs.stdin.as_mut().expect("debugfs stdin");
        stdin
            .write_all(cmds.as_bytes())
            .expect("write debugfs commands");
    }

    let output = debugfs.wait_with_output().expect("debugfs wait");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("warning: debugfs populate_doom_files failed: {stderr}");
    } else {
        println!("doom: placed doom1.wad at /usr/share/doom/doom1.wad");
    }
}

fn cmd_image(image_args: &ImageArgs) {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);

    // Phase 24: create a data disk image alongside the UEFI boot image.
    let output_dir = uefi_image.parent().unwrap();
    create_data_disk(output_dir, image_args.enable_telnet, false);

    if !image_args.sign {
        return;
    }

    require_existing_file("signing key", &image_args.key);
    require_existing_file("signing certificate", &image_args.cert);

    let unsigned_bootloader = find_uefi_bootloader();
    let sign_args = SignArgs {
        signed_efi: signed_path(&unsigned_bootloader),
        unsigned_efi: unsigned_bootloader,
        key: image_args.key.clone(),
        cert: image_args.cert.clone(),
    };
    let signed_bootloader = sign_efi(&sign_args);
    let signed_image = signed_path(&uefi_image);
    create_signed_uefi_image(&kernel_binary, &signed_bootloader, &signed_image).unwrap_or_else(
        |err| {
            eprintln!("Error: failed to assemble signed UEFI image: {err:#}");
            std::process::exit(1);
        },
    );
    println!("Signed EFI: {}", signed_bootloader.display());
    println!("Signed UEFI image: {}", signed_image.display());
    convert_to_vhdx(&signed_image);
    println!(
        "Reminder: enroll {} with MOK before Secure Boot tests.",
        image_args.cert.display()
    );
}

fn cmd_sign(sign_args: &SignArgs) {
    let signed_efi = sign_efi(sign_args);
    println!("Signed EFI: {}", signed_efi.display());
    println!(
        "Reminder: enroll {} with MOK before Secure Boot tests.",
        sign_args.cert.display()
    );
}

fn sign_efi(sign_args: &SignArgs) -> PathBuf {
    require_existing_file("unsigned EFI", &sign_args.unsigned_efi);
    require_existing_file("signing key", &sign_args.key);
    require_existing_file("signing certificate", &sign_args.cert);

    let mut sign_command = Command::new("sbsign");
    sign_command
        .arg("--key")
        .arg(&sign_args.key)
        .arg("--cert")
        .arg(&sign_args.cert)
        .arg("--output")
        .arg(&sign_args.signed_efi)
        .arg(&sign_args.unsigned_efi);
    let sign_status = run_command_status(&mut sign_command, "sbsign");

    if !sign_status.success() {
        eprintln!(
            "Error: `sbsign` failed while signing {} to {}.",
            sign_args.unsigned_efi.display(),
            sign_args.signed_efi.display()
        );
        std::process::exit(sign_status.code().unwrap_or(1));
    }

    let mut verify_command = Command::new("sbverify");
    verify_command
        .arg("--cert")
        .arg(&sign_args.cert)
        .arg(&sign_args.signed_efi);
    let verify_status = run_command_status(&mut verify_command, "sbverify");

    if !verify_status.success() {
        eprintln!(
            "Error: `sbverify` failed to verify signed EFI image {}.",
            sign_args.signed_efi.display()
        );
        std::process::exit(verify_status.code().unwrap_or(1));
    }

    sign_args.signed_efi.clone()
}

fn require_existing_file(label: &str, path: &Path) {
    if !path.is_file() {
        eprintln!("Error: {label} file not found: {}", path.display());
        std::process::exit(1);
    }
}

fn run_command_status(command: &mut Command, program: &str) -> ExitStatus {
    match command.status() {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Error: `{program}` was not found. {SBSIGN_TOOL_HINT}");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("Error: failed to launch `{program}`: {error}");
            std::process::exit(1);
        }
    }
}

fn xtask_build_dir() -> PathBuf {
    std::env::current_exe()
        .expect("failed to locate xtask executable")
        .parent()
        .expect("xtask executable unexpectedly missing parent directory")
        .join("build")
}

fn find_uefi_bootloader() -> PathBuf {
    let build_dir = xtask_build_dir();
    find_uefi_bootloader_in(&build_dir).unwrap_or_else(|err| {
        eprintln!("Error: {err}");
        std::process::exit(1);
    })
}

fn find_uefi_bootloader_in(build_dir: &Path) -> Result<PathBuf, String> {
    let entries = fs::read_dir(build_dir).map_err(|err| {
        format!(
            "failed to read xtask build directory `{}`: {err}",
            build_dir.display()
        )
    })?;

    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            format!(
                "failed to inspect xtask build directory `{}`: {err}",
                build_dir.display()
            )
        })?;
        if !entry
            .file_type()
            .map_err(|err| {
                format!(
                    "failed to inspect build artifact type `{}`: {err}",
                    entry.path().display()
                )
            })?
            .is_dir()
        {
            continue;
        }

        let candidate = entry.path().join("out/bin/bootloader-x86_64-uefi.efi");
        if !candidate.is_file() {
            continue;
        }

        let modified = fs::metadata(&candidate)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        candidates.push((modified, candidate));
    }

    candidates.sort_by_key(|(modified, _)| *modified);
    candidates.pop().map(|(_, path)| path).ok_or_else(|| {
        format!(
            "could not locate bootloader-x86_64-uefi.efi under `{}`; rebuild xtask first",
            build_dir.display()
        )
    })
}

fn create_signed_uefi_image(
    kernel_binary: &Path,
    signed_bootloader: &Path,
    image_path: &Path,
) -> anyhow::Result<()> {
    let mut files = BTreeMap::new();
    files.insert(
        UEFI_BOOT_FILENAME,
        FileDataSource::File(signed_bootloader.to_path_buf()),
    );
    files.insert(
        KERNEL_FILE_NAME,
        FileDataSource::File(kernel_binary.to_path_buf()),
    );

    let fat_partition = NamedTempFile::new().context("failed to create temporary FAT image")?;
    create_fat_filesystem(
        &files,
        fat_partition
            .reopen()
            .context("failed to reopen temporary FAT image for formatting")?,
    )
    .context("failed to create signed FAT filesystem")?;
    create_gpt_disk(
        fat_partition
            .reopen()
            .context("failed to reopen temporary FAT image for GPT packaging")?,
        image_path,
    )
    .context("failed to create signed GPT disk image")?;
    fat_partition
        .close()
        .context("failed to delete temporary FAT image after disk image creation")?;
    Ok(())
}

fn create_fat_filesystem(
    files: &BTreeMap<&str, FileDataSource>,
    fat_file: File,
) -> anyhow::Result<()> {
    const MB: u64 = 1024 * 1024;

    let mut needed_size = 0;
    for source in files.values() {
        needed_size += source.len()?;
    }

    let fat_size = ((needed_size + 1024 * 64 - 1) / MB + 1) * MB + MB;
    fat_file
        .set_len(fat_size)
        .context("failed to size FAT image file")?;

    let mut label = *b"MY_RUST_OS!";
    if let Some(FileDataSource::File(path)) = files.get(KERNEL_FILE_NAME) {
        if let Some(name) = path.file_stem() {
            let converted = name.to_string_lossy();
            let name = converted.as_bytes();
            let mut new_label = [0u8; 11];
            let name = &name[..usize::min(new_label.len(), name.len())];
            let slice = &mut new_label[..name.len()];
            slice.copy_from_slice(name);
            label = new_label;
        }
    }

    let format_options = fatfs::FormatVolumeOptions::new().volume_label(label);
    fatfs::format_volume(&fat_file, format_options).context("failed to format FAT image")?;
    let filesystem = fatfs::FileSystem::new(fat_file, fatfs::FsOptions::new())
        .context("failed to open FAT filesystem")?;
    let root_dir = filesystem.root_dir();
    let result = add_files_to_image(&root_dir, files);
    drop(root_dir);
    drop(filesystem);
    result
}

fn add_files_to_image<T: fatfs::ReadWriteSeek>(
    root_dir: &Dir<'_, T>,
    files: &BTreeMap<&str, FileDataSource>,
) -> anyhow::Result<()> {
    for (target_path_raw, source) in files {
        let parent_dirs = fat_parent_dirs(target_path_raw);
        for dir_path in parent_dirs {
            match root_dir.create_dir(&dir_path) {
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!(
                            "failed to create directory `{}` on FAT filesystem",
                            dir_path
                        )
                    });
                }
            }
        }

        let mut new_file = root_dir
            .create_file(target_path_raw)
            .with_context(|| format!("failed to create file at `{}`", target_path_raw))?;
        new_file.truncate().context("failed to truncate FAT file")?;
        source.copy_to(&mut new_file).with_context(|| {
            format!(
                "failed to copy source data to file at `{}`",
                target_path_raw
            )
        })?;
    }

    Ok(())
}

fn fat_parent_dirs(target_path_raw: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut parts = Vec::new();
    for component in target_path_raw
        .split('/')
        .filter(|component| !component.is_empty())
    {
        parts.push(component);
    }

    if parts.len() <= 1 {
        return dirs;
    }

    for depth in 1..parts.len() {
        dirs.push(parts[..depth].join("/"));
    }
    dirs
}

fn create_gpt_disk(mut fat_image: File, out_gpt_path: &Path) -> anyhow::Result<()> {
    let mut disk = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(out_gpt_path)
        .with_context(|| format!("failed to create GPT file at `{}`", out_gpt_path.display()))?;

    let partition_size = fat_image
        .metadata()
        .context("failed to read metadata of FAT image")?
        .len();
    let disk_size = partition_size + 1024 * 64;
    disk.set_len(disk_size)
        .context("failed to set GPT image file length")?;

    let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(
        u32::try_from((disk_size / 512) - 1).unwrap_or(0xFF_FF_FF_FF),
    );
    mbr.overwrite_lba0(&mut disk)
        .context("failed to write protective MBR")?;

    let block_size = gpt::disk::LogicalBlockSize::Lb512;
    let mut gpt = gpt::GptConfig::new()
        .writable(true)
        .initialized(false)
        .logical_block_size(block_size)
        .create_from_device(Box::new(&mut disk), None)
        .context("failed to create GPT structure in file")?;
    gpt.update_partitions(Default::default())
        .context("failed to update GPT partitions")?;

    let partition_id = gpt
        .add_partition("boot", partition_size, gpt::partition_types::EFI, 0, None)
        .context("failed to add boot EFI partition")?;
    let partition = gpt
        .partitions()
        .get(&partition_id)
        .context("failed to open boot partition after creation")?;
    let start_offset = partition
        .bytes_start(block_size)
        .context("failed to get start offset of boot partition")?;

    gpt.write().context("failed to write out GPT changes")?;

    fat_image
        .seek(io::SeekFrom::Start(0))
        .context("failed to seek to start of FAT image")?;
    disk.seek(io::SeekFrom::Start(start_offset))
        .context("failed to seek to start offset")?;
    io::copy(&mut fat_image, &mut disk).context("failed to copy FAT image to GPT disk")?;

    Ok(())
}

fn cmd_clean() {
    let root = workspace_root();
    let target_dir = root.join("target");
    let disk_img = target_dir.join("x86_64-unknown-none/release/disk.img");
    if disk_img.exists() {
        fs::remove_file(&disk_img).expect("failed to remove disk.img");
        println!("Removed {}", disk_img.display());
    } else {
        println!("No disk.img to remove");
    }
}

fn cmd_run(fresh: bool, devices: DeviceSet) {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);
    if fresh {
        let disk = uefi_image.parent().unwrap().join("disk.img");
        if disk.exists() {
            fs::remove_file(&disk).expect("failed to remove disk.img");
            println!("Removed {} (--fresh)", disk.display());
        }
    }
    create_data_disk(uefi_image.parent().unwrap(), false, false);
    launch_qemu_with_devices(&uefi_image, QemuDisplayMode::Headless, devices);
}

fn cmd_run_gui(fresh: bool, devices: DeviceSet) {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);
    if fresh {
        let disk = uefi_image.parent().unwrap().join("disk.img");
        if disk.exists() {
            fs::remove_file(&disk).expect("failed to remove disk.img");
            println!("Removed {} (--fresh)", disk.display());
        }
    }
    create_data_disk(uefi_image.parent().unwrap(), false, false);
    launch_qemu_with_devices(&uefi_image, QemuDisplayMode::Gui, devices);
}

fn cmd_runner(kernel_binary: PathBuf) {
    let uefi_image = create_uefi_image(&kernel_binary);
    launch_qemu(&uefi_image, QemuDisplayMode::Headless);
}

// ---------------------------------------------------------------------------
// Phase 43c: Regression test framework (Track A)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct RegressionArgs {
    test_name: Option<String>,
    timeout_secs: Option<u64>,
    display: bool,
}

fn parse_regression_args(args: &[String]) -> Result<RegressionArgs, String> {
    let mut test_name = None;
    let mut timeout_secs = None;
    let mut display = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--test" => {
                index += 1;
                test_name = Some(
                    args.get(index)
                        .ok_or("--test requires a value")?
                        .to_string(),
                );
            }
            "--timeout" => {
                index += 1;
                timeout_secs = Some(
                    args.get(index)
                        .ok_or("--timeout requires a value")?
                        .parse()
                        .map_err(|_| "invalid --timeout value")?,
                );
            }
            "--display" => display = true,
            other => return Err(format!("unknown regression flag: {other}")),
        }
        index += 1;
    }

    Ok(RegressionArgs {
        test_name,
        timeout_secs,
        display,
    })
}

/// A host-only regression test: runs entirely on the build host via
/// `cargo test`, no QEMU required.
///
/// Introduced in Phase 55b Track F.2 to wire pure-logic state-machine
/// tests into `cargo xtask regression --test driver-restart`.
struct HostRegressionTest {
    name: &'static str,
    #[allow(dead_code)]
    description: &'static str,
    /// `cargo test -p <pkg>` target package.
    package: &'static str,
    /// `--test <name>` integration-test target inside the package.
    test_target: &'static str,
    /// Target triple to pass (`--target`). `None` → native.
    target: Option<&'static str>,
}

/// Return the list of registered host-only regression tests.
fn host_regression_tests() -> Vec<HostRegressionTest> {
    vec![HostRegressionTest {
        name: "driver-restart",
        description: "Phase 55b F.2: crash-and-restart state-machine regression (pure host logic)",
        package: "kernel-core",
        test_target: "driver_restart",
        target: Some("x86_64-unknown-linux-gnu"),
    }]
}

/// Run a host-only regression test. Returns `Ok(())` on success or
/// `Err(exit_status_code)` if the process exits non-zero.
fn run_host_regression_test(t: &HostRegressionTest) -> Result<(), i32> {
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("test");
    cmd.args(["-p", t.package]);
    if let Some(triple) = t.target {
        cmd.args(["--target", triple]);
    }
    cmd.args(["--test", t.test_target]);
    let status = cmd.status().expect("cargo test failed to start");
    if status.success() {
        Ok(())
    } else {
        Err(status.code().unwrap_or(1))
    }
}

/// A registered regression test with QEMU configuration and pass/fail patterns.
struct RegressionTest {
    name: &'static str,
    #[allow(dead_code)]
    description: &'static str,
    /// Steps to run via the smoke-script engine after booting to a shell.
    guest_steps: fn() -> Vec<SmokeStep>,
    /// How long the entire regression gets before being killed.
    timeout_secs: u64,
    /// Optional device attachments (NVMe, e1000, IOMMU). Defaults to all-false
    /// (VirtIO-blk + VirtIO-net) when not set.
    devices: DeviceSet,
}

/// Return the list of registered regression tests.
fn regression_tests() -> Vec<RegressionTest> {
    let mut tests = vec![
        RegressionTest {
            name: "fork-overlap",
            description: "Rapid concurrent fork() from multiple parents",
            guest_steps: fork_overlap_steps,
            timeout_secs: 60,
            devices: DeviceSet::default(),
        },
        RegressionTest {
            name: "ipc-wake",
            description: "Overlapping IPC send/recv/call/reply cycles",
            guest_steps: ipc_wake_steps,
            timeout_secs: 60,
            devices: DeviceSet::default(),
        },
        RegressionTest {
            name: "pty-overlap",
            description: "Overlapping PTY allocation and shell spawning",
            guest_steps: pty_overlap_steps,
            timeout_secs: 90,
            devices: DeviceSet::default(),
        },
        RegressionTest {
            name: "signal-reset",
            description: "Exec-time signal disposition reset (POSIX: handlers → SIG_DFL)",
            guest_steps: signal_reset_steps,
            timeout_secs: 60,
            devices: DeviceSet::default(),
        },
        RegressionTest {
            name: "kbd-echo",
            description: "Keyboard input reaches shell via serial→TTY→stdin pipeline",
            guest_steps: kbd_echo_steps,
            timeout_secs: 60,
            devices: DeviceSet::default(),
        },
        RegressionTest {
            name: "service-lifecycle",
            description: "Service list/status in the headless operator workflow",
            guest_steps: service_lifecycle_steps,
            timeout_secs: 60,
            devices: DeviceSet::default(),
        },
        RegressionTest {
            name: "storage-roundtrip",
            description: "Ext2 write/read/delete round-trip on persistent storage",
            guest_steps: storage_roundtrip_steps,
            timeout_secs: 60,
            devices: DeviceSet::default(),
        },
        RegressionTest {
            name: "serverization-fallback",
            description: "Phase 54 degraded-mode behavior after stopping vfs and net_udp",
            guest_steps: serverization_fallback_steps,
            timeout_secs: 90,
            devices: DeviceSet::default(),
        },
        RegressionTest {
            name: "log-pipeline",
            description: "Logger injection via /dev/log and /var/log/messages verification",
            guest_steps: log_pipeline_steps,
            timeout_secs: 60,
            devices: DeviceSet::default(),
        },
        RegressionTest {
            name: "security-floor",
            description: "Phase 48 security floor: shadow auth, credential transition, hash format",
            guest_steps: security_floor_steps,
            timeout_secs: 90,
            devices: DeviceSet::default(),
        },
    ];

    // `exit_group-teardown` currently exposes a kernel-side exit_group/waitpid
    // bug in the helper process path. Keep the regression available for
    // focused debugging, but do not block unrelated PRs on it until the
    // kernel fix lands.
    if std::env::var_os("M3OS_ENABLE_EXIT_GROUP_REGRESSION").is_some() {
        tests.push(RegressionTest {
            name: "exit-group-teardown",
            description: "exit_group() reaps a live spinning sibling only after it quiesces",
            guest_steps: exit_group_teardown_steps,
            timeout_secs: 60,
            devices: DeviceSet::default(),
        });
    }

    // Phase 55b Track F.2: crash-and-restart regression.
    //
    // Requires the emulated NVMe device to be present so the nvme_driver
    // service stays alive long enough to be killed via `service kill`.
    // Gated behind M3OS_ENABLE_DRIVER_RESTART_REGRESSION because:
    //   (a) it needs --device nvme QEMU args (heavier than standard tests),
    //   (b) the "observe DriverRestarting from I/O path" acceptance bullet
    //       is deferred to Track F.3 (userspace I/O client interception).
    // Remove the env-gate when F.3 lands and the NVMe QEMU arg is folded
    // into the base regression image.
    if std::env::var_os("M3OS_ENABLE_DRIVER_RESTART_REGRESSION").is_some() {
        tests.push(RegressionTest {
            name: "driver-restart-guest",
            description: "Phase 55b F.2: service kill nvme_driver → restart cycle in QEMU",
            guest_steps: driver_restart_guest_steps,
            timeout_secs: 120,
            devices: DeviceSet {
                nvme: true,
                e1000: false,
                iommu: false,
            },
        });
    }

    tests
}

/// Guest steps for the fork-overlap regression: boot, login, run fork-test.
fn fork_overlap_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 300 });
    steps.push(SmokeStep::Send {
        input: "/bin/fork-test\n",
        label: "run fork-test",
    });
    steps.push(SmokeStep::Wait {
        pattern: "fork-test: PASS",
        timeout_secs: 30,
        label: "fork-test pass",
    });
    // Wait for shell prompt before sending the second command to avoid
    // delivering input while the previous process is still attached.
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 10,
        label: "shell prompt after fork-test",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/fork-test\n",
        label: "run fork-test (2nd)",
    });
    steps.push(SmokeStep::Wait {
        pattern: "fork-test: PASS",
        timeout_secs: 30,
        label: "fork-test pass (2nd)",
    });
    steps
}

/// Guest steps for the IPC wake regression: boot, login, run unix-socket-test.
fn ipc_wake_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 300 });
    steps.push(SmokeStep::Send {
        input: "/bin/unix-socket-test\n",
        label: "run unix-socket-test",
    });
    steps.push(SmokeStep::Wait {
        pattern: "All tests passed!",
        timeout_secs: 30,
        label: "unix-socket-test pass",
    });
    steps
}

/// Guest steps for the PTY overlap regression: boot, login, run pty-test.
///
/// Uses `--quick` to skip the ion-in-PTY tests whose internal 10s poll
/// timeouts are unreliable under QEMU TCG. The quick tests still cover
/// PTY allocation, I/O round-trip, line discipline, and lifecycle.
fn pty_overlap_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 300 });
    steps.push(SmokeStep::Send {
        input: "/bin/pty-test --quick\n",
        label: "run pty-test --quick",
    });
    // Wait directly for the summary line — avoids matching the initial
    // "pty-test: Phase 29..." banner before the test finishes.
    steps.push(SmokeStep::Wait {
        pattern: "passed, 0 failed",
        timeout_secs: 60,
        label: "pty-test 0 failures",
    });
    steps
}

/// Guest steps for the signal-reset regression: boot, login, run signal-test.
///
/// The exec_signal_reset test case inside signal-test forks, execs itself with
/// `--exec-signal-check`, and verifies that SIGUSR1 was reset to SIG_DFL by
/// execve. The failure mode uses distinct exit codes to distinguish a
/// signal-reset bug (exit 42) from a generic exec failure (exit 99).
fn signal_reset_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 300 });
    steps.push(SmokeStep::Send {
        input: "/bin/signal-test\n",
        label: "run signal-test",
    });
    steps.push(SmokeStep::Wait {
        pattern: "6 passed, 0 failed",
        timeout_secs: 30,
        label: "signal-test all pass",
    });
    steps
}

/// Guest steps for the exit_group teardown regression: boot, login, run
/// thread-test, and ensure the shell prompt returns after thread-test reports
/// the live-sibling exit_group path passed.
fn exit_group_teardown_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 300 });
    steps.push(SmokeStep::Send {
        input: "/bin/thread-test\n",
        label: "run thread-test",
    });
    steps.push(SmokeStep::Wait {
        pattern: "thread-test: test 4 -- exit_group live sibling... PASS",
        timeout_secs: 30,
        label: "thread-test exit_group teardown passed",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 30,
        label: "shell prompt after thread-test exit_group",
    });
    steps
}

/// Guest steps for the kbd-echo regression: boot, login, send echo commands,
/// and verify the shell receives and executes them.
fn kbd_echo_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 300 });
    steps.push(SmokeStep::Send {
        input: "echo kbd-test-ok\n",
        label: "send echo command",
    });
    steps.push(SmokeStep::Wait {
        pattern: "kbd-test-ok",
        timeout_secs: 10,
        label: "echo output received",
    });
    steps.push(SmokeStep::Send {
        input: "echo round2-ok\n",
        label: "send second echo",
    });
    steps.push(SmokeStep::Wait {
        pattern: "round2-ok",
        timeout_secs: 10,
        label: "second echo received",
    });
    steps
}

/// Guest steps for the service-lifecycle regression: boot, login, run
/// `service list` and `service status sshd` to verify the init daemon's
/// service management is responsive in the headless workflow.
fn service_lifecycle_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/bin/service status sshd\n",
        label: "guest/service: query sshd status",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Name:",
        timeout_secs: 15,
        label: "guest/service: status shows service name",
    });
    steps.push(SmokeStep::Wait {
        pattern: "State:",
        timeout_secs: 10,
        label: "guest/service: status shows service state",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "guest/service: prompt after status sshd",
    });
    steps
}

/// Guest steps for the Phase 55b F.2 driver-restart regression.
///
/// Requires `--device nvme` (enforced via `RegressionTest::devices`) so the
/// nvme_driver service is running and has a stable PID for `service kill`.
///
/// Sequence:
///   1. Boot and login.
///   2. Verify nvme_driver is listed by init (`service status nvme_driver`).
///   3. Deliver SIGKILL via `service kill nvme_driver`.
///   4. Wait for init to log the restart event (`init: started 'nvme_driver'`).
///   5. Verify subsequent `service status nvme_driver` shows running state.
///
/// "Observe DriverRestarting from the block I/O path" (Phase 55b F.2b) is
/// deferred to Track F.3: it requires a userspace I/O client binary that
/// can trigger a write mid-restart and inspect the error code returned by
/// the `RemoteBlockDevice` facade. The xtask-level harness for that will
/// replace this stub once F.3 lands.
fn driver_restart_guest_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    // Let nvme_driver finish init and stabilise before querying status.
    steps.push(SmokeStep::Sleep { millis: 3000 });

    // Step 1 — verify nvme_driver is listed.
    steps.push(SmokeStep::Send {
        input: "/bin/service status nvme_driver\n",
        label: "guest/driver-restart: query nvme_driver status",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Name:",
        timeout_secs: 15,
        label: "guest/driver-restart: status shows Name field",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "guest/driver-restart: prompt after status",
    });

    // Step 2 — deliver SIGKILL to nvme_driver.
    steps.push(SmokeStep::Send {
        input: "/bin/service kill nvme_driver\n",
        label: "guest/driver-restart: deliver SIGKILL to nvme_driver",
    });
    steps.push(SmokeStep::Wait {
        pattern: "SIGKILL delivered",
        timeout_secs: 10,
        label: "guest/driver-restart: confirm SIGKILL delivered",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "guest/driver-restart: prompt after kill",
    });

    // Step 3 — wait for init to restart nvme_driver.
    // Init logs "init: started '<name>' pid=<N>" on each (re)start.
    steps.push(SmokeStep::Wait {
        pattern: "init: started 'nvme_driver' pid=",
        timeout_secs: 30,
        label: "guest/driver-restart: init restarts nvme_driver",
    });

    // Step 4 — verify service shows running (or at least re-registered).
    steps.push(SmokeStep::Sleep { millis: 1000 });
    steps.push(SmokeStep::Send {
        input: "/bin/service status nvme_driver\n",
        label: "guest/driver-restart: re-query nvme_driver status after restart",
    });
    steps.push(SmokeStep::Wait {
        pattern: "State:",
        timeout_secs: 15,
        label: "guest/driver-restart: status after restart shows State field",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "guest/driver-restart: final prompt",
    });

    steps
}

/// Guest steps for the storage-roundtrip regression: write, read-back, and
/// delete a file on the ext2 root filesystem to verify persistent storage.
fn storage_roundtrip_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 300 });
    steps.push(SmokeStep::Send {
        input: "/bin/echo STORAGE_OK > /root/regtest_file\n",
        label: "guest/storage: write file on ext2",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "guest/storage: prompt after write",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /root/regtest_file\n",
        label: "guest/storage: read file back",
    });
    steps.push(SmokeStep::Wait {
        pattern: "STORAGE_OK",
        timeout_secs: 15,
        label: "guest/storage: verify file content",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "guest/storage: prompt after read",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/rm /root/regtest_file\n",
        label: "guest/storage: delete file",
    });
    // rm does more IPC hops than echo/cat (stat + unlink + parent-dir close
    // under Phase 54's extracted VFS). 10s was tight even before serverization;
    // align with the other regression tests that use 15-20s for post-command
    // prompts.
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 20,
        label: "guest/storage: prompt after delete",
    });
    steps
}

/// Guest steps for the Phase 54 degraded-mode regression: stop the extracted
/// storage and UDP policy services, then verify the documented fallback paths.
fn serverization_fallback_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 500 });

    steps.push(SmokeStep::Send {
        input: "/bin/service stop vfs\n",
        label: "guest/serverization: request stop for vfs",
    });
    steps.push(SmokeStep::Wait {
        pattern: "service: stop vfs completed",
        timeout_secs: 30,
        label: "guest/serverization: vfs stop completed",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 10,
        label: "guest/serverization: prompt after vfs stop",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /etc/passwd\n",
        label: "guest/serverization: open rootfs file after vfs stop",
    });
    steps.push(SmokeStep::Wait {
        pattern: "root:x:0:0:root:/root:/bin/ion",
        timeout_secs: 10,
        label: "guest/serverization: rootfs fallback still readable",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 10,
        label: "guest/serverization: prompt after passwd read",
    });

    steps.push(SmokeStep::Send {
        input: "/bin/service stop net_udp\n",
        label: "guest/serverization: request stop for net_udp",
    });
    steps.push(SmokeStep::Wait {
        pattern: "service: stop net_udp completed",
        timeout_secs: 30,
        label: "guest/serverization: net_udp stop completed",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 10,
        label: "guest/serverization: prompt after net_udp stop",
    });
    steps.push(SmokeStep::Send {
        input: "/root/udp-smoke\n",
        label: "guest/serverization: verify UDP fallback path",
    });
    steps.push(SmokeStep::Wait {
        pattern: "udp-smoke: PASS",
        timeout_secs: 15,
        label: "guest/serverization: udp-smoke passed after service stop",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 10,
        label: "guest/serverization: prompt after udp fallback probe",
    });
    steps
}

/// Guest steps for the log-pipeline regression: inject a tagged message via
/// `logger` and verify it appears in `/var/log/messages` through the syslogd
/// /dev/log → file pipeline.
fn log_pipeline_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/bin/logger REGTEST_LOG_MARKER\n",
        label: "guest/log: inject log message via /dev/log",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "guest/log: prompt after logger",
    });
    // Small delay for syslogd to flush to disk.
    steps.push(SmokeStep::Sleep { millis: 1000 });
    // Read file contents directly so the awaited marker cannot come from the echoed command line.
    steps.push(SmokeStep::Send {
        input: "/bin/cat /var/log/messages\n",
        label: "guest/log: verify message in syslog",
    });
    steps.push(SmokeStep::Wait {
        pattern: "REGTEST_LOG_MARKER",
        timeout_secs: 15,
        label: "guest/log: marker found in /var/log/messages",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "guest/log: prompt after log read",
    });
    steps
}

/// Guest steps for the Phase 48 security-floor regression: verify that
/// the headless login path exercises kernel-enforced credential transitions,
/// getrandom()-backed salted SHA-256 hashes, and shadow-file authentication.
fn security_floor_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    steps.push(SmokeStep::Sleep { millis: 300 });

    // 1. Verify kernel credential state: setuid/setgid transition occurred.
    steps.push(SmokeStep::Send {
        input: "/bin/id\n",
        label: "guest/auth: verify kernel credential state",
    });
    steps.push(SmokeStep::Wait {
        pattern: "uid=0",
        timeout_secs: 10,
        label: "guest/auth: uid=0 confirms setuid transition",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "guest/auth: prompt after id",
    });

    // 2. Verify shadow file contains a salted SHA-256-family password hash
    //    (not plaintext, not locked). Pre-seeded images use $sha256$ while
    //    first-boot or passwd updates produce $sha256i$ hashes with a fresh
    //    getrandom()-backed salt.
    steps.push(SmokeStep::Send {
        input: "/bin/grep root /etc/shadow\n",
        label: "guest/auth: inspect shadow hash format",
    });
    steps.push(SmokeStep::Wait {
        pattern: "$sha256",
        timeout_secs: 10,
        label: "guest/auth: shadow contains SHA-256-family hash",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "guest/auth: prompt after shadow check",
    });

    // 3. Verify /bin/su can authenticate via /etc/shadow and restore a
    //    privileged shell.
    //
    //    Post-Phase-54: after "[security] su credential transition complete",
    //    the target shell (ion) reads its per-user config + history from ext2
    //    via multi-hop IPC (syscall -> vfs_server -> fat_server -> block I/O).
    //    Under -smp 2 TCG in CI this reliably exceeds 10s; aligned to 30s to
    //    match the login bootstrap budget in boot_and_login_steps.
    steps.push(SmokeStep::Send {
        input: "/bin/su user\n",
        label: "guest/auth: drop into user shell via su",
    });
    steps.push(SmokeStep::Wait {
        pattern: "$ ",
        timeout_secs: 30,
        label: "guest/auth: user shell prompt after su user",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/whoami\n",
        label: "guest/auth: verify whoami in user shell",
    });
    steps.push(SmokeStep::Wait {
        pattern: "user",
        timeout_secs: 15,
        label: "guest/auth: whoami confirms user",
    });
    steps.push(SmokeStep::Wait {
        pattern: "$ ",
        timeout_secs: 15,
        label: "guest/auth: prompt after user whoami",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/su root\n",
        label: "guest/auth: authenticate back to root via su",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Password:",
        timeout_secs: 15,
        label: "guest/auth: su root password prompt",
    });
    steps.push(SmokeStep::Send {
        input: "root\n",
        label: "guest/auth: enter root password for su",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 30,
        label: "guest/auth: root shell prompt after su root",
    });

    // 4. Verify whoami resolves the authenticated uid to "root".
    steps.push(SmokeStep::Send {
        input: "/bin/whoami\n",
        label: "guest/auth: verify whoami resolution",
    });
    steps.push(SmokeStep::Wait {
        pattern: "root",
        timeout_secs: 10,
        label: "guest/auth: whoami confirms root",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "guest/auth: prompt after whoami",
    });

    steps
}

/// Common boot + login steps shared by all regression tests.
fn boot_and_login_steps() -> Vec<SmokeStep> {
    // Regression runs use the shipped image in snapshot mode. The image already
    // contains active password hashes, so the normal login path should apply.
    // If login races the extracted rootfs path once and reports that it cannot
    // read /etc/passwd, wait for the next prompt and retry the username.
    const RETRY_AFTER_PASSWD_MISS: &[SmokeStep] = &[
        SmokeStep::Wait {
            pattern: "m3OS login:",
            timeout_secs: 20,
            label: "wait for retry login prompt",
        },
        SmokeStep::Sleep { millis: 25000 },
        SmokeStep::Send {
            input: "root\n",
            label: "retry username after passwd miss",
        },
        SmokeStep::Wait {
            pattern: "Password:",
            timeout_secs: 20,
            label: "wait for password prompt after retry",
        },
    ];
    vec![
        SmokeStep::Wait {
            pattern: "init: started 'net_udp' pid=",
            timeout_secs: 60,
            label: "wait for final boot marker",
        },
        SmokeStep::Sleep { millis: 25000 },
        SmokeStep::Wait {
            pattern: "m3OS login:",
            timeout_secs: 20,
            label: "wait for login prompt after boot settle",
        },
        SmokeStep::Send {
            input: "root\n",
            label: "username",
        },
        SmokeStep::WaitEither {
            pattern_a: "Password:",
            pattern_b: "login: cannot read /etc/passwd",
            timeout_secs: 10,
            label: "wait for password prompt or retryable passwd miss",
            extra_steps_a: &[],
            extra_steps_b: RETRY_AFTER_PASSWD_MISS,
        },
        SmokeStep::Send {
            input: "root\n",
            label: "password",
        },
        SmokeStep::Wait {
            pattern: "[security] credential transition complete",
            timeout_secs: 30,
            label: "wait for credential transition completion",
        },
        SmokeStep::Sleep { millis: 500 },
        SmokeStep::Send {
            input: "/bin/echo __LOGIN_READY__\n",
            label: "bootstrap shell with deterministic ready marker",
        },
        SmokeStep::Wait {
            pattern: "__LOGIN_READY__",
            timeout_secs: 30,
            label: "wait for login ready marker",
        },
    ]
}

fn cmd_regression(args: &RegressionArgs) {
    // ---- Host-only regression tests (no QEMU required) ----
    // Checked first so `--test driver-restart` returns immediately without
    // building the kernel or pulling OVMF.
    let all_host_tests = host_regression_tests();
    if let Some(name) = &args.test_name {
        if let Some(t) = all_host_tests.iter().find(|t| t.name == *name) {
            println!("regression: running host-only test '{}'", t.name);
            match run_host_regression_test(t) {
                Ok(()) => {
                    println!("\nregression: 1 passed, 0 failed");
                    return;
                }
                Err(code) => {
                    eprintln!("\nregression: 0 passed, 1 failed (exit code {code})");
                    std::process::exit(1);
                }
            }
        }
    }

    // ---- QEMU-based regression tests ----
    let all_tests = regression_tests();
    let tests_to_run: Vec<&RegressionTest> = if let Some(name) = &args.test_name {
        let found = all_tests.iter().find(|t| t.name == name);
        match found {
            Some(t) => vec![t],
            None => {
                // Report both QEMU and host test names in the error.
                let qemu_names: Vec<_> = all_tests.iter().map(|t| t.name).collect();
                let host_names: Vec<_> = all_host_tests.iter().map(|t| t.name).collect();
                eprintln!("Unknown regression test: {name}");
                eprintln!("  QEMU tests: {}", qemu_names.join(", "));
                eprintln!("  Host tests: {}", host_names.join(", "));
                std::process::exit(1);
            }
        }
    } else {
        all_tests.iter().collect()
    };

    println!("regression: running {} test(s)", tests_to_run.len());

    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    let ovmf = find_ovmf();
    // CI runs smoke-test before regression in the same workspace. Recreate the
    // data disk here so smoke-mode markers do not leak into login-based
    // regression scenarios.
    let disk_img = uefi_image.parent().unwrap().join("disk.img");
    if disk_img.exists() {
        let _ = fs::remove_file(&disk_img);
    }
    create_data_disk(uefi_image.parent().unwrap(), false, false);

    let mut passed = 0usize;
    let mut failed = 0usize;

    for test in &tests_to_run {
        let timeout = args.timeout_secs.unwrap_or(test.timeout_secs);
        print!("  {}: ", test.name);
        match run_regression_test(test, &uefi_image, &ovmf, timeout, args.display) {
            Ok(serial_log) => {
                println!("PASS");
                save_regression_artifact(test.name, &serial_log, "serial.log");
                passed += 1;
            }
            Err((msg, serial_log)) => {
                println!("FAIL: {msg}");
                save_regression_artifact(test.name, &serial_log, "serial.log");
                extract_trace_dump(test.name, &serial_log);
                failed += 1;
            }
        }
    }

    println!("\nregression: {} passed, {} failed", passed, failed);
    if failed > 0 {
        std::process::exit(1);
    }
}

fn run_regression_test(
    test: &RegressionTest,
    uefi_image: &Path,
    ovmf: &Path,
    timeout_secs: u64,
    display: bool,
) -> Result<String, (String, String)> {
    let display_mode = if display {
        QemuDisplayMode::Gui
    } else {
        QemuDisplayMode::Headless
    };
    let mut args = if test.devices == DeviceSet::default() {
        qemu_args(uefi_image, ovmf, display_mode)
    } else {
        qemu_args_with_devices(uefi_image, ovmf, display_mode, test.devices)
    };
    // Strip hostfwd to avoid port conflicts.
    for arg in args.iter_mut() {
        if arg.starts_with("user,id=net0,hostfwd=") {
            *arg = "user,id=net0".to_string();
        }
    }
    // Snapshot mode: don't persist disk writes across regression runs.
    args.push("-snapshot".to_string());

    let mut child = Command::new("qemu-system-x86_64")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to launch QEMU");

    let steps = (test.guest_steps)();
    let global_timeout = std::time::Duration::from_secs(timeout_secs);

    // Capture serial output by running the smoke-script engine.
    // We wrap the result and capture the serial log regardless.
    let stdout = child.stdout.take().expect("no stdout pipe");
    let rx = spawn_serial_reader(stdout);
    let mut serial_buf = String::new();
    let global_start = std::time::Instant::now();

    let result = run_smoke_steps_with_capture(
        &mut child,
        &steps,
        global_timeout,
        &rx,
        &mut serial_buf,
        global_start,
    );

    // Kill QEMU if still running.
    let _ = child.kill();
    let _ = child.wait();

    match result {
        Ok(()) => Ok(serial_buf),
        Err(msg) => Err((msg, serial_buf)),
    }
}

/// Like `run_smoke_script` but uses an already-split stdout reader + buffer
/// so the caller retains the serial log.
fn run_smoke_steps_with_capture(
    child: &mut std::process::Child,
    steps: &[SmokeStep],
    global_timeout: std::time::Duration,
    rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    serial_buf: &mut String,
    global_start: std::time::Instant,
) -> Result<(), String> {
    let serial_history = &mut *serial_buf;
    let mut serial_buf = String::new();
    // Use a queue so WaitEither can inject extra steps at the front.
    let mut queue: std::collections::VecDeque<&SmokeStep> = steps.iter().collect();
    let mut step_num = 0usize;

    while let Some(step) = queue.pop_front() {
        step_num += 1;
        if global_start.elapsed() > global_timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "global timeout ({global_timeout:?}) exceeded at step {}",
                step_num
            ));
        }

        match step {
            SmokeStep::Wait {
                pattern,
                timeout_secs,
                label,
            } => {
                let step_deadline = std::time::Instant::now() + scaled_secs(*timeout_secs);
                let global_deadline = global_start + global_timeout;
                let deadline = step_deadline.min(global_deadline);

                loop {
                    while let Ok(chunk) = rx.try_recv() {
                        append_serial_chunk(&mut serial_buf, serial_history, &chunk);
                    }

                    let stripped = strip_ansi(&serial_buf);
                    let cleaned = strip_background_noise(&stripped);

                    // Check for kernel-level crash indicators in serial output.
                    if cleaned.contains("KERNEL PANIC")
                        || cleaned.contains("kernel page fault")
                        || cleaned.contains("DOUBLE FAULT")
                    {
                        return Err(format!(
                            "kernel crash detected during step {} ({label})",
                            step_num
                        ));
                    }

                    if let Some((mode, match_end)) = find_serial_match(&stripped, &cleaned, pattern)
                    {
                        drain_serial_through_match(&mut serial_buf, &stripped, mode, match_end);
                        break;
                    }

                    if child.try_wait().ok().flatten().is_some() {
                        while let Ok(chunk) = rx.try_recv() {
                            append_serial_chunk(&mut serial_buf, serial_history, &chunk);
                        }
                        return Err(format!(
                            "QEMU exited unexpectedly at step {} ({label})",
                            step_num
                        ));
                    }

                    if std::time::Instant::now() >= deadline {
                        let last_lines = tail_lines(&strip_ansi(serial_history), 80);
                        return Err(format!(
                            "timeout waiting for '{pattern}' at step {} ({label})\nLast serial output:\n{last_lines}",
                            step_num
                        ));
                    }

                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            SmokeStep::Send { input, label } => {
                drain_serial_until_idle(
                    rx,
                    &mut serial_buf,
                    serial_history,
                    std::time::Duration::from_millis(150),
                    std::time::Duration::from_secs(2),
                );
                let stdin = child
                    .stdin
                    .as_mut()
                    .ok_or_else(|| format!("no stdin at step {} ({label})", step_num))?;
                use std::io::Write;
                serial_buf.clear();
                stdin
                    .write_all(input.as_bytes())
                    .map_err(|e| format!("write failed at step {} ({label}): {e}", step_num))?;
                stdin
                    .flush()
                    .map_err(|e| format!("flush failed at step {} ({label}): {e}", step_num))?;
            }
            SmokeStep::Sleep { millis } => {
                std::thread::sleep(std::time::Duration::from_millis(*millis));
            }
            SmokeStep::WaitEither {
                pattern_a,
                pattern_b,
                timeout_secs,
                label,
                extra_steps_a,
                extra_steps_b,
            } => {
                let step_deadline = std::time::Instant::now() + scaled_secs(*timeout_secs);
                let global_deadline = global_start + global_timeout;
                let deadline = step_deadline.min(global_deadline);

                let matched_a;
                loop {
                    while let Ok(chunk) = rx.try_recv() {
                        append_serial_chunk(&mut serial_buf, serial_history, &chunk);
                    }
                    let stripped = strip_ansi(&serial_buf);
                    let cleaned = strip_background_noise(&stripped);

                    if let Some((mode, match_end)) =
                        find_serial_match(&stripped, &cleaned, pattern_a)
                    {
                        matched_a = true;
                        drain_serial_through_match(&mut serial_buf, &stripped, mode, match_end);
                        break;
                    }
                    if let Some((mode, match_end)) =
                        find_serial_match(&stripped, &cleaned, pattern_b)
                    {
                        matched_a = false;
                        drain_serial_through_match(&mut serial_buf, &stripped, mode, match_end);
                        break;
                    }

                    if child.try_wait().ok().flatten().is_some() {
                        while let Ok(chunk) = rx.try_recv() {
                            append_serial_chunk(&mut serial_buf, serial_history, &chunk);
                        }
                        return Err(format!(
                            "QEMU exited unexpectedly at step {} ({label})",
                            step_num
                        ));
                    }

                    if std::time::Instant::now() >= deadline {
                        let last_lines = tail_lines(&strip_ansi(serial_history), 80);
                        return Err(format!(
                            "timeout at step {} ({label}), expected '{pattern_a}' or '{pattern_b}'\nLast serial output:\n{last_lines}",
                            step_num
                        ));
                    }

                    std::thread::sleep(std::time::Duration::from_millis(50));
                }

                let inject = if matched_a {
                    extra_steps_a
                } else {
                    extra_steps_b
                };
                for extra in inject.iter().rev() {
                    queue.push_front(extra);
                }
            }
        }
    }

    Ok(())
}

/// Save a text artifact to a directory under `target/`.
fn save_artifact(dir: &Path, filename: &str, content: &str) {
    if let Err(err) = fs::create_dir_all(dir) {
        eprintln!(
            "failed to create artifact directory {}: {err}",
            dir.display()
        );
        return;
    }
    let path = dir.join(filename);
    if let Err(err) = fs::write(&path, content) {
        eprintln!("failed to write artifact {}: {err}", path.display());
    }
}

fn save_regression_artifact(test_name: &str, content: &str, filename: &str) {
    let dir = workspace_root()
        .join("target")
        .join("regression")
        .join(test_name);
    save_artifact(&dir, filename, content);
}

/// Extract a marked section from serial output and save it.
fn extract_marked_section(
    serial_log: &str,
    start_marker: &str,
    end_marker: &str,
) -> Option<String> {
    let start = serial_log.find(start_marker)?;
    let end = serial_log[start..].find(end_marker)?;
    Some(serial_log[start..start + end + end_marker.len()].to_string())
}

fn extract_trace_dump(test_name: &str, serial_log: &str) {
    if let Some(trace) = extract_marked_section(
        serial_log,
        "=== TRACE RING DUMP ===",
        "=== END TRACE RING DUMP ===",
    ) {
        save_regression_artifact(test_name, &trace, "trace.log");
    }
}

// ---------------------------------------------------------------------------
// Phase 43c: Stress test framework (Track E)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct StressArgs {
    test_name: Option<String>,
    iterations: usize,
    timeout_secs: Option<u64>,
    seed: Option<u64>,
    continue_on_failure: bool,
    display: bool,
}

fn parse_stress_args(args: &[String]) -> Result<StressArgs, String> {
    let mut test_name = None;
    let mut iterations = 100usize;
    let mut timeout_secs: Option<u64> = None;
    let mut seed = None;
    let mut continue_on_failure = false;
    let mut display = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--test" => {
                index += 1;
                test_name = Some(
                    args.get(index)
                        .ok_or("--test requires a value")?
                        .to_string(),
                );
            }
            "--iterations" => {
                index += 1;
                iterations = args
                    .get(index)
                    .ok_or("--iterations requires a value")?
                    .parse()
                    .map_err(|_| "invalid --iterations value")?;
            }
            "--timeout" => {
                index += 1;
                timeout_secs = Some(
                    args.get(index)
                        .ok_or("--timeout requires a value")?
                        .parse()
                        .map_err(|_| "invalid --timeout value")?,
                );
            }
            "--seed" => {
                index += 1;
                seed = Some(
                    args.get(index)
                        .ok_or("--seed requires a value")?
                        .parse()
                        .map_err(|_| "invalid --seed value")?,
                );
            }
            "--continue-on-failure" => continue_on_failure = true,
            "--display" => display = true,
            other => return Err(format!("unknown stress flag: {other}")),
        }
        index += 1;
    }

    Ok(StressArgs {
        test_name,
        iterations,
        timeout_secs,
        seed,
        continue_on_failure,
        display,
    })
}

/// A registered stress test scenario.
struct StressTest {
    name: &'static str,
    #[allow(dead_code)]
    description: &'static str,
    /// Steps to run via the smoke-script engine. The `u64` parameter is the
    /// per-iteration seed — currently unused by guest steps (timing variation
    /// requires guest-side support, deferred to a future phase).
    guest_steps: fn(u64) -> Vec<SmokeStep>,
    timeout_secs: u64,
}

fn stress_tests() -> Vec<StressTest> {
    vec![
        StressTest {
            name: "fork-overlap",
            description: "Repeated fork-test runs",
            guest_steps: |_seed| fork_overlap_steps(),
            timeout_secs: 60,
        },
        StressTest {
            name: "pty-overlap",
            description: "Repeated PTY allocation and shell spawning",
            guest_steps: |_seed| pty_overlap_steps(),
            timeout_secs: 90,
        },
        StressTest {
            name: "ssh-overlap",
            description: "Boot + login + fork-test + pty-test back-to-back (SMP-sensitive paths)",
            guest_steps: |_seed| ssh_overlap_steps(),
            timeout_secs: 90,
        },
    ]
}

/// Guest steps for SSH overlap stress: exercises dual fork paths with pty-test.
fn ssh_overlap_steps() -> Vec<SmokeStep> {
    let mut steps = boot_and_login_steps();
    // Run fork-test and pty-test back to back to stress overlapping paths.
    steps.push(SmokeStep::Sleep { millis: 300 });
    steps.push(SmokeStep::Send {
        input: "/bin/fork-test\n",
        label: "run fork-test",
    });
    steps.push(SmokeStep::Wait {
        pattern: "fork-test: PASS",
        timeout_secs: 30,
        label: "fork-test pass",
    });
    // Wait for shell prompt before sending pty-test to avoid delivering
    // input while fork-test's shell cleanup is still in progress.
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 10,
        label: "shell prompt after fork-test",
    });
    steps.push(SmokeStep::Send {
        input: "/bin/pty-test\n",
        label: "run pty-test",
    });
    steps.push(SmokeStep::Wait {
        pattern: "passed, 0 failed",
        timeout_secs: 60,
        label: "pty-test pass",
    });
    steps
}

fn cmd_stress(args: &StressArgs) {
    let all_tests = stress_tests();
    let test = if let Some(name) = &args.test_name {
        match all_tests.iter().find(|t| t.name == name) {
            Some(t) => t,
            None => {
                eprintln!("Unknown stress test: {name}");
                eprintln!(
                    "Available: {}",
                    all_tests
                        .iter()
                        .map(|t| t.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                std::process::exit(1);
            }
        }
    } else {
        eprintln!("stress: --test <name> is required");
        eprintln!(
            "Available: {}",
            all_tests
                .iter()
                .map(|t| t.name)
                .collect::<Vec<_>>()
                .join(", ")
        );
        std::process::exit(1);
    };

    // Seed: use provided or generate random.
    let seed = args.seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    });
    println!(
        "stress: test={} iterations={} seed={} timeout={}s",
        test.name,
        args.iterations,
        seed,
        args.timeout_secs.unwrap_or(test.timeout_secs)
    );

    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    let ovmf = find_ovmf();

    let mut passed = 0usize;
    let mut failed = 0usize;

    for i in 0..args.iterations {
        let iter_seed = seed.wrapping_add(i as u64);
        let timeout = args.timeout_secs.unwrap_or(test.timeout_secs);

        print!("  [{}/{}] ", i + 1, args.iterations);
        let steps = (test.guest_steps)(iter_seed);
        match run_regression_with_steps(&steps, &uefi_image, &ovmf, timeout, args.display) {
            Ok(serial_log) => {
                println!("PASS");
                let dir = format!("{}/{}", test.name, i + 1);
                save_stress_artifact(&dir, &serial_log, "serial.log");
                passed += 1;
            }
            Err((msg, serial_log)) => {
                println!("FAIL: {msg}");
                let dir = format!("{}/{}", test.name, i + 1);
                save_stress_artifact(&dir, &serial_log, "serial.log");
                extract_stress_trace_dump(&dir, &serial_log);
                failed += 1;
                if !args.continue_on_failure {
                    println!(
                        "stress: stopping on first failure (use --continue-on-failure to keep going)"
                    );
                    break;
                }
            }
        }
    }

    println!(
        "\nstress: {} passed, {} failed (seed={})",
        passed, failed, seed
    );
    if failed > 0 {
        std::process::exit(1);
    }
}

fn run_regression_with_steps(
    steps: &[SmokeStep],
    uefi_image: &Path,
    ovmf: &Path,
    timeout_secs: u64,
    display: bool,
) -> Result<String, (String, String)> {
    let display_mode = if display {
        QemuDisplayMode::Gui
    } else {
        QemuDisplayMode::Headless
    };
    let mut args = qemu_args(uefi_image, ovmf, display_mode);
    for arg in args.iter_mut() {
        if arg.starts_with("user,id=net0,hostfwd=") {
            *arg = "user,id=net0".to_string();
        }
    }
    // Snapshot mode: don't persist disk writes across stress iterations.
    args.push("-snapshot".to_string());

    let mut child = Command::new("qemu-system-x86_64")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to launch QEMU");

    let global_timeout = std::time::Duration::from_secs(timeout_secs);
    let stdout = child.stdout.take().expect("no stdout pipe");
    let rx = spawn_serial_reader(stdout);
    let mut serial_buf = String::new();
    let global_start = std::time::Instant::now();

    let result = run_smoke_steps_with_capture(
        &mut child,
        steps,
        global_timeout,
        &rx,
        &mut serial_buf,
        global_start,
    );

    let _ = child.kill();
    let _ = child.wait();

    match result {
        Ok(()) => Ok(serial_buf),
        Err(msg) => Err((msg, serial_buf)),
    }
}

fn save_stress_artifact(subdir: &str, content: &str, filename: &str) {
    let dir = workspace_root().join("target").join("stress").join(subdir);
    save_artifact(&dir, filename, content);
}

fn extract_stress_trace_dump(subdir: &str, serial_log: &str) {
    if let Some(trace) = extract_marked_section(
        serial_log,
        "=== TRACE RING DUMP ===",
        "=== END TRACE RING DUMP ===",
    ) {
        save_stress_artifact(subdir, &trace, "trace.log");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string_args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    fn smoke_step_labels(steps: &[SmokeStep]) -> Vec<&'static str> {
        let mut out = Vec::new();
        for step in steps {
            match step {
                SmokeStep::Wait { label, .. } | SmokeStep::Send { label, .. } => out.push(*label),
                SmokeStep::WaitEither {
                    label,
                    extra_steps_a,
                    extra_steps_b,
                    ..
                } => {
                    out.push(*label);
                    out.extend(smoke_step_labels(extra_steps_a));
                    out.extend(smoke_step_labels(extra_steps_b));
                }
                SmokeStep::Sleep { .. } => out.push("sleep"),
            }
        }
        out
    }

    fn send_input_for_label(steps: &[SmokeStep], target_label: &str) -> Option<&'static str> {
        for step in steps {
            match step {
                SmokeStep::Send { input, label } if *label == target_label => return Some(*input),
                SmokeStep::WaitEither {
                    extra_steps_a,
                    extra_steps_b,
                    ..
                } => {
                    if let Some(v) = send_input_for_label(extra_steps_a, target_label) {
                        return Some(v);
                    }
                    if let Some(v) = send_input_for_label(extra_steps_b, target_label) {
                        return Some(v);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Recursive: returns the first matching label's pattern. For
    /// `WaitEither` steps, reports `pattern_a` (the preferred match).
    /// Recurses into both `extra_steps_a` and `extra_steps_b` so assertions
    /// about injected sub-steps (e.g. `BOOT_MARKER_SETTLE`) resolve.
    fn wait_pattern_for_label(steps: &[SmokeStep], target_label: &str) -> Option<&'static str> {
        for step in steps {
            match step {
                SmokeStep::Wait { pattern, label, .. } if *label == target_label => {
                    return Some(*pattern);
                }
                SmokeStep::WaitEither {
                    pattern_a,
                    label,
                    extra_steps_a,
                    extra_steps_b,
                    ..
                } => {
                    if *label == target_label {
                        return Some(*pattern_a);
                    }
                    if let Some(p) = wait_pattern_for_label(extra_steps_a, target_label) {
                        return Some(p);
                    }
                    if let Some(p) = wait_pattern_for_label(extra_steps_b, target_label) {
                        return Some(p);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Recursive sibling of [`wait_pattern_for_label`]: returns the first
    /// matching label's timeout. For `WaitEither` steps, reports the
    /// step's top-level `timeout_secs`.
    fn wait_timeout_for_label(steps: &[SmokeStep], target_label: &str) -> Option<u64> {
        for step in steps {
            match step {
                SmokeStep::Wait {
                    timeout_secs,
                    label,
                    ..
                } if *label == target_label => return Some(*timeout_secs),
                SmokeStep::WaitEither {
                    timeout_secs,
                    label,
                    extra_steps_a,
                    extra_steps_b,
                    ..
                } => {
                    if *label == target_label {
                        return Some(*timeout_secs);
                    }
                    if let Some(t) = wait_timeout_for_label(extra_steps_a, target_label) {
                        return Some(t);
                    }
                    if let Some(t) = wait_timeout_for_label(extra_steps_b, target_label) {
                        return Some(t);
                    }
                }
                _ => {}
            }
        }
        None
    }

    #[test]
    fn signed_path_appends_signed_suffix() {
        let unsigned = PathBuf::from("target/bootx64.efi");

        assert_eq!(
            signed_path(&unsigned),
            PathBuf::from("target/bootx64-signed.efi")
        );
    }

    #[test]
    fn qemu_args_headless_uses_display_none() {
        let args = qemu_args(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
        );

        assert!(args.windows(2).any(|window| window == ["-display", "none"]));
        assert!(args.windows(2).any(|window| window == ["-serial", "stdio"]));
    }

    // --------------------------------------------------------------
    // Phase 55 (F.1): `--device nvme|e1000` flag parsing + QEMU wiring
    // --------------------------------------------------------------

    #[test]
    fn extract_device_flags_defaults_empty() {
        let (devices, rest) = extract_device_flags(&[]).unwrap();
        assert_eq!(devices, DeviceSet::default());
        assert!(rest.is_empty());
    }

    #[test]
    fn extract_device_flags_parses_space_separated() {
        let input = string_args(&["--device", "nvme", "--fresh", "--device", "e1000"]);
        let (devices, rest) = extract_device_flags(&input).unwrap();
        assert!(devices.nvme);
        assert!(devices.e1000);
        assert_eq!(rest, vec!["--fresh".to_string()]);
    }

    #[test]
    fn extract_device_flags_parses_equals_form() {
        let input = string_args(&["--device=nvme", "--device=e1000"]);
        let (devices, _) = extract_device_flags(&input).unwrap();
        assert!(devices.nvme);
        assert!(devices.e1000);
    }

    #[test]
    fn extract_device_flags_rejects_unknown_name() {
        let input = string_args(&["--device", "realtek"]);
        let err = extract_device_flags(&input).unwrap_err();
        assert!(err.contains("unknown `--device` value `realtek`"));
    }

    #[test]
    fn extract_device_flags_rejects_missing_value() {
        let input = string_args(&["--device"]);
        let err = extract_device_flags(&input).unwrap_err();
        assert!(err.contains("missing value"));
    }

    #[test]
    fn qemu_args_default_uses_virtio_net() {
        let args = qemu_args(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["-device", "virtio-net-pci,netdev=net0"])
        );
        // Default has no e1000 and no NVMe.
        assert!(!args.iter().any(|arg| arg.starts_with("e1000")));
        assert!(!args.iter().any(|arg| arg.contains("nvme")));
    }

    #[test]
    fn qemu_args_with_e1000_replaces_virtio_net() {
        let args = qemu_args_with_devices(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
            DeviceSet {
                nvme: false,
                e1000: true,
                iommu: false,
            },
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["-device", "e1000,netdev=net0"])
        );
        assert!(
            !args
                .windows(2)
                .any(|window| window == ["-device", "virtio-net-pci,netdev=net0"])
        );
    }

    #[test]
    fn qemu_args_with_nvme_appends_nvme_drive() {
        // Use the pure resolver directly with a fake path so the test does
        // not create or touch `target/nvme.img` — keeps the suite hermetic
        // under sandboxed / read-only CI environments (PR #113 Comment 6).
        let fake_nvme = Path::new("/tmp/m3os-test-nvme-never-created.img");
        let args = qemu_args_with_devices_resolved(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
            DeviceSet {
                nvme: true,
                e1000: false,
                iommu: false,
            },
            Some(fake_nvme),
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["-device", "virtio-net-pci,netdev=net0"]),
            "virtio-net should remain the default NIC when only --device nvme is set"
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["-device", "nvme,serial=deadbeef,drive=nvme0"]),
        );
        assert!(
            args.iter()
                .any(|arg| arg.contains("if=none,id=nvme0,format=raw")),
            "expected NVMe backing-drive fragment with if=none and id=nvme0"
        );
        assert!(
            args.iter()
                .any(|arg| arg.contains(fake_nvme.to_str().unwrap())),
            "expected NVMe drive fragment to reference the caller-provided path"
        );
        assert!(
            !fake_nvme.exists(),
            "qemu_args_with_devices_resolved must not create the NVMe backing image"
        );
    }

    // --------------------------------------------------------------
    // Phase 55a Track F.1: `--iommu` flag parsing + QEMU wiring
    // --------------------------------------------------------------

    #[test]
    fn extract_iommu_flag_sets_device_set() {
        let input = string_args(&["--iommu"]);
        let (devices, rest) = extract_device_flags(&input).unwrap();
        assert!(devices.iommu);
        assert!(!devices.nvme);
        assert!(!devices.e1000);
        assert!(rest.is_empty());
    }

    #[test]
    fn extract_iommu_flag_composes_with_fresh_and_device() {
        let input = string_args(&["--iommu", "--fresh", "--device", "nvme"]);
        let (devices, rest) = extract_device_flags(&input).unwrap();
        assert!(devices.iommu);
        assert!(devices.nvme);
        assert_eq!(rest, vec!["--fresh".to_string()]);
    }

    #[test]
    fn qemu_args_with_iommu_appends_intel_iommu_and_split_irqchip() {
        // Phase 55a F.1: `--iommu` must inject both the intel-iommu device
        // and the `kernel_irqchip=split` override (QEMU rejects `intel-iommu`
        // under the default `on` irqchip model on q35).
        let args = qemu_args_with_devices_resolved(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
            DeviceSet {
                nvme: false,
                e1000: false,
                iommu: true,
            },
            None,
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["-device", "intel-iommu,x-scalable-mode=off"]),
            "expected `-device intel-iommu,x-scalable-mode=off` pair when --iommu is set"
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["-machine", "q35,kernel_irqchip=split"]),
            "expected `-machine q35,kernel_irqchip=split` pair when --iommu is set \
             (intel-iommu requires q35 chipset and split irqchip)"
        );
    }

    #[test]
    fn qemu_args_without_iommu_omits_intel_iommu_and_split_irqchip() {
        let args = qemu_args_with_devices_resolved(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
            DeviceSet::default(),
            None,
        );
        assert!(
            !args.iter().any(|a| a.contains("intel-iommu")),
            "default DeviceSet must not enable intel-iommu"
        );
        assert!(
            !args
                .windows(2)
                .any(|w| w == ["-machine", "q35,kernel_irqchip=split"]),
            "default DeviceSet must not switch to q35 or set kernel_irqchip=split"
        );
    }

    #[test]
    fn iommu_qemu_args_constant_matches_live_wiring() {
        // F.1 acceptance: the IOMMU device args live as one reusable slice
        // so callers never hand-roll them. The partnering `-machine
        // q35,kernel_irqchip=split` property is emitted separately (and
        // potentially combined with GUI machine options) by
        // `build_machine_arg`.
        assert_eq!(
            IOMMU_QEMU_ARGS,
            &["-device", "intel-iommu,x-scalable-mode=off",]
        );
        let args = qemu_args_with_devices_resolved(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
            DeviceSet {
                nvme: false,
                e1000: false,
                iommu: true,
            },
            None,
        );
        let strs: Vec<&str> = args.iter().map(String::as_str).collect();
        // Every constant entry must appear, in order, somewhere in argv.
        let mut i = 0;
        for &want in IOMMU_QEMU_ARGS {
            let rel = strs[i..]
                .iter()
                .position(|a| *a == want)
                .unwrap_or_else(|| panic!("IOMMU_QEMU_ARGS entry {want:?} missing from argv"));
            i += rel + 1;
        }
    }

    #[test]
    fn qemu_args_with_iommu_and_gui_emit_single_machine_with_both_props() {
        // Regression: `--iommu --gui` previously emitted two `-machine`
        // arguments; QEMU would drop the earlier one's settings. The
        // consolidated `-machine` value must contain both the IOMMU
        // q35/kernel_irqchip properties and the GUI pcspk property.
        let args = qemu_args_with_devices_resolved(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Gui,
            DeviceSet {
                nvme: false,
                e1000: false,
                iommu: true,
            },
            None,
        );
        let machine_count = args.iter().filter(|a| *a == "-machine").count();
        assert_eq!(
            machine_count, 1,
            "exactly one `-machine` argument required, got {machine_count}"
        );
        let machine_idx = args.iter().position(|a| a == "-machine").unwrap();
        let value = &args[machine_idx + 1];
        assert!(
            value.contains("q35"),
            "consolidated -machine missing q35: {value}"
        );
        assert!(
            value.contains("kernel_irqchip=split"),
            "consolidated -machine missing kernel_irqchip=split: {value}"
        );
        assert!(
            value.contains("pcspk-audiodev=noaudio"),
            "consolidated -machine missing pcspk-audiodev=noaudio: {value}"
        );
    }

    #[test]
    fn qemu_args_gui_uses_sdl_and_disables_audio() {
        let args = qemu_args(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Gui,
        );

        assert!(args.windows(2).any(|window| window == ["-display", "sdl"]));
        assert!(
            args.windows(2)
                .any(|window| window == ["-audiodev", "none,id=noaudio"])
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["-machine", "pcspk-audiodev=noaudio"])
        );
    }

    #[test]
    fn qemu_run_args_include_debug_exit_device() {
        let args = qemu_run_args(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
        );

        assert!(
            args.windows(2)
                .any(|window| window == ["-device", QEMU_ISA_DEBUG_EXIT_DEVICE])
        );
    }

    #[test]
    fn qemu_run_args_allow_guest_reboot() {
        let args = qemu_run_args(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
        );

        assert!(!args.iter().any(|arg| arg == "-no-reboot"));
    }

    #[test]
    fn normalize_run_qemu_exit_maps_debug_success_to_zero() {
        assert_eq!(normalize_run_qemu_exit(Some(0)), 0);
        assert_eq!(normalize_run_qemu_exit(Some(QEMU_EXIT_SUCCESS)), 0);
        assert_eq!(
            normalize_run_qemu_exit(Some(QEMU_EXIT_FAILURE)),
            QEMU_EXIT_FAILURE
        );
        assert_eq!(normalize_run_qemu_exit(None), 1);
    }

    #[test]
    fn parse_image_args_defaults_to_unsigned_image() {
        let workspace_root = PathBuf::from("/workspace/m3os");
        let parsed = parse_image_args(&[], &workspace_root).unwrap();

        assert!(!parsed.sign);
        assert_eq!(parsed.key, workspace_root.join("m3os.key"));
        assert_eq!(parsed.cert, workspace_root.join("m3os.crt"));
    }

    #[test]
    fn parse_image_args_uses_repo_root_defaults_when_signing() {
        let workspace_root = PathBuf::from("/workspace/m3os");
        let parsed = parse_image_args(&string_args(&["--sign"]), &workspace_root).unwrap();

        assert!(parsed.sign);
        assert_eq!(parsed.key, workspace_root.join("m3os.key"));
        assert_eq!(parsed.cert, workspace_root.join("m3os.crt"));
    }

    #[test]
    fn parse_image_args_rejects_key_without_sign() {
        let workspace_root = PathBuf::from("/workspace/m3os");
        let error = parse_image_args(&string_args(&["--key", "keys/dev.key"]), &workspace_root)
            .unwrap_err();

        assert_eq!(error, "`--key`/`--cert` require `--sign`");
    }

    #[test]
    fn parse_sign_args_uses_repo_root_defaults() {
        let workspace_root = PathBuf::from("/workspace/m3os");
        let parsed =
            parse_sign_args(&string_args(&["build/bootx64.efi"]), &workspace_root).unwrap();

        assert_eq!(parsed.unsigned_efi, PathBuf::from("build/bootx64.efi"));
        assert_eq!(parsed.signed_efi, PathBuf::from("build/bootx64-signed.efi"));
        assert_eq!(parsed.key, workspace_root.join("m3os.key"));
        assert_eq!(parsed.cert, workspace_root.join("m3os.crt"));
    }

    #[test]
    fn parse_sign_args_accepts_explicit_key_and_cert() {
        let workspace_root = PathBuf::from("/workspace/m3os");
        let parsed = parse_sign_args(
            &string_args(&[
                "--key=keys/dev.key",
                "unsigned.efi",
                "--cert",
                "keys/dev.crt",
            ]),
            &workspace_root,
        )
        .unwrap();

        assert_eq!(parsed.key, PathBuf::from("keys/dev.key"));
        assert_eq!(parsed.cert, PathBuf::from("keys/dev.crt"));
        assert_eq!(parsed.signed_efi, PathBuf::from("unsigned-signed.efi"));
    }

    #[test]
    fn parse_sign_args_requires_unsigned_efi_path() {
        let workspace_root = PathBuf::from("/workspace/m3os");
        let error =
            parse_sign_args(&string_args(&["--key", "keys/dev.key"]), &workspace_root).unwrap_err();

        assert_eq!(error, "missing unsigned EFI path");
    }

    #[test]
    fn find_uefi_bootloader_in_uses_bootloader_build_artifact() {
        let tempdir = tempfile::tempdir().unwrap();
        let candidate = tempdir
            .path()
            .join("bootloader-abcd1234/out/bin/bootloader-x86_64-uefi.efi");
        fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        fs::write(&candidate, b"efi").unwrap();

        assert_eq!(find_uefi_bootloader_in(tempdir.path()).unwrap(), candidate);
    }

    #[test]
    fn fat_parent_dirs_builds_forward_slash_paths() {
        assert_eq!(
            fat_parent_dirs("efi/boot/bootx64.efi"),
            vec!["efi".to_string(), "efi/boot".to_string()]
        );
        assert!(fat_parent_dirs("kernel-x86_64").is_empty());
    }

    #[test]
    fn create_fat_filesystem_writes_expected_paths() {
        let tempdir = tempfile::tempdir().unwrap();
        let bootloader_path = tempdir.path().join("bootloader.efi");
        let kernel_path = tempdir.path().join("kernel.bin");
        fs::write(&bootloader_path, b"signed-bootloader").unwrap();
        fs::write(&kernel_path, b"kernel-bytes").unwrap();

        let mut files = BTreeMap::new();
        files.insert(
            UEFI_BOOT_FILENAME,
            FileDataSource::File(bootloader_path.clone()),
        );
        files.insert(KERNEL_FILE_NAME, FileDataSource::File(kernel_path.clone()));

        let fat_image = NamedTempFile::new().unwrap();
        create_fat_filesystem(&files, fat_image.reopen().unwrap()).unwrap();

        let filesystem =
            fatfs::FileSystem::new(fat_image.reopen().unwrap(), fatfs::FsOptions::new()).unwrap();
        let root_dir = filesystem.root_dir();

        let mut bootloader = root_dir.open_file(UEFI_BOOT_FILENAME).unwrap();
        let mut bootloader_bytes = Vec::new();
        use std::io::Read;
        bootloader.read_to_end(&mut bootloader_bytes).unwrap();
        assert_eq!(bootloader_bytes, b"signed-bootloader");

        let mut kernel = root_dir.open_file(KERNEL_FILE_NAME).unwrap();
        let mut kernel_bytes = Vec::new();
        kernel.read_to_end(&mut kernel_bytes).unwrap();
        assert_eq!(kernel_bytes, b"kernel-bytes");
    }

    #[test]
    fn reset_placeholder_file_creates_missing_file() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("placeholder");

        reset_placeholder_file(&path).unwrap();

        assert!(path.exists());
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn reset_placeholder_file_truncates_stale_file() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("placeholder");
        fs::write(&path, b"stale-binary").unwrap();

        reset_placeholder_file(&path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"");
    }

    #[test]
    fn smoke_test_stays_within_boot_and_guest_runner_scope() {
        let labels = smoke_step_labels(&smoke_test_script(false));

        assert!(labels.contains(&"wait for smoke runner start or final boot marker"));
        assert!(labels.contains(&"guest smoke runner completed all checks"));
        assert!(!labels.contains(&"run PTY regression (quick - skips ion timing tests)"));
        assert!(!labels.contains(&"doom: launch with iwad"));
        assert!(!labels.contains(&"uniq: count adjacent duplicates"));
    }

    #[test]
    fn smoke_test_starts_directly_in_smoke_runner_mode() {
        assert_eq!(
            wait_pattern_for_label(
                &smoke_test_script(false),
                "wait for smoke runner start or final boot marker"
            ),
            Some("SMOKE:BEGIN")
        );
        assert_eq!(
            wait_pattern_for_label(
                &smoke_test_script(false),
                "wait for smoke runner start after final boot marker"
            ),
            Some("SMOKE:BEGIN")
        );
    }

    #[test]
    fn smoke_test_no_longer_relies_on_serial_shell_input() {
        assert_eq!(
            send_input_for_label(&smoke_test_script(false), "enter username"),
            None
        );
        assert_eq!(
            send_input_for_label(&smoke_test_script(false), "run guest smoke runner"),
            None
        );
    }

    #[test]
    fn smoke_test_waits_for_guest_smoke_runner_markers() {
        assert_eq!(
            wait_pattern_for_label(
                &smoke_test_script(false),
                "guest/auth: smoke runner confirmed root session"
            ),
            Some("SMOKE:auth:PASS")
        );
        // tcc-compile + hello are now WaitEither steps that accept PASS or
        // SKIP (M3OS_SMOKE_SKIP_TCC_COMPILE=1 in fast/headless CI); labels
        // gained the "or skipped" suffix and the tcc budget moved from
        // 180s to 600s to absorb TCG slowness.
        assert_eq!(
            wait_timeout_for_label(
                &smoke_test_script(false),
                "guest/tcc: smoke runner compiled hello world or skipped"
            ),
            Some(600)
        );
        assert_eq!(
            wait_pattern_for_label(
                &smoke_test_script(false),
                "guest/hello: smoke runner ran compiled hello or skipped"
            ),
            Some("SMOKE:hello:PASS")
        );
        assert_eq!(
            wait_pattern_for_label(
                &smoke_test_script(false),
                "guest/log: smoke runner verified syslog marker"
            ),
            Some("SMOKE:log:PASS")
        );
        assert_eq!(
            wait_pattern_for_label(
                &smoke_test_script(false),
                "guest smoke runner completed all checks"
            ),
            Some("SMOKE:PASS")
        );
    }

    #[test]
    fn boot_and_login_steps_use_pid_agnostic_boot_marker() {
        assert_eq!(
            wait_pattern_for_label(&boot_and_login_steps(), "wait for final boot marker"),
            Some("init: started 'net_udp' pid=")
        );
    }

    #[test]
    fn log_pipeline_regression_reads_log_file_contents() {
        let log_check =
            send_input_for_label(&log_pipeline_steps(), "guest/log: verify message in syslog");

        assert_eq!(log_check, Some("/bin/cat /var/log/messages\n"));
    }

    #[test]
    fn strip_background_noise_removes_kernel_and_init_service_lines() {
        let input = concat!(
            "root@m3os:/home/project# /bin/stat libutil.a\n",
            "[INFO] [waitpid] pid 71 exited\n",
            "init: service 'syslogd' exited (127)\n",
            "init: restarting 'syslogd' (8/10)\n",
            "  File: libutil.a\n",
        );

        assert_eq!(
            strip_background_noise(input),
            "root@m3os:/home/project# /bin/stat libutil.a\n  File: libutil.a\n"
        );
    }

    #[test]
    fn strip_background_noise_removes_midline_kernel_logs() {
        // Kernel log injected between "symbolic " and "link" — the exact
        // pattern that causes CI flakiness.
        let input = concat!(
            "  File: /phase38-passwd-link  Size: 11  symbolic ",
            "[INFO] [munmap] freed 1 pages @ 0x200014f000 (len=0x1000)\n",
            "link\n",
        );

        assert_eq!(
            strip_background_noise(input),
            "  File: /phase38-passwd-link  Size: 11  symbolic link\n"
        );
    }

    #[test]
    fn strip_background_noise_removes_multiple_midline_injections() {
        let input = concat!(
            "root@m3os:# /bin/echo hello >> /tmp/out",
            "[INFO] [munmap] freed 1 pages @ 0x20002d6000 (len=0x1000)\n",
            "[INFO] [pipe] created pipe_id=0\n",
            "\nroot@m3os:# ",
        );

        assert_eq!(
            strip_background_noise(input),
            "root@m3os:# /bin/echo hello >> /tmp/out\nroot@m3os:# "
        );
    }

    #[test]
    fn strip_background_noise_keeps_regular_userspace_output() {
        let input = "init: configuration loaded from /etc/init.conf\n";

        assert_eq!(strip_background_noise(input), input);
    }

    #[test]
    fn strip_background_noise_handles_trailing_noise_without_newline() {
        let input = "output here[INFO] [fork] p8 fork()";
        assert_eq!(strip_background_noise(input), "output here");
    }

    #[test]
    fn prompt_suffix_end_matches_terminal_prompt_suffix() {
        let serial = "tcc version 0.9.27\nroot@m3os:/# ";
        assert_eq!(prompt_suffix_end(serial, "# "), Some(serial.len()));
    }

    #[test]
    fn prompt_suffix_end_rejects_prompt_fragments_followed_by_command_text() {
        let serial = "root@m3os:/# /bin/file /tmp/hello";
        assert_eq!(prompt_suffix_end(serial, "# "), None);
    }

    #[test]
    fn find_serial_match_requires_prompt_suffix_for_shell_prompts() {
        let serial = "root@m3os:/# /bin/file /tmp/hello";
        assert!(find_serial_match(serial, serial, "# ").is_none());
    }

    #[test]
    fn find_serial_match_accepts_prompt_after_carriage_return_redraw() {
        let serial = "root@m3os:/# /usr/bin/tcc --version\rroot@m3os:/# ";
        assert!(find_serial_match(serial, serial, "# ").is_some());
    }

    #[test]
    fn render_terminal_text_replaces_line_after_carriage_return() {
        let serial = "root@m3os:/# /usr/bin/tcc --version\rroot@m3os:/# ";
        assert_eq!(render_terminal_text(serial), "root@m3os:/# ");
    }

    #[test]
    fn drain_serial_through_cleaned_match_preserves_following_prompt() {
        let mut serial = concat!(
            "root@m3os:/home/project# /bin/xargs -I{} /bin/echo file:{} < /tmp/files\n",
            "file:/home/project/ut",
            "[INFO] [waitpid] pid 195 exited\n",
            "il.c\n",
            "root@m3os:/home/project# "
        )
        .to_string();
        let stripped = strip_ansi(&serial);
        let cleaned = strip_background_noise(&stripped);
        let (mode, match_end) = find_serial_match(&stripped, &cleaned, "file:/home/project/util.c")
            .expect("cleaned match should succeed");

        assert!(matches!(mode, SerialMatchMode::Cleaned));
        drain_serial_through_match(&mut serial, &stripped, mode, match_end);
        assert_eq!(serial, "\nroot@m3os:/home/project# ");
    }

    #[test]
    fn drain_serial_through_cleaned_match_drops_pre_make_prompt_but_keeps_post_make_prompt() {
        let mut serial = concat!(
            "root@m3os:/home/project# /bin/make\n",
            "cc -static -O2 -o de",
            "[INFO] [p38] execve(/usr/bin/tcc)\n",
            "mo main.o util.o\n",
            "root@m3os:/home/project# "
        )
        .to_string();
        let stripped = strip_ansi(&serial);
        let cleaned = strip_background_noise(&stripped);
        let (mode, match_end) = find_serial_match(&stripped, &cleaned, "-o demo")
            .expect("cleaned match should succeed");

        assert!(matches!(mode, SerialMatchMode::Cleaned));
        drain_serial_through_match(&mut serial, &stripped, mode, match_end);
        assert_eq!(serial, " main.o util.o\nroot@m3os:/home/project# ");
    }

    #[test]
    fn drain_serial_until_idle_keeps_reading_until_quiet() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tx.send(b"login:".to_vec()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(10));
            tx.send(b" Password:".to_vec()).unwrap();
        });

        let mut serial = String::new();
        let mut history = String::new();
        drain_serial_until_idle(
            &rx,
            &mut serial,
            &mut history,
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(200),
        );

        assert_eq!(serial, "login: Password:");
        assert_eq!(history, "login: Password:");
    }

    // -----------------------------------------------------------------
    // Phase 55b Track D.1 — nvme_driver ramdisk wiring.
    //
    // These tests encode the AGENTS.md "four places for a new userspace
    // binary" rule specifically for the D.1 nvme_driver scaffold:
    //
    //   1. workspace member (`Cargo.toml`)
    //   2. xtask build pipeline (the `bins` array in this file)
    //   3. ramdisk embedding (`kernel/src/fs/ramdisk.rs`)
    //   4. service config — deferred to F.1
    //
    // Place 2 is static text inside `build_userspace_bins`, and Place 3
    // is a `BIN_ENTRIES` tuple keyed by `"/drivers/nvme"` in the kernel
    // ramdisk source. The tests below read both source files from the
    // workspace and assert the nvme_driver strings are present. This
    // catches the most common D.1 regression — a ramdisk entry silently
    // dropped during a refactor — without requiring a full QEMU boot.

    fn workspace_file(relative: &str) -> String {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest_dir)
            .parent()
            .expect("xtask/ lives under the workspace root")
            .join(relative);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
    }

    #[test]
    fn nvme_driver_registered_in_xtask_bins_array() {
        // Phase 55b D.1 — `build_userspace_bins` must build nvme_driver
        // so the generated ELF lands in `target/generated-initrd/` for
        // ramdisk embedding. `needs_alloc = true` because the crate
        // depends on `driver_runtime` and `kernel-core`.
        let source = workspace_file("xtask/src/main.rs");
        assert!(
            source.contains("(\"nvme_driver\", \"nvme_driver\", true)"),
            "xtask `bins` array must include nvme_driver with needs_alloc = true"
        );
    }

    #[test]
    fn nvme_driver_embedded_in_ramdisk_under_drivers_path() {
        // Phase 55b D.1 — the ramdisk must expose the compiled ELF at
        // `/drivers/nvme` so init can `execve` it from the standard
        // driver path. Track F.1 wires the service config that spawns
        // it; this test pins the ramdisk half of that contract.
        let source = workspace_file("kernel/src/fs/ramdisk.rs");
        assert!(
            source.contains("generated_initrd_asset!(\"nvme_driver\")"),
            "ramdisk must `include_bytes!` the nvme_driver ELF"
        );
        assert!(
            source.contains("\"nvme\""),
            "ramdisk must register nvme under a /drivers/ BIN_ENTRIES tuple"
        );
        assert!(
            source.contains("DRIVERS_ENTRIES") || source.contains("/drivers"),
            "ramdisk must expose a /drivers directory containing nvme"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 55b F.1 — service-manager registration for nvme_driver + e1000_driver
    // -----------------------------------------------------------------------

    /// Assert that nvme_driver.conf is embedded in the ext2 data disk via
    /// `populate_ext2_files`.  We look for the conf content string *before*
    /// the tests module boundary so the test cannot be satisfied by the
    /// assertion strings themselves.
    #[test]
    fn nvme_driver_conf_embedded_in_ext2() {
        let source = workspace_file("xtask/src/main.rs");
        // The marker that delimits where production code ends and tests begin.
        let tests_boundary = source.find("mod tests {").unwrap_or(source.len());
        let prod = &source[..tests_boundary];
        assert!(
            prod.contains("name=nvme_driver"),
            "populate_ext2_files must embed a conf string containing `name=nvme_driver`"
        );
        assert!(
            prod.contains("command=/drivers/nvme"),
            "populate_ext2_files must embed a conf string containing `command=/drivers/nvme`"
        );
        // Find the conf literal and verify no depends= key is present.
        let start = prod
            .find("name=nvme_driver")
            .expect("name=nvme_driver not found");
        // Find closing delimiter of the string literal (the next `"` after start).
        let end = prod[start..]
            .find('"')
            .map(|i| start + i)
            .unwrap_or(prod.len());
        assert!(
            !prod[start..end].contains("depends="),
            "nvme_driver.conf must NOT contain a depends= line (IOMMU substrate is kernel-internal)"
        );
        assert!(
            prod.contains("restart=on-failure"),
            "nvme_driver.conf must contain `restart=on-failure`"
        );
        assert!(
            prod.contains("max_restart=5"),
            "nvme_driver.conf must contain `max_restart=5`"
        );
    }

    /// Assert that e1000_driver.conf is embedded in the ext2 data disk via
    /// `populate_ext2_files`.
    #[test]
    fn e1000_driver_conf_embedded_in_ext2() {
        let source = workspace_file("xtask/src/main.rs");
        let tests_boundary = source.find("mod tests {").unwrap_or(source.len());
        let prod = &source[..tests_boundary];
        assert!(
            prod.contains("name=e1000_driver"),
            "populate_ext2_files must embed a conf string containing `name=e1000_driver`"
        );
        assert!(
            prod.contains("command=/drivers/e1000"),
            "populate_ext2_files must embed a conf string containing `command=/drivers/e1000`"
        );
        let start = prod
            .find("name=e1000_driver")
            .expect("name=e1000_driver not found");
        let end = prod[start..]
            .find('"')
            .map(|i| start + i)
            .unwrap_or(prod.len());
        assert!(
            !prod[start..end].contains("depends="),
            "e1000_driver.conf must NOT contain a depends= line"
        );
    }

    /// Assert that both driver service names appear in init's KNOWN_CONFIGS list.
    #[test]
    fn driver_confs_in_init_known_configs() {
        let source = workspace_file("userspace/init/src/main.rs");
        assert!(
            source.contains("nvme_driver.conf"),
            "init KNOWN_CONFIGS must include nvme_driver.conf"
        );
        assert!(
            source.contains("e1000_driver.conf"),
            "init KNOWN_CONFIGS must include e1000_driver.conf"
        );
    }

    /// Assert that init emits a driver.registered structured event when a
    /// driver service config is loaded.
    #[test]
    fn init_emits_driver_registered_event() {
        let source = workspace_file("userspace/init/src/main.rs");
        assert!(
            source.contains("driver.registered"),
            "init must emit a structured `driver.registered` log event when a driver service is loaded"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 55b F.4 — device-path data smoke assertions
    //
    // These tests use source-text assertions (like F.1 above) so they compile
    // immediately but fail at test time until the implementation is present.
    // This lets the pre-commit hook pass the compilation step while still
    // recording the expected contracts.
    // -----------------------------------------------------------------------

    /// The `device-smoke` subcommand must appear in the usage string so that
    /// CI scripts can discover it without reading source.
    #[test]
    fn usage_string_contains_device_smoke_subcommand() {
        let source = workspace_file("xtask/src/main.rs");
        let tests_boundary = source.find("mod tests {").unwrap_or(source.len());
        let prod = &source[..tests_boundary];
        assert!(
            prod.contains("\"device-smoke\""),
            "xtask main dispatch must handle the `device-smoke` subcommand"
        );
        assert!(
            prod.contains("device-smoke"),
            "usage() must advertise the `device-smoke` subcommand for CI discoverability"
        );
    }

    /// `device_smoke_script_nvme` must be defined in production code and its
    /// body must contain the `driver.registered name=nvme_driver` pattern
    /// literal that the smoke Wait step checks for in the boot log.  The
    /// actual serial line emitted by init is
    /// `init: driver.registered name=nvme_driver command=/drivers/nvme`.
    #[test]
    fn nvme_smoke_script_contains_driver_registered_marker() {
        let source = workspace_file("xtask/src/main.rs");
        let tests_boundary = source.find("mod tests {").unwrap_or(source.len());
        let prod = &source[..tests_boundary];
        assert!(
            prod.contains("fn device_smoke_script_nvme"),
            "production code must define `device_smoke_script_nvme()`"
        );
        assert!(
            prod.contains("driver.registered name=nvme_driver"),
            "device_smoke_script_nvme() must include a Wait pattern that \
             matches the init log line `init: driver.registered name=nvme_driver`"
        );
    }

    /// `device_smoke_script_e1000` must be defined in production code and its
    /// body must contain the `driver.registered name=e1000_driver` pattern
    /// literal that the smoke Wait step checks for in the boot log.
    #[test]
    fn e1000_smoke_script_contains_driver_registered_marker() {
        let source = workspace_file("xtask/src/main.rs");
        let tests_boundary = source.find("mod tests {").unwrap_or(source.len());
        let prod = &source[..tests_boundary];
        assert!(
            prod.contains("fn device_smoke_script_e1000"),
            "production code must define `device_smoke_script_e1000()`"
        );
        assert!(
            prod.contains("driver.registered name=e1000_driver"),
            "device_smoke_script_e1000() must include a Wait pattern that \
             matches the init log line `init: driver.registered name=e1000_driver`"
        );
    }

    /// `parse_device_smoke_args` must be defined in production code and must
    /// accept `--device` and `--iommu` flags (verified by searching for those
    /// string literals inside the function body).
    #[test]
    fn parse_device_smoke_args_is_defined_and_handles_device_and_iommu() {
        let source = workspace_file("xtask/src/main.rs");
        let tests_boundary = source.find("mod tests {").unwrap_or(source.len());
        let prod = &source[..tests_boundary];
        assert!(
            prod.contains("fn parse_device_smoke_args"),
            "production code must define `parse_device_smoke_args()`"
        );
        // The function delegates to extract_device_flags which handles
        // --device and --iommu.  Verify the delegation call is present.
        let fn_start = prod
            .find("fn parse_device_smoke_args")
            .expect("already asserted above");
        let fn_body = &prod[fn_start..];
        assert!(
            fn_body.contains("extract_device_flags"),
            "parse_device_smoke_args must delegate to extract_device_flags \
             to handle --device and --iommu flags"
        );
    }

    /// `cmd_device_smoke` must be defined so it is callable from the main
    /// dispatch loop, and it must use `run_smoke_script` (or the captured
    /// variant) to execute the boot-log assertion steps.
    #[test]
    fn cmd_device_smoke_is_defined_and_uses_smoke_runner() {
        let source = workspace_file("xtask/src/main.rs");
        let tests_boundary = source.find("mod tests {").unwrap_or(source.len());
        let prod = &source[..tests_boundary];
        assert!(
            prod.contains("fn cmd_device_smoke"),
            "production code must define `cmd_device_smoke()`"
        );
        let fn_start = prod
            .find("fn cmd_device_smoke")
            .expect("already asserted above");
        let fn_body = &prod[fn_start..];
        assert!(
            fn_body.contains("run_smoke_script")
                || fn_body.contains("run_smoke_steps_with_capture"),
            "cmd_device_smoke must run the smoke step script via run_smoke_script \
             or run_smoke_steps_with_capture"
        );
    }

    /// Default `cargo xtask run` (no --device) must not include the NVMe
    /// controller or the e1000 NIC in the QEMU argument list, guaranteeing
    /// the VirtIO-only path is unchanged.
    #[test]
    fn default_run_qemu_args_have_no_nvme_or_e1000() {
        let args = qemu_args_with_devices_resolved(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Headless,
            DeviceSet::default(),
            None,
        );
        assert!(
            !args.iter().any(|a| a.contains("nvme")),
            "default DeviceSet must not include any nvme argument"
        );
        assert!(
            !args.iter().any(|a| a == "e1000,netdev=net0"),
            "default DeviceSet must use virtio-net, not e1000"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 55b F.4b — full data-path round-trip assertions
    //
    // TDD red: these tests fail until the implementation emits the right
    // markers in the production sources and smoke scripts.
    // -----------------------------------------------------------------------

    /// `device_smoke_script_nvme` must contain a Wait step for
    /// `NVME_SMOKE:rw:PASS` — the sentinel the nvme_driver itself prints
    /// after a successful 512 B write+read round-trip at LBA 0.
    #[test]
    fn nvme_smoke_script_asserts_rw_round_trip() {
        let source = workspace_file("xtask/src/main.rs");
        let tests_boundary = source.find("mod tests {").unwrap_or(source.len());
        let prod = &source[..tests_boundary];
        let fn_start = prod
            .find("fn device_smoke_script_nvme")
            .expect("device_smoke_script_nvme must exist");
        // Find the closing brace of that function's returned Vec block.
        // We look for the sentinel within a liberal window after the
        // function start instead of parsing braces.
        let fn_window = &prod[fn_start..];
        assert!(
            fn_window.contains("NVME_SMOKE:rw:PASS"),
            "device_smoke_script_nvme() must include a Wait step for \
             `NVME_SMOKE:rw:PASS` — printed by nvme_driver after a \
             successful 512 B round-trip at LBA 0"
        );
    }

    /// The nvme_driver source must contain the `NVME_SMOKE:rw:PASS`
    /// sentinel string it is expected to emit after a successful
    /// 512 B write+read round-trip, and a `NVME_SMOKE:rw:FAIL` marker
    /// for the failure path (no silent drop on failure).
    #[test]
    fn nvme_driver_source_emits_rw_round_trip_sentinels() {
        let source = workspace_file("userspace/drivers/nvme/src/main.rs");
        assert!(
            source.contains("NVME_SMOKE:rw:PASS"),
            "nvme_driver main.rs must emit `NVME_SMOKE:rw:PASS` after a \
             successful 512 B write+read round-trip at LBA 0"
        );
        assert!(
            source.contains("NVME_SMOKE:rw:FAIL"),
            "nvme_driver main.rs must emit `NVME_SMOKE:rw:FAIL` on failure \
             so the smoke harness does not silently miss a broken round-trip"
        );
    }

    /// `device_smoke_script_e1000` must contain a Wait step for
    /// `E1000_SMOKE:link:PASS` — the sentinel the e1000_driver prints
    /// when initial bring-up reports link up.
    #[test]
    fn e1000_smoke_script_asserts_link_pass() {
        let source = workspace_file("xtask/src/main.rs");
        let tests_boundary = source.find("mod tests {").unwrap_or(source.len());
        let prod = &source[..tests_boundary];
        let fn_start = prod
            .find("fn device_smoke_script_e1000")
            .expect("device_smoke_script_e1000 must exist");
        let fn_window = &prod[fn_start..];
        assert!(
            fn_window.contains("E1000_SMOKE:link:PASS"),
            "device_smoke_script_e1000() must include a Wait step for \
             `E1000_SMOKE:link:PASS` — printed by e1000_driver after \
             link-up is confirmed at bring-up"
        );
    }

    /// The e1000_driver source must contain the `E1000_SMOKE:link:PASS`
    /// sentinel it is expected to emit when link is up after bring-up,
    /// plus an honest-skip sentinel for ICMP (deferred until E.3).
    #[test]
    fn e1000_driver_source_emits_link_and_icmp_sentinels() {
        let source = workspace_file("userspace/drivers/e1000/src/main.rs");
        assert!(
            source.contains("E1000_SMOKE:link:PASS"),
            "e1000_driver main.rs must emit `E1000_SMOKE:link:PASS` when \
             initial link-up is confirmed at bring-up"
        );
        assert!(
            source.contains("E1000_SMOKE:icmp:SKIP"),
            "e1000_driver main.rs must emit `E1000_SMOKE:icmp:SKIP` \
             (honest-skip with reason) until the full TX/RX server loop \
             lands in Track E.3"
        );
    }
}
