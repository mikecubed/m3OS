/* whoami — print effective username (Phase 27) */
#include <unistd.h>
#include <fcntl.h>
#include <string.h>

static size_t safe_strlen(const char *s, size_t maxlen) {
    size_t i = 0;
    while (i < maxlen && s[i]) i++;
    return i;
}

int main(void) {
    unsigned euid = geteuid();

    /* Look up username in /data/etc/passwd */
    int fd = open("/data/etc/passwd", O_RDONLY);
    if (fd < 0) goto numeric;

    char buf[2048];
    int len = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (len <= 0) goto numeric;
    buf[len] = 0;

    char *line = buf;
    while (line && *line) {
        char *nl = strchr(line, '\n');
        if (nl) *nl = 0;

        /* Find username (field 0) and uid (field 2) */
        char *name_start = line;
        char *p = strchr(line, ':');
        if (!p) goto next;
        *p = 0;
        p++; /* skip 'x' field */
        p = strchr(p, ':');
        if (!p) goto next;
        p++;
        /* p now points to uid field */
        unsigned uid = 0;
        while (*p && *p != ':') { uid = uid * 10 + (*p - '0'); p++; }

        if (uid == euid) {
            size_t nlen = safe_strlen(name_start, 64);
            write(1, name_start, nlen);
            write(1, "\n", 1);
            return 0;
        }
next:
        line = nl ? nl + 1 : 0;
    }

numeric:;
    /* Fall back to printing numeric UID */
    char nbuf[12]; int i = 11;
    nbuf[i] = '\n';
    if (euid == 0) { write(1, "0\n", 2); return 0; }
    unsigned n = euid;
    while (n > 0) { nbuf[--i] = '0' + (n % 10); n /= 10; }
    write(1, &nbuf[i], 12 - i);
    return 0;
}
