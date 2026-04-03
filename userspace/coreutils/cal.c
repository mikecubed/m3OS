#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static const char *month_names[] = {
    "January", "February", "March",     "April",   "May",      "June",
    "July",    "August",   "September", "October", "November", "December",
};

static int is_leap_year(int year) {
    return (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
}

static int days_in_month(int year, int month) {
    static const int month_days[] = {31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31};
    if (month == 2 && is_leap_year(year)) {
        return 29;
    }
    return month_days[month - 1];
}

static int weekday(int year, int month, int day) {
    static const int offsets[] = {0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4};
    if (month < 3) {
        year--;
    }
    return (year + year / 4 - year / 100 + year / 400 + offsets[month - 1] + day) % 7;
}

static void print_day(int day, int highlight) {
    if (highlight) {
        printf("\x1b[7m%2d\x1b[0m", day);
    } else {
        printf("%2d", day);
    }
}

static void print_month(int month, int year, int highlight_day) {
    int first = weekday(year, month, 1);
    int days = days_in_month(year, month);
    int col;

    printf("     %s %d\n", month_names[month - 1], year);
    puts("Su Mo Tu We Th Fr Sa");

    for (col = 0; col < first; col++) {
        fputs("   ", stdout);
    }

    for (int day = 1; day <= days; day++) {
        print_day(day, day == highlight_day);
        if ((first + day) % 7 == 0 || day == days) {
            putchar('\n');
        } else {
            putchar(' ');
        }
    }
}

static void usage(void) {
    fputs("usage: cal [MONTH] YEAR\n       cal [YEAR]\n", stderr);
}

int main(int argc, char **argv) {
    time_t now;
    struct tm *tm_now;
    int month;
    int year;
    int today = 0;

    now = time(NULL);
    tm_now = localtime(&now);
    if (!tm_now) {
        fputs("cal: could not read current time\n", stderr);
        return 1;
    }

    if (argc == 1) {
        month = tm_now->tm_mon + 1;
        year = tm_now->tm_year + 1900;
        today = tm_now->tm_mday;
        print_month(month, year, today);
        return 0;
    }

    if (argc == 2) {
        year = atoi(argv[1]);
        if (year <= 0) {
            usage();
            return 1;
        }
        for (month = 1; month <= 12; month++) {
            print_month(month, year, 0);
            if (month != 12) {
                putchar('\n');
            }
        }
        return 0;
    }

    if (argc == 3) {
        month = atoi(argv[1]);
        year = atoi(argv[2]);
        if (month < 1 || month > 12 || year <= 0) {
            usage();
            return 1;
        }
        if (month == tm_now->tm_mon + 1 && year == tm_now->tm_year + 1900) {
            today = tm_now->tm_mday;
        }
        print_month(month, year, today);
        return 0;
    }

    usage();
    return 1;
}
