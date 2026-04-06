/* cal - display a calendar (simple version) */
#include <stdio.h>
#include <stdlib.h>

static int days_in_month[] = {31,28,31,30,31,30,31,31,30,31,30,31};
static const char *month_names[] = {
    "January","February","March","April","May","June",
    "July","August","September","October","November","December"
};

static int is_leap(int y) {
    return (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
}

/* Zeller's formula for day of week (0=Sun) */
static int dow(int y, int m, int d) {
    if (m < 3) { m += 12; y--; }
    return (d + 13*(m+1)/5 + y + y/4 - y/100 + y/400) % 7;
}

int main(int argc, char *argv[]) {
    int month = 1, year = 2026;
    if (argc >= 3) {
        month = atoi(argv[1]);
        year = atoi(argv[2]);
    } else if (argc == 2) {
        year = atoi(argv[1]);
    }
    if (month < 1 || month > 12) month = 1;

    int dim = days_in_month[month - 1];
    if (month == 2 && is_leap(year)) dim = 29;

    printf("   %s %d\n", month_names[month - 1], year);
    printf("Su Mo Tu We Th Fr Sa\n");

    int start = dow(year, month, 1);
    for (int i = 0; i < start; i++) printf("   ");
    for (int d = 1; d <= dim; d++) {
        printf("%2d ", d);
        if ((start + d) % 7 == 0) printf("\n");
    }
    if ((start + dim) % 7 != 0) printf("\n");
    return 0;
}
