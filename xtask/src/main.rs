use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::SystemTime;

use anyhow::Context;
use fatfs::Dir;
use tempfile::NamedTempFile;

const KERNEL_FILE_NAME: &str = "kernel-x86_64";
const UEFI_BOOT_FILENAME: &str = "efi/boot/bootx64.efi";
const SBSIGN_TOOL_HINT: &str = "Install `sbsigntool` to use `cargo xtask sign`.";

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
        Some("run") => cmd_run(),
        Some("run-gui") => cmd_run_gui(),
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
    "cargo xtask <image [--sign [--key <path>] [--cert <path>]]|run|run-gui|check|fmt [--fix]|test [--test <name>] [--timeout <secs>] [--display]|runner|sign <unsigned-efi> [--key <path>] [--cert <path>]>"
}

fn workspace_root() -> PathBuf {
    let output = Command::new(env!("CARGO"))
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .expect("failed to run cargo locate-project");
    let path = String::from_utf8(output.stdout).unwrap();
    PathBuf::from(path.trim()).parent().unwrap().to_path_buf()
}

/// Compile userspace Rust binaries and copy them into kernel/initrd/.
///
/// Includes Phase 11 test binaries (exit0, fork-test, echo-args) and
/// Phase 20 init + shell. Each is compiled for `x86_64-unknown-none`
/// (statically linked, no libc) in release mode. The resulting ELF files
/// are embedded in the kernel's ramdisk via `include_bytes!`.
fn build_userspace_bins() {
    let root = workspace_root();
    let initrd = root.join("kernel/initrd");

    // Ensure the initrd directory exists before copying.
    fs::create_dir_all(&initrd).unwrap_or_else(|e| {
        panic!(
            "failed to create initrd directory {}: {e}",
            initrd.display()
        );
    });

    let bins: &[(&str, &str)] = &[
        ("exit0", "exit0"),
        ("fork-test", "fork-test"),
        ("echo-args", "echo-args"),
        ("init", "init"),
        ("shell", "sh"),
    ];

    for (pkg, bin) in bins {
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
                "-Zbuild-std=core,compiler_builtins",
                "-Zbuild-std-features=compiler-builtins-mem",
            ])
            .status()
            .unwrap_or_else(|_| panic!("failed to build userspace binary {bin}"));

        if !status.success() {
            eprintln!("userspace build failed for {bin}");
            std::process::exit(1);
        }

        let src = root.join(format!("target/x86_64-unknown-none/release/{bin}"));
        let dst = initrd.join(format!("{bin}.elf"));
        fs::copy(&src, &dst).unwrap_or_else(|e| {
            panic!("failed to copy {bin} to initrd: {e}");
        });
        println!("userspace: {} → kernel/initrd/{bin}.elf", src.display());
    }
}

/// Compile Phase 12 musl-linked C binaries and copy them into kernel/initrd/.
///
/// Requires `musl-gcc` on the host PATH (package `musl-tools` on Debian/Ubuntu).
/// Each binary is compiled as a fully static ELF with `-static -O2`.
fn build_musl_bins() {
    let root = workspace_root();
    let initrd = root.join("kernel/initrd");
    fs::create_dir_all(&initrd).unwrap_or_else(|e| {
        panic!(
            "failed to create initrd directory {}: {e}",
            initrd.display()
        );
    });

    // (source path relative to workspace root, output name)
    let bins: &[(&str, &str)] = &[
        ("userspace/hello-c/hello.c", "hello"),
        ("userspace/tmpfs-test/tmpfs-test.c", "tmpfs-test"),
        // Phase 19 signal handler test
        ("userspace/signal-test/signal-test.c", "signal-test"),
        // Phase 14 core utilities
        ("userspace/coreutils/echo.c", "echo"),
        ("userspace/coreutils/true.c", "true"),
        ("userspace/coreutils/false.c", "false"),
        ("userspace/coreutils/cat.c", "cat"),
        ("userspace/coreutils/ls.c", "ls"),
        ("userspace/coreutils/pwd.c", "pwd"),
        ("userspace/coreutils/mkdir.c", "mkdir"),
        ("userspace/coreutils/rmdir.c", "rmdir"),
        ("userspace/coreutils/rm.c", "rm"),
        ("userspace/coreutils/cp.c", "cp"),
        ("userspace/coreutils/mv.c", "mv"),
        ("userspace/coreutils/env.c", "env"),
        ("userspace/coreutils/sleep.c", "sleep"),
        ("userspace/coreutils/grep.c", "grep"),
    ];

    for (src_rel, name) in bins {
        let src = root.join(src_rel);
        let dst = initrd.join(format!("{name}.elf"));
        let status = match Command::new("musl-gcc")
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
                eprintln!("warning: musl-gcc not found — skipping C binary builds (install musl-tools to enable)");
                // Create empty placeholders so include_bytes! doesn't fail.
                for (_, name) in bins {
                    let dst = initrd.join(format!("{name}.elf"));
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
            Err(e) => panic!("failed to run musl-gcc for {name}: {e}"),
        };
        if !status.success() {
            eprintln!("musl-gcc failed for {name}");
            std::process::exit(1);
        }
        println!("musl: {} → kernel/initrd/{name}.elf", src.display());
    }
}

fn build_kernel() -> PathBuf {
    let root = workspace_root();
    build_userspace_bins();
    build_musl_bins();
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

fn qemu_args(uefi_image: &Path, ovmf: &Path, display_mode: QemuDisplayMode) -> Vec<String> {
    let mut args = vec![
        "-bios".to_string(),
        ovmf.display().to_string(),
        "-drive".to_string(),
        format!("format=raw,file={}", uefi_image.display()),
        "-serial".to_string(),
        "stdio".to_string(),
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
                "-machine".to_string(),
                "pcspk-audiodev=noaudio".to_string(),
            ]);
        }
    }

    // Phase 16: virtio-net NIC with QEMU user-mode networking.
    args.extend([
        "-device".to_string(),
        "virtio-net-pci,netdev=net0".to_string(),
        "-netdev".to_string(),
        "user,id=net0".to_string(),
    ]);

    args.extend(["-no-reboot".to_string()]);
    args
}

fn launch_qemu(uefi_image: &Path, display_mode: QemuDisplayMode) {
    let ovmf = find_ovmf();
    let args = qemu_args(uefi_image, &ovmf, display_mode);

    if display_mode == QemuDisplayMode::Gui {
        println!("QEMU GUI mode: click the window to grab the keyboard, then press Ctrl+Alt+G to release it.");
    }

    let status = Command::new("qemu-system-x86_64")
        .args(&args)
        .status()
        .expect("failed to launch QEMU");

    std::process::exit(status.code().unwrap_or(1));
}

fn cmd_check() {
    let root = workspace_root();
    build_userspace_bins();
    build_musl_bins();

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

    // Clippy + tests for kernel-core (host target).
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "clippy",
            "--package",
            "kernel-core",
            "--target",
            "x86_64-unknown-linux-gnu",
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
            "x86_64-unknown-linux-gnu",
        ])
        .status()
        .expect("failed to run kernel-core tests");

    if !status.success() {
        eprintln!("kernel-core host tests failed");
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

    println!("check passed: clippy clean, formatting correct, host tests pass");
}

#[derive(Debug, Clone)]
struct TestArgs {
    test_name: Option<String>,
    timeout_secs: u64,
    display: bool,
}

fn parse_test_args(args: &[String]) -> Result<TestArgs, String> {
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
fn qemu_test_args(uefi_image: &Path, ovmf: &Path, display: bool) -> Vec<String> {
    let display_mode = if display {
        QemuDisplayMode::Gui
    } else {
        QemuDisplayMode::Headless
    };
    let mut args = qemu_args(uefi_image, ovmf, display_mode);
    // Add ISA debug exit device so the test kernel can signal pass/fail.
    args.extend([
        "-device".to_string(),
        "isa-debug-exit,iobase=0xf4,iosize=0x04".to_string(),
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
        let args = qemu_test_args(&uefi_image, &ovmf, test_args.display);

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

fn cmd_image(image_args: &ImageArgs) {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);

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

fn cmd_run() {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);
    launch_qemu(&uefi_image, QemuDisplayMode::Headless);
}

fn cmd_run_gui() {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);
    launch_qemu(&uefi_image, QemuDisplayMode::Gui);
}

fn cmd_runner(kernel_binary: PathBuf) {
    let uefi_image = create_uefi_image(&kernel_binary);
    launch_qemu(&uefi_image, QemuDisplayMode::Headless);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string_args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
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

    #[test]
    fn qemu_args_gui_uses_sdl_and_disables_audio() {
        let args = qemu_args(
            Path::new("target/boot-uefi-m3os.img"),
            Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
            QemuDisplayMode::Gui,
        );

        assert!(args.windows(2).any(|window| window == ["-display", "sdl"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["-audiodev", "none,id=noaudio"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["-machine", "pcspk-audiodev=noaudio"]));
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
}
