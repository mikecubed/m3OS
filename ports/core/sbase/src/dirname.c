/* dirname - strip last component from file name */
#include <stdio.h>
#include <string.h>

int main(int argc, char *argv[]) {
    char *p;
    if (argc < 2) {
        fprintf(stderr, "usage: dirname path\n");
        return 1;
    }
    p = strrchr(argv[1], '/');
    if (!p) {
        puts(".");
    } else if (p == argv[1]) {
        puts("/");
    } else {
        *p = '\0';
        puts(argv[1]);
    }
    return 0;
}
