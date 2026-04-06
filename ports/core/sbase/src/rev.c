/* rev - reverse lines of a file */
#include <stdio.h>
#include <string.h>

int main(void) {
    char line[4096];
    while (fgets(line, sizeof(line), stdin)) {
        size_t len = strlen(line);
        int has_nl = (len > 0 && line[len - 1] == '\n');
        if (has_nl) len--;
        for (size_t i = len; i > 0; i--)
            putchar(line[i - 1]);
        if (has_nl) putchar('\n');
    }
    return 0;
}
