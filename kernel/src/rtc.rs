//! CMOS Real-Time Clock (RTC) driver.
//!
//! Reads the hardware RTC via CMOS I/O ports 0x70/0x71 and converts the
//! date/time to a Unix epoch timestamp stored in [`BOOT_EPOCH_SECS`].

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

/// Boot wall-clock time as Unix epoch seconds, set once during init.
pub static BOOT_EPOCH_SECS: AtomicU64 = AtomicU64::new(0);

/// Standard CMOS century register address (used by QEMU and most hardware).
const CENTURY_REGISTER: u8 = 0x32;

/// Maximum number of consistent-read retries before giving up.
const MAX_RETRIES: usize = 5;

/// Read a single CMOS register.
///
/// Port 0x70 is the address/NMI-disable port (bit 7 disables NMI).
/// Port 0x71 is the data port.
///
/// # Safety
///
/// Performs raw x86 port I/O. Must only be called in ring 0.
unsafe fn cmos_read(register: u8) -> u8 {
    let mut addr_port = Port::<u8>::new(0x70);
    let mut data_port = Port::<u8>::new(0x71);
    unsafe {
        addr_port.write(register | 0x80); // bit 7 = disable NMI during read
        data_port.read()
    }
}

/// Returns `true` if the RTC update-in-progress flag (Status Register A, bit 7)
/// is set, meaning the RTC is currently latching new values.
fn update_in_progress() -> bool {
    // SAFETY: reading CMOS register 0x0A (Status Register A) via port I/O.
    (unsafe { cmos_read(0x0A) } & 0x80) != 0
}

/// Raw RTC register snapshot (before BCD/12h conversion).
#[derive(Clone, Copy, PartialEq, Eq)]
struct RtcSnapshot {
    second: u8,
    minute: u8,
    hour: u8,
    day: u8,
    month: u8,
    year: u8,
    century: u8,
}

impl RtcSnapshot {
    /// Read all time/date registers from the CMOS.
    fn read() -> Self {
        // SAFETY: reading well-known CMOS time registers via port I/O.
        unsafe {
            Self {
                second: cmos_read(0x00),
                minute: cmos_read(0x02),
                hour: cmos_read(0x04),
                day: cmos_read(0x07),
                month: cmos_read(0x08),
                year: cmos_read(0x09),
                century: cmos_read(CENTURY_REGISTER),
            }
        }
    }
}

/// Read the RTC with an atomic-read protocol and BCD/12h conversion.
///
/// Returns `(year, month, day, hour, minute, second)` in UTC.
pub fn read_rtc() -> (u32, u32, u32, u32, u32, u32) {
    // Step 1-4: Read registers twice and compare; retry if they differ
    // (ensures we did not read mid-update).
    let snap = {
        let mut result = None;
        for _ in 0..MAX_RETRIES {
            // Wait for any in-progress update to finish.
            while update_in_progress() {
                core::hint::spin_loop();
            }

            let first = RtcSnapshot::read();
            let second = RtcSnapshot::read();

            if first == second {
                result = Some(first);
                break;
            }
        }
        // If all retries failed, use the last read as best effort.
        result.unwrap_or_else(|| {
            log::warn!(
                "RTC: consistent read failed after {} retries, using last snapshot",
                MAX_RETRIES
            );
            RtcSnapshot::read()
        })
    };

    let mut second = snap.second as u32;
    let mut minute = snap.minute as u32;
    let mut hour = snap.hour as u32;
    let mut day = snap.day as u32;
    let mut month = snap.month as u32;
    let mut year = snap.year as u32;
    let mut century = snap.century as u32;

    // Step 5: Check Status Register B for data format.
    // SAFETY: reading CMOS register 0x0B (Status Register B) via port I/O.
    let status_b = unsafe { cmos_read(0x0B) };
    let is_binary = (status_b & 0x04) != 0;
    let is_24h = (status_b & 0x02) != 0;

    // Convert BCD to binary if needed (bit 2 clear = BCD mode).
    let bcd = |v: u32| kernel_core::time::bcd_to_binary(v as u8) as u32;
    if !is_binary {
        second = bcd(second);
        minute = bcd(minute);
        // For hours in 12h BCD mode, mask off the PM bit before BCD conversion.
        hour = if !is_24h && (hour & 0x80) != 0 {
            // PM flag set — convert lower 7 bits from BCD, then add 12.
            bcd(hour & 0x7F) + 12
        } else {
            bcd(hour)
        };
        day = bcd(day);
        month = bcd(month);
        year = bcd(year);
        century = bcd(century);
    } else if !is_24h && (hour & 0x80) != 0 {
        // Binary mode but 12h format with PM bit set.
        hour = (hour & 0x7F) + 12;
    }

    // Handle 12h→24h: 12 AM (midnight) should be 0, 12 PM (noon) stays 12.
    if !is_24h {
        if hour == 24 {
            hour = 12; // 12 PM after +12 correction
        } else if hour == 12 {
            hour = 0; // 12 AM → midnight
        }
    }

    // Step 6: Combine century and year.
    if century == 0 {
        // Century register not available or returned 0; assume 2000s.
        century = 20;
    }
    let full_year = century * 100 + year;

    (full_year, month, day, hour, minute, second)
}

/// Initialise the RTC: read the hardware clock, compute the boot epoch, and
/// store it in [`BOOT_EPOCH_SECS`].
pub fn init_rtc() {
    let (year, month, day, hour, minute, second) = read_rtc();
    let epoch = kernel_core::time::date_to_unix_timestamp(year, month, day, hour, minute, second);
    BOOT_EPOCH_SECS.store(epoch, Ordering::Relaxed);
    log::info!(
        "RTC: {}-{:02}-{:02} {:02}:{:02}:{:02} UTC (epoch={})",
        year,
        month,
        day,
        hour,
        minute,
        second,
        epoch
    );
}
