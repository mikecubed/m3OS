/* cols - columnate output */
#include <stdio.h>
#include <string.h>

int main(void) {
    char line[4096];
    while (fgets(line, sizeof(line), stdin)) {
        size_t len = strlen(line);
        if (len > 0 && line[len-1] == '\n') line[len-1] = '\0';
        printf("%-20s\n", line);
    }
    return 0;
}
