/* ls — list directory entries (uses getdents64, fstatat for -l) */
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/stat.h>

struct linux_dirent64 {
    unsigned long long d_ino;
    long long d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

static void write_str(int fd, const char *s) {
    write(fd, s, strlen(s)); /* DevSkim: ignore DS140021 */
}

static void write_uint(int fd, unsigned long long v) {
    char buf[20];
    int i = 20;
    if (v == 0) { write(fd, "0", 1); return; }
    while (v > 0) { buf[--i] = '0' + (v % 10); v /= 10; }
    write(fd, buf + i, 20 - i);
}

/* Format mode bits as "drwxrwxrwx" (10 chars) */
static void format_mode(unsigned int mode, char *out) {
    unsigned int ft = mode & 0170000;
    out[0] = (ft == 0040000) ? 'd' : (ft == 0120000) ? 'l' : '-';
    out[1] = (mode & 0400) ? 'r' : '-';
    out[2] = (mode & 0200) ? 'w' : '-';
    out[3] = (mode & 0100) ? 'x' : '-';
    out[4] = (mode & 040) ? 'r' : '-';
    out[5] = (mode & 020) ? 'w' : '-';
    out[6] = (mode & 010) ? 'x' : '-';
    out[7] = (mode & 04) ? 'r' : '-';
    out[8] = (mode & 02) ? 'w' : '-';
    out[9] = (mode & 01) ? 'x' : '-';
}

/* Right-align a number in a field of `width` characters */
static void write_padded_uint(int fd, unsigned long long v, int width) {
    char buf[20];
    int i = 20;
    if (v == 0) { buf[--i] = '0'; }
    else { while (v > 0) { buf[--i] = '0' + (v % 10); v /= 10; } }
    int digits = 20 - i;
    while (digits < width) { write(fd, " ", 1); digits++; }
    write(fd, buf + i, 20 - i);
}

int main(int argc, char **argv) {
    int long_format = 0;
    const char *path = ".";

    for (int i = 1; i < argc; i++) {
        if (argv[i][0] == '-') {
            for (int j = 1; argv[i][j]; j++) {
                if (argv[i][j] == 'l') long_format = 1;
            }
        } else {
            path = argv[i];
        }
    }

    int fd = open(path, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        write_str(2, "ls: cannot open directory\n");
        return 1;
    }

    /* Build the base path for fstatat calls */
    char basepath[256];
    {
        int len = strlen(path); /* DevSkim: ignore DS140021 */
        if (len >= (int)sizeof(basepath) - 2) len = sizeof(basepath) - 2;
        memcpy(basepath, path, len);
        if (len > 0 && basepath[len - 1] != '/') basepath[len++] = '/';
        basepath[len] = '\0';
    }

    char buf[2048];
    long nread;
    int ret = 0;
    while ((nread = syscall(SYS_getdents64, fd, buf, sizeof(buf))) > 0) {
        long pos = 0;
        while (pos < nread) {
            struct linux_dirent64 *d = (struct linux_dirent64 *)(buf + pos);

            /* Skip . and .. */
            if (d->d_name[0] == '.' && (d->d_name[1] == '\0' ||
                (d->d_name[1] == '.' && d->d_name[2] == '\0'))) {
                pos += d->d_reclen;
                continue;
            }

            if (long_format) {
                /* Build full path for fstatat */
                char fullpath[512];
                int blen = strlen(basepath); /* DevSkim: ignore DS140021 */
                int nlen = strlen(d->d_name); /* DevSkim: ignore DS140021 */
                if (blen + nlen < (int)sizeof(fullpath) - 1) {
                    memcpy(fullpath, basepath, blen);
                    memcpy(fullpath + blen, d->d_name, nlen);
                    fullpath[blen + nlen] = '\0';
                } else {
                    fullpath[0] = '\0';
                }

                struct stat st;
                memset(&st, 0, sizeof(st));
                /* Use newfstatat (syscall 262) */
                long sr = syscall(SYS_newfstatat, -100 /* AT_FDCWD */, fullpath, &st, 0);
                if (sr == 0) {
                    char mode_str[10];
                    format_mode(st.st_mode, mode_str);
                    write(1, mode_str, 10);
                    write(1, " ", 1);
                    write_padded_uint(1, st.st_uid, 5);
                    write(1, " ", 1);
                    write_padded_uint(1, st.st_gid, 5);
                    write(1, " ", 1);
                    write_padded_uint(1, st.st_size, 8);
                    write(1, " ", 1);
                } else {
                    /* stat failed — print placeholder */
                    write_str(1, "?????????? ? ? ? ");
                }
            }

            write_str(1, d->d_name);
            write(1, "\n", 1);
            pos += d->d_reclen;
        }
    }
    if (nread < 0) {
        write_str(2, "ls: getdents64 error\n");
        ret = 1;
    }
    close(fd);
    return ret;
}
