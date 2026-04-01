/* wc — count lines, words, and bytes */
#include <unistd.h>
#include <fcntl.h>
#include <string.h>

static void write_str(int fd, const char *s) {
    const char *p = s;
    while (*p) p++;
    write(fd, s, p - s);
}

static void write_num(int fd, long long n) {
    char buf[24];
    int i = sizeof(buf) - 1;
    buf[i] = '\0';
    if (n == 0) { buf[--i] = '0'; }
    while (n > 0) { buf[--i] = '0' + (n % 10); n /= 10; }
    write_str(fd, &buf[i]);
}

static int wc_fd(int fd, long long *tl, long long *tw, long long *tc) {
    char buf[4096];
    long long lines = 0, words = 0, bytes = 0;
    int in_word = 0;
    ssize_t n;
    while ((n = read(fd, buf, sizeof(buf))) > 0) {
        bytes += n;
        for (ssize_t i = 0; i < n; i++) {
            if (buf[i] == '\n') lines++;
            if (buf[i] == ' ' || buf[i] == '\t' || buf[i] == '\n' || buf[i] == '\r') {
                in_word = 0;
            } else if (!in_word) {
                in_word = 1;
                words++;
            }
        }
    }
    if (n < 0) write_str(2, "wc: read error\n");
    *tl += lines; *tw += words; *tc += bytes;
    return n < 0 ? -1 : 0;
}

int main(int argc, char **argv) {
    int show_lines = 0, show_words = 0, show_bytes = 0;
    int first_file = 1;

    /* Parse flags. */
    for (int i = 1; i < argc; i++) {
        if (argv[i][0] == '-' && argv[i][1] != '\0') {
            for (const char *p = argv[i] + 1; *p; p++) {
                if (*p == 'l') show_lines = 1;
                else if (*p == 'w') show_words = 1;
                else if (*p == 'c') show_bytes = 1;
            }
            first_file = i + 1;
        } else {
            break;
        }
    }

    /* Default: show all. */
    if (!show_lines && !show_words && !show_bytes) {
        show_lines = show_words = show_bytes = 1;
    }

    long long total_l = 0, total_w = 0, total_c = 0;
    int file_count = argc - first_file;

    int ret = 0;

    if (file_count == 0) {
        /* Read from stdin. */
        if (wc_fd(0, &total_l, &total_w, &total_c) < 0) ret = 1;
        if (show_lines) { write_num(1, total_l); write_str(1, " "); }
        if (show_words) { write_num(1, total_w); write_str(1, " "); }
        if (show_bytes) { write_num(1, total_c); }
        write_str(1, "\n");
        return ret;
    }

    for (int i = first_file; i < argc; i++) {
        int fd = open(argv[i], O_RDONLY);
        if (fd < 0) {
            write_str(2, "wc: cannot open: ");
            write_str(2, argv[i]);
            write_str(2, "\n");
            ret = 1;
            continue;
        }
        long long l = 0, w = 0, c = 0;
        if (wc_fd(fd, &l, &w, &c) < 0) ret = 1;
        close(fd);
        total_l += l; total_w += w; total_c += c;
        if (show_lines) { write_num(1, l); write_str(1, " "); }
        if (show_words) { write_num(1, w); write_str(1, " "); }
        if (show_bytes) { write_num(1, c); write_str(1, " "); }
        write_str(1, argv[i]);
        write_str(1, "\n");
    }

    if (file_count > 1) {
        if (show_lines) { write_num(1, total_l); write_str(1, " "); }
        if (show_words) { write_num(1, total_w); write_str(1, " "); }
        if (show_bytes) { write_num(1, total_c); write_str(1, " "); }
        write_str(1, "total\n");
    }
    return ret;
}
