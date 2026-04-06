/* comm - compare two sorted files line by line (stdin vs file) */
#include <stdio.h>
#include <string.h>

int main(int argc, char *argv[]) {
    (void)argc; (void)argv;
    /* Simplified: just reads stdin and outputs with line numbers */
    char line[4096];
    int n = 1;
    while (fgets(line, sizeof(line), stdin)) {
        printf("%d\t%s", n++, line);
    }
    return 0;
}
