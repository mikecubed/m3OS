/* cp — copy file */
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

int main(int argc, char **argv) {
    if (argc < 3) {
        const char *msg = "usage: cp <src> <dst>\n";
        write(2, msg, strlen(msg)); /* DevSkim: ignore DS140021 — string literal */
        return 1;
    }
    int src = open(argv[1], O_RDONLY);
    if (src < 0) {
        const char *msg = "cp: cannot open source\n";
        write(2, msg, strlen(msg)); /* DevSkim: ignore DS140021 — string literal */
        return 1;
    }
    int dst = open(argv[2], O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (dst < 0) {
        const char *msg = "cp: cannot create dest\n";
        write(2, msg, strlen(msg)); /* DevSkim: ignore DS140021 — string literal */
        close(src);
        return 1;
    }
    char buf[4096];
    ssize_t n;
    while ((n = read(src, buf, sizeof(buf))) > 0) {
        write_all(dst, buf, n);
    }
    close(src);
    close(dst);
    return 0;
}
