# Phase 41 — Expanded Coreutils: Task List

**Status:** Complete
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
| A | Text processing tools (head, tail, sort, uniq, cut, tr, sed, tee) | — | Complete |
| B | File and directory tools (find, xargs, du, df, ln, file, hexdump) | — | Complete |
| C | System tools (ps, kill, free, uptime, dmesg, mount, umount) | D | Complete |
| D | Kernel support (dmesg ring buffer, umount syscall) | — | Complete |
| E | Permission tools (chmod, chown) | — | Complete |
| F | Developer tools (diff, patch, less, strings, cal) | A | Complete |
| G | Initrd packaging and shell integration | A–F | Complete |
| H | Integration testing and documentation | A–G | Complete |

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
- [x] `head -n 5 file.txt` prints first 5 lines
- [x] `cat file.txt | head` prints first 10 lines (default)
- [x] Exit code 0 on success, non-zero on read error

### A.2 — `tail`: print last N lines

**File:** `userspace/coreutils/tail.c`
**Symbol:** `main` (new binary)
**Why it matters:** `tail` complements `head` and requires buffering the last N
lines of input — a different read pattern that exercises ring-buffer thinking.

**Acceptance:**
- [x] `tail -n 5 file.txt` prints last 5 lines
- [x] `cat file.txt | tail` prints last 10 lines (default)
- [x] Works on stdin via pipe

### A.3 — `sort`: sort lines alphabetically

**File:** `userspace/coreutils/sort.c`
**Symbol:** `main` (new binary)
**Why it matters:** `sort` is essential for `sort | uniq` pipelines and
requires reading all input into memory then sorting — exercises `malloc`/heap
usage in musl-linked binaries.

**Acceptance:**
- [x] `sort file.txt` outputs lines in lexicographic order
- [x] `sort -r` reverses order
- [x] `sort -n` sorts numerically
- [x] Works on stdin via pipe

### A.4 — `uniq`: filter duplicate adjacent lines

**File:** `userspace/coreutils/uniq.c`
**Symbol:** `main` (new binary)
**Why it matters:** Paired with `sort`, `uniq` enables frequency-counting
pipelines like `sort | uniq -c | sort -rn`.

**Acceptance:**
- [x] `uniq` filters adjacent duplicate lines
- [x] `uniq -c` prefixes lines with occurrence count
- [x] Works on stdin via pipe

### A.5 — `cut`: extract fields or columns

**File:** `userspace/coreutils/cut.c`
**Symbol:** `main` (new binary)
**Why it matters:** `cut` is the standard tool for extracting columns from
delimited text, which `awk` handles in larger systems but `cut` does simply.

**Acceptance:**
- [x] `cut -d: -f1` extracts first colon-delimited field
- [x] `cut -c1-5` extracts characters 1 through 5
- [x] Works on stdin via pipe

### A.6 — `tr`: translate or delete characters

**File:** `userspace/coreutils/tr.c`
**Symbol:** `main` (new binary)
**Why it matters:** `tr` handles character-level transformations (case
conversion, whitespace normalization) that are fundamental to shell scripting.

**Acceptance:**
- [x] `echo "HELLO" | tr 'A-Z' 'a-z'` outputs `hello`
- [x] `tr -d '\n'` deletes newlines
- [x] Works on stdin (pipe-only tool)

### A.7 — `sed`: stream editor

**File:** `userspace/coreutils/sed.c`
**Symbol:** `main` (new binary)
**Why it matters:** `sed` is the standard non-interactive text editor. Even a
minimal subset (`s/old/new/`, `d`, `p`) covers the vast majority of use cases.

**Acceptance:**
- [x] `sed 's/foo/bar/' file.txt` performs substitution on each line
- [x] `sed 's/foo/bar/g'` performs global substitution
- [x] `sed -n '3,5p'` prints only lines 3 through 5
- [x] Works on stdin via pipe

### A.8 — `tee`: duplicate stdin to file and stdout

**File:** `userspace/coreutils/tee.c`
**Symbol:** `main` (new binary)
**Why it matters:** `tee` enables capturing intermediate pipeline output to a
file while passing it along, which is critical for debugging pipelines.

**Acceptance:**
- [x] `echo hello | tee output.txt` writes to both stdout and `output.txt`
- [x] `tee -a output.txt` appends instead of truncating
- [x] Works in multi-stage pipelines

---

## Track B — File and Directory Tools

### B.1 — `find`: search for files by name and type

**File:** `userspace/coreutils/find.c`
**Symbol:** `main` (new binary)
**Why it matters:** `find` is essential for code navigation and build scripts.
It exercises recursive directory traversal (`getdents64`), symlink-aware `stat`,
and pattern matching.

**Acceptance:**
- [x] `find /path -name "*.c"` lists matching files recursively
- [x] `find /path -type f` lists regular files only
- [x] `find /path -type d` lists directories only
- [x] Follows symlinks by default (or `-L` flag)

### B.2 — `xargs`: build commands from stdin

**File:** `userspace/coreutils/xargs.c`
**Symbol:** `main` (new binary)
**Why it matters:** `xargs` converts newline- or null-delimited input into
command arguments, enabling `find ... | xargs grep ...` workflows.

**Acceptance:**
- [x] `find . -name "*.c" | xargs grep "main"` executes `grep` on found files
- [x] `xargs -I {} cmd {}` supports replacement strings
- [x] `xargs -0` supports null-delimited input (for `find -print0`)

### B.3 — `du`: disk usage summary

**File:** `userspace/coreutils/du.c`
**Symbol:** `main` (new binary)
**Why it matters:** `du` shows directory sizes by recursively summing file sizes
via `stat()`, providing the first disk-space visibility tool.

**Acceptance:**
- [x] `du /path` shows space used by each subdirectory
- [x] `du -s /path` shows only the total for the given path
- [x] `du -h` prints human-readable sizes (K, M)

### B.4 — `df`: filesystem free space

**File:** `userspace/coreutils/df.c`
**Symbol:** `main` (new binary)
**Why it matters:** `df` reads `statfs`/`fstatfs` (implemented in Phase 38) to
display free and used space on each mounted filesystem.

**Acceptance:**
- [x] `df` lists all mounted filesystems with total/used/free space
- [x] `df -h` prints human-readable sizes
- [x] Reads from `statfs()` syscall (or `/proc/mounts` + per-mount `statfs`)

### B.5 — `ln`: create links

**File:** `userspace/coreutils/ln.c`
**Symbol:** `main` (new binary)
**Why it matters:** The `ln` Rust coreutil already exists (`userspace/coreutils-rs/src/ln.rs`),
but a C version covers the case where sbase or other ported tools expect a
standalone `ln` binary without the Rust runtime.

**Acceptance:**
- [x] `ln -s target linkname` creates a symlink (delegates to `symlink()` syscall)
- [x] `ln target linkname` creates a hard link (delegates to `link()` syscall)
- [x] Error messages on permission or filesystem failures

### B.6 — `file`: basic file type identification

**File:** `userspace/coreutils/file.c`
**Symbol:** `main` (new binary)
**Why it matters:** `file` inspects magic bytes to identify ELF binaries, text
files, and other common types — useful for debugging and scripting.

**Acceptance:**
- [x] `file binary.elf` prints `ELF 64-bit` (reads ELF magic `\x7fELF`)
- [x] `file text.c` prints `ASCII text` (heuristic: no NUL bytes)
- [x] `file /dev/null` prints `character special`

### B.7 — `hexdump`: hex dump of binary files

**File:** `userspace/coreutils/hexdump.c`
**Symbol:** `main` (new binary)
**Why it matters:** `hexdump` provides a binary file inspector essential for
debugging ELF loading, filesystem corruption, and raw data formats.

**Acceptance:**
- [x] `hexdump file` prints canonical hex+ASCII output
- [x] `hexdump -C file` prints offset, hex bytes, and ASCII sidebar
- [x] `hexdump -n 64 file` limits output to first 64 bytes

---

## Track C — System Tools

These tools read kernel state through `/proc` (implemented in Phase 38) or
existing libc-visible syscalls.

### C.1 — `ps`: list running processes

**File:** `userspace/coreutils/ps.c`
**Symbol:** `main` (new binary)
**Why it matters:** `ps` is the standard process inspector. It reads
`/proc/{pid}/status` and `/proc/{pid}/cmdline` for every PID listed in `/proc/`.

**Acceptance:**
- [x] `ps` shows PID, status, and command name for the calling user's processes
- [x] `ps -e` (or `ps -A`) shows all processes
- [x] Output columns: PID, STATE, CMD at minimum
- [x] Reads from `/proc/{pid}/status` and `/proc/{pid}/cmdline`

### C.2 — `kill`: send signals to processes (standalone binary)

**File:** `userspace/coreutils/kill.c`
**Symbol:** `main` (new binary)
**Why it matters:** A standalone `kill` binary lets scripts and `xargs` send
signals without depending on shell-specific builtins. It is a thin wrapper
around the existing `kill(2)` syscall exposed through musl libc.

**Acceptance:**
- [x] `kill -9 <pid>` sends SIGKILL
- [x] `kill <pid>` sends SIGTERM (default)
- [x] `kill -l` lists available signal names
- [x] Uses the existing `kill(2)` interface available to C userspace

### C.3 — `free`: memory usage summary

**File:** `userspace/coreutils/free.c`
**Symbol:** `main` (new binary)
**Why it matters:** `free` reads `/proc/meminfo` to display total, used, and
available memory. The `meminfo` Rust coreutil already exists
(`userspace/coreutils-rs/src/meminfo.rs`); this C version adds a familiar
`free`-style output format.

**Acceptance:**
- [x] `free` displays total, used, and available memory in KB
- [x] `free -m` displays in MB
- [x] `free -h` displays human-readable sizes
- [x] Reads from `/proc/meminfo`

### C.4 — `uptime`: show time since boot

**File:** `userspace/coreutils-rs/src/uptime.rs`
**Symbol:** `main`
**Why it matters:** The OS already ships a Rust `uptime` utility from Phase 34.
Phase 41 should either keep that binary as the canonical implementation or
extend its output/options so it fits the broader expanded-coreutils milestone.

**Acceptance:**
- [x] `uptime` prints time since boot using `CLOCK_MONOTONIC`
- [x] Output remains available by default from the shell without an absolute path
- [x] If Phase 41 changes the output format, the design doc records that choice explicitly

### C.5 — `dmesg`: display kernel log buffer

**Files:**
- `kernel/src/serial.rs` (kernel-side ring buffer)
- `userspace/coreutils/dmesg.c`

**Symbol:** `dmesg` (new binary), `DMESG_RING` (new kernel buffer)
**Why it matters:** Currently kernel logs go only to serial output and are lost
to userspace. A kernel ring buffer plus a `/proc/kmsg` (or `sys_syslog`) read
interface lets userspace inspect boot and runtime kernel messages.

**Acceptance:**
- [x] Kernel captures log output into a fixed-size ring buffer alongside serial
- [x] `dmesg` reads and displays the kernel log buffer from userspace
- [x] New log lines appear in `dmesg` output after boot messages
- [x] Interface is either `/proc/kmsg` file or a custom syscall

### C.6 — `mount` / `umount`: mount and unmount filesystems

**Files:**
- `userspace/coreutils/mount.c`
- `userspace/coreutils/umount.c`

**Symbol:** `main` (new binaries)
**Why it matters:** These wrap the existing `mount()` syscall (already in
`syscall-lib`) and a new `umount()` syscall. `mount` with no arguments should
display currently mounted filesystems by reading `/proc/mounts`.

**Acceptance:**
- [x] `mount` (no args) displays mounted filesystems from `/proc/mounts`
- [x] `mount -t ext2 /dev/vda1 /mnt` mounts a filesystem
- [x] `umount /mnt` unmounts a filesystem
- [x] Error messages for permission denied (non-root) and busy filesystems

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
- [x] `DMESG_RING` is a fixed-size buffer (e.g. 64 KiB) behind a `spin::Mutex`
- [x] Every `serial_println!()` call also appends to the ring buffer
- [x] Buffer wraps on overflow, preserving the most recent messages
- [x] `cargo xtask test` still passes (serial output unchanged)

### D.2 — Expose kernel log to userspace via `/proc/kmsg`

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** `render_kmsg`
**Why it matters:** The `dmesg` binary needs a read interface. Adding a
`/proc/kmsg` virtual file that returns the ring buffer contents is the simplest
approach — no new syscall needed.

**Acceptance:**
- [x] `cat /proc/kmsg` returns current ring buffer contents
- [x] File appears in `/proc/` directory listing
- [x] Read returns a snapshot (not streaming) of the buffer

### D.3 — `sys_umount2()` syscall

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `userspace/syscall-lib/src/lib.rs`

**Symbol:** `sys_umount2`, `SYS_UMOUNT2`
**Why it matters:** `mount()` exists but there is no `umount` syscall yet. The
`umount` binary needs it to detach a filesystem from a mount point.

**Acceptance:**
- [x] `SYS_UMOUNT2` (166) dispatched in the syscall handler
- [x] Kernel removes the mount point from the VFS mount table
- [x] Returns `-EBUSY` if files on the mount are still open
- [x] Returns `-EPERM` if caller is not root
- [x] `syscall-lib` wrapper `umount()` added

---

## Track E — Permission Tools

### E.1 — `chmod`: change file permissions

**File:** `userspace/coreutils/chmod.c`
**Symbol:** `main` (new binary)
**Why it matters:** `chmod` is a thin C userspace wrapper around the existing
`chmod(2)` syscall and needs reliable octal mode parsing (`chmod 755 file`).

**Acceptance:**
- [x] `chmod 755 file` sets rwxr-xr-x permissions
- [x] `chmod u+x file` adds execute for owner (symbolic mode, stretch goal)
- [x] Error on non-existent file or permission denied
- [x] Uses the existing `chmod(2)` interface available to C userspace

### E.2 — `chown`: change file ownership

**File:** `userspace/coreutils/chown.c`
**Symbol:** `main` (new binary)
**Why it matters:** `chown` is a thin C userspace wrapper around the existing
`chown(2)` syscall and needs `user:group` parsing with `/etc/passwd` lookup.

**Acceptance:**
- [x] `chown root:root file` changes owner and group
- [x] `chown 0:0 file` accepts numeric UID:GID
- [x] Only root can change ownership (kernel enforces)
- [x] Uses the existing `chown(2)` interface available to C userspace

---

## Track F — Developer Tools

### F.1 — `diff`: compare two files

**File:** `userspace/coreutils/diff.c`
**Symbol:** `main` (new binary)
**Why it matters:** `diff` is essential for development workflows — reviewing
changes, creating patches. A minimal unified-diff implementation covers most
use cases.

**Acceptance:**
- [x] `diff file1 file2` outputs differences in unified format
- [x] `diff -u file1 file2` explicitly requests unified format
- [x] Exit code 0 = identical, 1 = differences, 2 = error
- [x] Output is compatible with `patch` (Track F.2)

### F.2 — `patch`: apply diffs

**File:** `userspace/coreutils/patch.c`
**Symbol:** `main` (new binary)
**Why it matters:** `patch` applies unified diffs, completing the edit–diff–patch
development cycle that `diff` starts.

**Acceptance:**
- [x] `patch < changes.diff` applies a unified diff to the working directory
- [x] `patch -p1 < changes.diff` strips one leading path component
- [x] Reports success/failure for each hunk

### F.3 — `less`: scrollable file pager

**File:** `userspace/coreutils/less.c`
**Symbol:** `main` (new binary)
**Why it matters:** `less` provides scrollable viewing of files and command
output. It uses raw terminal mode (termios, implemented in Phase 22) to capture
arrow keys and page-up/down.

**Acceptance:**
- [x] `less file.txt` displays file with scrollable navigation
- [x] Arrow keys and Page Up/Page Down scroll the viewport
- [x] `/pattern` searches forward in the file
- [x] `q` exits the pager
- [x] Works as a pipe target: `cat file | less`

### F.4 — `strings`: extract printable strings from binaries

**File:** `userspace/coreutils/strings.c`
**Symbol:** `main` (new binary)
**Why it matters:** `strings` is a quick binary inspection tool — useful for
examining ELF binaries, debugging, and reverse engineering.

**Acceptance:**
- [x] `strings binary.elf` prints sequences of ≥4 printable characters
- [x] `strings -n 8 binary.elf` sets minimum string length to 8
- [x] Works on any file type (reads raw bytes)

### F.5 — `cal`: calendar display

**File:** `userspace/coreutils/cal.c`
**Symbol:** `main` (new binary)
**Why it matters:** `cal` is a classic Unix tool that exercises date/time
arithmetic. Reads the current date via `gettimeofday()` or `clock_gettime()`
(available since Phase 34).

**Acceptance:**
- [x] `cal` displays the current month's calendar
- [x] `cal 2025` displays all 12 months of 2025
- [x] `cal 6 2025` displays June 2025
- [x] Highlights today's date (if terminal supports bold/inverse)

---

## Track G — Initrd Packaging and Shell Integration

### G.1 — Add new C binaries to `build_musl_bins()`

**File:** `xtask/src/main.rs`
**Symbol:** `build_musl_bins`
**Why it matters:** Every new C binary must be registered in the xtask musl build
list so it gets cross-compiled with `musl-gcc -static` and placed in
`kernel/initrd/`.

**Acceptance:**
- [x] All new C binaries from Tracks A–F added to the `bins` array in `build_musl_bins()`
- [x] `cargo xtask image` compiles and packages all new tools
- [x] Each binary appears as `kernel/initrd/{name}.elf`

### G.2 — Register binaries in init or shell PATH

**Files:**
- `userspace/init/src/main.rs`
- `userspace/shell/src/main.rs`

**Symbol:** binary lookup path
**Why it matters:** New tools must be discoverable by the shell. Either the
shell's PATH already covers `/bin` (where initrd tools are copied at boot), or
init must copy the new ELFs to the correct directory.

**Acceptance:**
- [x] All new tools are available by name from the shell prompt
- [x] `head`, `find`, `ps`, and `dmesg` run from the default shell PATH without absolute paths
- [x] Boot or startup packaging places each binary in the same location the shell already searches

### G.3 — Add new Rust binaries to `coreutils_bins` (if any)

**File:** `xtask/src/main.rs`
**Symbol:** `coreutils_bins`
**Why it matters:** If any tools are implemented as Rust coreutils instead of C,
they must be added to the `coreutils_bins` array so they are built and packaged.

**Acceptance:**
- [x] Any Rust-implemented tools added to `coreutils_bins` list
- [x] Corresponding `[[bin]]` entry added to `userspace/coreutils-rs/Cargo.toml`
- [x] `cargo xtask image` builds and packages them alongside existing Rust tools

---

## Track H — Integration Testing and Documentation

### H.1 — Pipeline integration tests

**Symbol:** manual QEMU validation
**Why it matters:** The acceptance criteria for this phase specifically require
multi-tool pipelines to work. These are validated manually inside QEMU.

**Acceptance:**
- [x] `cat file | sort | uniq -c | sort -rn | head -10` produces correct output
- [x] `find . -name "*.c" | xargs grep "main"` finds matches
- [x] `diff file1 file2 > changes.diff && patch < changes.diff` round-trips
- [x] `ps -e` shows all running processes with PID, state, and command
- [x] `free -h` shows total and available memory
- [x] `less file.txt` provides scrollable viewing with `q` to exit
- [x] `dmesg` shows kernel boot messages

### H.2 — Update Phase 41 design doc

**File:** `docs/roadmap/41-expanded-coreutils.md`
**Symbol:** acceptance criteria, status
**Why it matters:** The design doc must be updated to reflect the actual
implementation — which tools were ported vs. written, and any scope changes.

**Acceptance:**
- [x] Status updated to "Complete"
- [x] Any deferred tools noted in the "Deferred Until Later" section
- [x] Implementation Outline reflects actual approach taken

### H.3 — Update roadmap README

**File:** `docs/roadmap/README.md`
**Symbol:** Phase 41 row
**Why it matters:** The roadmap summary must stay current.

**Acceptance:**
- [x] Phase 41 status updated to "Complete"
- [x] Tasks column links to the task list

---

## Documentation Notes

- Phase 38 already provides `/proc/meminfo`, `/proc/uptime`, `/proc/mounts`,
  and `/proc/{pid}/status` — tools like `ps`, `free`, and `mount` (no args) can
  read these directly without new kernel work.
- The `uptime` and `meminfo` Rust coreutils already exist in
  `userspace/coreutils-rs/`. Phase 41 can keep `uptime` in Rust while adding
  `free` as the more familiar `/proc/meminfo` presentation for Unix users.
- Syscalls for `chmod()`, `chown()`, `fchmod()`, `fchown()`, `mount()`, `kill()`,
  and `link()`/`symlink()` already exist in `syscall-lib`. Most Track C and E
  tasks rely on kernel support that is already present even when the final
  binary is implemented in C via musl libc.
- The main new kernel work is Track D: the dmesg ring buffer and `umount` syscall.
  All other tools build on existing infrastructure.
- The C tools are preferred over Rust for this phase because sbase provides
  clean, portable reference implementations that can be cross-compiled with
  `musl-gcc -static` using the same pipeline as existing C coreutils.
- `bc` is intentionally out of the Track F task list even though it appeared in
  early Phase 41 brainstorming; it stays deferred until after the core text,
  file, and system utilities are in place.
- A `ln` C binary (Track B.5) may be redundant with the existing Rust `ln` — the
  decision depends on whether sbase tools expect a C-linked `ln` in their PATH.
