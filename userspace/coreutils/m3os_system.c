/* m3os_system.c — replacement for musl's system() that uses fork+exec
 * instead of posix_spawn (which requires CLONE_VM support).
 * Link this file when building programs that use system().
 */
#include <unistd.h>
#include <sys/wait.h>
#include <errno.h>

int system(const char *cmd) {
    if (cmd == (void *)0) return 1; /* shell available */

    int pid = fork();
    if (pid < 0) return -1;

    if (pid == 0) {
        /* child */
        execl("/bin/sh", "sh", "-c", cmd, (char *)0);
        _exit(127);
    }

    /* parent */
    int status = 0;
    while (waitpid(pid, &status, 0) < 0) {
        if (errno != EINTR)
            return -1;
    }
    return status;
}
