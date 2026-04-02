#include <dirent.h>
#include <errno.h>
#include <fnmatch.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

static const char *name_pattern = NULL;
static int type_filter = 0;
static int print0_mode = 0;
static int follow_links = 1;
static int status = 0;

static void usage(void) {
    fputs("usage: find [path] [-L] [-name PATTERN] [-type f|d] [-print0]\n", stderr);
}

static const char *base_name(const char *path) {
    const char *slash = strrchr(path, '/');
    return slash ? slash + 1 : path;
}

static void emit_path(const char *path) {
    fwrite(path, 1, strlen(path), stdout);
    fputc(print0_mode ? '\0' : '\n', stdout);
}

static int matches_path(const char *path, const struct stat *st) {
    if (name_pattern && fnmatch(name_pattern, base_name(path), 0) != 0) {
        return 0;
    }
    if (type_filter == 'f' && !S_ISREG(st->st_mode)) {
        return 0;
    }
    if (type_filter == 'd' && !S_ISDIR(st->st_mode)) {
        return 0;
    }
    return 1;
}

static void find_path(const char *path) {
    struct stat st;
    struct stat lst;
    const struct stat *view = &st;

    if (lstat(path, &lst) != 0) {
        fprintf(stderr, "find: cannot stat '%s': %s\n", path, strerror(errno));
        status = 1;
        return;
    }
    if (!follow_links || !S_ISLNK(lst.st_mode)) {
        st = lst;
    } else if (stat(path, &st) != 0) {
        st = lst;
        view = &lst;
    }

    if (matches_path(path, view)) {
        emit_path(path);
    }

    if (!S_ISDIR(view->st_mode)) {
        return;
    }

    DIR *dir = opendir(path);
    if (!dir) {
        fprintf(stderr, "find: cannot open '%s': %s\n", path, strerror(errno));
        status = 1;
        return;
    }

    for (struct dirent *ent = readdir(dir); ent != NULL; ent = readdir(dir)) {
        size_t path_len;
        size_t name_len;
        char *child;

        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) {
            continue;
        }

        path_len = strlen(path);
        name_len = strlen(ent->d_name);
        child = malloc(path_len + 1 + name_len + 1);
        if (!child) {
            fprintf(stderr, "find: out of memory\n");
            closedir(dir);
            status = 1;
            return;
        }

        memcpy(child, path, path_len);
        if (path_len == 0 || path[path_len - 1] != '/') {
            child[path_len++] = '/';
        }
        memcpy(child + path_len, ent->d_name, name_len + 1);
        find_path(child);
        free(child);
    }

    closedir(dir);
}

int main(int argc, char **argv) {
    const char *path = ".";
    int argi = 1;

    if (argi < argc && argv[argi][0] != '-') {
        path = argv[argi++];
    }

    while (argi < argc) {
        if (strcmp(argv[argi], "-L") == 0) {
            follow_links = 1;
            argi++;
        } else if (strcmp(argv[argi], "-name") == 0) {
            if (argi + 1 >= argc) {
                usage();
                return 1;
            }
            name_pattern = argv[argi + 1];
            argi += 2;
        } else if (strcmp(argv[argi], "-type") == 0) {
            if (argi + 1 >= argc || (argv[argi + 1][0] != 'f' && argv[argi + 1][0] != 'd')) {
                usage();
                return 1;
            }
            type_filter = argv[argi + 1][0];
            argi += 2;
        } else if (strcmp(argv[argi], "-print0") == 0) {
            print0_mode = 1;
            argi++;
        } else {
            usage();
            return 1;
        }
    }

    find_path(path);
    return status || ferror(stdout);
}
