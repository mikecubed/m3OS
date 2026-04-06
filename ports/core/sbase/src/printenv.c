/* printenv - print environment variables */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern char **environ;

int main(int argc, char *argv[]) {
    if (argc > 1) {
        for (int i = 1; i < argc; i++) {
            char *val = getenv(argv[i]);
            if (val) puts(val);
        }
    } else {
        for (char **env = environ; *env; env++)
            puts(*env);
    }
    return 0;
}
