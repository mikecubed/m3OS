/* ar — create and manage static library archives (.a files) */
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <sys/stat.h>

#define AR_MAGIC "!<arch>\n"
#define AR_MAGIC_LEN 8

static void write_str(int fd, const char *s) {
    const char *p = s;
    while (*p) p++;
    write(fd, s, p - s);
}

/* Format a number into a field of the given width, space-padded. */
static void fmt_field(char *buf, int width, unsigned long val) {
    char tmp[24];
    int i = sizeof(tmp) - 1;
    tmp[i] = '\0';
    if (val == 0) {
        tmp[--i] = '0';
    } else {
        while (val > 0) {
            tmp[--i] = '0' + (val % 10);
            val /= 10;
        }
    }
    int len = (int)(sizeof(tmp) - 1 - i);
    int j;
    for (j = 0; j < len && j < width; j++)
        buf[j] = tmp[i + j];
    for (; j < width; j++)
        buf[j] = ' ';
}

/* Write a 60-byte ar member header. */
static void write_header(int fd, const char *name, unsigned long size) {
    char hdr[60];
    memset(hdr, ' ', 60);

    /* name: 16 bytes, terminated with '/' */
    int nlen = 0;
    while (name[nlen] && nlen < 15) {
        hdr[nlen] = name[nlen];
        nlen++;
    }
    hdr[nlen] = '/';

    /* timestamp: 12 bytes at offset 16 — use 0 */
    fmt_field(hdr + 16, 12, 0);
    /* uid: 6 bytes at offset 28 */
    fmt_field(hdr + 28, 6, 0);
    /* gid: 6 bytes at offset 34 */
    fmt_field(hdr + 34, 6, 0);
    /* mode: 8 bytes at offset 40 — octal 100644 */
    memcpy(hdr + 40, "100644  ", 8);
    /* size: 10 bytes at offset 48 */
    fmt_field(hdr + 48, 10, size);
    /* magic: 2 bytes at offset 58 */
    hdr[58] = '`';
    hdr[59] = '\n';

    write(fd, hdr, 60);
}

static int do_create(const char *archive, int file_argc, char **file_argv) {
    int fd = open(archive, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        write_str(2, "ar: cannot create: ");
        write_str(2, archive);
        write_str(2, "\n");
        return 1;
    }

    write(fd, AR_MAGIC, AR_MAGIC_LEN);

    for (int i = 0; i < file_argc; i++) {
        /* Get file size. */
        struct stat st;
        if (stat(file_argv[i], &st) < 0) {
            write_str(2, "ar: cannot stat: ");
            write_str(2, file_argv[i]);
            write_str(2, "\n");
            close(fd);
            return 1;
        }

        /* Extract basename from path. */
        const char *basename = file_argv[i];
        for (const char *p = file_argv[i]; *p; p++) {
            if (*p == '/') basename = p + 1;
        }

        write_header(fd, basename, st.st_size);

        /* Copy file content. */
        int src = open(file_argv[i], O_RDONLY);
        if (src < 0) {
            write_str(2, "ar: cannot open: ");
            write_str(2, file_argv[i]);
            write_str(2, "\n");
            close(fd);
            return 1;
        }
        char buf[4096];
        ssize_t n;
        while ((n = read(src, buf, sizeof(buf))) > 0) {
            if (write(fd, buf, n) != n) {
                write_str(2, "ar: write error\n");
                close(src); close(fd); return 1;
            }
        }
        if (n < 0) {
            write_str(2, "ar: read error\n");
            close(src); close(fd); return 1;
        }
        close(src);

        /* Pad to even boundary. */
        if (st.st_size & 1) {
            write(fd, "\n", 1);
        }
    }

    close(fd);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        write_str(2, "usage: ar rcs ARCHIVE FILE...\n");
        return 1;
    }

    /* Parse operation: we support 'r' (replace), 'c' (create), 's' (index stub). */
    const char *op = argv[1];
    int do_replace = 0, do_create_flag = 0;
    for (const char *p = op; *p; p++) {
        if (*p == 'r') do_replace = 1;
        else if (*p == 'c') do_create_flag = 1;
        else if (*p == 's') { /* symbol index — no-op stub */ }
    }

    if (!do_replace) {
        write_str(2, "ar: only 'r' (replace/insert) is supported\n");
        return 1;
    }

    return do_create(argv[2], argc - 3, argv + 3);
}
