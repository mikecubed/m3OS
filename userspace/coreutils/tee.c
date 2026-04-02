#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void usage(void) {
    fputs("usage: tee [-a] [file...]\n", stderr);
}

int main(int argc, char **argv) {
    int append = 0;
    int argi = 1;
    int status = 0;
    FILE **outs = NULL;
    int out_count = 0;

    while (argi < argc && argv[argi][0] == '-' && argv[argi][1]) {
        if (strcmp(argv[argi], "--") == 0) {
            argi++;
            break;
        }
        if (strcmp(argv[argi], "-a") == 0) {
            append = 1;
            argi++;
            continue;
        }
        usage();
        return 1;
    }

    out_count = argc - argi;
    if (out_count > 0) {
        outs = calloc((size_t)out_count, sizeof(FILE *));
        if (!outs) {
            return 1;
        }
        for (int i = 0; i < out_count; i++) {
            outs[i] = fopen(argv[argi + i], append ? "a" : "w");
            if (!outs[i]) {
                fprintf(stderr, "tee: cannot open '%s': %s\n", argv[argi + i], strerror(errno));
                status = 1;
            }
        }
    }

    char buf[4096];
    size_t n;
    while ((n = fread(buf, 1, sizeof(buf), stdin)) > 0) {
        if (fwrite(buf, 1, n, stdout) != n) {
            status = 1;
            break;
        }
        for (int i = 0; i < out_count; i++) {
            if (!outs[i]) {
                continue;
            }
            if (fwrite(buf, 1, n, outs[i]) != n) {
                status = 1;
            }
        }
    }
    if (ferror(stdin)) {
        status = 1;
    }

    for (int i = 0; i < out_count; i++) {
        if (outs && outs[i]) {
            fclose(outs[i]);
        }
    }
    free(outs);
    return status;
}
