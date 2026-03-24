/* mv — rename file (fallback: cp + rm) */
#include <unistd.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 3) {
        const char *msg = "usage: mv <src> <dst>\n";
        write(2, msg, strlen(msg));
        return 1;
    }
    if (rename(argv[1], argv[2]) != 0) {
        const char *msg = "mv: rename failed\n";
        write(2, msg, strlen(msg));
        return 1;
    }
    return 0;
}
