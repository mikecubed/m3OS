#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/vfs.h>

static int human_readable = 0;

static void usage(void) {
    fputs("usage: df [-h]\n", stderr);
}

static void format_size(unsigned long long size, char *buf, size_t len) {
    static const char suffixes[] = {'B', 'K', 'M', 'G', 'T'};
    size_t suffix = 0;
    unsigned long long whole = size;
    unsigned long long rem = 0;

    while (whole >= 1024 && suffix + 1 < sizeof(suffixes)) {
        rem = whole % 1024;
        whole /= 1024;
        suffix++;
    }

    if (suffix == 0) {
        snprintf(buf, len, "%llu%c", whole, suffixes[suffix]);
        return;
    }
    snprintf(buf, len, "%llu.%01llu%c", whole, (rem * 10) / 1024, suffixes[suffix]);
}

static void print_row(const char *source, const char *mountpoint, const struct statfs *st) {
    unsigned long long block_size = st->f_bsize ? (unsigned long long)st->f_bsize : 1024ULL;
    unsigned long long total = (unsigned long long)st->f_blocks * block_size;
    unsigned long long avail = (unsigned long long)st->f_bavail * block_size;
    unsigned long long free_blocks = (unsigned long long)st->f_bfree * block_size;
    unsigned long long used = total >= free_blocks ? total - free_blocks : 0;

    if (human_readable) {
        char total_buf[32];
        char used_buf[32];
        char avail_buf[32];
        format_size(total, total_buf, sizeof(total_buf));
        format_size(used, used_buf, sizeof(used_buf));
        format_size(avail, avail_buf, sizeof(avail_buf));
        printf("%-12s %8s %8s %8s %s\n", source, total_buf, used_buf, avail_buf, mountpoint);
    } else {
        printf("%-12s %10llu %10llu %10llu %s\n",
               source,
               total / 1024ULL,
               used / 1024ULL,
               avail / 1024ULL,
               mountpoint);
    }
}

int main(int argc, char **argv) {
    FILE *mounts;
    char line[512];

    if (argc > 2 || (argc == 2 && strcmp(argv[1], "-h") != 0)) {
        usage();
        return 1;
    }
    if (argc == 2) {
        human_readable = 1;
    }

    mounts = fopen("/proc/mounts", "r");
    if (!mounts) {
        fprintf(stderr, "df: cannot open /proc/mounts: %s\n", strerror(errno));
        return 1;
    }

    if (human_readable) {
        puts("Filesystem       Size     Used    Avail Mounted on");
    } else {
        puts("Filesystem    1K-blocks       Used  Available Mounted on");
    }

    while (fgets(line, sizeof(line), mounts) != NULL) {
        char source[128];
        char mountpoint[128];
        struct statfs st;

        if (sscanf(line, "%127s %127s", source, mountpoint) != 2) {
            continue;
        }
        if (statfs(mountpoint, &st) != 0) {
            fprintf(stderr, "df: statfs failed for '%s': %s\n", mountpoint, strerror(errno));
            fclose(mounts);
            return 1;
        }
        print_row(source, mountpoint, &st);
    }

    fclose(mounts);
    return 0;
}
