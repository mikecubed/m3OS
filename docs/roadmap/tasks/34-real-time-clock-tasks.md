# Phase 34 — Real-Time Clock and Timekeeping: Task List

**Status:** Complete
**Source Ref:** phase-34
**Depends on:** Phase 15 (Hardware Discovery) ✅, Phase 3 (Interrupts) ✅
**Builds on:** Extends the earlier uptime-only clock syscalls by keeping `CLOCK_MONOTONIC` as the boot-time clock while replacing the old `CLOCK_REALTIME`/`gettimeofday()` behavior with RTC-backed wall-clock time.
**Goal:** The OS knows what time it is. Read the CMOS RTC at boot to establish
wall-clock time, distinguish `CLOCK_REALTIME` from `CLOCK_MONOTONIC` in
`clock_gettime()`, update `gettimeofday()`, and provide `date` and `uptime`
userspace utilities.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Time conversion in `kernel-core` (host-testable) | — | ✅ Done |
| B | CMOS RTC driver in kernel | — | ✅ Done |
| C | Boot epoch + clock_gettime/gettimeofday updates | A, B | ✅ Done |
| D | Userspace time library + `date` and `uptime` utilities | C | ✅ Done |
| E | Integration testing and documentation | All | ✅ Done |

---

## Track A — Time Conversion Library (kernel-core)

### A.1 — Implement `days_in_month()` and `is_leap_year()`
**File:** `kernel-core/src/time.rs`
**Symbol:** `days_in_month`, `is_leap_year`
**Why it matters:** Leap-year-aware month math is the base layer for every RTC conversion added later in the phase.
**Acceptance:**
- [x] Correct for years 1970–2099 (covers our operational range)
- [x] Leap years: 2000 (yes), 1900 (no), 2024 (yes), 2100 (no)
- [x] Days-in-month correct for all months including Feb in leap/non-leap years

### A.2 — Implement `date_to_unix_timestamp()`
**File:** `kernel-core/src/time.rs`
**Symbol:** `date_to_unix_timestamp`
**Why it matters:** The kernel needs a deterministic UTC-to-epoch conversion before it can turn RTC fields into `CLOCK_REALTIME`.
**Acceptance:**
- [x] `1970-01-01 00:00:00` → 0
- [x] `2000-01-01 00:00:00` → 946684800
- [x] `2026-04-01 12:00:00` → 1775044800
- [x] Handles month boundaries and leap days correctly

### A.3 — Implement `unix_timestamp_to_date()`
**File:** `kernel-core/src/time.rs`
**Symbol:** `unix_timestamp_to_date`
**Why it matters:** Userspace `date` formatting and round-trip validation both depend on converting epoch seconds back into calendar fields.
**Acceptance:**
- [x] Round-trip: `unix_timestamp_to_date(date_to_unix_timestamp(d))` == d
- [x] Weekday calculation correct for known dates
- [x] Handles dates up to at least year 2099

### A.4 — Implement BCD conversion helper
**File:** `kernel-core/src/time.rs`
**Symbol:** `bcd_to_binary`
**Why it matters:** CMOS RTC hardware commonly stores time in BCD, so a reusable helper avoids duplicating low-level conversion logic in the kernel.
**Acceptance:**
- [x] `bcd_to_binary(0x59)` → 59
- [x] `bcd_to_binary(0x12)` → 12
- [x] `bcd_to_binary(0x00)` → 0

### A.5 — Host-side unit tests for time conversion
**File:** `kernel-core/src/time.rs`
**Symbol:** `tests`
**Why it matters:** Keeping the pure time math host-testable lets Phase 34 validate its wall-clock foundation without booting QEMU.
**Acceptance:**
- [x] 15 unit tests covering all scenarios
- [x] All pass via `cargo test -p kernel-core`

---

## Track B — CMOS RTC Driver

### B.1 — Implement CMOS register read function
**File:** `kernel/src/rtc.rs`
**Symbol:** `cmos_read`
**Why it matters:** A minimal raw CMOS accessor is the hardware boundary that the rest of the RTC driver can safely build on.
**Acceptance:**
- [x] Reads CMOS registers without panicking
- [x] NMI disable bit set during read (bit 7 of port 0x70)

### B.2 — Implement atomic RTC read with BCD handling
**File:** `kernel/src/rtc.rs`
**Symbol:** `read_rtc`
**Why it matters:** `read_rtc()` turns fragile CMOS register access into a stable snapshot that higher-level clock syscalls can trust.
**Acceptance:**
- [x] Handles BCD mode (convert via `kernel-core` helper)
- [x] Handles 12-hour mode with AM/PM bit (bit 7 of hours register)
- [x] Retry loop detects torn reads (max 5 retries)
- [x] Century register 0x32 used (standard default, works with QEMU)

### B.3 — Wire RTC module into kernel build
**Files:**
- `kernel/src/main.rs`
- `kernel/src/rtc.rs`
**Symbol:** `rtc::init_rtc`
**Why it matters:** Boot-time RTC initialization is what replaces the earlier monotonic-only startup state with a real wall-clock epoch.
**Acceptance:**
- [x] `cargo xtask check` passes with new module
- [x] Boot log shows RTC time and epoch

---

## Track C — Boot Epoch and Syscall Updates

### C.1 — Store boot epoch timestamp
**File:** `kernel/src/rtc.rs`
**Symbol:** `BOOT_EPOCH_SECS`
**Why it matters:** The boot epoch is the bridge between a one-time RTC read and ongoing wall-clock syscalls driven by timer ticks.
**Acceptance:**
- [x] `BOOT_EPOCH_SECS` is non-zero after boot
- [x] Logged epoch value matches expected current time

### C.2 — Update `clock_gettime()` to dispatch on clock ID
**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_clock_gettime`
**Why it matters:** This is the main Phase 34 syscall change: it preserves the earlier monotonic uptime path while replacing `CLOCK_REALTIME` with epoch-based wall-clock time.
**Acceptance:**
- [x] `CLOCK_REALTIME` returns Unix timestamps (seconds since 1970)
- [x] `CLOCK_MONOTONIC` returns time since boot (unchanged behavior from the earlier uptime-only implementation)
- [x] Invalid clock IDs return `-EINVAL`
- [x] Nanosecond field is populated (10ms resolution from 100 Hz timer)

### C.3 — Update `gettimeofday()` to return wall-clock time
**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_gettimeofday`
**Why it matters:** `gettimeofday()` is the compatibility path that Phase 34 upgrades from the earlier boot-relative stub to the same wall-clock source as `CLOCK_REALTIME`.
**Acceptance:**
- [x] `gettimeofday()` returns wall-clock seconds since epoch
- [x] Microsecond field populated
- [x] Existing callers continue to work

---

## Track D — Userspace Utilities

### D.1 — Add time formatting to `syscall-lib`
**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `gmtime`, `format_datetime`, `DateTime`
**Why it matters:** Shared userspace formatting keeps `date` and future tools aligned with the new kernel wall-clock ABI.
**Acceptance:**
- [x] `gmtime()` converts epoch to broken-down time
- [x] `format_datetime()` produces human-readable output
- [x] Day-of-week names correct

### D.2 — Implement `date` userspace utility
**File:** `userspace/coreutils-rs/src/date.rs`
**Symbol:** `main`
**Why it matters:** `date` is the learner-visible proof that `CLOCK_REALTIME` now reports actual wall-clock time instead of uptime.
**Acceptance:**
- [x] `date` displays correct current date and time
- [x] Output format: "Wed Apr  1 12:30:00 UTC 2026"
- [x] Binary added to initrd

### D.3 — Implement `uptime` userspace utility
**File:** `userspace/coreutils-rs/src/uptime.rs`
**Symbol:** `main`
**Why it matters:** `uptime` makes the Phase 34 split explicit by continuing to expose the earlier monotonic-since-boot behavior alongside the new real-time clock.
**Acceptance:**
- [x] `uptime` displays time since boot
- [x] Format: "up H:MM:SS" or "up Xd H:MM:SS"
- [x] Binary added to initrd

### D.4 — Add initrd entries for new binaries
**Files:**
- `kernel/src/fs/ramdisk.rs`
- `xtask/src/main.rs`
**Symbol:** `DATE_ELF`, `UPTIME_ELF`, `coreutils_bins`
**Why it matters:** The new time utilities only become usable after both the ramdisk and build pipeline ship them into the boot image.
**Acceptance:**
- [x] `date` and `uptime` available in shell after boot
- [x] Build system compiles and copies both binaries

---

## Track E — Integration Testing and Documentation

### E.1 — Run full test suite
**File:** `kernel-core/src/time.rs`
**Symbol:** `tests`
**Why it matters:** Phase 34 touches host-testable math, boot-time hardware init, and userspace ABI, so regression coverage has to span all three layers.
**Acceptance:**
- [x] All existing QEMU tests pass
- [x] All kernel-core host tests pass (177 tests including 15 new time tests)
- [x] `cargo xtask check` clean (no warnings)

### E.2 — QEMU RTC verification test
**File:** `kernel/src/rtc.rs`
**Symbol:** `init_rtc`
**Why it matters:** A dedicated QEMU RTC test would turn the boot-log verification into an automated regression check.
**Acceptance:**
- [ ] Deferred — the RTC is currently verified at boot via the log message rather than a dedicated QEMU test
- [ ] Deferred — a standalone automated RTC regression can be added later if needed

### E.3 — Update documentation
**Files:**
- `docs/34-timekeeping.md`
- `docs/roadmap/tasks/34-real-time-clock-tasks.md`
**Symbol:** `Phase 34 — Real-Time Clock and Timekeeping`, `Phase 34 — Real-Time Clock and Timekeeping: Task List`
**Why it matters:** The written docs need to explain that Phase 34 replaced the earlier realtime stub while preserving the monotonic clock path.
**Acceptance:**
- [x] Phase 34 design doc created (`docs/34-timekeeping.md`)
- [x] Task list updated with completion status
