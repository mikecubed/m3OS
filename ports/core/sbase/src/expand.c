/* expand - convert tabs to spaces */
#include <stdio.h>

int main(void) {
    int c, col = 0, tabstop = 8;
    while ((c = getchar()) != EOF) {
        if (c == '\t') {
            int spaces = tabstop - (col % tabstop);
            for (int i = 0; i < spaces; i++) {
                putchar(' ');
                col++;
            }
        } else {
            putchar(c);
            col = (c == '\n') ? 0 : col + 1;
        }
    }
    return 0;
}
