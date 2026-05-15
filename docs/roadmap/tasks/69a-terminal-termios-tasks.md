# Phase 69a — Termios Raw Mode and Line Discipline: Task List

**Status:** Complete
**Source Ref:** phase-69a
**Depends on:** Phase 22 (TTY/PTY) ✅, Phase 29 (PTY Subsystem) ✅, Phase 69 (Terminal Contract Foundations)
**Goal:** Land the POSIX termios contract — full flag plumbing (`c_iflag` / `c_oflag` / `c_cflag` / `c_lflag` / `c_cc`), VMIN/VTIME timer semantics, ISIG-driven signal generation, and the `tcgetattr`/`tcsetattr` syscalls on both kernel TTY0 and the PTY pair — so editors and pagers can take byte-accurate input without canonical-mode line buffering.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Widen `kernel-core::tty::Termios` to the full POSIX shape; pick safe defaults | None | Complete |
| B | `TCGETS`/`TCSETS`/`TCSETSW`/`TCSETSF` ioctl branches; userspace `tcgetattr`/`tcsetattr` | A | Complete |
| C | `c_iflag` plumbing (`IGNCR` / `INLCR` / `ICRNL` / `IUTF8` / `IXON` / `IXOFF`) | B | Complete |
| D | `c_oflag` plumbing (`OPOST` / `ONLCR`) on both TTY and PTY write paths | B | Complete |
| E | `c_lflag` plumbing (`ICANON` / `ECHO` / `ECHOE`/`ECHOK`/`ECHONL` / `ISIG` / `IEXTEN`) | B | Complete |
| F | `c_cc` VMIN / VTIME timer state in the ldisc | E | Complete |
| G | ISIG-driven signal-from-terminal path (`VINTR`/`VQUIT`/`VSUSP`) | E | Complete |
| H | Userspace surface: `Termios`, `tcgetattr`, `tcsetattr`, `cfmakeraw` in `syscall-lib` | B | Complete |
| I | Validation: `tcsmoke` binary + `cargo xtask termios-smoke` gate | C, D, E, F, G, H | Complete |
| J | Documentation: Phase 22 / 29 cross-refs; appendix update; aligned legacy learning doc; kernel patch bump to 0.69.1 | I | Complete |

---

## Track A — Termios Struct Fidelity

### A.1 — Widen `Termios` to the full POSIX shape

**File:** `kernel-core/src/tty.rs`
**Symbol:** `Termios`
**Why it matters:** A short struct cannot represent the flag set every libc shim expects; widening it now (with defaults that preserve current behaviour) lets every later track plug in its bit cleanly.

**Acceptance:**
- [x] `Termios` contains `c_iflag: u32`, `c_oflag: u32`, `c_cflag: u32`, `c_lflag: u32`, `c_cc: [u8; NCCS]` with `NCCS = 19` (matching musl/Linux).
- [x] A `Termios::cooked_default()` constant returns the baseline ICRNL|IXON|OPOST|ONLCR|ICANON|ECHO|ECHOE|ECHOK|ISIG state with VEOF=0x04, VINTR=0x03, VQUIT=0x1C, VERASE=0x7F, VKILL=0x15, VEOL=0, VSUSP=0x1A, VMIN=1, VTIME=0.
- [x] A `Termios::raw_default()` constant matches `cfmakeraw` semantics for host tests.
- [x] Host tests cover: round-trip of every flag through `Termios::cooked_default`; sanity that `raw_default` clears `ICANON|ECHO|ISIG|IEXTEN|OPOST` and the four `c_iflag` mapping bits.

### A.2 — Public `Termios` constants (flag bits + control-char indices)

**File:** `kernel-core/src/tty.rs`
**Symbol:** `pub const ICANON`, `ECHO`, `ISIG`, `OPOST`, `ONLCR`, `ICRNL`, `IXON`, `INLCR`, `IGNCR`, `IUTF8`, `IXOFF`, `IEXTEN`, `ECHOE`, `ECHOK`, `ECHONL`, `VEOF`, `VEOL`, `VEOL2`, `VERASE`, `VINTR`, `VKILL`, `VMIN`, `VQUIT`, `VSUSP`, `VTIME`, `VSTART`, `VSTOP`, `VLNEXT`, `VWERASE`, `VREPRINT`, `VDISCARD`, `VSWTC`
**Why it matters:** Both the kernel and userspace `syscall-lib` need the same numeric values; defining them once in `kernel-core` avoids drift.

**Acceptance:**
- [x] Every constant uses the Linux numeric value (so musl shims work unchanged).
- [x] A host test asserts no two flag bits collide within their respective mode word.

---

## Track B — `tcgetattr` / `tcsetattr` Syscalls

### B.1 — Kernel TTY0 ioctl branches

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_ioctl` (TIOCSWINSZ branch nearby at line 11398)
**Why it matters:** Without `TCGETS`/`TCSETS` the userspace shim has nothing to call; cooked → raw switch is impossible.

**Acceptance:**
- [x] `TCGETS = 0x5401` copies the current `Termios` to user memory.
- [x] `TCSETS = 0x5402` (`TCSANOW`) applies immediately.
- [x] `TCSETSW = 0x5403` (`TCSADRAIN`) drains the output queue before applying.
- [x] `TCSETSF = 0x5404` (`TCSAFLUSH`) drains output and flushes input before applying.
- [x] Each branch validates the user pointer through the existing `read_user`/`write_user` helpers and returns `-EFAULT` on bad memory.

### B.2 — PTY slave ioctl branches

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (PTY ioctl path)
**Symbol:** PTY slave fd ioctl table
**Why it matters:** Editors run inside `term` against a PTY slave, not the kernel TTY; this is the path that actually fires.

**Acceptance:**
- [x] The same four ioctl codes are wired against the PTY slave fd.
- [x] Per-PTY-pair termios state lives in the existing `kernel-core::pty::PtyPair` struct (extended in Track F).
- [x] PTY master is unaffected — `TCGETS` against the master returns `-ENOTTY`.

---

## Track C — `c_iflag` Plumbing

### C.1 — Input mapping arms in the ldisc

**File:** `kernel-core/src/tty.rs`
**Symbol:** `Ldisc::consume`
**Why it matters:** Editors expect that turning these flags off makes the byte stream literal; today they are no-ops.

**Acceptance:**
- [x] `IGNCR` set → drop incoming `\r`.
- [x] `INLCR` set → map incoming `\n` to `\r`.
- [x] `ICRNL` set → map incoming `\r` to `\n`.
- [x] `IXON` set → incoming `VSTOP` (XOFF, 0x13) suspends output; `VSTART` (XON, 0x11) resumes.
- [x] `IXOFF` set → ldisc emits XOFF when the input buffer is ≥ 80% full.
- [x] `IUTF8` set → no behaviour change yet (full effect in Phase 69b); flag round-trips through get/set.
- [x] Host tests cover each mapping in isolation and combinations (e.g. `IGNCR | ICRNL`).

---

## Track D — `c_oflag` Plumbing

### D.1 — `OPOST` / `ONLCR` on TTY write path

**File:** `kernel/src/tty.rs`
**Symbol:** `tty_write`, `output_postprocess`
**Why it matters:** Phase 69's wire protocol assumes raw bytes go through unmodified when `OPOST` is off; today every write goes through cooked post-processing.

**Acceptance:**
- [x] `OPOST` cleared → write bytes are delivered verbatim.
- [x] `OPOST` set + `ONLCR` set → outgoing `\n` is expanded to `\r\n`.
- [x] `OPOST` set + `ONLCR` cleared → outgoing `\n` is delivered as `\n` only.
- [x] Host tests cover all four flag combinations on a canned byte stream.

### D.2 — `OPOST` / `ONLCR` on PTY master-to-slave write

**File:** `kernel-core/src/pty.rs`
**Symbol:** `PtyPair::master_write`
**Why it matters:** Same contract on the PTY path — Phase 69's bracketed-paste write relies on `OPOST` being off.

**Acceptance:**
- [x] Same four-combination behaviour as D.1, exercised against the PTY pair.

---

## Track E — `c_lflag` Plumbing

### E.1 — `ICANON` switch (cooked vs raw)

**File:** `kernel-core/src/tty.rs`
**Symbol:** `Ldisc::consume`, `Ldisc::is_canonical`
**Why it matters:** The cooked-mode line editor swallows bytes until newline; editors must read byte-by-byte.

**Acceptance:**
- [x] `ICANON` cleared → every byte delivered to the ldisc is immediately available for `read`.
- [x] `ICANON` set → existing canonical line-editor behaviour preserved (verified by the existing Phase 22 tests still passing).
- [x] A flag-flip mid-stream is observable on the next byte (no buffering of stale state).

### E.2 — `ECHO` family

**File:** `kernel-core/src/tty.rs`
**Symbol:** `Ldisc::echo_byte`
**Why it matters:** Editors render their own UI; the kernel must not double-echo.

**Acceptance:**
- [x] `ECHO` cleared → no characters are echoed on input.
- [x] `ECHO` set + `ECHOE` set → VERASE prints `^H \b` (the standard erase trio).
- [x] `ECHO` set + `ECHOK` set → VKILL prints a `\n` after killing the line.
- [x] `ECHO` set + `ECHONL` set + `ICANON` set → `\n` is echoed even if `ECHO` is cleared.

### E.3 — `ISIG` + `IEXTEN`

**File:** `kernel-core/src/tty.rs`
**Symbol:** `Ldisc::consume`
**Why it matters:** Editors disable `ISIG` so Ctrl-C is a normal byte; without flipping it, the kernel still sends SIGINT.

**Acceptance:**
- [x] `ISIG` cleared → `VINTR` / `VQUIT` / `VSUSP` are delivered as ordinary bytes.
- [x] `ISIG` set → those control chars trigger Track G's signal path.
- [x] `IEXTEN` set → `VLNEXT` (literal-next) and `VDISCARD` are honoured; cleared → delivered as ordinary bytes.

---

## Track F — `c_cc` VMIN / VTIME Timer

### F.1 — VMIN / VTIME state in the ldisc

**File:** `kernel-core/src/tty.rs`
**Symbol:** `Ldisc::poll_read_ready`, `Ldisc::tick`
**Why it matters:** Editors set `VMIN=1 VTIME=0` for byte-by-byte reads; pagers set `VMIN=0 VTIME=5` for a 500 ms poll. Both must work.

**Acceptance:**
- [x] `VMIN > 0, VTIME == 0` → `poll_read_ready` returns false until ≥ `VMIN` bytes are buffered.
- [x] `VMIN == 0, VTIME > 0` → `poll_read_ready` returns true after `VTIME * 100 ms` even with zero bytes; if a byte arrives sooner, returns true immediately.
- [x] `VMIN > 0, VTIME > 0` → inter-byte timer; first byte arms the timer; returns true on either `VMIN` reached or `VTIME * 100 ms` since the first byte.
- [x] `VMIN == 0, VTIME == 0` → poll: always returns true; `read` returns 0 if no data.
- [x] Host tests cover all four cases with a fake clock.

### F.2 — Thread the timer through the kernel blocking-read primitive

**File:** `kernel/src/tty.rs` (and PTY equivalent in `kernel-core::pty`)
**Symbol:** `tty_read`, `pty_slave_read`
**Why it matters:** Pure-logic timer state is useless without the kernel `WaitQueue` honouring it.

**Acceptance:**
- [x] `tty_read` consults `Ldisc::poll_read_ready` before blocking; if the predicate is true, returns immediately.
- [x] When blocked, the read wakes on either a byte-available event or a VTIME deadline (whichever comes first), via the existing `WaitQueue` block/wake primitive.
- [x] No busy-wait — the timer uses the kernel monotonic clock + a wake-up.

---

## Track G — ISIG Signal Generation

### G.1 — Signal-from-terminal in the ldisc

**File:** `kernel-core/src/tty.rs`
**Symbol:** `Ldisc::consume` → `Action::SignalForeground(SignalKind)`
**Why it matters:** Pure-logic emission of the signal-action keeps the policy host-testable; the kernel side just dispatches.

**Acceptance:**
- [x] When `ISIG` set: incoming `VINTR` returns `Action::SignalForeground(Signal::Int)`; `VQUIT` → `Quit`; `VSUSP` → `Tstp`.
- [x] When `ISIG` cleared: those bytes return `Action::DeliverToReader`.
- [x] Host tests cover both modes and verify byte delivery is suppressed only when the signal action fires.

### G.2 — Kernel dispatch

**File:** `kernel/src/tty.rs`
**Symbol:** `dispatch_ldisc_action`
**Why it matters:** Wire the ldisc action enum to the existing `send_signal_to_group`.

**Acceptance:**
- [x] `Action::SignalForeground(Signal::Int)` calls `send_signal_to_group(fg_pgrp, SIGINT)`.
- [x] Same for `Signal::Quit` → `SIGQUIT` and `Signal::Tstp` → `SIGTSTP`.
- [x] `tcsmoke isig` (Track I) verifies a self-installed SIGINT handler runs after a `VINTR` byte enters the ldisc.

---

## Track H — Userspace `syscall-lib` Surface

### H.1 — `Termios` struct + `tcgetattr` / `tcsetattr` / `cfmakeraw`

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `Termios`, `tcgetattr`, `tcsetattr`, `cfmakeraw`, `TCSANOW`, `TCSADRAIN`, `TCSAFLUSH`
**Why it matters:** The ABI surface every C/Rust app uses must match Linux/musl so future ports work without per-app shims.

**Acceptance:**
- [x] `pub struct Termios` matches the kernel ABI byte-for-byte (use `#[repr(C)]`).
- [x] `tcgetattr(fd) -> Result<Termios, i32>` returns the kernel state or an errno.
- [x] `tcsetattr(fd, when: TcsetWhen, &Termios) -> Result<(), i32>`.
- [x] `cfmakeraw(&mut Termios)` clears `IGNBRK|BRKINT|PARMRK|ISTRIP|INLCR|IGNCR|ICRNL|IXON`, `OPOST`, `ECHO|ECHONL|ICANON|ISIG|IEXTEN`, sets VMIN=1, VTIME=0.
- [x] Re-exports the flag constants from `kernel-core::tty` (or duplicates them with a host-side test asserting equality).

---

## Track I — Validation

### I.1 — `tcsmoke` binary

**Files:**
- `userspace/tcsmoke/Cargo.toml` (new) — or, by implementer's choice, new subcommands on `userspace/tui-smoke` from Phase 69
- `userspace/tcsmoke/src/main.rs` (new)
- workspace + xtask `bins` + ramdisk `BIN_ENTRIES`

**Symbol:** `program_main`
**Why it matters:** Phase 69a's acceptance is byte-level. `tcsmoke` exercises each termios path against a real PTY pair inside the kernel.

**Acceptance:**
- [x] Subcommands: `round-trip`, `icanon-off`, `echo-off`, `vmin-vtime`, `isig`, `opost-off`. Each prints `TC_SMOKE:<name>:ok` or `TC_SMOKE:<name>:fail <reason>`.
- [x] `round-trip` modifies every flag bit, calls `tcsetattr` + `tcgetattr`, and asserts bit-perfect equality.
- [x] `vmin-vtime` covers all four VMIN/VTIME quadrants.
- [x] `isig` self-installs a SIGINT handler and verifies it runs after a VINTR byte.

### I.2 — `cargo xtask termios-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `termios_smoke` subcommand
**Why it matters:** A CI gate is the load-bearing signal that termios behaviour does not regress as 69b/c/d build on top.

**Acceptance:**
- [x] `cargo xtask termios-smoke` boots, runs each `tcsmoke` subcommand, asserts all `:ok`, completes in < 60 s.
- [x] Wired into the pre-push hook behind `M3OS_TERMIOS_REGRESSION=1`.

---

## Track J — Documentation and Release

### J.1 — Cross-reference Phase 22 + Phase 29

**Files:**
- `docs/roadmap/22-tty-pty.md`
- `docs/roadmap/29-pty-subsystem.md`

**Symbol:** N/A
**Why it matters:** Both phases note "minimal termios" or "canonical mode only" in their scope; 69a is the closeout.

**Acceptance:**
- [x] Phase 22 doc notes that the full termios contract landed in Phase 69a.
- [x] Phase 29 doc cross-refs 69a for the PTY-side ioctl coverage.

### J.2 — Extend `docs/appendix/term-escape-sequences.md` with a Termios section

**File:** `docs/appendix/term-escape-sequences.md`
**Symbol:** N/A
**Why it matters:** Same canonical-reference principle as Phase 69 — one place to look up what each flag does in m3OS.

**Acceptance:**
- [x] New "Termios contract" section enumerates every supported flag in each mode word.
- [x] Cross-refs the userspace `syscall-lib` shim and the host test crate.

### J.3 — Create the aligned legacy learning doc

**File:** `docs/69a-terminal-termios.md`
**Symbol:** (new document)
**Why it matters:** Learners need a self-contained reference for the termios contract — the full POSIX flag set, VMIN/VTIME timer semantics, the ISIG signal-from-terminal path, and the cooked-vs-raw line-discipline split — without conflating it with Phase 22's initial TTY bring-up or Phase 29's PTY pair. The aligned legacy doc is the canonical companion to the roadmap design doc per `docs/appendix/doc-templates.md`.

**Acceptance:**
- [x] `docs/69a-terminal-termios.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 69a`, `**Status:** Complete`, `**Source Ref:** phase-69a`, `**Supersedes Legacy Doc:** new`).
- [x] Overview is one learner-friendly paragraph explaining what Phase 22 / 29 left as "minimal termios" and what Phase 69a closes (full flag plumbing, raw/cbreak mode, VMIN/VTIME, ISIG, `tcgetattr`/`tcsetattr` on both TTY0 and PTY slave).
- [x] Key Files table cites `kernel-core/src/tty.rs` (extended — full `Termios` + flag constants + ldisc state machine), `kernel-core/src/pty.rs` (per-pair termios state), `kernel/src/tty.rs` (ldisc action dispatch + blocking-read primitive), `kernel/src/arch/x86_64/syscall/mod.rs` (`TCGETS`/`TCSETS`/`TCSETSW`/`TCSETSF` ioctl branches), `userspace/syscall-lib/src/lib.rs` (`Termios`, `tcgetattr`, `tcsetattr`, `cfmakeraw`), and `userspace/tcsmoke/src/main.rs` (validation binary).
- [x] Closure of Related Phases section cross-refs Phase 22 (TTY/PTY) and Phase 29 (PTY Subsystem) and explicitly notes which "minimal termios" caveats each phase doc carried that Phase 69a closes.
- [x] Related Roadmap Docs links `docs/roadmap/69a-terminal-termios.md` and `docs/roadmap/tasks/69a-terminal-termios-tasks.md`.

### J.4 — Kernel patch bump to 0.69.1

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version`
**Why it matters:** 69a is a closeout/extension phase on top of 69; project convention is a patch bump (0.69.0 → 0.69.1), not a minor bump.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.69.1"`.
- [x] `Cargo.lock` regenerated.
- [x] `AGENTS.md` version cursor updated.
- [x] `cargo xtask check` passes.

---

## Documentation Notes

- 69a extends Phase 22's `kernel-core::tty::Termios` and Phase 29's PTY pair; no new file in either crate is required. Most new code lives in `kernel-core/src/tty.rs` (pure logic) with thin dispatch in `kernel/src/tty.rs`.
- The `ISIG` signal-from-terminal path reuses the existing `send_signal_to_group` helper that Phase 69's `TIOCSWINSZ` branch already uses for SIGWINCH.
- The `IUTF8` flag is parsed and round-tripped in 69a but has no behavioural effect until Phase 69b lands the UTF-8 decoder hook.
- `tcsendbreak`, hardware flow control, and baud-rate selection are explicitly deferred — m3OS has no serial-line driver yet.
