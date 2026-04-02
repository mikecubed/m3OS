# Phase 34 — Real-Time Clock and Timekeeping

**Aligned Roadmap Phase:** Phase 34
**Status:** Complete
**Source Ref:** phase-34

## Overview

Phase 34 adds wall-clock time awareness to m3OS by reading the CMOS Real-Time
Clock (RTC) at boot, computing the Unix epoch timestamp, and exposing it through
the `clock_gettime()` and `gettimeofday()` syscalls. Userspace utilities `date`
and `uptime` are provided.

## Architecture

### CMOS RTC Driver (`kernel/src/rtc.rs`)

Reads the MC146818-compatible RTC via CMOS I/O ports:

- **Port 0x70**: Address register (bit 7 = disable NMI during access)
- **Port 0x71**: Data register

Registers read: seconds (0x00), minutes (0x02), hours (0x04), day (0x07),
month (0x08), year (0x09), century (0x32).

**Atomic read protocol**: The RTC update cycle takes ~244us. To avoid torn
reads, the driver reads all registers twice and compares — if values differ,
it retries (max 5 attempts). It also checks Status Register A bit 7
(update-in-progress) before reading.

**BCD handling**: Most CMOS chips use BCD encoding by default. Status Register B
bit 2 indicates binary mode. If BCD, all values are converted via
`bcd_to_binary()`. 12-hour mode (Status Register B bit 1) is also handled.

**Century register**: Uses the standard register 0x32. Falls back to assuming
century=20 if the register returns 0.

### Time Conversion Library (`kernel-core/src/time.rs`)

Pure-logic, host-testable time conversion functions:

- `is_leap_year(year)` — Gregorian leap year check
- `days_in_month(year, month)` — days per month accounting for leap years
- `date_to_unix_timestamp(year, month, day, hour, minute, second)` — UTC to epoch
- `unix_timestamp_to_date(epoch_secs)` — epoch to DateTime struct with weekday
- `bcd_to_binary(bcd)` — BCD byte to binary conversion

15 host-side unit tests cover known timestamps, leap year boundaries, round-trip
correctness, weekday calculation, and BCD conversion.

### Boot Epoch (`BOOT_EPOCH_SECS`)

At kernel boot (after ACPI init), `rtc::init_rtc()` reads the RTC, converts to
Unix epoch seconds, and stores in a global `AtomicU64`. The boot log shows:

```
RTC: 2026-04-01 12:34:56 UTC (epoch=1775046896)
```

### Clock ID Dispatch

`clock_gettime()` now dispatches on the `clk_id` parameter:

| Clock ID | Value | Behavior |
|---|---|---|
| `CLOCK_REALTIME` | 0 | `BOOT_EPOCH_SECS + ticks / 100` |
| `CLOCK_MONOTONIC` | 1 | `ticks / 100` (time since boot) |
| `CLOCK_MONOTONIC_RAW` | 4 | Same as MONOTONIC |
| `CLOCK_REALTIME_COARSE` | 5 | Same as REALTIME |
| `CLOCK_MONOTONIC_COARSE` | 6 | Same as MONOTONIC |
| Other | — | Returns `-EINVAL` |

`gettimeofday()` returns wall-clock time (same as `CLOCK_REALTIME`).

### Userspace Utilities

- **`date`** (`coreutils-rs/src/date.rs`): Calls `clock_gettime(CLOCK_REALTIME)`,
  converts to broken-down time via `gmtime()`, formats as
  "Wed Apr  1 12:30:00 UTC 2026".

- **`uptime`** (`coreutils-rs/src/uptime.rs`): Calls
  `clock_gettime(CLOCK_MONOTONIC)`, formats as "up H:MM:SS" or "up Xd H:MM:SS".

Time formatting is implemented in `syscall-lib` using manual buffer formatting
(no alloc needed): `gmtime()`, `format_datetime()`.

## Known Limitations

- **10ms resolution**: LAPIC timer runs at ~100 Hz. Nanosecond/microsecond
  fields have 10ms granularity.
- **Drift**: Time is synthesized as `boot_epoch + ticks`. No NTP or clock
  steering. Accumulated drift depends on LAPIC timer accuracy.
- **UTC only**: No timezone support. All times are UTC.
- **Read-once**: RTC is read only at boot. No periodic re-sync.

## Files Changed

| File | Change |
|---|---|
| `kernel-core/src/time.rs` | New: time conversion library + 15 tests |
| `kernel-core/src/lib.rs` | Added `pub mod time` |
| `kernel/src/rtc.rs` | New: CMOS RTC driver |
| `kernel/src/main.rs` | Added `mod rtc` + `init_rtc()` call |
| `kernel/src/arch/x86_64/syscall.rs` | Updated clock_gettime/gettimeofday |
| `kernel/src/fs/ramdisk.rs` | Added date/uptime to initrd |
| `userspace/syscall-lib/src/lib.rs` | Added time wrappers + formatting |
| `userspace/coreutils-rs/src/date.rs` | New: date utility |
| `userspace/coreutils-rs/src/uptime.rs` | New: uptime utility |
| `userspace/coreutils-rs/Cargo.toml` | Added date/uptime binaries |
| `xtask/src/main.rs` | Added date/uptime to build list |
| `kernel/Cargo.toml` | Version bump 0.33.0 → 0.34.0 |
