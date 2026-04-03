#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void usage(void) {
    fputs("usage: strings [-n MIN] FILE...\n", stderr);
}

static int is_string_char(unsigned char ch) {
    return ch == '\t' || (ch >= 32 && ch <= 126);
}

static int scan_file(const char *path, size_t min_len) {
    FILE *fp = fopen(path, "rb");
    char *buf = NULL;
    size_t len = 0;
    size_t cap = 0;
    int ch;

    if (!fp) {
        fprintf(stderr, "strings: cannot open %s: %s\n", path, strerror(errno));
        return 1;
    }

    while ((ch = fgetc(fp)) != EOF) {
        if (is_string_char((unsigned char)ch)) {
            if (len + 1 >= cap) {
                size_t new_cap = cap ? cap * 2 : 64;
                char *new_buf = realloc(buf, new_cap);
                if (!new_buf) {
                    fprintf(stderr, "strings: out of memory\n");
                    free(buf);
                    fclose(fp);
                    return 1;
                }
                buf = new_buf;
                cap = new_cap;
            }
            buf[len++] = (char)ch;
        } else if (len >= min_len) {
            buf[len] = '\0';
            puts(buf);
            len = 0;
        } else {
            len = 0;
        }
    }

    if (len >= min_len) {
        buf[len] = '\0';
        puts(buf);
    }

    free(buf);
    fclose(fp);
    return 0;
}

int main(int argc, char **argv) {
    size_t min_len = 4;
    int argi = 1;
    int status = 0;

    if (argc < 2) {
        usage();
        return 1;
    }
    if (argi < argc && strcmp(argv[argi], "-n") == 0) {
        char *end = NULL;
        if (argi + 1 >= argc) {
            usage();
            return 1;
        }
        min_len = strtoul(argv[argi + 1], &end, 10);
        if (!end || *end != '\0' || min_len == 0) {
            usage();
            return 1;
        }
        argi += 2;
    }
    if (argi >= argc) {
        usage();
        return 1;
    }

    for (; argi < argc; argi++) {
        if (scan_file(argv[argi], min_len) != 0) {
            status = 1;
        }
    }
    return status;
}
