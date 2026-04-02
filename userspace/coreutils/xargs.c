#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static void usage(void) {
    fputs("usage: xargs [-0] [-I REPLSTR] command [args...]\n", stderr);
}

static char *dup_with_replacement(const char *template, const char *needle, const char *value) {
    const char *match = strstr(template, needle);
    char *out;
    size_t prefix;
    size_t needle_len;
    size_t value_len;
    size_t suffix_len;

    if (!match) {
        out = malloc(strlen(template) + 1);
        if (!out) {
            return NULL;
        }
        strcpy(out, template);
        return out;
    }

    prefix = (size_t)(match - template);
    needle_len = strlen(needle);
    value_len = strlen(value);
    suffix_len = strlen(match + needle_len);
    out = malloc(prefix + value_len + suffix_len + 1);
    if (!out) {
        return NULL;
    }
    memcpy(out, template, prefix);
    memcpy(out + prefix, value, value_len);
    memcpy(out + prefix + value_len, match + needle_len, suffix_len + 1);
    return out;
}

static char **read_items(int null_delimited, size_t *count_out) {
    char **items = NULL;
    size_t count = 0;
    size_t cap = 0;
    int ch;
    char *buf = NULL;
    size_t len = 0;
    size_t buf_cap = 0;

    while ((ch = getchar()) != EOF) {
        if ((null_delimited && ch == '\0') || (!null_delimited && ch == '\n')) {
            if (len == 0) {
                continue;
            }
            if (count == cap) {
                size_t new_cap = cap ? cap * 2 : 16;
                char **new_items = realloc(items, new_cap * sizeof(char *));
                if (!new_items) {
                    free(buf);
                    return NULL;
                }
                items = new_items;
                cap = new_cap;
            }
            buf[len] = '\0';
            items[count++] = buf;
            buf = NULL;
            len = 0;
            buf_cap = 0;
            continue;
        }

        if (len + 1 >= buf_cap) {
            size_t new_cap = buf_cap ? buf_cap * 2 : 64;
            char *new_buf = realloc(buf, new_cap);
            if (!new_buf) {
                free(buf);
                return NULL;
            }
            buf = new_buf;
            buf_cap = new_cap;
        }
        buf[len++] = (char)ch;
    }

    if (buf && len > 0) {
        if (count == cap) {
            size_t new_cap = cap ? cap * 2 : 16;
            char **new_items = realloc(items, new_cap * sizeof(char *));
            if (!new_items) {
                free(buf);
                return NULL;
            }
            items = new_items;
            cap = new_cap;
        }
        buf[len] = '\0';
        items[count++] = buf;
    } else {
        free(buf);
    }

    *count_out = count;
    return items;
}

static int run_command(char **argv) {
    pid_t pid = fork();
    int wstatus;

    if (pid < 0) {
        fprintf(stderr, "xargs: fork failed: %s\n", strerror(errno));
        return 1;
    }
    if (pid == 0) {
        execvp(argv[0], argv);
        fprintf(stderr, "xargs: exec failed for '%s': %s\n", argv[0], strerror(errno));
        _exit(127);
    }
    if (waitpid(pid, &wstatus, 0) < 0) {
        fprintf(stderr, "xargs: waitpid failed: %s\n", strerror(errno));
        return 1;
    }
    if (!WIFEXITED(wstatus) || WEXITSTATUS(wstatus) != 0) {
        return 1;
    }
    return 0;
}

int main(int argc, char **argv) {
    int null_delimited = 0;
    const char *replacement = NULL;
    int argi = 1;
    int status = 0;
    size_t count = 0;
    char **items;

    while (argi < argc && argv[argi][0] == '-' && argv[argi][1]) {
        if (strcmp(argv[argi], "-0") == 0) {
            null_delimited = 1;
            argi++;
        } else if (strcmp(argv[argi], "-I") == 0) {
            if (argi + 1 >= argc) {
                usage();
                return 1;
            }
            replacement = argv[argi + 1];
            argi += 2;
        } else {
            usage();
            return 1;
        }
    }

    if (argi >= argc) {
        usage();
        return 1;
    }

    items = read_items(null_delimited, &count);
    if (!items && ferror(stdin)) {
        fprintf(stderr, "xargs: read error\n");
        return 1;
    }
    if (!items || count == 0) {
        free(items);
        return 0;
    }

    if (replacement) {
        for (size_t item_idx = 0; item_idx < count; item_idx++) {
            int cmd_argc = argc - argi;
            char **cmd_argv = calloc((size_t)cmd_argc + 1, sizeof(char *));
            if (!cmd_argv) {
                status = 1;
                break;
            }
            for (int i = 0; i < cmd_argc; i++) {
                cmd_argv[i] = dup_with_replacement(argv[argi + i], replacement, items[item_idx]);
                if (!cmd_argv[i]) {
                    status = 1;
                    break;
                }
            }
            if (status == 0) {
                status |= run_command(cmd_argv);
            }
            for (int i = 0; i < cmd_argc; i++) {
                free(cmd_argv[i]);
            }
            free(cmd_argv);
            if (status != 0) {
                break;
            }
        }
    } else {
        int cmd_argc = argc - argi;
        char **cmd_argv = calloc((size_t)cmd_argc + count + 1, sizeof(char *));
        if (!cmd_argv) {
            status = 1;
        } else {
            for (int i = 0; i < cmd_argc; i++) {
                cmd_argv[i] = argv[argi + i];
            }
            for (size_t item_idx = 0; item_idx < count; item_idx++) {
                cmd_argv[cmd_argc + item_idx] = items[item_idx];
            }
            status = run_command(cmd_argv);
            free(cmd_argv);
        }
    }

    for (size_t item_idx = 0; item_idx < count; item_idx++) {
        free(items[item_idx]);
    }
    free(items);
    return status;
}
