#define _POSIX_C_SOURCE 200809L

#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <unistd.h>

typedef struct {
    char **items;
    size_t len;
    size_t cap;
} LineVec;

static struct termios saved_termios;
static int saved_termios_valid = 0;
static int tty_fd = STDIN_FILENO;
static const char *source_name = "(stdin)";

static void disable_raw_mode(void) {
    if (saved_termios_valid) {
        tcsetattr(tty_fd, TCSANOW, &saved_termios);
    }
}

static int enable_raw_mode(void) {
    struct termios raw;
    if (tcgetattr(tty_fd, &saved_termios) != 0) {
        return -1;
    }
    raw = saved_termios;
    raw.c_lflag &= ~(ICANON | ECHO);
    raw.c_cc[VMIN] = 1;
    raw.c_cc[VTIME] = 0;
    if (tcsetattr(tty_fd, TCSANOW, &raw) != 0) {
        return -1;
    }
    saved_termios_valid = 1;
    atexit(disable_raw_mode);
    return 0;
}

static void line_vec_free(LineVec *vec) {
    for (size_t i = 0; i < vec->len; i++) {
        free(vec->items[i]);
    }
    free(vec->items);
}

static int line_vec_push(LineVec *vec, const char *line) {
    if (vec->len == vec->cap) {
        size_t new_cap = vec->cap ? vec->cap * 2 : 32;
        char **new_items = realloc(vec->items, new_cap * sizeof(*new_items));
        if (!new_items) {
            return -1;
        }
        vec->items = new_items;
        vec->cap = new_cap;
    }
    vec->items[vec->len] = strdup(line);
    if (!vec->items[vec->len]) {
        return -1;
    }
    vec->len++;
    return 0;
}

static int read_lines(FILE *fp, LineVec *lines) {
    char *line = NULL;
    size_t cap = 0;
    ssize_t nread;
    while ((nread = getline(&line, &cap, fp)) != -1) {
        if (line_vec_push(lines, line) != 0) {
            free(line);
            return -1;
        }
        if (nread == 0) {
            break;
        }
    }
    free(line);
    if (lines->len == 0) {
        if (line_vec_push(lines, "") != 0) {
            return -1;
        }
    }
    return 0;
}

static void get_window_size(int *rows, int *cols) {
    struct winsize ws;
    if (ioctl(STDOUT_FILENO, TIOCGWINSZ, &ws) == 0 && ws.ws_row > 0 && ws.ws_col > 0) {
        *rows = ws.ws_row;
        *cols = ws.ws_col;
    } else {
        *rows = 24;
        *cols = 80;
    }
}

static void draw_status(const char *fmt, ...) {
    va_list ap;
    printf("\x1b[7m");
    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);
    printf("\x1b[K\x1b[0m");
}

static void draw_screen(const LineVec *lines, size_t top) {
    int rows;
    int cols;
    get_window_size(&rows, &cols);
    printf("\x1b[2J\x1b[H");
    for (int row = 0; row < rows - 1; row++) {
        size_t idx = top + (size_t)row;
        if (idx < lines->len) {
            const char *line = lines->items[idx];
            int printed = 0;
            while (*line && *line != '\n' && printed < cols) {
                putchar(*line++);
                printed++;
            }
        }
        printf("\x1b[K");
        if (row != rows - 2) {
            putchar('\n');
        }
    }
    printf("\x1b[%d;1H", rows);
    draw_status("%s  %zu/%zu  (q quit, arrows/space/b scroll, / search)", source_name, top + 1, lines->len);
    fflush(stdout);
}

static int search_forward(const LineVec *lines, size_t start, const char *pattern) {
    for (size_t i = start; i < lines->len; i++) {
        if (strstr(lines->items[i], pattern)) {
            return (int)i;
        }
    }
    return -1;
}

static void prompt_search(char *buf, size_t len) {
    size_t pos = 0;
    int rows;
    int cols;
    (void)cols;
    get_window_size(&rows, &cols);
    buf[0] = '\0';
    while (1) {
        char ch;
        printf("\x1b[%d;1H\x1b[K/%s", rows, buf);
        fflush(stdout);
        if (read(tty_fd, &ch, 1) != 1) {
            return;
        }
        if (ch == '\r' || ch == '\n') {
            buf[pos] = '\0';
            return;
        }
        if (ch == 27) {
            buf[0] = '\0';
            return;
        }
        if ((ch == 127 || ch == '\b') && pos > 0) {
            pos--;
            buf[pos] = '\0';
            continue;
        }
        if (isprint((unsigned char)ch) && pos + 1 < len) {
            buf[pos++] = ch;
            buf[pos] = '\0';
        }
    }
}

static int read_key(void) {
    char ch;
    if (read(tty_fd, &ch, 1) != 1) {
        return -1;
    }
    if (ch != 27) {
        return ch;
    }
    if (read(tty_fd, &ch, 1) != 1) {
        return 27;
    }
    if (ch != '[') {
        return 27;
    }
    if (read(tty_fd, &ch, 1) != 1) {
        return 27;
    }
    if (ch >= '0' && ch <= '9') {
        char last;
        if (read(tty_fd, &last, 1) != 1) {
            return 27;
        }
        if (ch == '5' && last == '~') {
            return 'P';
        }
        if (ch == '6' && last == '~') {
            return 'N';
        }
        return 27;
    }
    if (ch == 'A') {
        return 'K';
    }
    if (ch == 'B') {
        return 'J';
    }
    return 27;
}

static void usage(void) {
    fputs("usage: less [FILE]\n", stderr);
}

int main(int argc, char **argv) {
    FILE *fp = stdin;
    LineVec lines = {0};
    size_t top = 0;
    char search[128];

    if (argc > 2) {
        usage();
        return 1;
    }

    if (argc == 2) {
        source_name = argv[1];
        fp = fopen(argv[1], "r");
        if (!fp) {
            fprintf(stderr, "less: cannot open %s: %s\n", argv[1], strerror(errno));
            return 1;
        }
    }

    if (read_lines(fp, &lines) != 0) {
        fprintf(stderr, "less: out of memory\n");
        if (fp != stdin) {
            fclose(fp);
        }
        line_vec_free(&lines);
        return 1;
    }
    if (fp != stdin) {
        fclose(fp);
    }

    tty_fd = open("/dev/tty", O_RDWR);
    if (tty_fd < 0) {
        tty_fd = STDIN_FILENO;
    }
    if (enable_raw_mode() != 0) {
        fprintf(stderr, "less: could not enable raw mode\n");
        line_vec_free(&lines);
        if (tty_fd != STDIN_FILENO) {
            close(tty_fd);
        }
        return 1;
    }

    while (1) {
        int rows;
        int cols;
        int key;
        (void)cols;
        get_window_size(&rows, &cols);
        draw_screen(&lines, top);
        key = read_key();
        if (key == 'q' || key == 'Q') {
            break;
        }
        if ((key == 'j' || key == 'J') && top + 1 < lines.len) {
            top++;
        } else if ((key == 'k' || key == 'K') && top > 0) {
            top--;
        } else if ((key == ' ' || key == 'N') && top + (size_t)(rows - 1) < lines.len) {
            size_t step = (size_t)(rows - 1);
            top = (top + step < lines.len) ? top + step : lines.len - 1;
        } else if ((key == 'b' || key == 'P') && top > 0) {
            size_t step = (size_t)(rows - 1);
            top = top > step ? top - step : 0;
        } else if (key == '/') {
            prompt_search(search, sizeof(search));
            if (search[0] != '\0') {
                int found = search_forward(&lines, top + 1, search);
                if (found >= 0) {
                    top = (size_t)found;
                }
            }
        }
    }

    printf("\x1b[2J\x1b[H");
    fflush(stdout);
    disable_raw_mode();
    if (tty_fd != STDIN_FILENO) {
        close(tty_fd);
    }
    line_vec_free(&lines);
    return 0;
}
