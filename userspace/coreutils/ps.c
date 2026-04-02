#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

static int show_all = 0;

static int parse_status_field(const char *text, const char *label, char *out, size_t out_len) {
    const char *line = strstr(text, label);
    size_t i = 0;
    if (!line) {
        return -1;
    }
    line += strlen(label);
    while (*line == '\t' || *line == ' ') {
        line++;
    }
    while (*line && *line != '\n' && i + 1 < out_len) {
        out[i++] = *line++;
    }
    out[i] = '\0';
    return 0;
}

static int parse_uid(const char *text, uid_t *uid_out) {
    const char *line = strstr(text, "Uid:");
    if (!line) {
        return -1;
    }
    line += 4;
    while (*line == '\t' || *line == ' ') {
        line++;
    }
    *uid_out = (uid_t)strtoul(line, NULL, 10);
    return 0;
}

static void print_process(const char *pid, uid_t caller_uid) {
    char status_path[64];
    char cmdline_path[64];
    FILE *fp;
    char status_buf[1024];
    size_t nread;
    char state[32];
    char cmd[128];
    uid_t uid;

    snprintf(status_path, sizeof(status_path), "/proc/%s/status", pid);
    fp = fopen(status_path, "r");
    if (!fp) {
        return;
    }
    nread = fread(status_buf, 1, sizeof(status_buf) - 1, fp);
    fclose(fp);
    if (nread == 0) {
        return;
    }
    status_buf[nread] = '\0';

    if (parse_uid(status_buf, &uid) != 0) {
        return;
    }
    if (!show_all && uid != caller_uid) {
        return;
    }

    if (parse_status_field(status_buf, "State:", state, sizeof(state)) != 0) {
        strcpy(state, "?");
    }

    snprintf(cmdline_path, sizeof(cmdline_path), "/proc/%s/cmdline", pid);
    fp = fopen(cmdline_path, "r");
    if (fp) {
        nread = fread(cmd, 1, sizeof(cmd) - 1, fp);
        fclose(fp);
        if (nread > 0) {
            for (size_t i = 0; i < nread; i++) {
                if (cmd[i] == '\0') {
                    cmd[i] = ' ';
                }
            }
            cmd[nread] = '\0';
        } else {
            strcpy(cmd, "?");
        }
    } else {
        strcpy(cmd, "?");
    }

    printf("%-5s %-12s %s\n", pid, state, cmd);
}

int main(int argc, char **argv) {
    DIR *dir;
    struct dirent *ent;
    uid_t caller_uid = getuid();

    if (argc == 2 && (strcmp(argv[1], "-e") == 0 || strcmp(argv[1], "-A") == 0)) {
        show_all = 1;
    } else if (argc != 1) {
        fputs("usage: ps [-e|-A]\n", stderr);
        return 1;
    }

    dir = opendir("/proc");
    if (!dir) {
        fprintf(stderr, "ps: cannot open /proc: %s\n", strerror(errno));
        return 1;
    }

    puts("PID   STATE        CMD");
    while ((ent = readdir(dir)) != NULL) {
        int numeric = 1;
        for (size_t i = 0; ent->d_name[i] != '\0'; i++) {
            if (!isdigit((unsigned char)ent->d_name[i])) {
                numeric = 0;
                break;
            }
        }
        if (numeric) {
            print_process(ent->d_name, caller_uid);
        }
    }
    closedir(dir);
    return 0;
}
