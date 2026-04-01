# Phase 34 — Real-Time Clock and Timekeeping: Task List

**Depends on:** Phase 15 (Hardware Discovery) ✅, Phase 3 (Interrupts) ✅
**Goal:** The OS knows what time it is. Read the CMOS RTC at boot to establish
wall-clock time, distinguish `CLOCK_REALTIME` from `CLOCK_MONOTONIC` in
`clock_gettime()`, update `gettimeofday()`, and provide `date` and `uptime`
userspace utilities.

## Prerequisite Analysis

Current state (post-Phase 33, confirmed via roadmap):
- `clock_gettime()` syscall exists but ignores `clk_id` — always returns monotonic ticks
- `gettimeofday()` returns monotonic ticks, not wall-clock time
- LAPIC timer running at ~100 Hz, providing `TICK_COUNT` via atomics
- ACPI table parsing present (Phase 15) — can read FADT for century register
- I/O port access infrastructure available (`x86_64::instructions::port`)
- No CMOS RTC driver
- No Unix timestamp conversion functions
- No wall-clock time awareness

Already implemented (no new work needed):
- I/O port read/write (`Port::new(0x70)`, `Port::new(0x71)`)
- LAPIC tick counter (`TICK_COUNT` atomic)
- `clock_gettime()` and `gettimeofday()` syscall plumbing
- ACPI FADT parsing (century register field accessible)
- Userspace `syscall-lib` wrappers for time syscalls

Needs to be added:
- CMOS RTC driver (read hardware clock)
- BCD-to-binary conversion
- Unix timestamp conversion (date → epoch seconds)
- Boot epoch storage (`BOOT_EPOCH_SECS`)
- `clock_gettime()` clock ID dispatch (REALTIME vs MONOTONIC)
- `gettimeofday()` wall-clock update
- `date` userspace utility
- `uptime` userspace utility
- Time formatting library for userspace

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Time conversion in `kernel-core` (host-testable) | — | Not started |
| B | CMOS RTC driver in kernel | — | Not started |
| C | Boot epoch + clock_gettime/gettimeofday updates | A, B | Not started |
| D | Userspace time library + `date` and `uptime` utilities | C | Not started |
| E | Integration testing and documentation | All | Not started |

### Implementation Notes

- **Time conversion in `kernel-core`**: Date-to-epoch and epoch-to-date functions are
  pure math — no hardware access. Host-testable via `cargo test -p kernel-core`.
- **CMOS RTC reads require I/O port access**: The driver itself must live in `kernel/`
  since it uses `in`/`out` instructions. Keep the logic minimal — read registers, call
  into `kernel-core` for conversion.
- **BCD encoding**: Most CMOS chips use BCD by default. Status Register B bit 2 indicates
  binary mode — check it, but expect BCD and convert.
- **Atomic read protocol**: The RTC update cycle takes ~244 µs. Read twice and compare
  to avoid torn reads. Check Status Register A bit 7 (update-in-progress) first.
- **Century register**: ACPI FADT byte offset 108 (`century` field) gives the CMOS
  register number for the century digit. Fall back to assuming 20xx if not available.

---

## Track A — Time Conversion Library (kernel-core)

Pure-logic date/time conversion functions, host-testable.

### A.1 — Implement `days_in_month()` and `is_leap_year()`

**File:** `kernel-core/src/time.rs` (new)

```rust
pub fn is_leap_year(year: u32) -> bool { ... }
pub fn days_in_month(year: u32, month: u32) -> u32 { ... }
```

Standard leap year rules: divisible by 4, except centuries, except 400-year cycles.

**Acceptance:**
- [ ] Correct for years 1970–2099 (covers our operational range)
- [ ] Leap years: 2000 (yes), 1900 (no), 2024 (yes), 2100 (no)
- [ ] Days-in-month correct for all months including Feb in leap/non-leap years

### A.2 — Implement `date_to_unix_timestamp()`

**File:** `kernel-core/src/time.rs`

```rust
pub fn date_to_unix_timestamp(
    year: u32, month: u32, day: u32,
    hour: u32, minute: u32, second: u32,
) -> u64 { ... }
```

Convert a UTC date/time to seconds since Unix epoch (1970-01-01 00:00:00 UTC).
Algorithm: count days from 1970 to the target date, multiply by 86400, add HMS.

**Acceptance:**
- [ ] `1970-01-01 00:00:00` → 0
- [ ] `2000-01-01 00:00:00` → 946684800
- [ ] `2026-04-01 12:00:00` → correct value (verify against external tool)
- [ ] Handles month boundaries and leap days correctly

### A.3 — Implement `unix_timestamp_to_date()`

**File:** `kernel-core/src/time.rs`

```rust
pub struct DateTime {
    pub year: u32,
    pub month: u32,   // 1–12
    pub day: u32,     // 1–31
    pub hour: u32,    // 0–23
    pub minute: u32,  // 0–59
    pub second: u32,  // 0–59
    pub weekday: u32, // 0=Sun, 1=Mon, ..., 6=Sat
}

pub fn unix_timestamp_to_date(epoch_secs: u64) -> DateTime { ... }
```

Inverse of `date_to_unix_timestamp()`, plus weekday calculation.
Weekday: `(days_since_epoch + 4) % 7` (1970-01-01 was a Thursday = day 4).

**Acceptance:**
- [ ] Round-trip: `unix_timestamp_to_date(date_to_unix_timestamp(d))` == d
- [ ] Weekday calculation correct for known dates
- [ ] Handles dates up to at least year 2099

### A.4 — Implement BCD conversion helper

**File:** `kernel-core/src/time.rs`

```rust
pub fn bcd_to_binary(bcd: u8) -> u8 {
    (bcd >> 4) * 10 + (bcd & 0x0F)
}
```

Trivial but needs to be correct and tested.

**Acceptance:**
- [ ] `bcd_to_binary(0x59)` → 59
- [ ] `bcd_to_binary(0x12)` → 12
- [ ] `bcd_to_binary(0x00)` → 0

### A.5 — Host-side unit tests for time conversion

**File:** `kernel-core/src/time.rs` (tests module)

Comprehensive test vectors:
- Known epoch timestamps (1970, 2000, 2024, 2026, 2038, 2099)
- Leap year boundaries (Feb 28/29 transitions)
- End-of-year rollover (Dec 31 → Jan 1)
- Round-trip property tests
- BCD conversion edge cases
- Weekday verification for known dates

**Acceptance:**
- [ ] At least 10 unit tests covering the above scenarios
- [ ] All pass via `cargo test -p kernel-core`

---

## Track B — CMOS RTC Driver

Read the hardware real-time clock via CMOS I/O ports.

### B.1 — Implement CMOS register read function

**File:** `kernel/src/rtc.rs` (new)

```rust
/// Read a CMOS register. Port 0x70 selects the register (with NMI disable
/// bit 7 preserved), port 0x71 reads the value.
unsafe fn cmos_read(register: u8) -> u8 {
    let mut addr_port = Port::<u8>::new(0x70);
    let mut data_port = Port::<u8>::new(0x71);
    unsafe {
        addr_port.write(register | 0x80); // bit 7 = disable NMI during read
        data_port.read()
    }
}
```

**Acceptance:**
- [ ] Reads CMOS registers without panicking
- [ ] NMI disable bit set during read (bit 7 of port 0x70)

### B.2 — Implement atomic RTC read with BCD handling

**File:** `kernel/src/rtc.rs`

```rust
pub fn read_rtc() -> (u32, u32, u32, u32, u32, u32) {
    // 1. Wait for update-in-progress to clear (reg 0x0A bit 7)
    // 2. Read seconds, minutes, hours, day, month, year, century
    // 3. Read again and compare — if different, retry
    // 4. Check Status Register B for BCD vs binary mode
    // 5. Check Status Register B for 12h vs 24h mode
    // 6. Convert BCD to binary if needed
    // 7. Apply century register (from ACPI FADT or default to 20)
    // Returns: (year, month, day, hour, minute, second)
}
```

**Acceptance:**
- [ ] Handles BCD mode (convert via `kernel-core` helper)
- [ ] Handles 12-hour mode with AM/PM bit (bit 7 of hours register)
- [ ] Retry loop detects torn reads (values changed between first and second read)
- [ ] Century register used if available from ACPI FADT, otherwise defaults to 20

### B.3 — Wire RTC module into kernel build

**File:** `kernel/src/main.rs`

Add `mod rtc;` declaration and call `rtc::read_rtc()` during boot sequence
(after ACPI init, before userspace launch). Log the detected time.

**Acceptance:**
- [ ] `cargo xtask check` passes with new module
- [ ] Boot log shows: `RTC: 2026-04-01 12:34:56 UTC` (or similar)

---

## Track C — Boot Epoch and Syscall Updates

Store boot wall-clock time and update time-related syscalls.

### C.1 — Store boot epoch timestamp

**File:** `kernel/src/rtc.rs`

```rust
use core::sync::atomic::{AtomicU64, Ordering};

pub static BOOT_EPOCH_SECS: AtomicU64 = AtomicU64::new(0);

pub fn init_rtc() {
    let (year, month, day, hour, minute, second) = read_rtc();
    let epoch = kernel_core::time::date_to_unix_timestamp(year, month, day, hour, minute, second);
    BOOT_EPOCH_SECS.store(epoch, Ordering::Relaxed);
    log::info!("RTC: {}-{:02}-{:02} {:02}:{:02}:{:02} UTC (epoch={})",
               year, month, day, hour, minute, second, epoch);
}
```

Call `init_rtc()` from kernel boot sequence.

**Acceptance:**
- [ ] `BOOT_EPOCH_SECS` is non-zero after boot
- [ ] Logged epoch value matches expected current time

### C.2 — Update `clock_gettime()` to dispatch on clock ID

**File:** `kernel/src/arch/x86_64/syscall.rs` (or wherever `sys_clock_gettime` lives)

Currently ignores `clk_id`. Update to:

```rust
match clk_id {
    CLOCK_REALTIME | CLOCK_REALTIME_COARSE => {
        let boot_epoch = rtc::BOOT_EPOCH_SECS.load(Ordering::Relaxed);
        let ticks = TICK_COUNT.load(Ordering::Relaxed);
        let secs = boot_epoch + ticks / TICKS_PER_SEC;
        let nsecs = (ticks % TICKS_PER_SEC) * (1_000_000_000 / TICKS_PER_SEC);
        // write to user timespec
    }
    CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE => {
        // existing behavior: ticks since boot
    }
    _ => return -EINVAL,
}
```

**Acceptance:**
- [ ] `CLOCK_REALTIME` returns Unix timestamps (seconds since 1970)
- [ ] `CLOCK_MONOTONIC` returns time since boot (unchanged behavior)
- [ ] Invalid clock IDs return `-EINVAL`
- [ ] Nanosecond field is populated (10ms resolution from 100 Hz timer)

### C.3 — Update `gettimeofday()` to return wall-clock time

**File:** `kernel/src/arch/x86_64/syscall.rs` (or wherever `sys_gettimeofday` lives)

`gettimeofday()` should return wall-clock time (like `CLOCK_REALTIME`), not
monotonic ticks.

```rust
fn sys_gettimeofday(tv_ptr: usize) -> i64 {
    let boot_epoch = rtc::BOOT_EPOCH_SECS.load(Ordering::Relaxed);
    let ticks = TICK_COUNT.load(Ordering::Relaxed);
    let tv_sec = boot_epoch + ticks / TICKS_PER_SEC;
    let tv_usec = (ticks % TICKS_PER_SEC) * (1_000_000 / TICKS_PER_SEC);
    // write tv_sec and tv_usec to user pointer
}
```

**Acceptance:**
- [ ] `gettimeofday()` returns wall-clock seconds since epoch
- [ ] Microsecond field populated
- [ ] Existing callers continue to work

---

## Track D — Userspace Utilities

### D.1 — Add time formatting to `syscall-lib`

**File:** `userspace/syscall-lib/src/time.rs` (new)

Minimal time formatting for userspace:

```rust
pub fn gmtime(epoch_secs: u64) -> DateTime { ... }  // reuse kernel-core logic or reimplement
pub fn format_datetime(dt: &DateTime) -> String { ... }  // "Tue Apr  1 12:30:00 UTC 2026"
```

Weekday names: `["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]`
Month names: `["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]`

Note: `syscall-lib` is `no_std` with `alloc`. Use `alloc::format!` or manual
formatting. Alternatively, share the `DateTime` struct and conversion from
`kernel-core` if the dependency is feasible, or duplicate the small amount of
conversion logic.

**Acceptance:**
- [ ] `gmtime()` converts epoch to broken-down time
- [ ] `format_datetime()` produces human-readable output
- [ ] Day-of-week names correct

### D.2 — Implement `date` userspace utility

**File:** `userspace/coreutils/` (add `date.c`) or `userspace/coreutils-rs/` (add `date` binary)

Decide: C in `coreutils/` or Rust in `coreutils-rs/`. Rust preferred for
consistency with recent utilities and to reuse `syscall-lib` time functions.

```
$ date
Tue Apr  1 12:30:00 UTC 2026
```

Implementation:
1. Call `clock_gettime(CLOCK_REALTIME)` or `gettimeofday()`
2. Convert epoch seconds to broken-down time
3. Format and print

**Acceptance:**
- [ ] `date` displays correct current date and time
- [ ] Output matches host time within a few seconds (verified in QEMU)
- [ ] Binary added to initrd

### D.3 — Implement `uptime` userspace utility

**File:** `userspace/coreutils-rs/` (add `uptime` binary)

```
$ uptime
up 0:01:23
```

Implementation:
1. Call `clock_gettime(CLOCK_MONOTONIC)`
2. Format as `H:MM:SS` or `up X days, H:MM:SS`

**Acceptance:**
- [ ] `uptime` displays time since boot
- [ ] Increases over time (not stuck at 0)
- [ ] Binary added to initrd

### D.4 — Add initrd entries for new binaries

**File:** `kernel/initrd/` and build system

Ensure `date` and `uptime` binaries are built, stripped, and included in the
initrd so they're available from the shell.

**Acceptance:**
- [ ] `date` and `uptime` available in shell after boot
- [ ] `cargo xtask run` shows them in `/` or `/bin`

---

## Track E — Integration Testing and Documentation

### E.1 — Run full test suite

Verify all existing tests pass with the new RTC and time changes:

```bash
cargo xtask test
cargo test -p kernel-core
cargo xtask check
```

**Acceptance:**
- [ ] All existing QEMU tests pass
- [ ] All kernel-core host tests pass (including new time tests)
- [ ] `cargo xtask check` clean (no warnings)

### E.2 — QEMU RTC verification test

**File:** `kernel/tests/rtc.rs` (QEMU test) — optional

A QEMU test that reads `CLOCK_REALTIME` and verifies the timestamp is
reasonable (after year 2020, before year 2100). QEMU provides a virtual RTC
that reflects the host time by default.

**Acceptance:**
- [ ] Test passes in QEMU
- [ ] Timestamp is in a plausible range

### E.3 — Update documentation

**Files:**
- `docs/34-timekeeping.md` (new phase design doc)
- Update `docs/08-roadmap.md` if needed
- Update `docs/roadmap/34-real-time-clock.md` companion task list reference

Document:
- RTC driver design (CMOS registers, BCD handling, atomic read)
- Boot epoch calculation
- `CLOCK_REALTIME` vs `CLOCK_MONOTONIC` distinction
- Known limitations (10ms resolution, drift over time, no NTP)

**Acceptance:**
- [ ] Phase 34 design doc created
- [ ] Roadmap companion reference updated
