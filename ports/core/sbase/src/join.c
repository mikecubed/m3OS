/* join - join lines of two files on a common field (stdin passthrough) */
#include <stdio.h>

int main(void) {
    char line[4096];
    while (fgets(line, sizeof(line), stdin))
        fputs(line, stdout);
    return 0;
}
