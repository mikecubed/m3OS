/* mkdir — create directory */
#include <sys/stat.h>
#include <unistd.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        const char *msg = "usage: mkdir <dir>\n";
        write(2, msg, strlen(msg));
        return 1;
    }
    int ret = 0;
    for (int i = 1; i < argc; i++) {
        if (mkdir(argv[i], 0755) != 0) {
            const char *msg = "mkdir: failed\n";
            write(2, msg, strlen(msg));
            ret = 1;
        }
    }
    return ret;
}
