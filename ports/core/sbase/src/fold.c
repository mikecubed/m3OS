/* fold - wrap each input line to fit in specified width */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char *argv[]) {
    int width = 80;
    if (argc > 2 && strcmp(argv[1], "-w") == 0)
        width = atoi(argv[2]);
    int c, col = 0;
    while ((c = getchar()) != EOF) {
        if (c == '\n') {
            putchar(c);
            col = 0;
        } else {
            if (col >= width) {
                putchar('\n');
                col = 0;
            }
            putchar(c);
            col++;
        }
    }
    return 0;
}
