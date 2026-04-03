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
        Some("smoke-test") => {
            let smoke_args = parse_smoke_test_args(&args[2..]).unwrap_or_else(|err| {
                eprintln!("Error: {err}");
                eprintln!("Usage: {}", usage());
                std::process::exit(1);
            });
            cmd_smoke_test(&smoke_args);
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
    "cargo xtask <image [--sign [--key <path>] [--cert <path>]]|run|run-gui|check|fmt [--fix]|test [--test <name>] [--timeout <secs>] [--display]|smoke-test [--display] [--timeout <secs>]|runner|sign <unsigned-efi> [--key <path>] [--cert <path>]>"
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

    // (package, binary, needs_alloc)
    let bins: &[(&str, &str, bool)] = &[
        ("exit0", "exit0", false),
        ("fork-test", "fork-test", false),
        ("echo-args", "echo-args", false),
        ("ping", "ping", false),
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
        ("sshd", "sshd", true), // Phase 43: SSH server
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
        println!("userspace: {} → kernel/initrd/{bin}", src.display());
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
        println!("userspace: {} → kernel/initrd/{bin}", src.display());
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

    for (src_rel, name) in bins {
        let src = root.join(src_rel);
        let dst = initrd.join(format!("{name}"));
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
                eprintln!(
                    "warning: musl-gcc not found — skipping C binary builds (install musl-tools to enable)"
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
            Err(e) => panic!("failed to run musl-gcc for {name}: {e}"),
        };
        if !status.success() {
            eprintln!("musl-gcc failed for {name}");
            std::process::exit(1);
        }
        println!("musl: {} → kernel/initrd/{name}", src.display());
    }
}

/// Cross-compile ion shell for musl and place it in kernel/initrd/.
///
/// Strategy: clone ion from GitHub (or use cached clone in target/ion-src/),
/// build with `cargo build --release --target x86_64-unknown-linux-musl`,
/// strip, and copy to kernel/initrd/ion.
///
/// If the ion binary already exists and is newer than ion's Cargo.toml,
/// the build is skipped (cache hit).
fn build_ion() {
    let root = workspace_root();
    let initrd = root.join("kernel/initrd");
    let ion_elf = initrd.join("ion");

    // If a pre-built ion binary exists, skip the build.
    if ion_elf.exists() && ion_elf.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        println!("ion: using cached {}", ion_elf.display());
        return;
    }

    fs::create_dir_all(&initrd).unwrap();

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
            fs::copy(&built, &ion_elf).expect("failed to copy ion binary to initrd");
        }
    }
    println!("ion: {} → kernel/initrd/ion", built.display());
}

/// Phase 32: Cross-compile pdpmake (POSIX make) for the OS.
///
/// Strategy: clone pdpmake from GitHub (or use cached clone in target/pdpmake-src/),
/// build with `musl-gcc -static -O2`, and place the resulting binary in
/// kernel/initrd/make.
fn build_pdpmake() {
    let root = workspace_root();
    let initrd = root.join("kernel/initrd");
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

    let cc = if Command::new("x86_64-linux-musl-gcc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        "x86_64-linux-musl-gcc"
    } else {
        "musl-gcc"
    };

    let status = match Command::new(cc).args(&args).status() {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("warning: {cc} not found — skipping pdpmake build");
            if !make_elf.exists() {
                fs::write(&make_elf, b"").unwrap();
            }
            return;
        }
        Err(e) => panic!("failed to run {cc} for pdpmake: {e}"),
    };
    if !status.success() {
        eprintln!("warning: pdpmake build failed");
        if !make_elf.exists() {
            fs::write(&make_elf, b"").unwrap();
        }
        return;
    }

    println!("pdpmake: built → kernel/initrd/make");
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
    let cc = if Command::new("x86_64-linux-musl-gcc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        "x86_64-linux-musl-gcc"
    } else if Command::new("musl-gcc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        "musl-gcc"
    } else {
        eprintln!(
            "warning: musl cross-compiler not found — skipping TCC build \
             (install musl-tools to enable Phase 31)"
        );
        return None;
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

    // Configure TCC.
    // Use --extra-ldflags=-static to produce a fully static, non-PIE binary.
    // The --extra-cflags=-static alone doesn't prevent PIE on newer toolchains.
    println!("tcc: configuring with {cc} (static, --prefix=/usr)...");
    let configure_status = Command::new("sh")
        .current_dir(&tcc_src)
        .args([
            "./configure",
            "--prefix=/usr",
            &format!("--cc={cc}"),
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
    // Phase 31: cross-compile TCC (result used during disk image creation).
    build_tcc();
    build_ion();
    // Phase 32: cross-compile pdpmake (POSIX make).
    build_pdpmake();
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
        // Phase 36: increase RAM to 1 GB for larger disk image and extended storage workloads.
        "-m".to_string(),
        "1024".to_string(),
        // Phase 25: SMP — boot with 4 CPU cores.
        "-smp".to_string(),
        "4".to_string(),
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
    // Phase 30: port-forward host 2323 → guest 23 for telnet access.
    // Use a plain netdev for test mode to avoid port conflicts.
    args.extend([
        "-device".to_string(),
        "virtio-net-pci,netdev=net0".to_string(),
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

    args.extend(["-no-reboot".to_string()]);
    args
}

fn launch_qemu(uefi_image: &Path, display_mode: QemuDisplayMode) {
    let ovmf = find_ovmf();
    let args = qemu_args(uefi_image, &ovmf, display_mode);

    if display_mode == QemuDisplayMode::Gui {
        println!(
            "QEMU GUI mode: click the window to grab the keyboard, then press Ctrl+Alt+G to release it."
        );
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
    build_ion();
    build_pdpmake();

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
    // Strip hostfwd from netdev to avoid port conflicts during tests.
    for arg in args.iter_mut() {
        if arg.starts_with("user,id=net0,hostfwd=") {
            *arg = "user,id=net0".to_string();
        }
    }
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

/// Strip lines containing kernel log tags from serial output.
///
/// The kernel's `log` crate emits lines like `[INFO] [p3] fork()` on the
/// same serial port as userspace output.  When a tag appears mid-line (the
/// kernel interrupted a userspace write), the entire line is corrupted and
/// must be discarded to avoid false pattern matches.
///
/// Operates line-by-line: any line containing a recognised tag is removed.
/// Tags recognised: `[INFO]`, `[DEBUG]`, `[WARN]`, `[ERROR]`, `[TRACE]`.
fn strip_kernel_logs(input: &str) -> String {
    const TAGS: &[&str] = &["[INFO]", "[DEBUG]", "[WARN]", "[ERROR]", "[TRACE]"];

    let mut out = String::with_capacity(input.len());
    for line in input.split_inclusive('\n') {
        if !TAGS.iter().any(|tag| line.contains(tag)) {
            out.push_str(line);
        }
    }
    // Handle trailing content without a newline (incomplete line).
    // split_inclusive already handles this — if the input doesn't end with
    // '\n', the last segment is returned without a newline.
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

/// Run an expect-style smoke test script against a running QEMU instance.
///
/// Returns `Ok(())` on success or `Err(message)` on failure.
fn run_smoke_script(
    child: &mut std::process::Child,
    steps: &[SmokeStep],
    global_timeout: std::time::Duration,
) -> Result<(), String> {
    let stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let rx = spawn_serial_reader(stdout);

    let mut serial_buf = String::new();
    let global_start = std::time::Instant::now();
    let total = steps.len();

    for (i, step) in steps.iter().enumerate() {
        // Global timeout check.
        if global_start.elapsed() > global_timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "global timeout ({global_timeout:?}) exceeded at step {}/{}",
                i + 1,
                total
            ));
        }

        match step {
            SmokeStep::Wait {
                pattern,
                timeout_secs,
                label,
            } => {
                println!(
                    "[step {}/{}] wait: {label} ({}s)",
                    i + 1,
                    total,
                    timeout_secs
                );
                let step_deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(*timeout_secs);
                let global_deadline = global_start + global_timeout;
                let deadline = step_deadline.min(global_deadline);

                loop {
                    // Drain any available output.
                    while let Ok(chunk) = rx.try_recv() {
                        let text = String::from_utf8_lossy(&chunk);
                        serial_buf.push_str(&text);
                    }

                    // Check for pattern in stripped output.  Also try with
                    // kernel log lines removed — the kernel can inject
                    // `[INFO] [mmap] ...` mid-line, splitting userspace
                    // output and preventing a contiguous match.
                    let stripped = strip_ansi(&serial_buf);
                    // First try the normal stripped output.  If that fails,
                    // try again with kernel log noise removed.
                    let cleaned;
                    let (search_str, used_cleaned) = if stripped.contains(pattern) {
                        (&stripped, false)
                    } else {
                        cleaned = strip_kernel_logs(&stripped);
                        if cleaned.contains(pattern) {
                            (&cleaned, true)
                        } else {
                            (&stripped, false)
                        }
                    };
                    if let Some(pos) = search_str.find(pattern) {
                        if used_cleaned {
                            // Kernel log lines were interleaved — we can't
                            // precisely map cleaned positions back to raw
                            // positions.  Drain up to the last newline to
                            // avoid dropping post-match content (e.g., the
                            // next prompt already in the buffer).
                            if let Some(nl) = serial_buf.rfind('\n') {
                                serial_buf.drain(..=nl);
                            } else if serial_buf.len() > 4096 {
                                let drain = serial_buf.len() - 4096;
                                serial_buf.drain(..drain);
                            }
                            break;
                        }
                        // Drain buffer up to end of match to avoid re-matching
                        // old output while preserving any post-match content.
                        let drain_end = pos + pattern.len();
                        // The stripped string may differ in length from
                        // serial_buf (ANSI sequences removed), so drain the
                        // same number of *raw* characters that correspond to
                        // the stripped prefix.  A simple and correct approach:
                        // rebuild the stripped prefix from serial_buf and find
                        // how many raw chars produce `drain_end` stripped chars.
                        let mut raw_idx = 0;
                        let mut stripped_count = 0;
                        let raw_bytes = serial_buf.as_bytes();
                        while stripped_count < drain_end && raw_idx < raw_bytes.len() {
                            if raw_bytes[raw_idx] == 0x1b {
                                // Skip ESC sequence.
                                raw_idx += 1;
                                if raw_idx < raw_bytes.len() && raw_bytes[raw_idx] == b'[' {
                                    raw_idx += 1;
                                    // CSI final byte is in '@'..='~' range,
                                    // matching strip_ansi()'s terminator rule.
                                    while raw_idx < raw_bytes.len()
                                        && !(b'@'..=b'~').contains(&raw_bytes[raw_idx])
                                    {
                                        raw_idx += 1;
                                    }
                                    if raw_idx < raw_bytes.len() {
                                        raw_idx += 1; // skip final letter
                                    }
                                } else if raw_idx < raw_bytes.len() {
                                    raw_idx += 1; // skip single-char escape
                                }
                            } else {
                                raw_idx += 1;
                                stripped_count += 1;
                            }
                        }
                        serial_buf.drain(..raw_idx);
                        break;
                    }

                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let tail = tail_lines(&strip_ansi(&serial_buf), 50);
                        return Err(format!(
                            "step {}/{} timed out: {label}\n\
                             expected pattern: \"{pattern}\"\n\
                             last serial output:\n{tail}",
                            i + 1,
                            total
                        ));
                    }

                    // Wait a bit before polling again.
                    match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(chunk) => {
                            let text = String::from_utf8_lossy(&chunk);
                            serial_buf.push_str(&text);
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            // QEMU exited — check if the pattern arrived
                            // before the pipe closed. Only treat as success
                            // on the final step; mid-script disconnect means
                            // subsequent steps would fail anyway.
                            let _ = child.wait();
                            let stripped = strip_ansi(&serial_buf);
                            if stripped.contains(pattern) && i + 1 == total {
                                serial_buf.clear();
                                break;
                            }
                            let tail = tail_lines(&stripped, 50);
                            return Err(format!(
                                "QEMU exited while waiting for step {}/{}: {label}\n\
                                 expected pattern: \"{pattern}\"\n\
                                 last serial output:\n{tail}",
                                i + 1,
                                total
                            ));
                        }
                    }

                    // Trim buffer to prevent unbounded growth (keep last 64 KB).
                    if serial_buf.len() > 64 * 1024 {
                        let mut cut = serial_buf.len() - 48 * 1024;
                        // Advance to next char boundary to avoid splitting a multi-byte UTF-8 sequence.
                        while cut < serial_buf.len() && !serial_buf.is_char_boundary(cut) {
                            cut += 1;
                        }
                        serial_buf.drain(..cut);
                    }
                }
            }

            SmokeStep::Send { input, label } => {
                println!("[step {}/{}] send: {label}", i + 1, total);
                if let Some(stdin) = child.stdin.as_mut() {
                    use std::io::Write;
                    if stdin.write_all(input.as_bytes()).is_err() {
                        return Err(format!("failed to send input at step {}: {label}", i + 1));
                    }
                    let _ = stdin.flush();
                } else {
                    return Err(format!("no stdin pipe at step {}: {label}", i + 1));
                }
            }

            SmokeStep::Sleep { millis } => {
                println!("[step {}/{}] sleep {}ms", i + 1, total, millis);
                std::thread::sleep(std::time::Duration::from_millis(*millis));
            }
        }
    }

    // All steps passed — kill QEMU.
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// Return the last `n` lines of a string.
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Helper: send a command and wait for the shell prompt to return.
/// Includes a small sleep before sending to avoid serial input races
/// where characters get consumed by ANSI escape sequence processing.
fn cmd_then_prompt(
    input: &'static str,
    send_label: &'static str,
    wait_label: &'static str,
    timeout: u64,
) -> Vec<SmokeStep> {
    vec![
        SmokeStep::Sleep { millis: 500 },
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
fn smoke_test_script() -> Vec<SmokeStep> {
    let mut steps = Vec::new();

    // -----------------------------------------------------------------------
    // 1. Boot and login
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Wait {
        pattern: "login:",
        timeout_secs: 60,
        label: "wait for login prompt",
    });
    steps.push(SmokeStep::Send {
        input: "rooo\x08t\n",
        label: "enter username with backspace correction",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Password:",
        timeout_secs: 10,
        label: "wait for password prompt",
    });
    steps.push(SmokeStep::Send {
        input: "root\n",
        label: "enter password",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 30,
        label: "wait for shell prompt",
    });

    // -----------------------------------------------------------------------
    // 2. Basic coreutils sanity
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/bin/echo SMOKE_OK\n",
        label: "echo test",
    });
    steps.push(SmokeStep::Wait {
        pattern: "SMOKE_OK",
        timeout_secs: 5,
        label: "verify echo output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after echo",
    });

    // -----------------------------------------------------------------------
    // 3. TCC compiler (Phase 31 regression)
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/usr/bin/tcc --version\n",
        label: "tcc --version",
    });
    steps.push(SmokeStep::Wait {
        pattern: "tcc version",
        timeout_secs: 15,
        label: "verify TCC version",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after tcc --version",
    });

    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/usr/bin/tcc -static /usr/src/hello.c -o /tmp/hello\n",
        label: "compile hello.c with TCC",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 30,
        label: "wait for hello.c compilation",
    });
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/tmp/hello\n",
        label: "run compiled hello",
    });
    steps.push(SmokeStep::Wait {
        pattern: "hello, world",
        timeout_secs: 15,
        label: "verify hello world output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after hello",
    });

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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    // 5. Demo project: build with make
    // -----------------------------------------------------------------------
    steps.extend(cmd_then_prompt(
        "cd /home/project\n",
        "send: cd /home/project",
        "wait: prompt after cd",
        5,
    ));

    // Full build (use absolute path — bare 'make' loses 'm' to ANSI SGR)
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/bin/make\n",
        label: "make: build demo project",
    });
    // Wait for the final link step to appear, then wait for the prompt.
    // (Just waiting for `# ` can match sub-shell prompts between make recipes.)
    steps.push(SmokeStep::Wait {
        pattern: "-o demo main.o util.o",
        timeout_secs: 45,
        label: "wait for make link step",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 20,
        label: "wait for prompt after make",
    });

    // Run the built binary
    steps.push(SmokeStep::Sleep { millis: 1000 });
    steps.push(SmokeStep::Send {
        input: "/home/project/demo\n",
        label: "run demo binary",
    });
    steps.push(SmokeStep::Wait {
        pattern: "Demo project running!",
        timeout_secs: 20,
        label: "verify demo output",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after demo",
    });

    // -----------------------------------------------------------------------
    // 6. ar — create a static library (using util.o from make build)
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
        timeout_secs: 5,
        label: "prompt after tmpfs-test",
    });

    steps.extend(cmd_then_prompt(
        "/bin/ln -s /bin/sh0 /tmp/mysh\n",
        "send: ln -s /bin/sh0 /tmp/mysh",
        "wait: prompt after ln",
        10,
    ));
    steps.push(SmokeStep::Sleep { millis: 500 });
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
        "/bin/ln -s /././././././././././././././././././././././././././././././etc/passwd /phase38-passwd-link\n",
        "send: ln -s /etc/passwd /phase38-passwd-link",
        "wait: prompt after ext2 symlink create",
        10,
    ));
    steps.push(SmokeStep::Sleep { millis: 500 });
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
        pattern: "/etc/passwd",
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/bin/cat /etc/passwd /etc/passwd | /bin/sort | /bin/uniq -c\n",
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/bin/echo alpha > /tmp/diff-a\n",
        label: "diff fixture: write alpha",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after diff fixture alpha",
    });
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/bin/echo beta > /tmp/diff-b\n",
        label: "diff fixture: write beta",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 5,
        label: "prompt after diff fixture beta",
    });
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/bin/less /etc/passwd\n",
        label: "less: open pager",
    });
    steps.push(SmokeStep::Wait {
        pattern: "root:",
        timeout_secs: 10,
        label: "verify less initial content",
    });
    steps.push(SmokeStep::Sleep { millis: 500 });
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
    // 17. make clean
    // -----------------------------------------------------------------------
    steps.push(SmokeStep::Sleep { millis: 500 });
    steps.push(SmokeStep::Send {
        input: "/bin/make clean\n",
        label: "make clean",
    });
    steps.push(SmokeStep::Wait {
        pattern: "# ",
        timeout_secs: 15,
        label: "wait for make clean",
    });

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
    create_data_disk(uefi_image.parent().unwrap());

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

    println!("smoke-test: launching QEMU...");
    let mut child = Command::new("qemu-system-x86_64")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to launch QEMU");

    let steps = smoke_test_script();
    let global_timeout = std::time::Duration::from_secs(smoke_args.timeout_secs);
    let start = std::time::Instant::now();

    match run_smoke_script(&mut child, &steps, global_timeout) {
        Ok(()) => {
            let elapsed = start.elapsed().as_secs();
            println!("smoke-test: PASSED ({} steps in {}s)", steps.len(), elapsed);
        }
        Err(msg) => {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("smoke-test: FAILED\n{msg}");
            std::process::exit(1);
        }
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

/// Create a 64 MB raw data disk image with an MBR partition table and an
/// ext2-formatted partition. The image is placed at `output_dir/disk.img`.
/// Skips creation if the image already exists to preserve persisted data.
///
/// Requires `e2fsprogs` on the host: `mkfs.ext2`, `debugfs`, `e2fsck`.
fn create_data_disk(output_dir: &Path) -> PathBuf {
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
    let mkfs_status = Command::new("mkfs.ext2")
        .args(["-b", "4096", "-L", "m3data", "-O", "none", "-r", "0", "-q"])
        .arg(&part_tmp)
        .status()
        .expect("failed to run mkfs.ext2 — is e2fsprogs installed?");
    if !mkfs_status.success() {
        eprintln!("Error: mkfs.ext2 failed (exit {})", mkfs_status);
        std::process::exit(1);
    }

    // Populate files using debugfs.
    populate_ext2_files(&part_tmp, output_dir);

    // Phase 31: populate TCC, musl headers/libs, and test files.
    let root = workspace_root();
    let tcc_staging = root.join("target/tcc-staging");
    if tcc_staging.join("usr/bin/tcc").exists() {
        populate_tcc_files(&part_tmp, &tcc_staging);
    }

    // Phase 32: populate demo project for make/build-tools testing.
    populate_demo_project(&part_tmp, &root);

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
fn populate_ext2_files(part_path: &Path, output_dir: &Path) {
    // Standard Unix root filesystem layout.
    let passwd_content =
        "root:x:0:0:root:/root:/bin/ion\nuser:x:1000:1000:user:/home/user:/bin/ion\n";
    let shadow_content = "root:$sha256$726f6f7473616c74$e95f58b3cda26426125bb223a690ddfde7444ac5d859e260fade5e515b91e7be::::::\nuser:$sha256$7573657273616c74$9df26fef99d129060bdc8b3c35db9cdffd52cfc58361c4045ce3d37eb46160fe::::::\n";
    let group_content = "root:x:0:root\nuser:x:1000:user\n";

    // Create temp host files for debugfs `write` command.
    let passwd_tmp = output_dir.join("_tmp_passwd");
    let shadow_tmp = output_dir.join("_tmp_shadow");
    let group_tmp = output_dir.join("_tmp_group");
    fs::write(&passwd_tmp, passwd_content).expect("write temp passwd");
    fs::write(&shadow_tmp, shadow_content).expect("write temp shadow");
    fs::write(&group_tmp, group_content).expect("write temp group");

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
         mkdir tmp\n\
         mkdir var\n\
         mkdir dev\n\
         write \"{passwd}\" etc/passwd\n\
         write \"{shadow}\" etc/shadow\n\
         write \"{group}\" etc/group\n\
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
         sif home mode 0x41ED\n\
         sif home uid 0\n\
         sif home gid 0\n\
         sif home/user mode 0x41ED\n\
         sif home/user uid 1000\n\
         sif home/user gid 1000\n\
         sif tmp mode 0x43FF\n\
         sif tmp uid 0\n\
         sif tmp gid 0\n\
         sif var mode 0x41ED\n\
         sif var uid 0\n\
         sif var gid 0\n\
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
         q\n",
        passwd = passwd_tmp.display(),
        shadow = shadow_tmp.display(),
        group = group_tmp.display(),
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

fn cmd_image(image_args: &ImageArgs) {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);

    // Phase 24: create a data disk image alongside the UEFI boot image.
    let output_dir = uefi_image.parent().unwrap();
    create_data_disk(output_dir);

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
    create_data_disk(uefi_image.parent().unwrap());
    launch_qemu(&uefi_image, QemuDisplayMode::Headless);
}

fn cmd_run_gui() {
    let kernel_binary = build_kernel();
    let uefi_image = create_uefi_image(&kernel_binary);
    convert_to_vhdx(&uefi_image);
    create_data_disk(uefi_image.parent().unwrap());
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
