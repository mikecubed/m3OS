/* install — copy files and set attributes */
#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <string.h>

static void write_str(int fd, const char *s) {
    const char *p = s;
    while (*p) p++;
    write(fd, s, p - s);
}

static int copy_file(const char *src, const char *dst) {
    int in = open(src, O_RDONLY);
    if (in < 0) {
        write_str(2, "install: cannot open source: ");
        write_str(2, src);
        write_str(2, "\n");
        return 1;
    }
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    if (out < 0) {
        write_str(2, "install: cannot create: ");
        write_str(2, dst);
        write_str(2, "\n");
        close(in);
        return 1;
    }
    char buf[4096];
    ssize_t n;
    while ((n = read(in, buf, sizeof(buf))) > 0) {
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(out, buf + off, n - off);
            if (w <= 0) { close(in); close(out); return 1; }
            off += w;
        }
    }
    close(in);
    close(out);
    if (n < 0) {
        write_str(2, "install: read error\n");
        return 1;
    }
    return 0;
}

int main(int argc, char **argv) {
    int dir_mode = 0;
    int first_arg = 1;

    /* Parse -d flag for directory creation. */
    if (argc >= 2 && strcmp(argv[1], "-d") == 0) {
        dir_mode = 1;
        first_arg = 2;
    }

    if (first_arg >= argc) {
        write_str(2, "usage: install [-d] DIR...\n");
        write_str(2, "       install SRC DEST\n");
        return 1;
    }

    if (dir_mode) {
        /* Create directories. */
        int ret = 0;
        for (int i = first_arg; i < argc; i++) {
            if (mkdir(argv[i], 0755) < 0) {
                /* Ignore EEXIST — check if it's already a directory. */
                struct stat st;
                if (stat(argv[i], &st) == 0 && (st.st_mode & 0xF000) == 0x4000) {
                    continue;
                }
                write_str(2, "install: cannot create directory: ");
                write_str(2, argv[i]);
                write_str(2, "\n");
                ret = 1;
            }
        }
        return ret;
    }

    /* Copy mode: install SRC DEST */
    if (argc - first_arg != 2) {
        write_str(2, "install: expected SRC DEST\n");
        return 1;
    }
    return copy_file(argv[first_arg], argv[first_arg + 1]);
}
