/* nl - number lines of files */
#include <stdio.h>

int main(void) {
    char line[4096];
    int n = 1;
    while (fgets(line, sizeof(line), stdin)) {
        if (line[0] == '\n')
            printf("       %s", line);
        else
            printf("  %4d\t%s", n++, line);
    }
    return 0;
}
