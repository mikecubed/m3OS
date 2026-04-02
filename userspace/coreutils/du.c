#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

static int summarize_only = 0;
static int human_readable = 0;
static int status = 0;

static void usage(void) {
    fputs("usage: du [-s] [-h] [path...]\n", stderr);
}

static unsigned long long disk_usage_bytes(const struct stat *st) {
    if (st->st_blocks > 0) {
        return (unsigned long long)st->st_blocks * 512ULL;
    }
    return (unsigned long long)st->st_size;
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

static void print_total(unsigned long long total, const char *path) {
    char human[32];

    if (human_readable) {
        format_size(total, human, sizeof(human));
        printf("%s\t%s\n", human, path);
    } else {
        printf("%llu\t%s\n", total / 1024ULL, path);
    }
}

static unsigned long long du_path(const char *path, int is_top) {
    struct stat st;
    unsigned long long total;

    if (lstat(path, &st) != 0) {
        fprintf(stderr, "du: cannot stat '%s': %s\n", path, strerror(errno));
        status = 1;
        return 0;
    }

    total = disk_usage_bytes(&st);
    if (!S_ISDIR(st.st_mode)) {
        print_total(total, path);
        return total;
    }

    DIR *dir = opendir(path);
    if (!dir) {
        fprintf(stderr, "du: cannot open '%s': %s\n", path, strerror(errno));
        status = 1;
        return total;
    }

    for (struct dirent *ent = readdir(dir); ent != NULL; ent = readdir(dir)) {
        size_t base_len;
        size_t name_len;
        char *child;

        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) {
            continue;
        }

        base_len = strlen(path);
        name_len = strlen(ent->d_name);
        child = malloc(base_len + 1 + name_len + 1);
        if (!child) {
            fprintf(stderr, "du: out of memory\n");
            closedir(dir);
            status = 1;
            return total;
        }
        memcpy(child, path, base_len);
        if (base_len == 0 || path[base_len - 1] != '/') {
            child[base_len++] = '/';
        }
        memcpy(child + base_len, ent->d_name, name_len + 1);
        total += du_path(child, 0);
        free(child);
    }

    closedir(dir);
    if (!summarize_only || is_top) {
        print_total(total, path);
    }
    return total;
}

int main(int argc, char **argv) {
    int argi = 1;

    while (argi < argc && argv[argi][0] == '-' && argv[argi][1]) {
        if (strcmp(argv[argi], "-s") == 0) {
            summarize_only = 1;
        } else if (strcmp(argv[argi], "-h") == 0) {
            human_readable = 1;
        } else {
            usage();
            return 1;
        }
        argi++;
    }

    if (argi == argc) {
        du_path(".", 1);
        return status;
    }

    for (; argi < argc; argi++) {
        du_path(argv[argi], 1);
    }
    return status;
}
