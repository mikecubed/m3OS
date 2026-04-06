# Phase 44 — Rust Cross-Compilation Pipeline: Task List

**Status:** In Progress
**Source Ref:** phase-44
**Depends on:** Phase 12 (POSIX Compat) ✅, Phase 14 (Shell and Tools) ✅, Phase 24 (Persistent Storage) ✅
**Goal:** Enable Rust programs cross-compiled on the host (via
`x86_64-unknown-linux-musl`) to run natively inside m3OS, with xtask integration
for automated building and disk image packaging, and a custom m3os target spec
for `#![no_std]` native programs.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Host toolchain setup and hello-rust | — | Planned |
| B | Missing syscall support for Rust std | A | Planned |
| C | Demonstration programs | A, B | Planned |
| D | xtask integration for musl Rust binaries | A | Planned |
| E | Custom m3os target specification (no_std) | A | Planned |
| F | Integration testing and documentation | A–E | Planned |

---

## Track A — Host Toolchain Setup and Hello-Rust

Set up the `x86_64-unknown-linux-musl` Rust target on the host and validate that
a minimal Rust binary runs inside m3OS.

### A.1 — Install and verify musl Rust target on the host

**File:** `docs/roadmap/44-rust-cross-compilation.md`
**Symbol:** (toolchain documentation)
**Why it matters:** The `x86_64-unknown-linux-musl` target produces statically-linked
ELF binaries that make Linux syscalls — exactly what m3OS's Linux compat layer
(Phase 12) handles. Verifying the target is installed and produces the right binary
format is the foundation for everything else in this phase.

**Acceptance:**
- [ ] `rustup target add x86_64-unknown-linux-musl` succeeds on the host
- [ ] `cargo build --target x86_64-unknown-linux-musl --release` produces a static ELF binary
- [ ] `file` confirms the output is `ELF 64-bit LSB executable, x86-64, statically linked`

### A.2 — Create `userspace/hello-rust/` crate

**Files:**
- `userspace/hello-rust/Cargo.toml`
- `userspace/hello-rust/src/main.rs`

**Symbol:** `hello-rust`
**Why it matters:** This is the first Rust `std` program targeting m3OS. Unlike existing
userspace crates (which are `no_std` and use `syscall-lib`), this crate uses Rust's
standard library and is compiled with `--target x86_64-unknown-linux-musl`. It
validates that musl-linked Rust binaries can run on the Linux syscall ABI that m3OS
provides.

**Acceptance:**
- [ ] `userspace/hello-rust/` exists as a standard Rust binary crate (uses `std`)
- [ ] `main()` prints `Hello from Rust on m3OS!` to stdout and exits cleanly
- [ ] Cross-compiles with `cargo build --target x86_64-unknown-linux-musl --release`
- [ ] Produces a statically-linked ELF binary under `target/x86_64-unknown-linux-musl/release/hello-rust`

### A.3 — Run hello-rust inside m3OS

**Files:**
- `userspace/hello-rust/src/main.rs`
- `xtask/src/main.rs`

**Symbol:** `hello-rust` (runtime validation)
**Why it matters:** This is the key proof-of-concept: a musl-linked Rust binary running
inside m3OS using the Linux syscall compatibility layer. If it prints its message and
exits cleanly, the cross-compilation pipeline works. Any failure here reveals missing
or broken syscalls that Track B must address.

**Acceptance:**
- [ ] `hello-rust` binary is manually copied to the initrd for testing
- [ ] Running `hello-rust` from the m3OS shell prints `Hello from Rust on m3OS!`
- [ ] Process exits with code 0 (no crash, no hang)
- [ ] Any missing syscalls encountered are documented for Track B

---

## Track B — Missing Syscall Support for Rust std

Identify and implement syscalls that musl-linked Rust `std` programs require but
m3OS does not yet provide. These are discovered by running hello-rust and the
demonstration programs.

### B.1 — Audit musl startup syscalls

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/process/mod.rs`

**Symbol:** `sys_*` (syscall handlers)
**Why it matters:** Before `main()` runs, musl's CRT startup code makes several
syscalls to set up the C runtime (e.g., `set_tid_address`, `sigaltstack`,
`rt_sigprocmask`). If any of these return an unhandled-syscall error, the program
aborts before reaching user code. This task catalogs which startup syscalls are
missing or return incorrect values.

**Acceptance:**
- [ ] Run `hello-rust` with serial logging enabled for unhandled syscall numbers
- [ ] Each unhandled startup syscall is listed with its number, arguments, and expected behavior
- [ ] Stubs or implementations are added so musl CRT startup completes without error

### B.2 — Implement missing syscalls for `std::fs`

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/fs/mod.rs`

**Symbol:** `sys_*` (file-related syscall handlers)
**Why it matters:** Rust's `std::fs` (via musl) uses syscalls like `openat`, `fstat`,
`getdents64`, `fstatat`, and `lseek`. Some of these may already be implemented for
C programs (Phase 12) but may need adjustments for the flags or struct layouts that
Rust's musl binaries use.

**Acceptance:**
- [ ] `std::fs::read_to_string()` works on an existing file
- [ ] `std::fs::write()` creates and writes a file
- [ ] `std::fs::read_dir()` enumerates directory entries
- [ ] `std::fs::metadata()` returns correct file size and type

### B.3 — Implement missing syscalls for `std::net`

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/net/mod.rs`

**Symbol:** `sys_*` (network-related syscall handlers)
**Why it matters:** Rust's `std::net` uses `socket`, `bind`, `listen`, `accept`,
`connect`, `setsockopt`, `getsockname`, and `getpeername` via musl. The kernel
already has TCP/UDP socket support (Phase 23) but may handle flags or options
differently from what musl expects (e.g., `SOCK_CLOEXEC`, `SOCK_NONBLOCK` flags
in the `socket` type argument).

**Acceptance:**
- [ ] `TcpListener::bind("0.0.0.0:8080")` succeeds
- [ ] `TcpStream::connect()` establishes a connection
- [ ] `std::net::UdpSocket::bind()` and `send_to`/`recv_from` work
- [ ] Socket options like `SO_REUSEADDR` are accepted (even if no-op)

### B.4 — Implement missing syscalls for `std::io` and terminal I/O

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/tty.rs`

**Symbol:** `sys_*` (I/O-related syscall handlers)
**Why it matters:** Interactive Rust programs use `std::io::stdin().read_line()` and
`std::io::stdout().write_all()` which go through musl's `read`/`write` wrappers.
Terminal-aware programs may also call `ioctl(TCGETS)` / `ioctl(TCSETS)` for raw mode.
These must work for the interactive demonstration programs (calc, todo).

**Acceptance:**
- [ ] `std::io::stdin().read_line()` blocks and returns a line from terminal input
- [ ] `std::io::stdout().write_all()` outputs text to the terminal
- [ ] `print!` and `println!` macros work correctly
- [ ] Programs reading stdin detect EOF correctly

---

## Track C — Demonstration Programs

Write and cross-compile non-trivial Rust programs that showcase m3OS capabilities.
Each program exercises a different area of `std`.

### C.1 — Create `sysinfo` program (std::fs)

**Files:**
- `userspace/sysinfo-rust/Cargo.toml`
- `userspace/sysinfo-rust/src/main.rs`

**Symbol:** `sysinfo-rust`
**Why it matters:** A system information tool that reads kernel-exported data through
the filesystem (e.g., `/proc/meminfo` or similar). Validates that `std::fs` file
reading works end-to-end through the musl → syscall → VFS → tmpfs/procfs path.

**Acceptance:**
- [ ] Reads and displays system information (memory usage, uptime, or similar)
- [ ] Uses `std::fs::read_to_string()` to read files
- [ ] Cross-compiles with `--target x86_64-unknown-linux-musl`
- [ ] Runs inside m3OS and produces meaningful output

### C.2 — Create `httpd` program (std::net)

**Files:**
- `userspace/httpd-rust/Cargo.toml`
- `userspace/httpd-rust/src/main.rs`

**Symbol:** `httpd-rust`
**Why it matters:** A minimal HTTP server demonstrates TCP networking from Rust.
This validates `TcpListener`, `TcpStream`, request parsing, and response writing
through the musl → syscall → network stack path. It is a tangible demonstration
that m3OS can run real networked Rust software.

**Acceptance:**
- [ ] Listens on a configurable port (default 8080)
- [ ] Serves a static "Hello from Rust on m3OS!" HTML page
- [ ] Handles multiple sequential HTTP requests
- [ ] Responds with correct HTTP/1.1 headers (Content-Length, Content-Type)
- [ ] Cross-compiles and runs inside m3OS

### C.3 — Create `calc` program (std::io)

**Files:**
- `userspace/calc-rust/Cargo.toml`
- `userspace/calc-rust/src/main.rs`

**Symbol:** `calc-rust`
**Why it matters:** An interactive calculator validates terminal I/O from Rust.
Reading lines from stdin, parsing expressions, and printing results exercises the
`std::io` path. This confirms that interactive musl-linked Rust programs work with
m3OS's TTY subsystem.

**Acceptance:**
- [ ] Reads arithmetic expressions from stdin (e.g., `2 + 3 * 4`)
- [ ] Evaluates and prints results with correct operator precedence
- [ ] Handles integer and floating-point arithmetic
- [ ] Prints a prompt and loops until EOF or `quit`
- [ ] Cross-compiles and runs interactively inside m3OS

### C.4 — Create `todo` program (std::fs persistence)

**Files:**
- `userspace/todo-rust/Cargo.toml`
- `userspace/todo-rust/src/main.rs`

**Symbol:** `todo-rust`
**Why it matters:** A persistent todo list validates file I/O round-tripping: reading
a data file, modifying it in memory, and writing it back. This exercises `std::fs`
write paths (create, truncate, write) that go through VFS → FAT32 to persistent
disk storage (Phase 24).

**Acceptance:**
- [ ] `todo add "Buy milk"` appends to a todo file
- [ ] `todo list` reads and displays all todos
- [ ] `todo done 1` marks a todo as complete
- [ ] Data persists across program invocations (stored in a file)
- [ ] Cross-compiles and runs inside m3OS

---

## Track D — xtask Integration for musl Rust Binaries

Extend the `cargo xtask` build system to cross-compile musl-linked Rust programs
and include them in the disk image alongside existing no_std and C binaries.

### D.1 — Add `build_musl_rust_bins()` function to xtask

**File:** `xtask/src/main.rs`
**Symbol:** `build_musl_rust_bins`
**Why it matters:** Currently xtask builds Rust userspace with `x86_64-unknown-none`
(no_std) and C userspace with `musl-gcc`. musl-linked Rust binaries need a third
build path using `cargo build --target x86_64-unknown-linux-musl`. This function
manages the list of musl Rust crates, invokes cargo with the right target and flags,
and copies the outputs to the initrd directory.

**Acceptance:**
- [ ] `build_musl_rust_bins()` compiles a configurable list of musl Rust crates
- [ ] Uses `--target x86_64-unknown-linux-musl --release` with static linking flags
- [ ] Copies built binaries to `kernel/initrd/`
- [ ] Called from `build_kernel()` alongside `build_userspace_bins()` and `build_musl_bins()`
- [ ] `cargo xtask image` produces a disk image containing the musl Rust binaries

### D.2 — Strip musl Rust binaries for size

**File:** `xtask/src/main.rs`
**Symbol:** `strip_binary` or inline in `build_musl_rust_bins`
**Why it matters:** musl-linked Rust binaries with `std` are significantly larger than
no_std binaries (often 1–5 MB vs. 50–200 KB). Stripping debug symbols and using
`opt-level = "z"` reduces binary size, which is important because the initrd is
embedded in the kernel image and affects boot time and memory usage.

**Acceptance:**
- [ ] Built binaries are stripped with `strip` or `--strip` cargo flag
- [ ] `Cargo.toml` profile for musl builds uses `opt-level = "z"` and `lto = true`
- [ ] Final binary sizes are printed during the build for visibility
- [ ] `hello-rust` binary is under 500 KB after stripping

### D.3 — Prevent musl target from interfering with no_std builds

**File:** `xtask/src/main.rs`
**Symbol:** `build_musl_rust_bins`
**Why it matters:** The workspace default target is `x86_64-unknown-none` (set in
`.cargo/config.toml`). Building musl Rust crates must explicitly override this to
`x86_64-unknown-linux-musl` and must not pollute the `target/` directory in ways
that confuse subsequent no_std builds. The musl crates should not be workspace
members (or should be in a separate workspace) to avoid `-Zbuild-std` conflicts.

**Acceptance:**
- [ ] musl Rust crates compile without affecting the existing no_std build
- [ ] `cargo xtask run` builds both no_std and musl Rust binaries in one invocation
- [ ] No `-Zbuild-std` flag is passed for musl builds (musl target has prebuilt std)
- [ ] Build artifacts for the two targets do not conflict

---

## Track E — Custom m3os Target Specification (no_std)

Create a custom Rust target spec for `#![no_std]` programs that use m3OS's native
syscall ABI directly, and provide a thin `m3os-syscall` crate.

### E.1 — Create `x86_64-m3os.json` target specification

**File:** `x86_64-m3os.json` (project root or `targets/` directory)
**Symbol:** (target specification)
**Why it matters:** A custom target spec formalizes m3OS as a Rust compilation target.
Programs compiled for this target use `#![no_std]` and call m3OS syscalls directly
via the `syscall` crate, without the Linux compatibility layer. This is conceptually
cleaner and avoids relying on Linux ABI emulation for native programs.

**Acceptance:**
- [ ] `x86_64-m3os.json` exists with correct LLVM target triple, data layout, and ABI settings
- [ ] `"os": "m3os"`, `"panic-strategy": "abort"`, `"disable-redzone": true`
- [ ] `cargo build --target x86_64-m3os.json -Zbuild-std=core,alloc` compiles a trivial no_std binary
- [ ] The produced binary is functionally equivalent to one built with `x86_64-unknown-none`

### E.2 — Document relationship between custom target and existing x86_64-unknown-none

**File:** `docs/roadmap/44-rust-cross-compilation.md`
**Symbol:** (documentation)
**Why it matters:** The project currently uses `x86_64-unknown-none` for all kernel
and userspace no_std code. The new `x86_64-m3os` target is an alternative that
labels the OS explicitly. This task documents when to use which target and whether
existing crates should migrate.

**Acceptance:**
- [ ] Design doc updated to explain the two targets and their use cases
- [ ] Decision documented: whether existing no_std crates migrate to the new target or stay on `x86_64-unknown-none`
- [ ] Any differences in codegen, linking, or behavior between the two targets are noted

---

## Track F — Integration Testing and Documentation

Validate the full cross-compilation pipeline end-to-end and update project
documentation.

### F.1 — End-to-end test: build and run all Rust demo programs

**Files:**
- `userspace/hello-rust/src/main.rs`
- `userspace/sysinfo-rust/src/main.rs`
- `userspace/httpd-rust/src/main.rs`
- `userspace/calc-rust/src/main.rs`
- `userspace/todo-rust/src/main.rs`

**Symbol:** (integration test)
**Why it matters:** The ultimate validation is running `cargo xtask run` and
exercising each demo program from the m3OS shell. This tests the entire pipeline:
cross-compilation, initrd packaging, kernel boot, and program execution.

**Acceptance:**
- [ ] `cargo xtask run` builds and packages all musl Rust binaries
- [ ] `hello-rust` prints its message and exits
- [ ] At least 3 of the 4 demo programs (sysinfo, httpd, calc, todo) run correctly
- [ ] No regressions in existing no_std userspace binaries or C programs

### F.2 — Verify no regressions in existing tests

**Files:**
- `kernel/tests/*.rs`
- `userspace/*/src/main.rs`

**Symbol:** (all existing tests)
**Why it matters:** Adding musl Rust binaries to the build pipeline and potentially
new syscall implementations could break existing functionality. All existing tests
must continue to pass.

**Acceptance:**
- [ ] `cargo xtask check` passes (clippy + fmt)
- [ ] `cargo xtask test` passes (all existing QEMU tests)
- [ ] `cargo test -p kernel-core` passes (host-side unit tests)

### F.3 — Document the cross-compilation workflow

**Files:**
- `docs/roadmap/44-rust-cross-compilation.md`
- `docs/roadmap/README.md`
- `CLAUDE.md`

**Symbol:** (documentation)
**Why it matters:** The cross-compilation workflow must be reproducible by anyone
cloning the repository. This includes host toolchain setup, build commands, and
how to add new musl Rust programs to the build.

**Acceptance:**
- [ ] Design doc updated with final implementation details and status set to `Complete`
- [ ] README row updated with task list link and status
- [ ] CLAUDE.md updated with new musl Rust crates and documentation references
- [ ] A "How to add a new Rust program" section exists in the design doc or a companion doc

---

## Documentation Notes

- Phase 44 introduces a second Rust compilation path: musl-linked `std` programs
  alongside the existing `x86_64-unknown-none` no_std programs. The two paths serve
  different purposes — musl for programs that want a full standard library, no_std
  for minimal programs with direct syscall access.
- The musl cross-compilation leverages the Linux syscall ABI compatibility layer from
  Phase 12. musl-linked binaries think they are running on Linux; the kernel translates
  their syscalls. This is the same mechanism that runs C musl programs.
- Binary size is a concern: musl-linked Rust `std` binaries are 10–50x larger than
  no_std equivalents. Stripping and LTO are essential. The initrd is embedded in the
  kernel binary, so every byte counts.
- The `x86_64-m3os.json` custom target (Track E) is a naming exercise more than a
  functional change — it produces the same code as `x86_64-unknown-none` but with
  `os = "m3os"`. Its primary value is enabling `#[cfg(target_os = "m3os")]` for
  OS-specific conditional compilation in shared crates.
- musl Rust crates should NOT be workspace members if they conflict with the
  workspace-wide `x86_64-unknown-none` default target. They may need a separate
  Cargo workspace or explicit `--target` overrides in xtask.
