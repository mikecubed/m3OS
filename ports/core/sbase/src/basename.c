/* basename - strip directory and suffix from filenames */
#include <stdio.h>
#include <string.h>

int main(int argc, char *argv[]) {
    char *p;
    if (argc < 2) {
        fprintf(stderr, "usage: basename string [suffix]\n");
        return 1;
    }
    p = strrchr(argv[1], '/');
    p = p ? p + 1 : argv[1];
    if (argc > 2) {
        size_t plen = strlen(p);
        size_t slen = strlen(argv[2]);
        if (plen > slen && strcmp(p + plen - slen, argv[2]) == 0)
            p[plen - slen] = '\0';
    }
    puts(p);
    return 0;
}
