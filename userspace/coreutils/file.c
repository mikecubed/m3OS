#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

static void usage(void) {
    fputs("usage: file FILE...\n", stderr);
}

static const char *describe_magic(const unsigned char *buf, size_t len) {
    size_t i;

    if (len >= 4 && buf[0] == 0x7f && buf[1] == 'E' && buf[2] == 'L' && buf[3] == 'F') {
        return "ELF 64-bit";
    }

    for (i = 0; i < len; i++) {
        if (buf[i] == '\0') {
            return "data";
        }
        if (!isprint(buf[i]) && !isspace(buf[i])) {
            return "data";
        }
    }

    return "ASCII text";
}

static int describe_path(const char *path) {
    struct stat st;
    FILE *fp;
    unsigned char buf[256];
    size_t nread;
    const char *kind;

    if (lstat(path, &st) != 0) {
        fprintf(stderr, "file: cannot stat '%s': %s\n", path, strerror(errno));
        return 1;
    }

    if (S_ISCHR(st.st_mode)) {
        printf("%s: character special\n", path);
        return 0;
    }
    if (S_ISDIR(st.st_mode)) {
        printf("%s: directory\n", path);
        return 0;
    }
    if (S_ISLNK(st.st_mode)) {
        printf("%s: symbolic link\n", path);
        return 0;
    }

    fp = fopen(path, "rb");
    if (!fp) {
        fprintf(stderr, "file: cannot open '%s': %s\n", path, strerror(errno));
        return 1;
    }
    nread = fread(buf, 1, sizeof(buf), fp);
    if (ferror(fp)) {
        fprintf(stderr, "file: cannot read '%s'\n", path);
        fclose(fp);
        return 1;
    }
    fclose(fp);

    kind = describe_magic(buf, nread);
    printf("%s: %s\n", path, kind);
    return 0;
}

int main(int argc, char **argv) {
    int status = 0;

    if (argc < 2) {
        usage();
        return 1;
    }

    for (int i = 1; i < argc; i++) {
        if (describe_path(argv[i]) != 0) {
            status = 1;
        }
    }

    return status;
}
