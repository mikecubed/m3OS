#include <errno.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    FILE *fp = fopen("/proc/kmsg", "r");
    char buf[512];

    if (!fp) {
        fprintf(stderr, "dmesg: cannot open /proc/kmsg: %s\n", strerror(errno));
        return 1;
    }

    while (fgets(buf, sizeof(buf), fp) != NULL) {
        fputs(buf, stdout);
    }
    if (ferror(fp)) {
        fprintf(stderr, "dmesg: read error\n");
        fclose(fp);
        return 1;
    }
    fclose(fp);
    return 0;
}
