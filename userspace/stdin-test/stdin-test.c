/* Minimal stdin→stdout echo test for Phase 21 debugging.
 * Reads one line from stdin, writes it to stdout, exits.
 */
#include <unistd.h>

int main(void) {
    const char *prompt = "stdin-test> ";
    write(1, prompt, 12);

    char buf[256];
    ssize_t n = read(0, buf, sizeof(buf));
    if (n > 0) {
        write(1, "GOT: ", 5);
        write(1, buf, n);
    } else {
        const char *err = "stdin-test: read returned <= 0\n";
        write(1, err, 31);
    }
    return 0;
}
