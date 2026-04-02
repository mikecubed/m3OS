#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

static void usage(void) {
    fputs("usage: chown OWNER[:GROUP] FILE...\n", stderr);
}

static int parse_u32(const char *s, unsigned *out) {
    char *end = NULL;
    unsigned long value;
    if (!s[0]) {
        return -1;
    }
    value = strtoul(s, &end, 10);
    if ((end && *end) || value > 0xffffffffUL) {
        return -1;
    }
    *out = (unsigned)value;
    return 0;
}

static int is_numeric(const char *s) {
    if (!s[0]) {
        return 0;
    }
    while (*s) {
        if (!isdigit((unsigned char)*s)) {
            return 0;
        }
        s++;
    }
    return 1;
}

static int lookup_name_id(const char *path, const char *name, int id_field, unsigned *out) {
    FILE *fp = fopen(path, "r");
    char *line = NULL;
    size_t cap = 0;
    int found = -1;

    if (!fp) {
        return -1;
    }

    while (getline(&line, &cap, fp) >= 0) {
        char *fields[8] = {0};
        int field = 0;
        char *save = NULL;
        for (char *tok = strtok_r(line, ":\n", &save); tok && field < 8;
             tok = strtok_r(NULL, ":\n", &save)) {
            fields[field++] = tok;
        }
        if (field > id_field && strcmp(fields[0], name) == 0) {
            unsigned value;
            if (parse_u32(fields[id_field], &value) == 0) {
                *out = value;
                found = 0;
            }
            break;
        }
    }

    free(line);
    fclose(fp);
    return found;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        usage();
        return 1;
    }

    char spec_buf[128];
    if (strlen(argv[1]) >= sizeof(spec_buf)) {
        fputs("chown: owner spec too long\n", stderr);
        return 1;
    }
    strcpy(spec_buf, argv[1]);

    char *group_name = strchr(spec_buf, ':');
    if (group_name) {
        *group_name++ = '\0';
    }

    int status = 0;
    for (int i = 2; i < argc; i++) {
        struct stat st;
        unsigned uid;
        unsigned gid;

        if (stat(argv[i], &st) != 0) {
            fprintf(stderr, "chown: cannot stat '%s': %s\n", argv[i], strerror(errno));
            status = 1;
            continue;
        }

        uid = st.st_uid;
        gid = st.st_gid;

        if (spec_buf[0]) {
            if (is_numeric(spec_buf)) {
                if (parse_u32(spec_buf, &uid) != 0) {
                    fprintf(stderr, "chown: invalid owner '%s'\n", spec_buf);
                    status = 1;
                    continue;
                }
            } else if (lookup_name_id("/etc/passwd", spec_buf, 2, &uid) != 0) {
                fprintf(stderr, "chown: unknown owner '%s'\n", spec_buf);
                status = 1;
                continue;
            }
        }

        if (group_name && group_name[0]) {
            if (is_numeric(group_name)) {
                if (parse_u32(group_name, &gid) != 0) {
                    fprintf(stderr, "chown: invalid group '%s'\n", group_name);
                    status = 1;
                    continue;
                }
            } else if (lookup_name_id("/etc/group", group_name, 2, &gid) != 0) {
                fprintf(stderr, "chown: unknown group '%s'\n", group_name);
                status = 1;
                continue;
            }
        }

        if (chown(argv[i], (uid_t)uid, (gid_t)gid) != 0) {
            fprintf(stderr, "chown: cannot change '%s': %s\n", argv[i], strerror(errno));
            status = 1;
        }
    }

    return status;
}
