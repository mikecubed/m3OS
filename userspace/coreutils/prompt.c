/* /bin/PROMPT — ion shell prompt command.
 * Ion executes this as a command to generate the prompt string.
 * Output is used verbatim as the prompt (ANSI escapes supported).
 */
#include <unistd.h>
#include <string.h>
#include <sys/types.h>

/* Convert uid to decimal string, return length. */
static int uid_to_str(uid_t uid, char *buf, int bufsz) {
    if (uid == 0) { buf[0] = '0'; return 1; }
    char tmp[16];
    int pos = 0;
    while (uid > 0 && pos < (int)sizeof(tmp)) {
        tmp[pos++] = '0' + (uid % 10);
        uid /= 10;
    }
    if (pos > bufsz) pos = bufsz;
    for (int i = 0; i < pos; i++) buf[i] = tmp[pos - 1 - i];
    return pos;
}

int main(void) {
    /* Build prompt: \x1b[94m<user>\x1b[0m@\x1b[96mm3os\x1b[0m:<cwd>$ */
    char prompt[512];
    int len = 0;

    /* Username from $USER, fallback to uid. */
    const char *user = NULL;
    /* Read USER env var via /proc-style or getenv - use extern. */
    extern char **environ;
    if (environ) {
        for (char **e = environ; *e; e++) {
            if ((*e)[0]=='U' && (*e)[1]=='S' && (*e)[2]=='E' && (*e)[3]=='R' && (*e)[4]=='=') {
                user = *e + 5;
                break;
            }
        }
    }

    /* Light blue username. */
    memcpy(prompt + len, "\x1b[94m", 5); len += 5;
    if (user && *user) {
        int ul = strlen(user);
        if (ul > 32) ul = 32;
        memcpy(prompt + len, user, ul); len += ul;
    } else {
        /* Fallback: show uid. */
        len += uid_to_str(getuid(), prompt + len, 16);
    }
    memcpy(prompt + len, "\x1b[0m", 4); len += 4;

    /* @hostname in cyan. */
    prompt[len++] = '@';
    memcpy(prompt + len, "\x1b[96m", 5); len += 5;
    memcpy(prompt + len, "m3os", 4); len += 4;
    memcpy(prompt + len, "\x1b[0m", 4); len += 4;

    /* :cwd */
    prompt[len++] = ':';
    char cwd[256];
    if (getcwd(cwd, sizeof(cwd))) {
        int cl = strlen(cwd);
        if (cl > 128) cl = 128;
        memcpy(prompt + len, cwd, cl); len += cl;
    }

    /* # for root, $ for others. */
    if (getuid() == 0)
        memcpy(prompt + len, "# ", 2);
    else
        memcpy(prompt + len, "$ ", 2);
    len += 2;

    write(1, prompt, len);
    return 0;
}
