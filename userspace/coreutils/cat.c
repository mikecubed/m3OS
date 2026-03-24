/* cat — read file(s) or stdin and write to stdout */
#include <unistd.h>
#include <fcntl.h>

static void cat_fd(int fd) {
    char buf[4096];
    ssize_t n;
    while ((n = read(fd, buf, sizeof(buf))) > 0) {
        write(1, buf, n);
    }
}

int main(int argc, char **argv) {
    if (argc <= 1) {
        cat_fd(0);
        return 0;
    }
    for (int i = 1; i < argc; i++) {
        int fd = open(argv[i], O_RDONLY);
        if (fd < 0) {
            const char *msg = "cat: cannot open file\n";
            write(2, msg, 22);
            continue;
        }
        cat_fd(fd);
        close(fd);
    }
    return 0;
}
