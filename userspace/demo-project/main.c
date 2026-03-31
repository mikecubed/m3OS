#include "util.h"
#include <unistd.h>

static void write_str(const char *s) {
    const char *p = s;
    while (*p) p++;
    write(1, s, p - s);
}

int main(void) {
    write_str("Demo project running!\n");
    write_str("add(3, 4) = ");
    int result = add(3, 4);
    char buf[2];
    buf[0] = '0' + result;
    buf[1] = '\n';
    write(1, buf, 2);
    write_str("factorial(5) = ");
    int f = factorial(5);
    /* Print 120 */
    char fbuf[4];
    fbuf[0] = '0' + (f / 100);
    fbuf[1] = '0' + ((f / 10) % 10);
    fbuf[2] = '0' + (f % 10);
    fbuf[3] = '\n';
    write(1, fbuf, 4);
    return 0;
}
