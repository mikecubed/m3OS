#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    char **items;
    size_t len;
    size_t cap;
} LineVec;

static void line_vec_free(LineVec *vec) {
    for (size_t i = 0; i < vec->len; i++) {
        free(vec->items[i]);
    }
    free(vec->items);
}

static int line_vec_push(LineVec *vec, const char *line) {
    if (vec->len == vec->cap) {
        size_t new_cap = vec->cap ? vec->cap * 2 : 16;
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

static int read_lines(const char *path, LineVec *vec) {
    FILE *fp = fopen(path, "r");
    char *line = NULL;
    size_t cap = 0;
    ssize_t nread;

    if (!fp) {
        fprintf(stderr, "diff: cannot open %s: %s\n", path, strerror(errno));
        return -1;
    }
    while ((nread = getline(&line, &cap, fp)) != -1) {
        if (line_vec_push(vec, line) != 0) {
            fprintf(stderr, "diff: out of memory\n");
            free(line);
            fclose(fp);
            return -1;
        }
        if (nread == 0) {
            break;
        }
    }
    free(line);
    fclose(fp);
    return 0;
}

static int lines_equal(const LineVec *a, const LineVec *b) {
    if (a->len != b->len) {
        return 0;
    }
    for (size_t i = 0; i < a->len; i++) {
        if (strcmp(a->items[i], b->items[i]) != 0) {
            return 0;
        }
    }
    return 1;
}

static void print_range(size_t count, char *buf, size_t len) {
    if (count == 0) {
        snprintf(buf, len, "0,0");
    } else if (count == 1) {
        snprintf(buf, len, "1");
    } else {
        snprintf(buf, len, "1,%zu", count);
    }
}

static void print_prefixed(char prefix, const char *line) {
    putchar(prefix);
    fputs(line, stdout);
    if (line[0] == '\0' || line[strlen(line) - 1] != '\n') {
        putchar('\n');
    }
}

static void usage(void) {
    fputs("usage: diff [-u] FILE1 FILE2\n", stderr);
}

int main(int argc, char **argv) {
    LineVec left = {0};
    LineVec right = {0};
    char old_range[32];
    char new_range[32];
    int argi = 1;
    int status = 0;

    if (argi < argc && strcmp(argv[argi], "-u") == 0) {
        argi++;
    }
    if (argc - argi != 2) {
        usage();
        return 2;
    }

    if (read_lines(argv[argi], &left) != 0 || read_lines(argv[argi + 1], &right) != 0) {
        line_vec_free(&left);
        line_vec_free(&right);
        return 2;
    }

    if (lines_equal(&left, &right)) {
        line_vec_free(&left);
        line_vec_free(&right);
        return 0;
    }

    print_range(left.len, old_range, sizeof(old_range));
    print_range(right.len, new_range, sizeof(new_range));

    printf("--- %s\n", argv[argi]);
    printf("+++ %s\n", argv[argi + 1]);
    printf("@@ -%s +%s @@\n", old_range, new_range);

    for (size_t i = 0; i < left.len; i++) {
        print_prefixed('-', left.items[i]);
    }
    for (size_t i = 0; i < right.len; i++) {
        print_prefixed('+', right.items[i]);
    }
    status = 1;

    line_vec_free(&left);
    line_vec_free(&right);
    return status;
}
