/*
 * mandoc - minimal man page formatter for m3OS
 *
 * A simplified roff/man page renderer that handles basic man macros:
 * .TH (title), .SH (section heading), .PP (paragraph), .B (bold),
 * .I (italic shown as underline), .BR/.BI (mixed), .TP (tagged paragraph).
 *
 * Reads a man page source from a file or stdin and renders plain text.
 */

#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <ctype.h>

static void print_bold(const char *s) {
    /* Print text as-is (no terminal formatting in this minimal version) */
    while (*s && *s != '\n') {
        putchar(*s);
        s++;
    }
}

static void print_underline(const char *s) {
    while (*s && *s != '\n') {
        putchar(*s);
        s++;
    }
}

static void process_line(const char *line) {
    if (line[0] != '.') {
        /* Regular text — output as-is */
        fputs(line, stdout);
        return;
    }

    /* Skip the dot and get the macro name */
    const char *p = line + 1;
    char macro[8] = {0};
    int i = 0;
    while (*p && !isspace(*p) && i < 7) {
        macro[i++] = *p++;
    }
    while (*p == ' ' || *p == '\t') p++;

    if (strcmp(macro, "TH") == 0) {
        /* Title header */
        char name[64] = {0}, section[16] = {0};
        sscanf(p, "%63s %15s", name, section);
        printf("\n%s(%s)\n\n", name, section);
    } else if (strcmp(macro, "SH") == 0) {
        /* Section heading */
        printf("\n");
        print_bold(p);
        printf("\n");
    } else if (strcmp(macro, "SS") == 0) {
        /* Subsection heading */
        printf("\n  ");
        print_bold(p);
        printf("\n");
    } else if (strcmp(macro, "PP") == 0 || strcmp(macro, "P") == 0) {
        /* New paragraph */
        printf("\n");
    } else if (strcmp(macro, "B") == 0) {
        /* Bold text */
        printf("  ");
        print_bold(p);
        printf("\n");
    } else if (strcmp(macro, "I") == 0) {
        /* Italic (rendered as underlined or plain) */
        printf("  ");
        print_underline(p);
        printf("\n");
    } else if (strcmp(macro, "BR") == 0 || strcmp(macro, "BI") == 0) {
        /* Mixed bold/roman or bold/italic */
        printf("  %s", p);
    } else if (strcmp(macro, "TP") == 0) {
        /* Tagged paragraph */
        printf("\n");
    } else if (strcmp(macro, "IP") == 0) {
        /* Indented paragraph */
        printf("\n    ");
        if (*p) printf("%s", p);
    } else if (strcmp(macro, "nf") == 0) {
        /* No-fill mode (preformatted) */
    } else if (strcmp(macro, "fi") == 0) {
        /* Fill mode */
    } else if (strcmp(macro, "\\\"") == 0 || macro[0] == '\\') {
        /* Comment — skip */
    } else {
        /* Unknown macro — output the text portion */
        if (*p) printf("  %s", p);
    }
}

int main(int argc, char *argv[]) {
    FILE *fp = stdin;
    char line[1024];

    if (argc > 1) {
        if (strcmp(argv[1], "--help") == 0 || strcmp(argv[1], "-h") == 0) {
            printf("Usage: mandoc [file]\n");
            printf("Format and display man pages.\n");
            return 0;
        }
        fp = fopen(argv[1], "r");
        if (!fp) {
            fprintf(stderr, "mandoc: cannot open '%s'\n", argv[1]);
            return 1;
        }
    }

    while (fgets(line, sizeof(line), fp)) {
        process_line(line);
    }
    printf("\n");

    if (fp != stdin) fclose(fp);
    return 0;
}
