#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int reverse_sort = 0;
static int numeric_sort = 0;

static void usage(void) {
    fputs("usage: sort [-r] [-n] [file...]\n", stderr);
}

static int cmp_lex(const void *lhs, const void *rhs) {
    const char *const *a = lhs;
    const char *const *b = rhs;
    int cmp = strcmp(*a, *b);
    return reverse_sort ? -cmp : cmp;
}

static int cmp_num(const void *lhs, const void *rhs) {
    const char *const *a = lhs;
    const char *const *b = rhs;
    double da = strtod(*a, NULL);
    double db = strtod(*b, NULL);
    int cmp = (da > db) - (da < db);
    if (cmp == 0) {
        cmp = strcmp(*a, *b);
    }
    return reverse_sort ? -cmp : cmp;
}

static int append_stream(FILE *fp, char ***lines, size_t *count, size_t *cap) {
    char *line = NULL;
    size_t line_cap = 0;
    ssize_t len;

    while ((len = getline(&line, &line_cap, fp)) >= 0) {
        char *copy = malloc((size_t)len + 1);
        if (!copy) {
            free(line);
            return -1;
        }
        memcpy(copy, line, (size_t)len + 1);
        if (*count == *cap) {
            size_t new_cap = *cap ? *cap * 2 : 16;
            char **new_lines = realloc(*lines, new_cap * sizeof(char *));
            if (!new_lines) {
                free(copy);
                free(line);
                return -1;
            }
            *lines = new_lines;
            *cap = new_cap;
        }
        (*lines)[(*count)++] = copy;
    }

    free(line);
    return ferror(fp) ? -1 : 0;
}

int main(int argc, char **argv) {
    int argi = 1;
    int status = 0;
    char **lines = NULL;
    size_t count = 0;
    size_t cap = 0;

    while (argi < argc && argv[argi][0] == '-' && argv[argi][1]) {
        const char *opt;
        if (strcmp(argv[argi], "--") == 0) {
            argi++;
            break;
        }
        for (opt = argv[argi] + 1; *opt; opt++) {
            if (*opt == 'r') {
                reverse_sort = 1;
                continue;
            }
            if (*opt == 'n') {
                numeric_sort = 1;
                continue;
            }
            usage();
            return 1;
        }
        argi++;
    }

    if (argi == argc) {
        if (append_stream(stdin, &lines, &count, &cap) != 0) {
            fputs("sort: read error\n", stderr);
            status = 1;
        }
    } else {
        for (; argi < argc; argi++) {
            FILE *fp = fopen(argv[argi], "r");
            if (!fp) {
                fprintf(stderr, "sort: cannot open '%s': %s\n", argv[argi], strerror(errno));
                status = 1;
                continue;
            }
            if (append_stream(fp, &lines, &count, &cap) != 0) {
                fprintf(stderr, "sort: read error on '%s'\n", argv[argi]);
                status = 1;
            }
            fclose(fp);
        }
    }

    if (count > 0) {
        qsort(lines, count, sizeof(char *), numeric_sort ? cmp_num : cmp_lex);
        for (size_t i = 0; i < count; i++) {
            fputs(lines[i], stdout);
            free(lines[i]);
        }
    }
    free(lines);
    return status;
}
