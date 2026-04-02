#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef enum {
    MODE_NONE,
    MODE_FIELDS,
    MODE_CHARS,
} CutMode;

static void usage(void) {
    fputs("usage: cut (-f N [-d C] | -c M-N) [file...]\n", stderr);
}

static int parse_positive(const char *s, long *out) {
    char *end = NULL;
    long value = strtol(s, &end, 10);
    if (!s[0] || (end && *end) || value <= 0) {
        return -1;
    }
    *out = value;
    return 0;
}

static int parse_positive_span(const char *s, size_t len, long *out) {
    char buf[32];
    if (len == 0 || len >= sizeof(buf)) {
        return -1;
    }
    memcpy(buf, s, len);
    buf[len] = '\0';
    return parse_positive(buf, out);
}

static int process_stream(FILE *fp, CutMode mode, char delim, long field, long start, long end) {
    char *line = NULL;
    size_t cap = 0;
    ssize_t len;

    while ((len = getline(&line, &cap, fp)) >= 0) {
        if (mode == MODE_FIELDS) {
            long current = 1;
            char *segment = line;
            char *cursor = line;
            while (*cursor && *cursor != '\n') {
                if (*cursor == delim) {
                    if (current == field) {
                        break;
                    }
                    current++;
                    segment = cursor + 1;
                }
                cursor++;
            }
            if (current == field) {
                char *end_ptr = cursor;
                while (*end_ptr && *end_ptr != delim && *end_ptr != '\n') {
                    end_ptr++;
                }
                fwrite(segment, 1, (size_t)(end_ptr - segment), stdout);
                fputc('\n', stdout);
            }
        } else {
            long begin = start - 1;
            long finish = end;
            long text_len = len;
            if (text_len > 0 && line[text_len - 1] == '\n') {
                text_len--;
            }
            if (begin < text_len) {
                long slice_end = finish > text_len ? text_len : finish;
                if (slice_end > begin) {
                    fwrite(line + begin, 1, (size_t)(slice_end - begin), stdout);
                }
            }
            fputc('\n', stdout);
        }
    }

    free(line);
    return ferror(fp) ? -1 : 0;
}

int main(int argc, char **argv) {
    CutMode mode = MODE_NONE;
    char delim = '\t';
    long field = 0;
    long start = 0;
    long end = 0;
    int argi = 1;
    int status = 0;

    while (argi < argc && argv[argi][0] == '-' && argv[argi][1]) {
        if (strcmp(argv[argi], "--") == 0) {
            argi++;
            break;
        }
        if (strcmp(argv[argi], "-d") == 0) {
            if (argi + 1 >= argc || argv[argi + 1][0] == '\0') {
                usage();
                return 1;
            }
            delim = argv[argi + 1][0];
            argi += 2;
            continue;
        }
        if (strncmp(argv[argi], "-d", 2) == 0 && argv[argi][2] != '\0') {
            delim = argv[argi][2];
            argi++;
            continue;
        }
        if (strcmp(argv[argi], "-f") == 0) {
            if (argi + 1 >= argc || parse_positive(argv[argi + 1], &field) != 0) {
                usage();
                return 1;
            }
            mode = MODE_FIELDS;
            argi += 2;
            continue;
        }
        if (strncmp(argv[argi], "-f", 2) == 0 && argv[argi][2] != '\0') {
            if (parse_positive(argv[argi] + 2, &field) != 0) {
                usage();
                return 1;
            }
            mode = MODE_FIELDS;
            argi++;
            continue;
        }
        if (strcmp(argv[argi], "-c") == 0) {
            const char *range;
            const char *dash;
            if (argi + 1 >= argc) {
                usage();
                return 1;
            }
            range = argv[argi + 1];
            dash = strchr(range, '-');
            if (!dash) {
                usage();
                return 1;
            }
            if (dash == range
                || parse_positive_span(range, (size_t)(dash - range), &start) != 0
                || parse_positive(dash + 1, &end) != 0
                || end < start) {
                usage();
                return 1;
            }
            mode = MODE_CHARS;
            argi += 2;
            continue;
        }
        if (strncmp(argv[argi], "-c", 2) == 0 && argv[argi][2] != '\0') {
            const char *range = argv[argi] + 2;
            const char *dash = strchr(range, '-');
            if (!dash
                || dash == range
                || parse_positive_span(range, (size_t)(dash - range), &start) != 0
                || parse_positive(dash + 1, &end) != 0
                || end < start) {
                usage();
                return 1;
            }
            mode = MODE_CHARS;
            argi++;
            continue;
        }
        usage();
        return 1;
    }

    if (mode == MODE_NONE) {
        usage();
        return 1;
    }

    if (argi == argc) {
        return process_stream(stdin, mode, delim, field, start, end) != 0;
    }

    for (; argi < argc; argi++) {
        FILE *fp = fopen(argv[argi], "r");
        if (!fp) {
            fprintf(stderr, "cut: cannot open '%s': %s\n", argv[argi], strerror(errno));
            status = 1;
            continue;
        }
        if (process_stream(fp, mode, delim, field, start, end) != 0) {
            fprintf(stderr, "cut: read error on '%s'\n", argv[argi]);
            status = 1;
        }
        fclose(fp);
    }

    return status;
}
