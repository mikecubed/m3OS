#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef enum {
    CMD_SUBST,
    CMD_PRINT_RANGE,
    CMD_DELETE_RANGE,
} SedCommandType;

typedef struct {
    SedCommandType type;
    int global;
    long start;
    long end;
    char *old_text;
    char *new_text;
} SedCommand;

static void usage(void) {
    fputs("usage: sed [-n] SCRIPT [file...]\n", stderr);
}

static int parse_range(const char *script, long *start, long *end, char expected_op) {
    char *mid;
    char *endptr;

    if (!script[0]) {
        return -1;
    }

    *start = strtol(script, &endptr, 10);
    if (endptr == script || *start <= 0) {
        return -1;
    }

    if (*endptr == expected_op && endptr[1] == '\0') {
        *end = *start;
        return 0;
    }

    if (*endptr != ',') {
        return -1;
    }
    mid = endptr + 1;
    *end = strtol(mid, &endptr, 10);
    if (endptr == mid || *end < *start || *endptr != expected_op || endptr[1] != '\0') {
        return -1;
    }

    return 0;
}

static char *copy_span(const char *start, const char *end) {
    size_t len = (size_t)(end - start);
    char *copy = malloc(len + 1);
    if (!copy) {
        return NULL;
    }
    memcpy(copy, start, len);
    copy[len] = '\0';
    return copy;
}

static int parse_subst(const char *script, SedCommand *cmd) {
    char delim;
    const char *old_start;
    const char *old_end;
    const char *new_start;
    const char *new_end;

    if (script[0] != 's' || script[1] == '\0') {
        return -1;
    }

    delim = script[1];
    old_start = script + 2;
    old_end = strchr(old_start, delim);
    if (!old_end || old_end == old_start) {
        return -1;
    }

    new_start = old_end + 1;
    new_end = strchr(new_start, delim);
    if (!new_end) {
        return -1;
    }

    cmd->old_text = copy_span(old_start, old_end);
    cmd->new_text = copy_span(new_start, new_end);
    if (!cmd->old_text || !cmd->new_text) {
        return -1;
    }

    cmd->type = CMD_SUBST;
    cmd->global = 0;
    if (new_end[1] == 'g' && new_end[2] == '\0') {
        cmd->global = 1;
    } else if (new_end[1] != '\0') {
        return -1;
    }

    return 0;
}

static int parse_script(const char *script, SedCommand *cmd) {
    memset(cmd, 0, sizeof(*cmd));

    if (script[0] == 's') {
        return parse_subst(script, cmd);
    }
    if (parse_range(script, &cmd->start, &cmd->end, 'p') == 0) {
        cmd->type = CMD_PRINT_RANGE;
        return 0;
    }
    if (parse_range(script, &cmd->start, &cmd->end, 'd') == 0) {
        cmd->type = CMD_DELETE_RANGE;
        return 0;
    }

    return -1;
}

static int append_bytes(char **buf, size_t *len, size_t *cap, const char *data, size_t data_len) {
    size_t needed = *len + data_len + 1;
    if (needed > *cap) {
        size_t new_cap = *cap ? *cap * 2 : 64;
        while (new_cap < needed) {
            new_cap *= 2;
        }
        char *new_buf = realloc(*buf, new_cap);
        if (!new_buf) {
            return -1;
        }
        *buf = new_buf;
        *cap = new_cap;
    }
    memcpy(*buf + *len, data, data_len);
    *len += data_len;
    (*buf)[*len] = '\0';
    return 0;
}

static int print_substituted_line(const char *line, const SedCommand *cmd) {
    const char *cursor = line;
    const char *match;
    size_t old_len = strlen(cmd->old_text);
    size_t new_len = strlen(cmd->new_text);
    char *out = NULL;
    size_t out_len = 0;
    size_t out_cap = 0;
    int status = 0;

    while ((match = strstr(cursor, cmd->old_text)) != NULL) {
        if (append_bytes(&out, &out_len, &out_cap, cursor, (size_t)(match - cursor)) != 0
            || append_bytes(&out, &out_len, &out_cap, cmd->new_text, new_len) != 0) {
            status = -1;
            goto done;
        }
        cursor = match + old_len;
        if (!cmd->global) {
            break;
        }
    }

    if (append_bytes(&out, &out_len, &out_cap, cursor, strlen(cursor)) != 0) {
        status = -1;
        goto done;
    }

    fputs(out, stdout);

done:
    free(out);
    return status;
}

static int line_in_range(long line_no, long start, long end) {
    return line_no >= start && line_no <= end;
}

static int process_stream(FILE *fp, const SedCommand *cmd, int quiet) {
    char *line = NULL;
    size_t cap = 0;
    ssize_t len;
    long line_no = 0;

    while ((len = getline(&line, &cap, fp)) >= 0) {
        (void)len;
        line_no++;
        switch (cmd->type) {
            case CMD_SUBST:
                if (!quiet && print_substituted_line(line, cmd) != 0) {
                    free(line);
                    return -1;
                }
                break;
            case CMD_PRINT_RANGE:
                if (!quiet) {
                    fputs(line, stdout);
                }
                if (line_in_range(line_no, cmd->start, cmd->end)) {
                    fputs(line, stdout);
                }
                break;
            case CMD_DELETE_RANGE:
                if (!line_in_range(line_no, cmd->start, cmd->end) && !quiet) {
                    fputs(line, stdout);
                }
                break;
        }
    }

    free(line);
    return ferror(fp) || ferror(stdout) ? -1 : 0;
}

int main(int argc, char **argv) {
    SedCommand cmd;
    int quiet = 0;
    int argi = 1;
    int status = 0;

    if (argi < argc && strcmp(argv[argi], "-n") == 0) {
        quiet = 1;
        argi++;
    }

    if (argi >= argc || parse_script(argv[argi], &cmd) != 0) {
        usage();
        free(cmd.old_text);
        free(cmd.new_text);
        return 1;
    }
    argi++;

    if (argi == argc) {
        status = process_stream(stdin, &cmd, quiet);
    } else {
        for (; argi < argc; argi++) {
            FILE *fp = fopen(argv[argi], "r");
            if (!fp) {
                fprintf(stderr, "sed: cannot open '%s': %s\n", argv[argi], strerror(errno));
                status = 1;
                continue;
            }
            if (process_stream(fp, &cmd, quiet) != 0) {
                fprintf(stderr, "sed: read error on '%s'\n", argv[argi]);
                status = 1;
            }
            fclose(fp);
        }
    }

    free(cmd.old_text);
    free(cmd.new_text);
    return status != 0;
}
