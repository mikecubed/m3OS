/// Determine the day of week for a given date using Tomohiko Sakamoto's algorithm.
/// Returns 0=Sunday, 1=Monday, ..., 6=Saturday.
pub fn day_of_week(year: i32, month: u32, day: u32) -> u32 {
    const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let mut y = year;
    if month < 3 {
        y -= 1;
    }
    ((y + y / 4 - y / 100 + y / 400 + OFFSETS[(month - 1) as usize] + day as i32).rem_euclid(7))
        as u32
}

/// Return true if the given year is a leap year.
pub fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Return the number of days in the given month of the given year.
pub fn days_in_month(year: i32, month: u32) -> u32 {
    const DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if month == 2 && is_leap_year(year) {
        29
    } else {
        DAYS[(month - 1) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- day_of_week ---

    #[test]
    fn day_of_week_2024_01_01_is_monday() {
        assert_eq!(day_of_week(2024, 1, 1), 1, "2024-01-01 should be Monday");
    }

    #[test]
    fn day_of_week_2024_02_29_is_thursday() {
        assert_eq!(day_of_week(2024, 2, 29), 4, "2024-02-29 should be Thursday");
    }

    #[test]
    fn day_of_week_2000_01_01_is_saturday() {
        assert_eq!(day_of_week(2000, 1, 1), 6, "2000-01-01 should be Saturday");
    }

    #[test]
    fn day_of_week_1970_01_01_is_thursday() {
        assert_eq!(day_of_week(1970, 1, 1), 4, "1970-01-01 should be Thursday");
    }

    #[test]
    fn day_of_week_2023_12_31_is_sunday() {
        assert_eq!(day_of_week(2023, 12, 31), 0, "2023-12-31 should be Sunday");
    }

    // --- is_leap_year ---

    #[test]
    fn is_leap_year_divisible_by_400() {
        assert!(is_leap_year(2000));
    }

    #[test]
    fn is_leap_year_divisible_by_100_but_not_400_is_not_leap() {
        assert!(!is_leap_year(1900));
    }

    #[test]
    fn is_leap_year_2024_is_leap() {
        assert!(is_leap_year(2024));
    }

    #[test]
    fn is_leap_year_2023_is_not_leap() {
        assert!(!is_leap_year(2023));
    }

    #[test]
    fn is_leap_year_2100_is_not_leap() {
        assert!(!is_leap_year(2100));
    }

    // --- days_in_month ---

    #[test]
    fn days_in_month_january_has_31_days() {
        assert_eq!(days_in_month(2024, 1), 31);
    }

    #[test]
    fn days_in_month_february_leap_year_has_29_days() {
        assert_eq!(days_in_month(2024, 2), 29);
    }

    #[test]
    fn days_in_month_february_non_leap_year_has_28_days() {
        assert_eq!(days_in_month(2023, 2), 28);
    }

    #[test]
    fn days_in_month_april_has_30_days() {
        assert_eq!(days_in_month(2024, 4), 30);
    }

    #[test]
    fn days_in_month_december_has_31_days() {
        assert_eq!(days_in_month(2024, 12), 31);
    }

    #[test]
    fn days_in_month_all_months_non_leap_year() {
        let expected = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for (m, &exp) in expected.iter().enumerate() {
            assert_eq!(
                days_in_month(2023, m as u32 + 1),
                exp,
                "month {} should have {} days",
                m + 1,
                exp
            );
        }
    }
}
