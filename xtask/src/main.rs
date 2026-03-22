use std::path::PathBuf;
use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        Some("image") => cmd_image(),
        Some("run") => cmd_run(),
        Some("check") => cmd_check(),
        Some("fmt") => {
            let fix = args.iter().any(|a| a == "--fix");
            cmd_fmt(fix);
        }
        Some("runner") => {
            let kernel_binary = args
                .get(2)
                .expect("usage: cargo xtask runner <kernel-binary>");
            cmd_runner(PathBuf::from(kernel_binary));
        }
        Some(other) => {
            eprintln!("Unknown subcommand: {other}");
            eprintln!("Usage: cargo xtask <image|run|check|fmt [--fix]|runner>");
            std::process::exit(1);
        }
        None => {
            eprintln!("Usage: cargo xtask <image|run|check|fmt [--fix]|runner>");
            std::process::exit(1);
        }
    }
}

fn workspace_root() -> PathBuf {
    let output = Command::new(env!("CARGO"))
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .expect("failed to run cargo locate-project");
    let path = String::from_utf8(output.stdout).unwrap();
    PathBuf::from(path.trim())
        .parent()
        .unwrap()
        .to_path_buf()
}

fn build_kernel() -> PathBuf {
    let root = workspace_root();
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

fn create_uefi_image(kernel_binary: &PathBuf) -> PathBuf {
    let uefi_path = kernel_binary
        .parent()
        .unwrap()
        .join("boot-uefi-ostest.img");

    let builder = bootloader::DiskImageBuilder::new(kernel_binary.clone());
    builder
        .create_uefi_image(&uefi_path)
        .expect("failed to create UEFI disk image");

    println!("UEFI image: {}", uefi_path.display());
    uefi_path
}

fn convert_to_vhdx(uefi_image: &PathBuf) {
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
    // Check environment variable first
    if let Ok(path) = std::env::var("OVMF_PATH") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return p;
        }
    }

    // Common system paths
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

fn launch_qemu(uefi_image: &PathBuf) {
    let ovmf = find_ovmf();

    let status = Command::new("qemu-system-x86_64")
        .args(["-bios"])
        .arg(&ovmf)
        .args(["-drive", &format!("format=raw,file={}", uefi_image.display())])
        .args(["-serial", "stdio"])
        .args(["-display", "none"])
        .args(["-no-reboot"])
        .status()
        .expect("failed to launch QEMU");

    std::process::exit(status.code().unwrap_or(1));
}

fn cmd_check() {
    let root = workspace_root();

    // clippy on the kernel (bare-metal target requires -Zbuild-std)
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

    // rustfmt check (host target is fine for formatting)
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["fmt", "--package", "kernel", "--", "--check"])
        .status()
        .expect("failed to run cargo fmt");

    if !status.success() {
        eprintln!("rustfmt found unformatted code — run `cargo xtask fmt --fix` to fix");
        std::process::exit(1);
    }

    println!("check passed: clippy clean, formatting correct");
}

fn cmd_fmt(fix: bool) {
    let root = workspace_root();
    let mut args = vec!["fmt", "--package", "kernel"];
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

fn cmd_image() {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);
}

fn cmd_run() {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);
    launch_qemu(&uefi_image);
}

fn cmd_runner(kernel_binary: PathBuf) {
    let uefi_image = create_uefi_image(&kernel_binary);
    launch_qemu(&uefi_image);
}
