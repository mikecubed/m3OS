/* ls — list directory entries (uses getdents64) */
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <sys/syscall.h>

struct linux_dirent64 {
    unsigned long long d_ino;
    long long d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

int main(int argc, char **argv) {
    const char *path = (argc > 1) ? argv[1] : "/tmp";
    int fd = open(path, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        /* Failed to open directory: report error and exit. */
        const char *msg = "ls: cannot open directory\n";
        write(2, msg, strlen(msg));
        return 1;
    }

    char buf[1024];
    long nread;
    int ret = 0;
    while ((nread = syscall(SYS_getdents64, fd, buf, sizeof(buf))) > 0) {
        long pos = 0;
        while (pos < nread) {
            struct linux_dirent64 *d = (struct linux_dirent64 *)(buf + pos);
            write(1, d->d_name, strlen(d->d_name));
            write(1, "\n", 1);
            pos += d->d_reclen;
        }
    }
    if (nread < 0) {
        const char *msg = "ls: getdents64 error\n";
        write(2, msg, strlen(msg));
        ret = 1;
    }
    close(fd);
    return ret;
}
