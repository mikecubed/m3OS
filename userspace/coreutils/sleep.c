/* sleep — sleep for N seconds (uses nanosleep syscall) */
#include <unistd.h>
#include <time.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        const char *msg = "usage: sleep <seconds>\n";
        write(2, msg, strlen(msg));
        return 1;
    }
    /* Simple atoi. */
    unsigned int secs = 0;
    for (const char *p = argv[1]; *p >= '0' && *p <= '9'; p++) {
        secs = secs * 10 + (*p - '0');
    }
    struct timespec ts = { .tv_sec = secs, .tv_nsec = 0 };
    nanosleep(&ts, NULL);
    return 0;
}
