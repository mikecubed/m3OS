/* hello.c — Phase 12 musl integration test.
 *
 * A minimal C program compiled with musl-gcc -static.  Exercises:
 *   - C runtime startup (_start → __libc_start_main → main)
 *   - write(1, ...) via the Linux syscall ABI
 *   - malloc / free (musl heap via brk/mmap)
 *   - exit(0) via syscall 60
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    puts("hello from musl!");

    /* Exercise malloc so musl calls brk or mmap. */
    char *buf = malloc(64);
    if (buf) {
        strcpy(buf, "malloc works\n");
        fputs(buf, stdout);
        free(buf);
    }

    return 0;
}
