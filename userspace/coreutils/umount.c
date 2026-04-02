#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static void usage(void) {
    fputs("usage: umount TARGET\n", stderr);
}

int main(int argc, char **argv) {
    if (argc != 2) {
        usage();
        return 1;
    }

    if (syscall(166, argv[1], 0) != 0) {
        fprintf(stderr, "umount: %s\n", strerror(errno));
        return 1;
    }
    return 0;
}
