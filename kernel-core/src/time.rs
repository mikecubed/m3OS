//! Pure-logic time conversion library for kernel timekeeping.
//! No hardware access — suitable for host testing.

/// A date-time representation with weekday.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday, 1 = Monday, ..., 6 = Saturday
    pub weekday: u32,
}

/// Returns true if `year` is a leap year under the Gregorian calendar.
pub fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Returns the number of days in the given month (1-indexed) for the given year.
///
/// Panics if `month` is 0 or greater than 12.
pub fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 => 31,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        3 => 31,
        4 => 30,
        5 => 31,
        6 => 30,
        7 => 31,
        8 => 31,
        9 => 30,
        10 => 31,
        11 => 30,
        12 => 31,
        _ => panic!("invalid month"),
    }
}

/// Converts a UTC date/time to seconds since the Unix epoch (1970-01-01 00:00:00 UTC).
pub fn date_to_unix_timestamp(
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> u64 {
    // Count days from 1970-01-01 to the start of `year`
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }
    // Add days for completed months in the target year
    for m in 1..month {
        days += days_in_month(year, m) as u64;
    }
    // Add remaining days (day is 1-indexed, so subtract 1)
    days += (day - 1) as u64;

    days * 86400 + hour as u64 * 3600 + minute as u64 * 60 + second as u64
}

/// Converts seconds since the Unix epoch back to a `DateTime`, including weekday.
pub fn unix_timestamp_to_date(epoch_secs: u64) -> DateTime {
    let total_days = epoch_secs / 86400;
    let remaining_secs = epoch_secs % 86400;

    // 1970-01-01 was Thursday (4)
    let weekday = ((total_days + 4) % 7) as u32;

    let hour = (remaining_secs / 3600) as u32;
    let minute = ((remaining_secs % 3600) / 60) as u32;
    let second = (remaining_secs % 60) as u32;

    // Walk years from 1970
    let mut year = 1970u32;
    let mut days_left = total_days;
    loop {
        let days_in_year: u64 = if is_leap_year(year) { 366 } else { 365 };
        if days_left < days_in_year {
            break;
        }
        days_left -= days_in_year;
        year += 1;
    }

    // Walk months within the year
    let mut month = 1u32;
    loop {
        let dim = days_in_month(year, month) as u64;
        if days_left < dim {
            break;
        }
        days_left -= dim;
        month += 1;
    }

    let day = days_left as u32 + 1;

    DateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
        weekday,
    }
}

/// Converts a BCD-encoded byte to binary. Used for CMOS RTC register values.
pub fn bcd_to_binary(bcd: u8) -> u8 {
    (bcd >> 4) * 10 + (bcd & 0x0F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leap_year() {
        assert!(is_leap_year(2000)); // divisible by 400
        assert!(!is_leap_year(1900)); // divisible by 100 but not 400
        assert!(is_leap_year(2024)); // divisible by 4, not by 100
        assert!(!is_leap_year(2100)); // divisible by 100 but not 400
        assert!(!is_leap_year(2023)); // not divisible by 4
    }

    #[test]
    fn test_days_in_month_all() {
        let expected = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for (i, &exp) in expected.iter().enumerate() {
            assert_eq!(days_in_month(2023, (i + 1) as u32), exp, "month {}", i + 1);
        }
    }

    #[test]
    fn test_days_in_feb_leap() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2000, 2), 29);
    }

    #[test]
    fn test_days_in_feb_non_leap() {
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(1900, 2), 28);
    }

    #[test]
    fn test_epoch_zero() {
        assert_eq!(date_to_unix_timestamp(1970, 1, 1, 0, 0, 0), 0);
    }

    #[test]
    fn test_y2k_timestamp() {
        assert_eq!(date_to_unix_timestamp(2000, 1, 1, 0, 0, 0), 946684800);
    }

    #[test]
    fn test_2026_apr_01_noon() {
        assert_eq!(date_to_unix_timestamp(2026, 4, 1, 12, 0, 0), 1775044800);
    }

    #[test]
    fn test_roundtrip() {
        let timestamps = [0u64, 946684800, 1775044800, 86399, 1_000_000_000];
        for &ts in &timestamps {
            let dt = unix_timestamp_to_date(ts);
            let back =
                date_to_unix_timestamp(dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second);
            assert_eq!(back, ts, "roundtrip failed for timestamp {}", ts);
        }
    }

    #[test]
    fn test_weekday_epoch() {
        // 1970-01-01 was Thursday = 4
        let dt = unix_timestamp_to_date(0);
        assert_eq!(dt.weekday, 4);
    }

    #[test]
    fn test_weekday_2026_apr_01() {
        // 2026-04-01 is Wednesday = 3
        let ts = date_to_unix_timestamp(2026, 4, 1, 12, 0, 0);
        let dt = unix_timestamp_to_date(ts);
        assert_eq!(dt.weekday, 3);
    }

    #[test]
    fn test_bcd_to_binary() {
        assert_eq!(bcd_to_binary(0x59), 59);
        assert_eq!(bcd_to_binary(0x12), 12);
        assert_eq!(bcd_to_binary(0x00), 0);
        assert_eq!(bcd_to_binary(0x99), 99);
        assert_eq!(bcd_to_binary(0x31), 31);
    }

    #[test]
    fn test_end_of_year_rollover() {
        // 2023-12-31 23:59:59 -> next second is 2024-01-01 00:00:00
        let ts = date_to_unix_timestamp(2023, 12, 31, 23, 59, 59);
        let dt_next = unix_timestamp_to_date(ts + 1);
        assert_eq!(dt_next.year, 2024);
        assert_eq!(dt_next.month, 1);
        assert_eq!(dt_next.day, 1);
        assert_eq!(dt_next.hour, 0);
        assert_eq!(dt_next.minute, 0);
        assert_eq!(dt_next.second, 0);
    }

    #[test]
    fn test_feb_28_29_transition_leap() {
        // In a leap year, Feb 28 -> Feb 29
        let ts = date_to_unix_timestamp(2024, 2, 28, 23, 59, 59);
        let dt = unix_timestamp_to_date(ts + 1);
        assert_eq!(dt.year, 2024);
        assert_eq!(dt.month, 2);
        assert_eq!(dt.day, 29);
    }

    #[test]
    fn test_feb_28_mar_1_transition_non_leap() {
        // In a non-leap year, Feb 28 -> Mar 1
        let ts = date_to_unix_timestamp(2023, 2, 28, 23, 59, 59);
        let dt = unix_timestamp_to_date(ts + 1);
        assert_eq!(dt.year, 2023);
        assert_eq!(dt.month, 3);
        assert_eq!(dt.day, 1);
    }

    #[test]
    fn test_feb_29_mar_1_transition_leap() {
        // In a leap year, Feb 29 -> Mar 1
        let ts = date_to_unix_timestamp(2024, 2, 29, 23, 59, 59);
        let dt = unix_timestamp_to_date(ts + 1);
        assert_eq!(dt.year, 2024);
        assert_eq!(dt.month, 3);
        assert_eq!(dt.day, 1);
    }
}
