#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void usage(void) {
    fputs("usage: uniq [-c] [file]\n", stderr);
}

static void emit_line(const char *line, unsigned long count, int show_count) {
    if (show_count) {
        printf("%lu %s", count, line);
    } else {
        fputs(line, stdout);
    }
}

static int uniq_stream(FILE *fp, int show_count) {
    char *line = NULL;
    char *prev = NULL;
    size_t cap = 0;
    unsigned long count = 0;
    ssize_t len;
    int status = 0;

    while ((len = getline(&line, &cap, fp)) >= 0) {
        if (!prev) {
            prev = malloc((size_t)len + 1);
            if (!prev) {
                status = 1;
                break;
            }
            memcpy(prev, line, (size_t)len + 1);
            count = 1;
            continue;
        }
        if (strcmp(prev, line) == 0) {
            count++;
            continue;
        }
        emit_line(prev, count, show_count);
        free(prev);
        prev = malloc((size_t)len + 1);
        if (!prev) {
            status = 1;
            break;
        }
        memcpy(prev, line, (size_t)len + 1);
        count = 1;
    }

    if (prev) {
        emit_line(prev, count, show_count);
    }
    free(prev);
    free(line);
    return status || ferror(fp);
}

int main(int argc, char **argv) {
    int show_count = 0;
    int argi = 1;

    while (argi < argc && argv[argi][0] == '-' && argv[argi][1]) {
        if (strcmp(argv[argi], "--") == 0) {
            argi++;
            break;
        }
        if (strcmp(argv[argi], "-c") == 0) {
            show_count = 1;
            argi++;
            continue;
        }
        usage();
        return 1;
    }

    if (argi == argc) {
        return uniq_stream(stdin, show_count);
    }
    if (argi + 1 != argc) {
        usage();
        return 1;
    }

    FILE *fp = fopen(argv[argi], "r");
    if (!fp) {
        fprintf(stderr, "uniq: cannot open '%s': %s\n", argv[argi], strerror(errno));
        return 1;
    }
    int status = uniq_stream(fp, show_count);
    fclose(fp);
    return status ? 1 : 0;
}
