#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void usage(void) {
    fputs("usage: head [-n lines] [file...]\n", stderr);
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

static int head_stream(FILE *fp, long count) {
    char *line = NULL;
    size_t cap = 0;
    long printed = 0;
    ssize_t len;
    int status = 0;

    while (printed < count && (len = getline(&line, &cap, fp)) >= 0) {
        if (fwrite(line, 1, (size_t)len, stdout) != (size_t)len) {
            status = 1;
            break;
        }
        printed++;
    }
    if (ferror(fp)) {
        status = 1;
    }
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
        return head_stream(stdin, count);
    }

    for (; argi < argc; argi++) {
        FILE *fp = fopen(argv[argi], "r");
        if (!fp) {
            fprintf(stderr, "head: cannot open '%s': %s\n", argv[argi], strerror(errno));
            status = 1;
            continue;
        }
        if (head_stream(fp, count) != 0) {
            fprintf(stderr, "head: read error on '%s'\n", argv[argi]);
            status = 1;
        }
        fclose(fp);
    }

    return status;
}
