//! cal — display a calendar.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, gettimeofday, gmtime, write_str, write_u64};

syscall_lib::entry_point!(main);

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: i32) -> i32 {
    const DAYS: [i32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if month == 2 && is_leap_year(year) {
        29
    } else {
        DAYS[(month - 1) as usize]
    }
}

fn weekday(mut year: i32, month: i32, day: i32) -> i32 {
    const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    if month < 3 {
        year -= 1;
    }
    (year + year / 4 - year / 100 + year / 400 + OFFSETS[(month - 1) as usize] + day).rem_euclid(7)
}

fn print_day(day: i32, highlight: bool) {
    if highlight {
        write_str(STDOUT_FILENO, "\x1b[7m");
    }
    if day < 10 {
        write_str(STDOUT_FILENO, " ");
    }
    write_u64(STDOUT_FILENO, day as u64);
    if highlight {
        write_str(STDOUT_FILENO, "\x1b[0m");
    }
}

fn print_month(month: i32, year: i32, highlight_day: i32) {
    write_str(STDOUT_FILENO, "     ");
    write_str(STDOUT_FILENO, MONTH_NAMES[(month - 1) as usize]);
    write_str(STDOUT_FILENO, " ");
    write_u64(STDOUT_FILENO, year as u64);
    write_str(STDOUT_FILENO, "\n");
    write_str(STDOUT_FILENO, "Su Mo Tu We Th Fr Sa\n");

    let first = weekday(year, month, 1);
    let days = days_in_month(year, month);

    for _ in 0..first {
        write_str(STDOUT_FILENO, "   ");
    }

    for day in 1..=days {
        print_day(day, day == highlight_day);
        if (first + day) % 7 == 0 || day == days {
            write_str(STDOUT_FILENO, "\n");
        } else {
            write_str(STDOUT_FILENO, " ");
        }
    }
}

fn parse_i32(s: &str) -> Option<i32> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut v: i32 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.wrapping_mul(10).wrapping_add((b - b'0') as i32);
    }
    Some(v)
}

fn main(args: &[&str]) -> i32 {
    let (tv_sec, _) = gettimeofday();
    let now = gmtime(tv_sec.max(0) as u64);
    let cur_month = now.month as i32;
    let cur_year = now.year as i32;
    let cur_day = now.day as i32;

    match args.len() - 1 {
        0 => {
            print_month(cur_month, cur_year, cur_day);
        }
        1 => {
            let year = match parse_i32(args[1]) {
                Some(y) if y > 0 => y,
                _ => {
                    write_str(
                        STDERR_FILENO,
                        "usage: cal [MONTH] YEAR\n       cal [YEAR]\n",
                    );
                    return 1;
                }
            };
            for month in 1..=12i32 {
                print_month(month, year, 0);
                if month != 12 {
                    write_str(STDOUT_FILENO, "\n");
                }
            }
        }
        2 => {
            let month = match parse_i32(args[1]) {
                Some(m) if (1..=12).contains(&m) => m,
                _ => {
                    write_str(
                        STDERR_FILENO,
                        "usage: cal [MONTH] YEAR\n       cal [YEAR]\n",
                    );
                    return 1;
                }
            };
            let year = match parse_i32(args[2]) {
                Some(y) if y > 0 => y,
                _ => {
                    write_str(
                        STDERR_FILENO,
                        "usage: cal [MONTH] YEAR\n       cal [YEAR]\n",
                    );
                    return 1;
                }
            };
            let highlight = if month == cur_month && year == cur_year {
                cur_day
            } else {
                0
            };
            print_month(month, year, highlight);
        }
        _ => {
            write_str(
                STDERR_FILENO,
                "usage: cal [MONTH] YEAR\n       cal [YEAR]\n",
            );
            return 1;
        }
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
