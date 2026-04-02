#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void usage(void) {
    fputs("usage: hexdump [-C] [-n BYTES] [file...]\n", stderr);
}

static void dump_stream(FILE *fp, const char *label, long limit) {
    unsigned char buf[16];
    unsigned long offset = 0;

    (void)label;
    while (limit != 0) {
        size_t want = sizeof(buf);
        size_t got;
        if (limit > 0 && (unsigned long)limit < want) {
            want = (size_t)limit;
        }
        got = fread(buf, 1, want, fp);
        if (got == 0) {
            break;
        }

        printf("%08lx  ", offset);
        for (size_t i = 0; i < 16; i++) {
            if (i < got) {
                printf("%02x ", buf[i]);
            } else {
                fputs("   ", stdout);
            }
            if (i == 7) {
                putchar(' ');
            }
        }
        fputs(" |", stdout);
        for (size_t i = 0; i < got; i++) {
            putchar(isprint(buf[i]) ? buf[i] : '.');
        }
        for (size_t i = got; i < 16; i++) {
            putchar(' ');
        }
        fputs("|\n", stdout);

        offset += got;
        if (limit > 0) {
            limit -= (long)got;
        }
    }
}

int main(int argc, char **argv) {
    int argi = 1;
    int status = 0;
    long limit = -1;

    while (argi < argc && argv[argi][0] == '-' && argv[argi][1]) {
        if (strcmp(argv[argi], "-C") == 0) {
            argi++;
            continue;
        }
        if (strcmp(argv[argi], "-n") == 0) {
            char *end = NULL;
            if (argi + 1 >= argc) {
                usage();
                return 1;
            }
            limit = strtol(argv[argi + 1], &end, 10);
            if (!argv[argi + 1][0] || (end && *end) || limit < 0) {
                usage();
                return 1;
            }
            argi += 2;
            continue;
        }
        usage();
        return 1;
    }

    if (argi == argc) {
        dump_stream(stdin, "<stdin>", limit);
        return ferror(stdin) || ferror(stdout);
    }

    for (; argi < argc; argi++) {
        FILE *fp = fopen(argv[argi], "rb");
        if (!fp) {
            fprintf(stderr, "hexdump: cannot open '%s': %s\n", argv[argi], strerror(errno));
            status = 1;
            continue;
        }
        dump_stream(fp, argv[argi], limit);
        if (ferror(fp) || ferror(stdout)) {
            fprintf(stderr, "hexdump: read error on '%s'\n", argv[argi]);
            status = 1;
        }
        fclose(fp);
    }

    return status;
}
