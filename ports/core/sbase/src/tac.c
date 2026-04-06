/* tac - concatenate and print files in reverse */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_LINES 8192

int main(void) {
    char *lines[MAX_LINES];
    char buf[4096];
    int count = 0;

    while (count < MAX_LINES && fgets(buf, sizeof(buf), stdin)) {
        size_t len = strlen(buf);
        lines[count] = (char *)malloc(len + 1);
        if (!lines[count]) break;
        memcpy(lines[count], buf, len + 1);
        count++;
    }

    for (int i = count - 1; i >= 0; i--) {
        fputs(lines[i], stdout);
        free(lines[i]);
    }
    return 0;
}
