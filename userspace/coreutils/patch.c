#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    char kind;
    char *text;
} HunkLine;

typedef struct {
    int old_start;
    int old_count;
    int new_start;
    int new_count;
    HunkLine *lines;
    size_t line_count;
    size_t line_cap;
} Hunk;

typedef struct {
    char *old_path;
    char *new_path;
    Hunk *hunks;
    size_t hunk_count;
    size_t hunk_cap;
} FilePatch;

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

static int line_vec_push(LineVec *vec, const char *text) {
    if (vec->len == vec->cap) {
        size_t new_cap = vec->cap ? vec->cap * 2 : 16;
        char **new_items = realloc(vec->items, new_cap * sizeof(*new_items));
        if (!new_items) {
            return -1;
        }
        vec->items = new_items;
        vec->cap = new_cap;
    }
    vec->items[vec->len] = strdup(text);
    if (!vec->items[vec->len]) {
        return -1;
    }
    vec->len++;
    return 0;
}

static void hunk_free(Hunk *hunk) {
    for (size_t i = 0; i < hunk->line_count; i++) {
        free(hunk->lines[i].text);
    }
    free(hunk->lines);
}

static void file_patch_free(FilePatch *patch) {
    free(patch->old_path);
    free(patch->new_path);
    for (size_t i = 0; i < patch->hunk_count; i++) {
        hunk_free(&patch->hunks[i]);
    }
    free(patch->hunks);
}

static int hunk_add_line(Hunk *hunk, char kind, const char *text) {
    if (hunk->line_count == hunk->line_cap) {
        size_t new_cap = hunk->line_cap ? hunk->line_cap * 2 : 16;
        HunkLine *new_lines = realloc(hunk->lines, new_cap * sizeof(*new_lines));
        if (!new_lines) {
            return -1;
        }
        hunk->lines = new_lines;
        hunk->line_cap = new_cap;
    }
    hunk->lines[hunk->line_count].kind = kind;
    hunk->lines[hunk->line_count].text = strdup(text);
    if (!hunk->lines[hunk->line_count].text) {
        return -1;
    }
    hunk->line_count++;
    return 0;
}

static int file_patch_add_hunk(FilePatch *patch, Hunk *hunk) {
    if (patch->hunk_count == patch->hunk_cap) {
        size_t new_cap = patch->hunk_cap ? patch->hunk_cap * 2 : 4;
        Hunk *new_hunks = realloc(patch->hunks, new_cap * sizeof(*new_hunks));
        if (!new_hunks) {
            return -1;
        }
        patch->hunks = new_hunks;
        patch->hunk_cap = new_cap;
    }
    patch->hunks[patch->hunk_count++] = *hunk;
    return 0;
}

static char *dup_trimmed_path(const char *line) {
    const char *end = line;
    while (*end && *end != '\n' && *end != '\t' && *end != ' ') {
        end++;
    }
    return strndup(line, (size_t)(end - line));
}

static int parse_range(const char **cursor, int *start, int *count) {
    char *end = NULL;
    long start_val = strtol(*cursor, &end, 10);
    long count_val = 1;

    if (end == *cursor) {
        return -1;
    }
    *cursor = end;
    if (**cursor == ',') {
        (*cursor)++;
        count_val = strtol(*cursor, &end, 10);
        if (end == *cursor) {
            return -1;
        }
        *cursor = end;
    }

    *start = (int)start_val;
    *count = (int)count_val;
    return 0;
}

static int parse_hunk_header(const char *line, Hunk *hunk) {
    const char *cursor = strstr(line, "-");
    if (!cursor) {
        return -1;
    }
    cursor++;
    if (parse_range(&cursor, &hunk->old_start, &hunk->old_count) != 0) {
        return -1;
    }
    while (*cursor == ' ') {
        cursor++;
    }
    if (*cursor != '+') {
        return -1;
    }
    cursor++;
    if (parse_range(&cursor, &hunk->new_start, &hunk->new_count) != 0) {
        return -1;
    }
    return 0;
}

static int read_patch_lines(LineVec *lines) {
    char *line = NULL;
    size_t cap = 0;
    ssize_t nread;

    while ((nread = getline(&line, &cap, stdin)) != -1) {
        if (line_vec_push(lines, line) != 0) {
            free(line);
            return -1;
        }
        if (nread == 0) {
            break;
        }
    }
    free(line);
    return 0;
}

static int parse_patch(LineVec *lines, FilePatch **patches_out, size_t *count_out) {
    FilePatch *patches = NULL;
    size_t patch_count = 0;
    size_t patch_cap = 0;
    size_t i = 0;

    while (i < lines->len) {
        FilePatch patch = {0};
        if (strncmp(lines->items[i], "--- ", 4) != 0) {
            i++;
            continue;
        }
        patch.old_path = dup_trimmed_path(lines->items[i] + 4);
        i++;
        if (i >= lines->len || strncmp(lines->items[i], "+++ ", 4) != 0) {
            free(patch.old_path);
            free(patches);
            return -1;
        }
        patch.new_path = dup_trimmed_path(lines->items[i] + 4);
        i++;

        while (i < lines->len && strncmp(lines->items[i], "@@ ", 3) == 0) {
            Hunk hunk = {0};
            if (parse_hunk_header(lines->items[i], &hunk) != 0) {
                file_patch_free(&patch);
                free(patches);
                return -1;
            }
            i++;
            while (i < lines->len) {
                char kind = lines->items[i][0];
                if (strncmp(lines->items[i], "@@ ", 3) == 0 || strncmp(lines->items[i], "--- ", 4) == 0) {
                    break;
                }
                if (kind == '\\') {
                    i++;
                    continue;
                }
                if (kind != ' ' && kind != '+' && kind != '-') {
                    break;
                }
                if (hunk_add_line(&hunk, kind, lines->items[i] + 1) != 0) {
                    hunk_free(&hunk);
                    file_patch_free(&patch);
                    free(patches);
                    return -1;
                }
                i++;
            }
            if (file_patch_add_hunk(&patch, &hunk) != 0) {
                hunk_free(&hunk);
                file_patch_free(&patch);
                free(patches);
                return -1;
            }
        }

        if (patch_count == patch_cap) {
            size_t new_cap = patch_cap ? patch_cap * 2 : 4;
            FilePatch *new_patches = realloc(patches, new_cap * sizeof(*new_patches));
            if (!new_patches) {
                file_patch_free(&patch);
                free(patches);
                return -1;
            }
            patches = new_patches;
            patch_cap = new_cap;
        }
        patches[patch_count++] = patch;
    }

    *patches_out = patches;
    *count_out = patch_count;
    return 0;
}

static char *strip_components(const char *path, int strip_count) {
    const char *cursor = path;
    if (strip_count <= 0) {
        return strdup(path);
    }
    while (*cursor == '/') {
        cursor++;
    }
    while (strip_count > 0 && *cursor) {
        while (*cursor && *cursor != '/') {
            cursor++;
        }
        while (*cursor == '/') {
            cursor++;
        }
        strip_count--;
    }
    return strdup(*cursor ? cursor : path);
}

static int read_file_lines(const char *path, LineVec *vec) {
    FILE *fp = fopen(path, "r");
    char *line = NULL;
    size_t cap = 0;
    ssize_t nread;

    if (!fp) {
        fprintf(stderr, "patch: cannot open %s: %s\n", path, strerror(errno));
        return -1;
    }
    while ((nread = getline(&line, &cap, fp)) != -1) {
        if (line_vec_push(vec, line) != 0) {
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

static int write_file_lines(const char *path, const LineVec *vec) {
    FILE *fp = fopen(path, "w");
    if (!fp) {
        fprintf(stderr, "patch: cannot write %s: %s\n", path, strerror(errno));
        return -1;
    }
    for (size_t i = 0; i < vec->len; i++) {
        fputs(vec->items[i], fp);
    }
    fclose(fp);
    return 0;
}

static int append_line(LineVec *vec, const char *line) {
    if (line_vec_push(vec, line) != 0) {
        fprintf(stderr, "patch: out of memory\n");
        return -1;
    }
    return 0;
}

static int apply_file_patch(const FilePatch *patch, int strip_count) {
    char *path = NULL;
    LineVec old_lines = {0};
    LineVec new_lines = {0};
    size_t cursor = 0;

    if (patch->hunk_count == 0) {
        return 0;
    }

    path = strip_components(
        strcmp(patch->old_path, "/dev/null") == 0 ? patch->new_path : patch->old_path,
        strip_count
    );
    if (!path) {
        return -1;
    }

    if (strcmp(patch->old_path, "/dev/null") != 0 && read_file_lines(path, &old_lines) != 0) {
        free(path);
        return -1;
    }

    for (size_t h = 0; h < patch->hunk_count; h++) {
        const Hunk *hunk = &patch->hunks[h];
        size_t target = hunk->old_start > 0 ? (size_t)(hunk->old_start - 1) : 0;

        while (cursor < target && cursor < old_lines.len) {
            if (append_line(&new_lines, old_lines.items[cursor++]) != 0) {
                free(path);
                line_vec_free(&old_lines);
                line_vec_free(&new_lines);
                return -1;
            }
        }

        for (size_t i = 0; i < hunk->line_count; i++) {
            const HunkLine *line = &hunk->lines[i];
            if (line->kind == ' ' || line->kind == '-') {
                if (cursor >= old_lines.len || strcmp(old_lines.items[cursor], line->text) != 0) {
                    fprintf(stderr, "patch: hunk %zu failed for %s\n", h + 1, path);
                    free(path);
                    line_vec_free(&old_lines);
                    line_vec_free(&new_lines);
                    return -1;
                }
            }
            if (line->kind == ' ') {
                if (append_line(&new_lines, old_lines.items[cursor]) != 0) {
                    free(path);
                    line_vec_free(&old_lines);
                    line_vec_free(&new_lines);
                    return -1;
                }
                cursor++;
            } else if (line->kind == '-') {
                cursor++;
            } else if (line->kind == '+') {
                if (append_line(&new_lines, line->text) != 0) {
                    free(path);
                    line_vec_free(&old_lines);
                    line_vec_free(&new_lines);
                    return -1;
                }
            }
        }
        printf("patch: applied hunk %zu to %s\n", h + 1, path);
    }

    while (cursor < old_lines.len) {
        if (append_line(&new_lines, old_lines.items[cursor++]) != 0) {
            free(path);
            line_vec_free(&old_lines);
            line_vec_free(&new_lines);
            return -1;
        }
    }

    if (write_file_lines(path, &new_lines) != 0) {
        free(path);
        line_vec_free(&old_lines);
        line_vec_free(&new_lines);
        return -1;
    }

    free(path);
    line_vec_free(&old_lines);
    line_vec_free(&new_lines);
    return 0;
}

static void usage(void) {
    fputs("usage: patch [-pN]\n", stderr);
}

int main(int argc, char **argv) {
    LineVec patch_lines = {0};
    FilePatch *patches = NULL;
    size_t patch_count = 0;
    int strip_count = 0;
    int status = 0;

    if (argc == 2 && strncmp(argv[1], "-p", 2) == 0) {
        strip_count = atoi(argv[1] + 2);
    } else if (argc != 1) {
        usage();
        return 1;
    }

    if (read_patch_lines(&patch_lines) != 0) {
        line_vec_free(&patch_lines);
        return 1;
    }
    if (parse_patch(&patch_lines, &patches, &patch_count) != 0) {
        fprintf(stderr, "patch: failed to parse patch\n");
        line_vec_free(&patch_lines);
        return 1;
    }

    for (size_t i = 0; i < patch_count; i++) {
        if (apply_file_patch(&patches[i], strip_count) != 0) {
            status = 1;
        }
        file_patch_free(&patches[i]);
    }

    free(patches);
    line_vec_free(&patch_lines);
    return status;
}
