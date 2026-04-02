#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    int sig;
    const char *name;
} SignalName;

static const SignalName signal_names[] = {
    {1, "HUP"},   {2, "INT"},   {9, "KILL"}, {15, "TERM"},
    {17, "CHLD"}, {18, "CONT"}, {19, "STOP"}, {20, "TSTP"},
};

static void usage(void) {
    fputs("usage: kill [-SIGNAL] PID...\n", stderr);
}

static void list_signals(void) {
    for (size_t i = 0; i < sizeof(signal_names) / sizeof(signal_names[0]); i++) {
        if (i > 0) {
            putchar(' ');
        }
        printf("%s", signal_names[i].name);
    }
    putchar('\n');
}

int main(int argc, char **argv) {
    int sig = SIGTERM;
    int argi = 1;
    int status = 0;

    if (argc < 2) {
        usage();
        return 1;
    }
    if (strcmp(argv[1], "-l") == 0) {
        list_signals();
        return 0;
    }
    if (argv[1][0] == '-' && argv[1][1]) {
        sig = atoi(argv[1] + 1);
        if (sig <= 0) {
            usage();
            return 1;
        }
        argi++;
    }
    if (argi >= argc) {
        usage();
        return 1;
    }

    for (; argi < argc; argi++) {
        int pid = atoi(argv[argi]);
        if (pid <= 0 || kill(pid, sig) != 0) {
            fprintf(stderr, "kill: %s\n", strerror(errno));
            status = 1;
        }
    }
    return status;
}
