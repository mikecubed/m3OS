/* touch — create files or update modification timestamps */
#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <utime.h>

static void write_str(int fd, const char *s) {
    const char *p = s;
    while (*p) p++;
    write(fd, s, p - s);
}

int main(int argc, char **argv) {
    if (argc < 2) {
        write_str(2, "usage: touch FILE...\n");
        return 1;
    }
    int ret = 0;
    for (int i = 1; i < argc; i++) {
        struct stat st;
        if (stat(argv[i], &st) == 0) {
            /* File exists — update timestamps to current time. */
            if (utime(argv[i], (void *)0) < 0) {
                write_str(2, "touch: cannot update timestamps: ");
                write_str(2, argv[i]);
                write_str(2, "\n");
                ret = 1;
            }
        } else {
            /* File does not exist — create it. */
            int fd = open(argv[i], O_WRONLY | O_CREAT, 0644);
            if (fd < 0) {
                write_str(2, "touch: cannot create: ");
                write_str(2, argv[i]);
                write_str(2, "\n");
                ret = 1;
            } else {
                close(fd);
            }
        }
    }
    return ret;
}
