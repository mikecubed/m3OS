/* rm — remove files */
#include <unistd.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        const char *msg = "usage: rm <file>\n";
        write(2, msg, strlen(msg));
        return 1;
    }
    int ret = 0;
    for (int i = 1; i < argc; i++) {
        if (unlink(argv[i]) != 0) {
            const char *msg = "rm: failed\n";
            write(2, msg, strlen(msg));
            ret = 1;
        }
    }
    return ret;
}
