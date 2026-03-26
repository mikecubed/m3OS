/* env — print environment variables */
#include <unistd.h>
#include <string.h>

extern char **environ;

int main(void) {
    if (environ) {
        for (char **e = environ; *e; e++) {
            write(1, *e, strlen(*e)); /* DevSkim: ignore DS140021 — environ entries are null-terminated by the C runtime */
            write(1, "\n", 1);
        }
    }
    return 0;
}
