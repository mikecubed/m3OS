/* grep — search stdin or files for a fixed string, print matching lines */
#include <unistd.h>
#include <fcntl.h>
#include <string.h>

static void write_all(int fd, const char *buf, ssize_t len) {
    ssize_t off = 0;
    while (off < len) {
        ssize_t w = write(fd, buf + off, len - off);
        if (w <= 0) break;
        off += w;
    }
}

static void grep_fd(int fd, const char *pattern) {
    char buf[4096];
    ssize_t n;
    int line_start = 0;

    while ((n = read(fd, buf + line_start, sizeof(buf) - line_start - 1)) > 0) {
        n += line_start;
        buf[n] = '\0';
        char *p = buf;
        while (1) {
            char *nl = memchr(p, '\n', buf + n - p);
            if (!nl) {
                /* No newline — move leftover to start of buf. */
                line_start = buf + n - p;
                if (line_start > 0 && p != buf) {
                    memmove(buf, p, line_start);
                }
                break;
            }
            *nl = '\0';
            if (strstr(p, pattern)) {
                write_all(1, p, nl - p);
                write_all(1, "\n", 1);
            }
            p = nl + 1;
            line_start = 0;
        }
    }
    /* Check last line (no trailing newline). */
    if (line_start > 0) {
        buf[line_start] = '\0';
        if (strstr(buf, pattern)) {
            write_all(1, buf, line_start);
            write_all(1, "\n", 1);
        }
    }
}

int main(int argc, char **argv) {
    if (argc < 2) {
        const char *msg = "usage: grep <pattern> [file...]\n";
        write(2, msg, strlen(msg));
        return 1;
    }
    const char *pattern = argv[1];
    if (argc == 2) {
        /* Read from stdin. */
        grep_fd(0, pattern);
    } else {
        for (int i = 2; i < argc; i++) {
            int fd = open(argv[i], O_RDONLY);
            if (fd < 0) continue;
            grep_fd(fd, pattern);
            close(fd);
        }
    }
    return 0;
}
