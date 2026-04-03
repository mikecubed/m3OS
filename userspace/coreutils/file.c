#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

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
    int fd;
    unsigned char buf[256];
    ssize_t nread;
    const char *kind;

    fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0) {
        if (errno == ELOOP) {
            printf("%s: symbolic link\n", path);
            return 0;
        }
        fprintf(stderr, "file: cannot open '%s': %s\n", path, strerror(errno));
        return 1;
    }

    if (fstat(fd, &st) != 0) {
        fprintf(stderr, "file: cannot stat '%s': %s\n", path, strerror(errno));
        close(fd);
        return 1;
    }

    if (S_ISCHR(st.st_mode)) {
        printf("%s: character special\n", path);
        close(fd);
        return 0;
    }
    if (S_ISDIR(st.st_mode)) {
        printf("%s: directory\n", path);
        close(fd);
        return 0;
    }

    nread = read(fd, buf, sizeof(buf));
    if (nread < 0) {
        fprintf(stderr, "file: cannot read '%s'\n", path);
        close(fd);
        return 1;
    }
    close(fd);

    kind = describe_magic(buf, (size_t)nread);
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
