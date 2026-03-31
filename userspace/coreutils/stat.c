/* stat — display file metadata */
#include <unistd.h>
#include <sys/stat.h>
#include <string.h>
#include <time.h>

static void write_str(int fd, const char *s) {
    const char *p = s;
    while (*p) p++;
    write(fd, s, p - s);
}

static void write_num(int fd, long long n) {
    char buf[24];
    int neg = 0;
    if (n < 0) { neg = 1; n = -n; }
    int i = sizeof(buf) - 1;
    buf[i] = '\0';
    if (n == 0) { buf[--i] = '0'; }
    while (n > 0) { buf[--i] = '0' + (n % 10); n /= 10; }
    if (neg) buf[--i] = '-';
    write_str(fd, &buf[i]);
}

static void write_oct(int fd, unsigned mode) {
    char buf[8];
    int i = 7;
    buf[i] = '\0';
    for (int j = 0; j < 4; j++) {
        buf[--i] = '0' + (mode & 7);
        mode >>= 3;
    }
    write_str(fd, &buf[i]);
}

static const char *filetype(unsigned mode) {
    switch (mode & 0xF000) {
        case 0x8000: return "regular file";
        case 0x4000: return "directory";
        case 0x2000: return "character device";
        case 0x6000: return "block device";
        case 0x1000: return "FIFO";
        case 0xA000: return "symbolic link";
        case 0xC000: return "socket";
        default: return "unknown";
    }
}

int main(int argc, char **argv) {
    if (argc < 2) {
        write_str(2, "usage: stat FILE...\n");
        return 1;
    }
    int ret = 0;
    for (int i = 1; i < argc; i++) {
        struct stat st;
        if (stat(argv[i], &st) < 0) {
            write_str(2, "stat: cannot stat '");
            write_str(2, argv[i]);
            write_str(2, "'\n");
            ret = 1;
            continue;
        }
        write_str(1, "  File: ");
        write_str(1, argv[i]);
        write_str(1, "\n");
        write_str(1, "  Size: ");
        write_num(1, st.st_size);
        write_str(1, "\tBlocks: ");
        write_num(1, st.st_blocks);
        write_str(1, "\tIO Block: ");
        write_num(1, st.st_blksize);
        write_str(1, "\t");
        write_str(1, filetype(st.st_mode));
        write_str(1, "\n");
        write_str(1, "Inode: ");
        write_num(1, st.st_ino);
        write_str(1, "\tLinks: ");
        write_num(1, st.st_nlink);
        write_str(1, "\n");
        write_str(1, "Access: (0");
        write_oct(1, st.st_mode & 07777);
        write_str(1, ")\tUid: ");
        write_num(1, st.st_uid);
        write_str(1, "\tGid: ");
        write_num(1, st.st_gid);
        write_str(1, "\n");
        write_str(1, "Access: ");
        write_num(1, st.st_atime);
        write_str(1, "\n");
        write_str(1, "Modify: ");
        write_num(1, st.st_mtime);
        write_str(1, "\n");
        write_str(1, "Change: ");
        write_num(1, st.st_ctime);
        write_str(1, "\n");
    }
    return ret;
}
