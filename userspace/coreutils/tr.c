#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void usage(void) {
    fputs("usage: tr [-d] SET1 [SET2]\n", stderr);
}

static int parse_escape_char(char c) {
    switch (c) {
        case 'n':
            return '\n';
        case 'r':
            return '\r';
        case 't':
            return '\t';
        case '\\':
            return '\\';
        default:
            return (unsigned char)c;
    }
}

static size_t expand_set(const char *spec, unsigned char *out, size_t max_len) {
    size_t len = 0;
    size_t i = 0;

    while (spec[i] != '\0' && len < max_len) {
        unsigned char first;
        unsigned char last;

        if (spec[i] == '\\' && spec[i + 1] != '\0') {
            first = (unsigned char)parse_escape_char(spec[i + 1]);
            i += 2;
        } else {
            first = (unsigned char)spec[i++];
        }

        if (spec[i] == '-' && spec[i + 1] != '\0') {
            i++;
            if (spec[i] == '\\' && spec[i + 1] != '\0') {
                last = (unsigned char)parse_escape_char(spec[i + 1]);
                i += 2;
            } else {
                last = (unsigned char)spec[i++];
            }
            if (first <= last) {
                for (unsigned int ch = first; ch <= last && len < max_len; ch++) {
                    out[len++] = (unsigned char)ch;
                }
            } else {
                for (int ch = first; ch >= (int)last && len < max_len; ch--) {
                    out[len++] = (unsigned char)ch;
                }
            }
            continue;
        }

        out[len++] = first;
    }

    return len;
}

int main(int argc, char **argv) {
    int delete_mode = 0;
    int argi = 1;
    unsigned char set1[256];
    unsigned char set2[256];
    size_t set1_len;
    size_t set2_len = 0;
    int map[256];
    int ch;

    if (argi < argc && strcmp(argv[argi], "-d") == 0) {
        delete_mode = 1;
        argi++;
    }

    if ((delete_mode && argi + 1 != argc) || (!delete_mode && argi + 2 != argc)) {
        usage();
        return 1;
    }

    set1_len = expand_set(argv[argi], set1, sizeof(set1));
    if (set1_len == 0) {
        usage();
        return 1;
    }

    for (size_t i = 0; i < 256; i++) {
        map[i] = (int)i;
    }

    if (delete_mode) {
        for (size_t i = 0; i < set1_len; i++) {
            map[set1[i]] = -1;
        }
    } else {
        set2_len = expand_set(argv[argi + 1], set2, sizeof(set2));
        if (set2_len == 0) {
            usage();
            return 1;
        }
        for (size_t i = 0; i < set1_len; i++) {
            unsigned char replacement = set2[i < set2_len ? i : set2_len - 1];
            map[set1[i]] = replacement;
        }
    }

    while ((ch = getchar()) != EOF) {
        int mapped = map[(unsigned char)ch];
        if (mapped >= 0) {
            putchar(mapped);
        }
    }

    return ferror(stdin) || ferror(stdout);
}
