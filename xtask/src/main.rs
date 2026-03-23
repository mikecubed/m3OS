use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

const SBSIGN_TOOL_HINT: &str = "Install `sbsigntool` to use `cargo xtask sign`.";

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
    "cargo xtask <image|run|check|fmt [--fix]|runner|sign <unsigned-efi> [--key <path>] [--cert <path>]>"
}

fn workspace_root() -> PathBuf {
    let output = Command::new(env!("CARGO"))
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .expect("failed to run cargo locate-project");
    let path = String::from_utf8(output.stdout).unwrap();
    PathBuf::from(path.trim()).parent().unwrap().to_path_buf()
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
    let uefi_path = kernel_binary.parent().unwrap().join("boot-uefi-ostest.img");

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

fn launch_qemu(uefi_image: &PathBuf) {
    let ovmf = find_ovmf();

    let status = Command::new("qemu-system-x86_64")
        .args(["-bios"])
        .arg(&ovmf)
        .args([
            "-drive",
            &format!("format=raw,file={}", uefi_image.display()),
        ])
        .args(["-serial", "stdio"])
        .args(["-display", "none"])
        .args(["-no-reboot"])
        .status()
        .expect("failed to launch QEMU");

    std::process::exit(status.code().unwrap_or(1));
}

fn cmd_check() {
    let root = workspace_root();

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SignArgs {
    unsigned_efi: PathBuf,
    signed_efi: PathBuf,
    key: PathBuf,
    cert: PathBuf,
}

fn default_key_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("ostest.key")
}

fn default_cert_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("ostest.crt")
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
        signed_efi: signed_efi_path(&unsigned_efi),
        unsigned_efi,
        key: key.unwrap_or_else(|| default_key_path(workspace_root)),
        cert: cert.unwrap_or_else(|| default_cert_path(workspace_root)),
    })
}

fn signed_efi_path(unsigned_efi: &Path) -> PathBuf {
    let stem = unsigned_efi
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("bootx64");
    let file_name = match unsigned_efi.extension().and_then(|ext| ext.to_str()) {
        Some(extension) if !extension.is_empty() => format!("{stem}-signed.{extension}"),
        _ => format!("{stem}-signed"),
    };

    match unsigned_efi.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    }
}

fn cmd_sign(sign_args: &SignArgs) {
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

    println!("Signed EFI: {}", sign_args.signed_efi.display());
    println!(
        "Reminder: enroll {} with MOK before Secure Boot tests.",
        sign_args.cert.display()
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sign_args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn signed_efi_path_appends_signed_suffix() {
        let unsigned = PathBuf::from("target/bootx64.efi");

        assert_eq!(
            signed_efi_path(&unsigned),
            PathBuf::from("target/bootx64-signed.efi")
        );
    }

    #[test]
    fn parse_sign_args_uses_repo_root_defaults() {
        let workspace_root = PathBuf::from("/workspace/ostest");
        let parsed = parse_sign_args(&sign_args(&["build/bootx64.efi"]), &workspace_root).unwrap();

        assert_eq!(parsed.unsigned_efi, PathBuf::from("build/bootx64.efi"));
        assert_eq!(parsed.signed_efi, PathBuf::from("build/bootx64-signed.efi"));
        assert_eq!(parsed.key, workspace_root.join("ostest.key"));
        assert_eq!(parsed.cert, workspace_root.join("ostest.crt"));
    }

    #[test]
    fn parse_sign_args_accepts_explicit_key_and_cert() {
        let workspace_root = PathBuf::from("/workspace/ostest");
        let parsed = parse_sign_args(
            &sign_args(&[
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
        let workspace_root = PathBuf::from("/workspace/ostest");
        let error =
            parse_sign_args(&sign_args(&["--key", "keys/dev.key"]), &workspace_root).unwrap_err();

        assert_eq!(error, "missing unsigned EFI path");
    }
}
