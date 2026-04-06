/* sponge - soak up stdin and write to a file */
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char *argv[]) {
    char buf[4096];
    size_t total = 0, cap = 65536;
    char *data = malloc(cap);
    size_t n;

    if (!data) { perror("malloc"); return 1; }

    while ((n = fread(buf, 1, sizeof(buf), stdin)) > 0) {
        if (total + n > cap) {
            cap *= 2;
            data = realloc(data, cap);
            if (!data) { perror("realloc"); return 1; }
        }
        memcpy(data + total, buf, n);
        total += n;
    }

    FILE *out = (argc > 1) ? fopen(argv[1], "w") : stdout;
    if (!out) { perror("fopen"); free(data); return 1; }
    fwrite(data, 1, total, out);
    if (out != stdout) fclose(out);
    free(data);
    return 0;
}
