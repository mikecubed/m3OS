#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void usage(void) {
    fputs("usage: tail [-n lines] [file...]\n", stderr);
}

static int parse_count(const char *arg, long *count) {
    char *end = NULL;
    long value = strtol(arg, &end, 10);
    if (!arg[0] || (end && *end) || value < 0) {
        return -1;
    }
    *count = value;
    return 0;
}

static int tail_stream(FILE *fp, long count) {
    char *line = NULL;
    size_t cap = 0;
    ssize_t len;
    int status = 0;

    if (count == 0) {
        return 0;
    }

    char **lines = calloc((size_t)count, sizeof(char *));
    if (!lines) {
        return 1;
    }

    long total = 0;
    long next = 0;
    while ((len = getline(&line, &cap, fp)) >= 0) {
        char *copy = malloc((size_t)len + 1);
        if (!copy) {
            status = 1;
            break;
        }
        memcpy(copy, line, (size_t)len + 1);
        if (total >= count) {
            free(lines[next]);
        }
        lines[next] = copy;
        next = (next + 1) % count;
        if (total < count) {
            total++;
        }
    }
    if (ferror(fp)) {
        status = 1;
    }

    if (status == 0) {
        long start = total == count ? next : 0;
        for (long i = 0; i < total; i++) {
            long idx = (start + i) % count;
            fputs(lines[idx], stdout);
        }
    }

    for (long i = 0; i < count; i++) {
        free(lines[i]);
    }
    free(lines);
    free(line);
    return status;
}

int main(int argc, char **argv) {
    long count = 10;
    int argi = 1;
    int status = 0;

    while (argi < argc && argv[argi][0] == '-' && argv[argi][1]) {
        if (strcmp(argv[argi], "--") == 0) {
            argi++;
            break;
        }
        if (strcmp(argv[argi], "-n") == 0) {
            if (argi + 1 >= argc || parse_count(argv[argi + 1], &count) != 0) {
                usage();
                return 1;
            }
            argi += 2;
            continue;
        }
        if (strncmp(argv[argi], "-n", 2) == 0) {
            if (parse_count(argv[argi] + 2, &count) != 0) {
                usage();
                return 1;
            }
            argi++;
            continue;
        }
        usage();
        return 1;
    }

    if (argi == argc) {
        return tail_stream(stdin, count);
    }

    for (; argi < argc; argi++) {
        FILE *fp = fopen(argv[argi], "r");
        if (!fp) {
            fprintf(stderr, "tail: cannot open '%s': %s\n", argv[argi], strerror(errno));
            status = 1;
            continue;
        }
        if (tail_stream(fp, count) != 0) {
            fprintf(stderr, "tail: read error on '%s'\n", argv[argi]);
            status = 1;
        }
        fclose(fp);
    }

    return status;
}
