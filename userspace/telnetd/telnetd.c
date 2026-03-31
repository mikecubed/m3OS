/* telnetd — Telnet server for m3OS (Phase 30).
 *
 * Listens on TCP port 23 (configurable), accepts connections, allocates a
 * PTY pair per session, and relays data between the TCP socket and the
 * terminal via a forked login process.
 */

#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <stdlib.h>
#include <errno.h>

/* ------------------------------------------------------------------ */
/* Telnet protocol constants                                          */
/* ------------------------------------------------------------------ */

#define TEL_IAC   255
#define TEL_DONT  254
#define TEL_DO    253
#define TEL_WONT  252
#define TEL_WILL  251
#define TEL_SB    250
#define TEL_SE    240
#define TEL_NOP   241
#define TEL_GA    249

/* Option codes */
#define TELOPT_ECHO  1
#define TELOPT_SGA   3
#define TELOPT_NAWS  31

/* ioctl requests (must match kernel values) */
#define TIOCGPTN   0x80045430
#define TIOCSPTLCK 0x40045431
#define TIOCSCTTY  0x540E
#define TIOCSWINSZ 0x5414

/* ------------------------------------------------------------------ */
/* IAC parser state machine                                           */
/* ------------------------------------------------------------------ */

enum iac_state {
    IAC_NORMAL,
    IAC_SEEN,
    IAC_OPTION,
    IAC_SUBNEG,
    IAC_SUBNEG_IAC,
};

struct telnet_state {
    enum iac_state state;
    unsigned char  subneg_opt;
    unsigned char  subneg_buf[16];
    int            subneg_len;
    int            naws_received;
    unsigned short naws_cols;
    unsigned short naws_rows;
};

static void telnet_state_init(struct telnet_state *ts) {
    memset(ts, 0, sizeof(*ts));
    ts->state = IAC_NORMAL;
}

/* Parse raw TCP data, strip IAC sequences, write clean data to out_buf.
 * Returns the number of clean bytes written to out_buf. */
static int telnet_parse(const unsigned char *buf, int len,
                        unsigned char *out_buf, int out_size,
                        struct telnet_state *ts) {
    int out_len = 0;
    for (int i = 0; i < len; i++) {
        unsigned char c = buf[i];
        switch (ts->state) {
        case IAC_NORMAL:
            if (c == TEL_IAC) {
                ts->state = IAC_SEEN;
            } else {
                if (out_len < out_size)
                    out_buf[out_len++] = c;
            }
            break;
        case IAC_SEEN:
            if (c == TEL_IAC) {
                /* Escaped 0xFF — literal byte */
                if (out_len < out_size)
                    out_buf[out_len++] = 0xFF;
                ts->state = IAC_NORMAL;
            } else if (c == TEL_WILL || c == TEL_WONT ||
                       c == TEL_DO   || c == TEL_DONT) {
                ts->state = IAC_OPTION;
            } else if (c == TEL_SB) {
                ts->state = IAC_SUBNEG;
                ts->subneg_len = 0;
                ts->subneg_opt = 0;
            } else {
                /* NOP, GA, or other command — ignore */
                ts->state = IAC_NORMAL;
            }
            break;
        case IAC_OPTION:
            /* We received WILL/WONT/DO/DONT <option> — just consume. */
            ts->state = IAC_NORMAL;
            break;
        case IAC_SUBNEG:
            if (c == TEL_IAC) {
                ts->state = IAC_SUBNEG_IAC;
            } else {
                if (ts->subneg_len == 0) {
                    ts->subneg_opt = c;
                }
                if (ts->subneg_len < (int)sizeof(ts->subneg_buf))
                    ts->subneg_buf[ts->subneg_len++] = c;
            }
            break;
        case IAC_SUBNEG_IAC:
            if (c == TEL_SE) {
                /* End of subnegotiation — process it */
                if (ts->subneg_opt == TELOPT_NAWS && ts->subneg_len >= 5) {
                    ts->naws_cols = ((unsigned short)ts->subneg_buf[1] << 8) |
                                     ts->subneg_buf[2];
                    ts->naws_rows = ((unsigned short)ts->subneg_buf[3] << 8) |
                                     ts->subneg_buf[4];
                    ts->naws_received = 1;
                }
                ts->state = IAC_NORMAL;
            } else if (c == TEL_IAC) {
                /* IAC IAC inside subnegotiation = literal 0xFF byte */
                if (ts->subneg_len < (int)sizeof(ts->subneg_buf))
                    ts->subneg_buf[ts->subneg_len++] = 0xFF;
                ts->state = IAC_SUBNEG;
            } else {
                /* Other IAC command inside subneg — ignore and continue */
                ts->state = IAC_SUBNEG;
            }
            break;
        }
    }
    return out_len;
}

/* ------------------------------------------------------------------ */
/* Helpers                                                            */
/* ------------------------------------------------------------------ */

static int write_all(int fd, const unsigned char *buf, int len) {
    int off = 0;
    while (off < len) {
        ssize_t w = write(fd, buf + off, len - off);
        if (w < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (w == 0)
            return -1;
        off += (int)w;
    }
    return 0;
}

static void write_str(int fd, const char *s) {
    int len = 0;
    while (s[len]) len++;
    write(fd, s, len);
}

/* ------------------------------------------------------------------ */
/* Telnet option negotiation                                          */
/* ------------------------------------------------------------------ */

static void telnet_send_option(int fd, unsigned char cmd, unsigned char opt) {
    unsigned char buf[3] = { TEL_IAC, cmd, opt };
    write_all(fd, buf, 3);
}

static void telnet_negotiate(int fd) {
    telnet_send_option(fd, TEL_WILL, TELOPT_ECHO);
    telnet_send_option(fd, TEL_WILL, TELOPT_SGA);
    telnet_send_option(fd, TEL_DO,   TELOPT_SGA);
    telnet_send_option(fd, TEL_DO,   TELOPT_NAWS);
}

/* Build a "/dev/pts/N\0" path from a PTY number. */
static void pts_path(unsigned int n, char *buf, int buflen) {
    const char *prefix = "/dev/pts/";
    int plen = 9;
    if (buflen <= plen + 1)
        return;
    for (int i = 0; i < plen; i++)
        buf[i] = prefix[i];
    /* Convert n to decimal */
    char tmp[8];
    int tlen = 0;
    if (n == 0) {
        tmp[tlen++] = '0';
    } else {
        while (n > 0 && tlen < 8) {
            tmp[tlen++] = '0' + (n % 10);
            n /= 10;
        }
    }
    /* Clamp digits to available space */
    int avail = buflen - plen - 1;
    if (tlen > avail)
        tlen = avail;
    for (int i = 0; i < tlen; i++)
        buf[plen + i] = tmp[tlen - 1 - i];
    buf[plen + tlen] = '\0';
}

/* CR/LF translation: socket → PTY (NVT → Unix).
 * CR NUL → CR, CR LF → LF, bare CR → CR. */
static int crlf_to_unix(const unsigned char *in, int inlen,
                         unsigned char *out, int outsize) {
    int olen = 0;
    for (int i = 0; i < inlen && olen < outsize; i++) {
        if (in[i] == '\r') {
            if (i + 1 < inlen) {
                if (in[i + 1] == '\n') {
                    out[olen++] = '\n';
                    i++; /* skip LF */
                } else if (in[i + 1] == '\0') {
                    out[olen++] = '\r';
                    i++; /* skip NUL */
                } else {
                    out[olen++] = '\r';
                }
            } else {
                out[olen++] = '\r';
            }
        } else {
            out[olen++] = in[i];
        }
    }
    return olen;
}

/* CR/LF translation: PTY → socket (Unix → NVT).
 * Bare LF → CR LF. Also escape 0xFF as IAC IAC. */
static int unix_to_crlf(const unsigned char *in, int inlen,
                         unsigned char *out, int outsize) {
    int olen = 0;
    for (int i = 0; i < inlen; i++) {
        if (in[i] == '\n') {
            if (olen + 2 <= outsize) {
                out[olen++] = '\r';
                out[olen++] = '\n';
            }
        } else if (in[i] == 0xFF) {
            /* IAC IAC escaping */
            if (olen + 2 <= outsize) {
                out[olen++] = TEL_IAC;
                out[olen++] = TEL_IAC;
            }
        } else {
            if (olen < outsize)
                out[olen++] = in[i];
        }
    }
    return olen;
}

/* ------------------------------------------------------------------ */
/* Connection handler (runs in child process)                         */
/* ------------------------------------------------------------------ */

static void handle_connection(int client_fd) {
    /* DEBUG: poll on socket + PTY master together */
    {
        int mfd = open("/dev/ptmx", O_RDWR);
        if (mfd < 0) { write_str(2, "telnetd: ptmx open failed\n"); _exit(1); }
        int unlock = 0;
        ioctl(mfd, TIOCSPTLCK, &unlock);

        unsigned char buf[256];
        const char *hello = "ECHO-2FD> ";
        write(client_fd, hello, 10);

        struct pollfd pfds[2];
        pfds[0].fd = client_fd;
        pfds[0].events = POLLIN;
        pfds[1].fd = mfd;
        pfds[1].events = POLLIN;
        for (;;) {
            pfds[0].revents = 0;
            pfds[1].revents = 0;
            int r = poll(pfds, 2, -1);
            if (r <= 0) {
                write_str(2, "telnetd: 2fd poll failed\n");
                break;
            }
            if (pfds[0].revents & POLLIN) {
                ssize_t n = read(client_fd, buf, sizeof(buf));
                if (n <= 0) break;
                write(client_fd, buf, n);
            }
            if (pfds[0].revents & 0x010) break;
            if (pfds[1].revents & 0x010) {
                write_str(2, "telnetd: pty POLLHUP\n");
                break;
            }
        }
        close(mfd);
        close(client_fd);
        _exit(0);
    }

    /* telnet_negotiate(client_fd); */

    /* Allocate PTY pair */
    int master_fd = open("/dev/ptmx", O_RDWR);
    if (master_fd < 0) {
        write_str(client_fd, "telnetd: PTY allocation failed\r\n");
        close(client_fd);
        _exit(1);
    }

    /* Unlock the PTY slave */
    int unlock = 0;
    ioctl(master_fd, TIOCSPTLCK, &unlock);

    /* Get PTY number */
    unsigned int pty_num = 0;
    ioctl(master_fd, TIOCGPTN, &pty_num);

    /* Build slave path */
    char slave_path[32];
    pts_path(pty_num, slave_path, sizeof(slave_path));

    /* Fork grandchild for login session */
    int child_pid = fork();
    if (child_pid < 0) {
        write_str(client_fd, "telnetd: fork failed\r\n");
        close(master_fd);
        close(client_fd);
        _exit(1);
    }

    if (child_pid == 0) {
        /* Grandchild: become session leader, set up PTY slave as
         * controlling terminal, redirect stdio, exec login. */
        close(master_fd);
        close(client_fd);

        setsid();

        int slave_fd = open(slave_path, O_RDWR);
        if (slave_fd < 0)
            _exit(1);

        /* Set controlling terminal */
        ioctl(slave_fd, TIOCSCTTY, 0);

        /* Redirect stdin/stdout/stderr */
        dup2(slave_fd, 0);
        dup2(slave_fd, 1);
        dup2(slave_fd, 2);
        if (slave_fd > 2)
            close(slave_fd);

        /* exec login */
        char *login_argv[] = { "/bin/login", NULL };
        char *login_envp[] = {
            "PATH=/bin:/sbin:/usr/bin",
            "HOME=/",
            "TERM=xterm",
            "EDITOR=/bin/edit",
            NULL
        };
        execve("/bin/login", login_argv, login_envp);
        _exit(1);
    }

    /* Child (relay process): relay between socket and PTY master. */
    struct telnet_state ts;
    telnet_state_init(&ts);

    struct pollfd pfds[2];
    pfds[0].fd = client_fd;
    pfds[0].events = POLLIN;
    pfds[1].fd = master_fd;
    pfds[1].events = POLLIN;

    unsigned char rbuf[512];
    unsigned char wbuf[1024];

    for (;;) {
        int ret = poll(pfds, 2, -1);
        if (ret <= 0)
            continue;

        /* Apply NAWS if received */
        if (ts.naws_received) {
            ts.naws_received = 0;
            struct { unsigned short rows, cols, xpixel, ypixel; } ws;
            memset(&ws, 0, sizeof(ws));
            ws.rows = ts.naws_rows;
            ws.cols = ts.naws_cols;
            ioctl(master_fd, TIOCSWINSZ, &ws);
        }

        /* Socket → PTY master */
        if (pfds[0].revents & POLLIN) {
            ssize_t n = read(client_fd, rbuf, sizeof(rbuf));
            if (n < 0) {
                if (errno == EINTR) continue;
                break;
            }
            if (n == 0)
                break; /* Client disconnected */
            /* Strip IAC sequences */
            unsigned char clean[512];
            int clen = telnet_parse(rbuf, (int)n, clean, sizeof(clean), &ts);
            if (clen > 0) {
                /* CR/LF translation: NVT → Unix */
                unsigned char unix_buf[512];
                int ulen = crlf_to_unix(clean, clen, unix_buf, sizeof(unix_buf));
                if (ulen > 0 && write_all(master_fd, unix_buf, ulen) < 0)
                    break;
            }
            /* Check NAWS again after parse */
            if (ts.naws_received) {
                ts.naws_received = 0;
                struct { unsigned short rows, cols, xpixel, ypixel; } ws;
                memset(&ws, 0, sizeof(ws));
                ws.rows = ts.naws_rows;
                ws.cols = ts.naws_cols;
                ioctl(master_fd, TIOCSWINSZ, &ws);
            }
        }

        /* PTY master → Socket */
        if (pfds[1].revents & POLLIN) {
            ssize_t n = read(master_fd, rbuf, sizeof(rbuf));
            if (n < 0) {
                if (errno == EINTR) continue;
                break;
            }
            if (n == 0)
                break; /* Shell exited */
            /* CR/LF translation: Unix → NVT + IAC escaping */
            int wlen = unix_to_crlf(rbuf, (int)n, wbuf, sizeof(wbuf));
            if (wlen > 0 && write_all(client_fd, wbuf, wlen) < 0)
                break;
        }

        /* Socket closed */
        if (pfds[0].revents & POLLHUP)
            break;

        /* PTY closed (shell exited) */
        if (pfds[1].revents & POLLHUP)
            break;
    }

    /* Cleanup */
    close(master_fd);
    close(client_fd);
    waitpid(child_pid, NULL, 0);
    _exit(0);
}

/* ------------------------------------------------------------------ */
/* Main entry point                                                   */
/* ------------------------------------------------------------------ */

int main(int argc, char **argv) {
    int port = 23;
    if (argc > 1) {
        port = atoi(argv[1]);
        if (port <= 0 || port > 65535)
            port = 23;
    }

    /* Create TCP listening socket */
    int listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (listen_fd < 0) {
        write_str(2, "telnetd: socket() failed\n");
        return 1;
    }

    int opt = 1;
    setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = INADDR_ANY;

    if (bind(listen_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        write_str(2, "telnetd: bind() failed\n");
        close(listen_fd);
        return 1;
    }

    if (listen(listen_fd, 5) < 0) {
        write_str(2, "telnetd: listen() failed\n");
        close(listen_fd);
        return 1;
    }

    /* Startup banner */
    write_str(1, "telnetd: listening on port ");
    {
        char pbuf[8];
        int plen = 0;
        int p = port;
        if (p == 0) { pbuf[plen++] = '0'; }
        else {
            char tmp[8];
            int tlen = 0;
            while (p > 0) { tmp[tlen++] = '0' + (p % 10); p /= 10; }
            for (int i = tlen - 1; i >= 0; i--)
                pbuf[plen++] = tmp[i];
        }
        write(1, pbuf, plen);
    }
    write_str(1, "\n");

    /* Accept loop */
    for (;;) {
        /* Reap finished children (non-blocking) */
        while (waitpid(-1, NULL, WNOHANG) > 0)
            ;

        /* Wait for incoming connections with a timeout so we can
         * reap children regularly even when idle. */
        struct pollfd lpfd;
        lpfd.fd = listen_fd;
        lpfd.events = POLLIN;
        lpfd.revents = 0;
        int pr = poll(&lpfd, 1, 1000);
        if (pr <= 0 || !(lpfd.revents & POLLIN))
            continue;

        struct sockaddr_in client_addr;
        socklen_t client_len = sizeof(client_addr);
        int client_fd = accept(listen_fd, (struct sockaddr *)&client_addr,
                               &client_len);
        if (client_fd < 0)
            continue;

        /* Fork a handler for this connection */
        int pid = fork();
        if (pid < 0) {
            close(client_fd);
            continue;
        }
        if (pid == 0) {
            /* Child: close listening socket, handle connection */
            close(listen_fd);
            handle_connection(client_fd);
            /* handle_connection calls _exit(), but just in case: */
            _exit(0);
        }
        /* Parent: close client socket, continue accepting */
        close(client_fd);
    }

    return 0;
}
