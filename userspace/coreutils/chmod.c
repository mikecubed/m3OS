#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

static void usage(void) {
    fputs("usage: chmod MODE FILE...\n", stderr);
}

int main(int argc, char **argv) {
    if (argc < 3) {
        usage();
        return 1;
    }

    char *end = NULL;
    long mode = strtol(argv[1], &end, 8);
    if (!argv[1][0] || (end && *end) || mode < 0 || mode > 07777) {
        fputs("chmod: invalid mode\n", stderr);
        return 1;
    }

    int status = 0;
    for (int i = 2; i < argc; i++) {
        if (chmod(argv[i], (mode_t)mode) != 0) {
            fprintf(stderr, "chmod: cannot change '%s': %s\n", argv[i], strerror(errno));
            status = 1;
        }
    }
    return status;
}
