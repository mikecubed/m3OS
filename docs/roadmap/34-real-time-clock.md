# Phase 34 - Real-Time Clock and Timekeeping

## Milestone Goal

The OS knows what time it is. A CMOS RTC driver reads the hardware clock at boot to
establish the current wall-clock time. `clock_gettime(CLOCK_REALTIME)` returns actual
Unix timestamps, distinct from `CLOCK_MONOTONIC` which tracks time since boot. The
`date` command displays the current date and time.

## Learning Goals

- Understand the difference between wall-clock time (RTC, NTP) and monotonic time
  (tick counter).
- Learn how the CMOS RTC hardware works (I/O ports 0x70/0x71, BCD encoding, update-in-progress bit).
- See why `CLOCK_REALTIME` and `CLOCK_MONOTONIC` are separate clocks and when each
  is appropriate.
- Understand time representation: Unix epoch, `struct timespec`, UTC vs local time.

## Feature Scope

### CMOS RTC Driver

Read the MC146818-compatible real-time clock present in all PC systems:

**I/O Ports:**
- 0x70: Address/NMI-disable register (write index, bit 7 = disable NMI)
- 0x71: Data register (read/write the selected CMOS register)

**CMOS Registers:**

| Register | Content |
|---|---|
| 0x00 | Seconds (0–59) |
| 0x02 | Minutes (0–59) |
| 0x04 | Hours (0–23 or 1–12 with AM/PM) |
| 0x06 | Day of week (1–7) |
| 0x07 | Day of month (1–31) |
| 0x08 | Month (1–12) |
| 0x09 | Year (0–99) |
| 0x0A | Status Register A (bit 7 = update-in-progress) |
| 0x0B | Status Register B (bit 1 = 24h mode, bit 2 = binary mode) |
| 0x32 | Century register (if available via ACPI FADT) |

**Reading protocol:**
1. Wait until Status Register A bit 7 is clear (update not in progress).
2. Read all time fields.
3. Read again and compare — if values changed, retry (atomic read).
4. Convert from BCD to binary if Status Register B bit 2 is clear.
5. Convert 12-hour to 24-hour if Status Register B bit 1 is clear.

### Boot Epoch Calculation

At kernel boot (after ACPI/PCI init), read the RTC and convert to a Unix timestamp:

```
boot_epoch = rtc_to_unix_timestamp(year, month, day, hour, minute, second)
```

Store as `static BOOT_EPOCH_SECS: AtomicU64`.

### Wall-Clock Time Synthesis

Wall-clock time = `BOOT_EPOCH_SECS + (TICK_COUNT / TICKS_PER_SEC)`

This gives ~10ms resolution (100 Hz LAPIC timer). Higher resolution can be achieved
later with TSC or HPET.

### clock_gettime Improvements

Currently `clock_gettime()` ignores the `clk_id` parameter. Fix this:

| Clock ID | Behavior |
|---|---|
| `CLOCK_REALTIME` (0) | Wall-clock time (boot epoch + ticks) |
| `CLOCK_MONOTONIC` (1) | Time since boot (ticks only, unaffected by clock changes) |
| `CLOCK_MONOTONIC_RAW` (4) | Same as MONOTONIC for now |
| `CLOCK_REALTIME_COARSE` (5) | Same as REALTIME (no high-res timer yet) |
| `CLOCK_MONOTONIC_COARSE` (6) | Same as MONOTONIC |

### gettimeofday Update

Update `gettimeofday()` to return wall-clock time (currently returns monotonic ticks).

### Userspace Utilities

- **`date`** — Display current date and time in human-readable format.
  `date` → `Tue Mar 31 14:30:00 UTC 2026`
- **`uptime`** — Show time since boot using monotonic clock.

### Time Conversion Library

Implement or port minimal time conversion functions for userspace:
- `gmtime()` — Unix timestamp to broken-down UTC time.
- `mktime()` — Broken-down time to Unix timestamp.
- Days-in-month, leap year calculations.

musl provides these, but the kernel needs its own conversion for `boot_epoch` calculation.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 15 (Hardware Discovery) | ACPI FADT for century register location |
| Phase 3 (Interrupts) | I/O port access infrastructure |

## Implementation Outline

1. Write CMOS RTC read functions (with BCD conversion and atomic read loop).
2. Implement `rtc_to_unix_timestamp()` in kernel-core (host-testable).
3. Read RTC during kernel boot and store `BOOT_EPOCH_SECS`.
4. Update `clock_gettime()` to distinguish `CLOCK_REALTIME` vs `CLOCK_MONOTONIC`.
5. Update `gettimeofday()` to use wall-clock time.
6. Write `date` userspace utility.
7. Write host-side tests for time conversion (known dates → expected timestamps).
8. Verify: `date` inside the OS matches host system time (within a few seconds).

## Acceptance Criteria

- `clock_gettime(CLOCK_REALTIME)` returns correct Unix timestamps (verified against host).
- `clock_gettime(CLOCK_MONOTONIC)` returns time since boot (unchanged from current behavior).
- `date` command displays the correct current date and time.
- RTC read handles BCD vs binary mode correctly.
- RTC read handles 12-hour vs 24-hour mode correctly.
- Time conversion passes test vectors (known dates → Unix timestamps).
- Boot log shows the detected wall-clock time.

## Companion Task List

- [Phase 34 Task List](./tasks/34-real-time-clock-tasks.md)

## How Real OS Implementations Differ

Real systems use:
- **NTP** (Network Time Protocol) to synchronize wall-clock time with atomic clocks.
- **HPET** or **TSC** for nanosecond-resolution timekeeping (vs our 10ms LAPIC ticks).
- **vDSO** to serve `clock_gettime()` from userspace without a syscall.
- **Timezone databases** (tzdata) for local time conversion.
- **RTC IRQ** (interrupt on alarm or periodic) for wakeup-from-sleep.
- **adjtimex()** syscall for fine-grained clock steering.

Our approach reads the RTC once at boot and synthesizes time from the tick counter.
This drifts over time but is sufficient for a toy OS. NTP would be a future enhancement.

## Deferred Until Later

- NTP time synchronization
- High-resolution timers (HPET, TSC)
- vDSO for fast clock_gettime
- Timezone support
- RTC alarm / wakeup
- adjtimex() clock steering
- Hardware watchdog timer
