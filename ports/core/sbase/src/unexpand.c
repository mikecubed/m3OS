/* unexpand - convert spaces to tabs */
#include <stdio.h>

int main(void) {
    int c, col = 0, spaces = 0, tabstop = 8;
    while ((c = getchar()) != EOF) {
        if (c == ' ') {
            spaces++;
            col++;
            if (col % tabstop == 0) {
                putchar('\t');
                spaces = 0;
            }
        } else {
            while (spaces > 0) { putchar(' '); spaces--; }
            putchar(c);
            col = (c == '\n') ? 0 : col + 1;
        }
    }
    return 0;
}
