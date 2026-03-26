/* echo — print arguments to stdout */
#include <unistd.h>
#include <string.h>

int main(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        write(1, argv[i], strlen(argv[i])); /* DevSkim: ignore DS140021 — argv is null-terminated by the C runtime */
        if (i < argc - 1) write(1, " ", 1);
    }
    write(1, "\n", 1);
    return 0;
}
