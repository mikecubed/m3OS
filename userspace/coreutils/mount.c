#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static void usage(void) {
    fputs("usage: mount -t TYPE SOURCE TARGET\n", stderr);
}

int main(int argc, char **argv) {
    const char *fstype = NULL;
    const char *source = NULL;
    const char *target = NULL;
    int argi = 1;

    if (argc == 1) {
        FILE *fp = fopen("/proc/mounts", "r");
        char buf[512];
        if (!fp) {
            fprintf(stderr, "mount: cannot open /proc/mounts: %s\n", strerror(errno));
            return 1;
        }
        while (fgets(buf, sizeof(buf), fp) != NULL) {
            fputs(buf, stdout);
        }
        fclose(fp);
        return 0;
    }

    if (argi < argc && strcmp(argv[argi], "-t") == 0) {
        if (argi + 1 >= argc) {
            usage();
            return 1;
        }
        fstype = argv[argi + 1];
        argi += 2;
    }
    if (argc - argi != 2 || !fstype) {
        usage();
        return 1;
    }
    source = argv[argi];
    target = argv[argi + 1];

    if (syscall(SYS_mount, source, target, fstype) != 0) {
        fprintf(stderr, "mount: %s\n", strerror(errno));
        return 1;
    }
    return 0;
}
