/*
 * bc - arbitrary precision calculator for m3OS
 *
 * A minimal implementation supporting basic arithmetic operations.
 * Reads expressions from stdin (one per line) and prints results.
 * Supports: +, -, *, /, % on integers.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>

static long parse_number(const char **s) {
    long n = 0;
    int neg = 0;
    while (isspace(**s)) (*s)++;
    if (**s == '-') { neg = 1; (*s)++; }
    while (isdigit(**s)) {
        n = n * 10 + (**s - '0');
        (*s)++;
    }
    return neg ? -n : n;
}

static long evaluate(const char *expr) {
    const char *s = expr;
    long result = parse_number(&s);

    while (*s) {
        while (isspace(*s)) s++;
        if (*s == '\0' || *s == '\n') break;

        char op = *s++;
        long operand = parse_number(&s);

        switch (op) {
            case '+': result += operand; break;
            case '-': result -= operand; break;
            case '*': result *= operand; break;
            case '/':
                if (operand == 0) {
                    fprintf(stderr, "divide by zero\n");
                    return 0;
                }
                result /= operand;
                break;
            case '%':
                if (operand == 0) {
                    fprintf(stderr, "divide by zero\n");
                    return 0;
                }
                result %= operand;
                break;
            default:
                fprintf(stderr, "unknown operator: %c\n", op);
                return result;
        }
    }

    return result;
}

int main(int argc, char **argv) {
    char line[1024];

    if (argc > 1 && strcmp(argv[1], "-e") == 0 && argc > 2) {
        /* Evaluate expression from command line */
        printf("%ld\n", evaluate(argv[2]));
        return 0;
    }

    /* Interactive mode: read lines from stdin */
    while (fgets(line, sizeof(line), stdin)) {
        /* Skip empty lines and comments */
        if (line[0] == '\n' || line[0] == '#') continue;
        /* "quit" exits */
        if (strncmp(line, "quit", 4) == 0) break;

        printf("%ld\n", evaluate(line));
    }

    return 0;
}
