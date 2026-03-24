/* env — print environment variables */
#include <unistd.h>
#include <string.h>

extern char **environ;

int main(void) {
    if (environ) {
        for (char **e = environ; *e; e++) {
            write(1, *e, strlen(*e));
            write(1, "\n", 1);
        }
    }
    return 0;
}
