# Phase 41 — Expanded Coreutils: Task List

**Status:** Planned
**Source Ref:** phase-41
**Depends on:** Phase 14 (Shell and Tools) ✅, Phase 27 (User Accounts) ✅, Phase 38 (Filesystem Enhancements) ✅
**Goal:** Ship a comprehensive set of Unix utilities beyond the minimal Phase 14 set.
The OS gains text processing (`head`, `tail`, `sort`, `uniq`, `cut`, `tr`, `sed`),
file inspection (`find`, `xargs`, `du`, `df`, `hexdump`), system diagnostics
(`ps`, `free`, `dmesg`, `kill`, `mount`), permission tools (`chmod`, `chown`), and
developer workflow tools (`diff`, `patch`, `less`, `tee`). Every tool composes via
shell pipes and works with the existing procfs, syscall, and filesystem infrastructure.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Text processing tools (head, tail, sort, uniq, cut, tr, sed, tee) | — | Planned |
| B | File and directory tools (find, xargs, du, df, ln, file, hexdump) | — | Planned |
| C | System tools (ps, kill, free, uptime, dmesg, mount, umount) | D | Planned |
| D | Kernel support (dmesg ring buffer, umount syscall) | — | Planned |
| E | Permission tools (chmod, chown) | — | Planned |
| F | Developer tools (diff, patch, less, strings, cal) | A | Planned |
| G | Initrd packaging and shell integration | A–F | Planned |
| H | Integration testing and documentation | A–G | Planned |

---

## Track A — Text Processing Tools

Implement or port small, composable text processing utilities. Preferred source
for ports is [sbase](https://git.suckless.org/sbase/) (suckless.org), compiled
with `musl-gcc -static`.

### A.1 — `head`: print first N lines

**File:** `userspace/coreutils/head.c`
**Symbol:** `main` (new binary)
**Why it matters:** `head` is one of the most-used pipeline filters. It needs
`read()`, `write()`, and basic argument parsing — all already available.

**Acceptance:**
- [ ] `head -n 5 file.txt` prints first 5 lines
- [ ] `cat file.txt | head` prints first 10 lines (default)
- [ ] Exit code 0 on success, non-zero on read error

### A.2 — `tail`: print last N lines

**File:** `userspace/coreutils/tail.c`
**Symbol:** `main` (new binary)
**Why it matters:** `tail` complements `head` and requires buffering the last N
lines of input — a different read pattern that exercises ring-buffer thinking.

**Acceptance:**
- [ ] `tail -n 5 file.txt` prints last 5 lines
- [ ] `cat file.txt | tail` prints last 10 lines (default)
- [ ] Works on stdin via pipe

### A.3 — `sort`: sort lines alphabetically

**File:** `userspace/coreutils/sort.c`
**Symbol:** `main` (new binary)
**Why it matters:** `sort` is essential for `sort | uniq` pipelines and
requires reading all input into memory then sorting — exercises `malloc`/heap
usage in musl-linked binaries.

**Acceptance:**
- [ ] `sort file.txt` outputs lines in lexicographic order
- [ ] `sort -r` reverses order
- [ ] `sort -n` sorts numerically
- [ ] Works on stdin via pipe

### A.4 — `uniq`: filter duplicate adjacent lines

**File:** `userspace/coreutils/uniq.c`
**Symbol:** `main` (new binary)
**Why it matters:** Paired with `sort`, `uniq` enables frequency-counting
pipelines like `sort | uniq -c | sort -rn`.

**Acceptance:**
- [ ] `uniq` filters adjacent duplicate lines
- [ ] `uniq -c` prefixes lines with occurrence count
- [ ] Works on stdin via pipe

### A.5 — `cut`: extract fields or columns

**File:** `userspace/coreutils/cut.c`
**Symbol:** `main` (new binary)
**Why it matters:** `cut` is the standard tool for extracting columns from
delimited text, which `awk` handles in larger systems but `cut` does simply.

**Acceptance:**
- [ ] `cut -d: -f1` extracts first colon-delimited field
- [ ] `cut -c1-5` extracts characters 1 through 5
- [ ] Works on stdin via pipe

### A.6 — `tr`: translate or delete characters

**File:** `userspace/coreutils/tr.c`
**Symbol:** `main` (new binary)
**Why it matters:** `tr` handles character-level transformations (case
conversion, whitespace normalization) that are fundamental to shell scripting.

**Acceptance:**
- [ ] `echo "HELLO" | tr 'A-Z' 'a-z'` outputs `hello`
- [ ] `tr -d '\n'` deletes newlines
- [ ] Works on stdin (pipe-only tool)

### A.7 — `sed`: stream editor

**File:** `userspace/coreutils/sed.c`
**Symbol:** `main` (new binary)
**Why it matters:** `sed` is the standard non-interactive text editor. Even a
minimal subset (`s/old/new/`, `d`, `p`) covers the vast majority of use cases.

**Acceptance:**
- [ ] `sed 's/foo/bar/' file.txt` performs substitution on each line
- [ ] `sed 's/foo/bar/g'` performs global substitution
- [ ] `sed -n '3,5p'` prints only lines 3 through 5
- [ ] Works on stdin via pipe

### A.8 — `tee`: duplicate stdin to file and stdout

**File:** `userspace/coreutils/tee.c`
**Symbol:** `main` (new binary)
**Why it matters:** `tee` enables capturing intermediate pipeline output to a
file while passing it along, which is critical for debugging pipelines.

**Acceptance:**
- [ ] `echo hello | tee output.txt` writes to both stdout and `output.txt`
- [ ] `tee -a output.txt` appends instead of truncating
- [ ] Works in multi-stage pipelines

---

## Track B — File and Directory Tools

### B.1 — `find`: search for files by name and type

**File:** `userspace/coreutils/find.c`
**Symbol:** `main` (new binary)
**Why it matters:** `find` is essential for code navigation and build scripts.
It exercises recursive directory traversal (`getdents64`), symlink-aware `stat`,
and pattern matching.

**Acceptance:**
- [ ] `find /path -name "*.c"` lists matching files recursively
- [ ] `find /path -type f` lists regular files only
- [ ] `find /path -type d` lists directories only
- [ ] Follows symlinks by default (or `-L` flag)

### B.2 — `xargs`: build commands from stdin

**File:** `userspace/coreutils/xargs.c`
**Symbol:** `main` (new binary)
**Why it matters:** `xargs` converts newline- or null-delimited input into
command arguments, enabling `find ... | xargs grep ...` workflows.

**Acceptance:**
- [ ] `find . -name "*.c" | xargs grep "main"` executes `grep` on found files
- [ ] `xargs -I {} cmd {}` supports replacement strings
- [ ] `xargs -0` supports null-delimited input (for `find -print0`)

### B.3 — `du`: disk usage summary

**File:** `userspace/coreutils/du.c`
**Symbol:** `main` (new binary)
**Why it matters:** `du` shows directory sizes by recursively summing file sizes
via `stat()`, providing the first disk-space visibility tool.

**Acceptance:**
- [ ] `du /path` shows space used by each subdirectory
- [ ] `du -s /path` shows only the total for the given path
- [ ] `du -h` prints human-readable sizes (K, M)

### B.4 — `df`: filesystem free space

**File:** `userspace/coreutils/df.c`
**Symbol:** `main` (new binary)
**Why it matters:** `df` reads `statfs`/`fstatfs` (implemented in Phase 38) to
display free and used space on each mounted filesystem.

**Acceptance:**
- [ ] `df` lists all mounted filesystems with total/used/free space
- [ ] `df -h` prints human-readable sizes
- [ ] Reads from `statfs()` syscall (or `/proc/mounts` + per-mount `statfs`)

### B.5 — `ln`: create links

**File:** `userspace/coreutils/ln.c`
**Symbol:** `main` (new binary)
**Why it matters:** The `ln` Rust coreutil already exists (`userspace/coreutils-rs/src/ln.rs`),
but a C version covers the case where sbase or other ported tools expect a
standalone `ln` binary without the Rust runtime.

**Acceptance:**
- [ ] `ln -s target linkname` creates a symlink (delegates to `symlink()` syscall)
- [ ] `ln target linkname` creates a hard link (delegates to `link()` syscall)
- [ ] Error messages on permission or filesystem failures

### B.6 — `file`: basic file type identification

**File:** `userspace/coreutils/file.c`
**Symbol:** `main` (new binary)
**Why it matters:** `file` inspects magic bytes to identify ELF binaries, text
files, and other common types — useful for debugging and scripting.

**Acceptance:**
- [ ] `file binary.elf` prints `ELF 64-bit` (reads ELF magic `\x7fELF`)
- [ ] `file text.c` prints `ASCII text` (heuristic: no NUL bytes)
- [ ] `file /dev/null` prints `character special`

### B.7 — `hexdump`: hex dump of binary files

**File:** `userspace/coreutils/hexdump.c`
**Symbol:** `main` (new binary)
**Why it matters:** `hexdump` provides a binary file inspector essential for
debugging ELF loading, filesystem corruption, and raw data formats.

**Acceptance:**
- [ ] `hexdump file` prints canonical hex+ASCII output
- [ ] `hexdump -C file` prints offset, hex bytes, and ASCII sidebar
- [ ] `hexdump -n 64 file` limits output to first 64 bytes

---

## Track C — System Tools

These tools read kernel state through `/proc` (implemented in Phase 38) or
existing syscall wrappers.

### C.1 — `ps`: list running processes

**File:** `userspace/coreutils/ps.c`
**Symbol:** `main` (new binary)
**Why it matters:** `ps` is the standard process inspector. It reads
`/proc/{pid}/status` and `/proc/{pid}/cmdline` for every PID listed in `/proc/`.

**Acceptance:**
- [ ] `ps` shows PID, status, and command name for the calling user's processes
- [ ] `ps -e` (or `ps -A`) shows all processes
- [ ] Output columns: PID, STATE, CMD at minimum
- [ ] Reads from `/proc/{pid}/status` and `/proc/{pid}/cmdline`

### C.2 — `kill`: send signals to processes (standalone binary)

**File:** `userspace/coreutils/kill.c`
**Symbol:** `main` (new binary)
**Why it matters:** The shell has a built-in `kill`, but a standalone binary
allows scripts and `xargs` to use it. Wraps the existing `kill()` syscall.

**Acceptance:**
- [ ] `kill -9 <pid>` sends SIGKILL
- [ ] `kill <pid>` sends SIGTERM (default)
- [ ] `kill -l` lists available signal names
- [ ] Wraps `syscall-lib::kill()`

### C.3 — `free`: memory usage summary

**File:** `userspace/coreutils/free.c`
**Symbol:** `main` (new binary)
**Why it matters:** `free` reads `/proc/meminfo` to display total, used, and
available memory. The `meminfo` Rust coreutil already exists
(`userspace/coreutils-rs/src/meminfo.rs`); this C version adds a familiar
`free`-style output format.

**Acceptance:**
- [ ] `free` displays total, used, and available memory in KB
- [ ] `free -m` displays in MB
- [ ] `free -h` displays human-readable sizes
- [ ] Reads from `/proc/meminfo`

### C.4 — `dmesg`: display kernel log buffer

**Files:**
- `kernel/src/serial.rs` (kernel-side ring buffer)
- `userspace/coreutils/dmesg.c`

**Symbol:** `dmesg` (new binary), `DMESG_RING` (new kernel buffer)
**Why it matters:** Currently kernel logs go only to serial output and are lost
to userspace. A kernel ring buffer plus a `/proc/kmsg` (or `sys_syslog`) read
interface lets userspace inspect boot and runtime kernel messages.

**Acceptance:**
- [ ] Kernel captures log output into a fixed-size ring buffer alongside serial
- [ ] `dmesg` reads and displays the kernel log buffer from userspace
- [ ] New log lines appear in `dmesg` output after boot messages
- [ ] Interface is either `/proc/kmsg` file or a custom syscall

### C.5 — `mount` / `umount`: mount and unmount filesystems

**Files:**
- `userspace/coreutils/mount.c`
- `userspace/coreutils/umount.c`

**Symbol:** `main` (new binaries)
**Why it matters:** These wrap the existing `mount()` syscall (already in
`syscall-lib`) and a new `umount()` syscall. `mount` with no arguments should
display currently mounted filesystems by reading `/proc/mounts`.

**Acceptance:**
- [ ] `mount` (no args) displays mounted filesystems from `/proc/mounts`
- [ ] `mount -t ext2 /dev/vda1 /mnt` mounts a filesystem
- [ ] `umount /mnt` unmounts a filesystem
- [ ] Error messages for permission denied (non-root) and busy filesystems

---

## Track D — Kernel Support

Minimal kernel additions required by Track C tools that go beyond what Phase 38
already provides.

### D.1 — Kernel log ring buffer for `dmesg`

**File:** `kernel/src/serial.rs`
**Symbol:** `DMESG_RING` (new static)
**Why it matters:** Today `serial_println!()` writes to COM1 and discards output.
Adding a fixed-size ring buffer that captures output alongside serial lets
userspace read boot and runtime logs via `dmesg`.

**Acceptance:**
- [ ] `DMESG_RING` is a fixed-size buffer (e.g. 64 KiB) behind a `spin::Mutex`
- [ ] Every `serial_println!()` call also appends to the ring buffer
- [ ] Buffer wraps on overflow, preserving the most recent messages
- [ ] `cargo xtask test` still passes (serial output unchanged)

### D.2 — Expose kernel log to userspace via `/proc/kmsg`

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** `render_kmsg`
**Why it matters:** The `dmesg` binary needs a read interface. Adding a
`/proc/kmsg` virtual file that returns the ring buffer contents is the simplest
approach — no new syscall needed.

**Acceptance:**
- [ ] `cat /proc/kmsg` returns current ring buffer contents
- [ ] File appears in `/proc/` directory listing
- [ ] Read returns a snapshot (not streaming) of the buffer

### D.3 — `sys_umount2()` syscall

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `userspace/syscall-lib/src/lib.rs`

**Symbol:** `sys_umount2`, `SYS_UMOUNT2`
**Why it matters:** `mount()` exists but there is no `umount` syscall yet. The
`umount` binary needs it to detach a filesystem from a mount point.

**Acceptance:**
- [ ] `SYS_UMOUNT2` (166) dispatched in the syscall handler
- [ ] Kernel removes the mount point from the VFS mount table
- [ ] Returns `-EBUSY` if files on the mount are still open
- [ ] Returns `-EPERM` if caller is not root
- [ ] `syscall-lib` wrapper `umount()` added

---

## Track E — Permission Tools

### E.1 — `chmod`: change file permissions

**File:** `userspace/coreutils/chmod.c`
**Symbol:** `main` (new binary)
**Why it matters:** `chmod` wraps the existing `chmod()` syscall (already in
`syscall-lib`). Needs octal mode parsing (`chmod 755 file`).

**Acceptance:**
- [ ] `chmod 755 file` sets rwxr-xr-x permissions
- [ ] `chmod u+x file` adds execute for owner (symbolic mode, stretch goal)
- [ ] Error on non-existent file or permission denied
- [ ] Wraps `syscall-lib::chmod()`

### E.2 — `chown`: change file ownership

**File:** `userspace/coreutils/chown.c`
**Symbol:** `main` (new binary)
**Why it matters:** `chown` wraps the existing `chown()` syscall. Needs
`user:group` parsing with `/etc/passwd` lookup.

**Acceptance:**
- [ ] `chown root:root file` changes owner and group
- [ ] `chown 0:0 file` accepts numeric UID:GID
- [ ] Only root can change ownership (kernel enforces)
- [ ] Wraps `syscall-lib::chown()`

---

## Track F — Developer Tools

### F.1 — `diff`: compare two files

**File:** `userspace/coreutils/diff.c`
**Symbol:** `main` (new binary)
**Why it matters:** `diff` is essential for development workflows — reviewing
changes, creating patches. A minimal unified-diff implementation covers most
use cases.

**Acceptance:**
- [ ] `diff file1 file2` outputs differences in unified format
- [ ] `diff -u file1 file2` explicitly requests unified format
- [ ] Exit code 0 = identical, 1 = differences, 2 = error
- [ ] Output is compatible with `patch` (Track F.2)

### F.2 — `patch`: apply diffs

**File:** `userspace/coreutils/patch.c`
**Symbol:** `main` (new binary)
**Why it matters:** `patch` applies unified diffs, completing the edit–diff–patch
development cycle that `diff` starts.

**Acceptance:**
- [ ] `patch < changes.diff` applies a unified diff to the working directory
- [ ] `patch -p1 < changes.diff` strips one leading path component
- [ ] Reports success/failure for each hunk

### F.3 — `less`: scrollable file pager

**File:** `userspace/coreutils/less.c`
**Symbol:** `main` (new binary)
**Why it matters:** `less` provides scrollable viewing of files and command
output. It uses raw terminal mode (termios, implemented in Phase 22) to capture
arrow keys and page-up/down.

**Acceptance:**
- [ ] `less file.txt` displays file with scrollable navigation
- [ ] Arrow keys and Page Up/Page Down scroll the viewport
- [ ] `/pattern` searches forward in the file
- [ ] `q` exits the pager
- [ ] Works as a pipe target: `cat file | less`

### F.4 — `strings`: extract printable strings from binaries

**File:** `userspace/coreutils/strings.c`
**Symbol:** `main` (new binary)
**Why it matters:** `strings` is a quick binary inspection tool — useful for
examining ELF binaries, debugging, and reverse engineering.

**Acceptance:**
- [ ] `strings binary.elf` prints sequences of ≥4 printable characters
- [ ] `strings -n 8 binary.elf` sets minimum string length to 8
- [ ] Works on any file type (reads raw bytes)

### F.5 — `cal`: calendar display

**File:** `userspace/coreutils/cal.c`
**Symbol:** `main` (new binary)
**Why it matters:** `cal` is a classic Unix tool that exercises date/time
arithmetic. Reads the current date via `gettimeofday()` or `clock_gettime()`
(available since Phase 34).

**Acceptance:**
- [ ] `cal` displays the current month's calendar
- [ ] `cal 2025` displays all 12 months of 2025
- [ ] `cal 6 2025` displays June 2025
- [ ] Highlights today's date (if terminal supports bold/inverse)

---

## Track G — Initrd Packaging and Shell Integration

### G.1 — Add new C binaries to `build_musl_bins()`

**File:** `xtask/src/main.rs`
**Symbol:** `build_musl_bins`
**Why it matters:** Every new C binary must be registered in the xtask musl build
list so it gets cross-compiled with `musl-gcc -static` and placed in
`kernel/initrd/`.

**Acceptance:**
- [ ] All new C binaries from Tracks A–F added to the `bins` array in `build_musl_bins()`
- [ ] `cargo xtask image` compiles and packages all new tools
- [ ] Each binary appears as `kernel/initrd/{name}.elf`

### G.2 — Register binaries in init or shell PATH

**Files:**
- `userspace/init/src/main.rs`
- `userspace/shell/src/main.rs`

**Symbol:** binary lookup path
**Why it matters:** New tools must be discoverable by the shell. Either the
shell's PATH already covers `/bin` (where initrd tools are copied at boot), or
init must copy the new ELFs to the correct directory.

**Acceptance:**
- [ ] All new tools are available by name from the shell prompt
- [ ] Tab completion (if supported by ion/sh0) includes new tool names
- [ ] `which head` (or equivalent) resolves to the correct path

### G.3 — Add new Rust binaries to `coreutils_bins` (if any)

**File:** `xtask/src/main.rs`
**Symbol:** `coreutils_bins`
**Why it matters:** If any tools are implemented as Rust coreutils instead of C,
they must be added to the `coreutils_bins` array so they are built and packaged.

**Acceptance:**
- [ ] Any Rust-implemented tools added to `coreutils_bins` list
- [ ] Corresponding `[[bin]]` entry added to `userspace/coreutils-rs/Cargo.toml`
- [ ] `cargo xtask image` builds and packages them alongside existing Rust tools

---

## Track H — Integration Testing and Documentation

### H.1 — Pipeline integration tests

**Symbol:** manual QEMU validation
**Why it matters:** The acceptance criteria for this phase specifically require
multi-tool pipelines to work. These are validated manually inside QEMU.

**Acceptance:**
- [ ] `cat file | sort | uniq -c | sort -rn | head -10` produces correct output
- [ ] `find . -name "*.c" | xargs grep "main"` finds matches
- [ ] `diff file1 file2 > changes.diff && patch < changes.diff` round-trips
- [ ] `ps -e` shows all running processes with PID, state, and command
- [ ] `free -h` shows total and available memory
- [ ] `less file.txt` provides scrollable viewing with `q` to exit
- [ ] `dmesg` shows kernel boot messages

### H.2 — Update Phase 41 design doc

**File:** `docs/roadmap/41-expanded-coreutils.md`
**Symbol:** acceptance criteria, status
**Why it matters:** The design doc must be updated to reflect the actual
implementation — which tools were ported vs. written, and any scope changes.

**Acceptance:**
- [ ] Status updated to "Complete"
- [ ] Any deferred tools noted in the "Deferred Until Later" section
- [ ] Implementation Outline reflects actual approach taken

### H.3 — Update roadmap README

**File:** `docs/roadmap/README.md`
**Symbol:** Phase 41 row
**Why it matters:** The roadmap summary must stay current.

**Acceptance:**
- [ ] Phase 41 status updated to "Complete"
- [ ] Tasks column links to the task list

---

## Documentation Notes

- Phase 38 already provides `/proc/meminfo`, `/proc/uptime`, `/proc/mounts`,
  and `/proc/{pid}/status` — tools like `ps`, `free`, and `mount` (no args) can
  read these directly without new kernel work.
- The `uptime` and `meminfo` Rust coreutils already exist in
  `userspace/coreutils-rs/`. Phase 41 C equivalents (`free`, `uptime` standalone)
  provide the familiar output format expected by Unix users and scripts.
- Syscalls for `chmod()`, `chown()`, `fchmod()`, `fchown()`, `mount()`, `kill()`,
  and `link()`/`symlink()` already exist in `syscall-lib`. Most Track C and E
  tools are thin wrappers around these.
- The main new kernel work is Track D: the dmesg ring buffer and `umount` syscall.
  All other tools build on existing infrastructure.
- The C tools are preferred over Rust for this phase because sbase provides
  clean, portable reference implementations that can be cross-compiled with
  `musl-gcc -static` using the same pipeline as existing C coreutils.
- A `ln` C binary (Track B.5) may be redundant with the existing Rust `ln` — the
  decision depends on whether sbase tools expect a C-linked `ln` in their PATH.
