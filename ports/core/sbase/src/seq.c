/* seq - print a sequence of numbers */
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char *argv[]) {
    long first = 1, inc = 1, last;
    if (argc == 2) {
        last = atol(argv[1]);
    } else if (argc == 3) {
        first = atol(argv[1]);
        last = atol(argv[2]);
    } else if (argc == 4) {
        first = atol(argv[1]);
        inc = atol(argv[2]);
        last = atol(argv[3]);
    } else {
        fprintf(stderr, "usage: seq [first [increment]] last\n");
        return 1;
    }
    if (inc == 0) return 1;
    if (inc > 0) {
        for (long i = first; i <= last; i += inc)
            printf("%ld\n", i);
    } else {
        for (long i = first; i >= last; i += inc)
            printf("%ld\n", i);
    }
    return 0;
}
