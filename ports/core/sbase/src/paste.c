/* paste - merge lines of files (stdin only) */
#include <stdio.h>
#include <string.h>

int main(int argc, char *argv[]) {
    char delim = '\t';
    char line[4096];
    int serial = 0;

    if (argc > 1 && strcmp(argv[1], "-s") == 0) serial = 1;
    if (argc > 2 && strcmp(argv[1], "-d") == 0) delim = argv[2][0];

    if (serial) {
        int first = 1;
        while (fgets(line, sizeof(line), stdin)) {
            size_t len = strlen(line);
            if (len > 0 && line[len-1] == '\n') line[len-1] = '\0';
            if (!first) putchar(delim);
            fputs(line, stdout);
            first = 0;
        }
        putchar('\n');
    } else {
        /* Without -s, just pass through (single file mode) */
        while (fgets(line, sizeof(line), stdin))
            fputs(line, stdout);
    }
    return 0;
}
