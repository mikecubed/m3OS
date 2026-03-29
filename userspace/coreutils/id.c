/* id — print user identity (Phase 27) */
#include <unistd.h>
#include <fcntl.h>
#include <string.h>

static void write_str(const char *s) { write(1, s, strlen(s)); }
static void write_u32(unsigned n) {
    char buf[12]; int i = 11;
    buf[i] = 0;
    if (n == 0) { write(1, "0", 1); return; }
    while (n > 0) { buf[--i] = '0' + (n % 10); n /= 10; }
    write(1, &buf[i], 11 - i);
}

/* Look up a name from /data/etc/passwd or /data/etc/group by numeric id */
static int lookup_name(const char *file, unsigned id, int id_field, char *out, int outlen) {
    int fd = open(file, O_RDONLY);
    if (fd < 0) return 0;
    char buf[2048];
    int len = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (len <= 0) return 0;
    buf[len] = 0;

    char *line = buf;
    while (line && *line) {
        char *nl = strchr(line, '\n');
        if (nl) *nl = 0;

        /* Parse fields separated by ':' */
        char *fields[7]; int nf = 0;
        char *p = line;
        while (nf < 7) {
            fields[nf++] = p;
            char *c = strchr(p, ':');
            if (!c) break;
            *c = 0;
            p = c + 1;
        }

        if (nf > id_field) {
            unsigned fid = 0;
            for (char *d = fields[id_field]; *d; d++)
                fid = fid * 10 + (*d - '0');
            if (fid == id) {
                int slen = strlen(fields[0]);
                if (slen >= outlen) slen = outlen - 1;
                memcpy(out, fields[0], slen);
                out[slen] = 0;
                return 1;
            }
        }

        line = nl ? nl + 1 : 0;
    }
    return 0;
}

int main(void) {
    unsigned uid = getuid();
    unsigned gid = getgid();

    char uname[64] = "";
    char gname[64] = "";
    lookup_name("/data/etc/passwd", uid, 2, uname, sizeof(uname));
    lookup_name("/data/etc/group", gid, 2, gname, sizeof(gname));

    write_str("uid=");
    write_u32(uid);
    if (uname[0]) { write_str("("); write_str(uname); write_str(")"); }
    write_str(" gid=");
    write_u32(gid);
    if (gname[0]) { write_str("("); write_str(gname); write_str(")"); }
    write_str("\n");
    return 0;
}
