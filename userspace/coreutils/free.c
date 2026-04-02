#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int unit_mode = 0; /* 0=KB, 1=MB, 2=human */

static void usage(void) {
    fputs("usage: free [-m] [-h]\n", stderr);
}

static unsigned long long read_meminfo_value(const char *label, const char *text) {
    const char *line = strstr(text, label);
    unsigned long long value = 0;
    if (!line) {
        return 0;
    }
    sscanf(line + strlen(label), "%llu", &value);
    return value;
}

static void format_value(unsigned long long kb, char *buf, size_t len) {
    static const char suffixes[] = {'K', 'M', 'G', 'T'};
    size_t suffix = 0;
    unsigned long long whole = kb;
    unsigned long long rem = 0;

    if (unit_mode == 1) {
        snprintf(buf, len, "%llu", kb / 1024ULL);
        return;
    }
    if (unit_mode == 0) {
        snprintf(buf, len, "%llu", kb);
        return;
    }

    while (whole >= 1024 && suffix + 1 < sizeof(suffixes)) {
        rem = whole % 1024;
        whole /= 1024;
        suffix++;
    }
    snprintf(buf, len, "%llu.%01llu%c", whole, (rem * 10) / 1024, suffixes[suffix]);
}

int main(int argc, char **argv) {
    FILE *fp;
    char buf[2048];
    size_t nread;
    unsigned long long total;
    unsigned long long available;
    unsigned long long used;
    char total_buf[32];
    char used_buf[32];
    char avail_buf[32];

    if (argc > 2) {
        usage();
        return 1;
    }
    if (argc == 2) {
        if (strcmp(argv[1], "-m") == 0) {
            unit_mode = 1;
        } else if (strcmp(argv[1], "-h") == 0) {
            unit_mode = 2;
        } else {
            usage();
            return 1;
        }
    }

    fp = fopen("/proc/meminfo", "r");
    if (!fp) {
        fprintf(stderr, "free: cannot open /proc/meminfo: %s\n", strerror(errno));
        return 1;
    }
    nread = fread(buf, 1, sizeof(buf) - 1, fp);
    if (ferror(fp)) {
        fprintf(stderr, "free: cannot read /proc/meminfo\n");
        fclose(fp);
        return 1;
    }
    fclose(fp);
    buf[nread] = '\0';

    total = read_meminfo_value("MemTotal:", buf);
    available = read_meminfo_value("MemAvailable:", buf);
    used = total >= available ? total - available : 0;

    format_value(total, total_buf, sizeof(total_buf));
    format_value(used, used_buf, sizeof(used_buf));
    format_value(available, avail_buf, sizeof(avail_buf));

    puts("              total        used   available");
    printf("Mem: %12s %11s %11s\n", total_buf, used_buf, avail_buf);
    return 0;
}
