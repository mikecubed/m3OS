// Phase 86b — non-blocking connect() smoke test.
//
// Proves m3OS's new POSIX non-blocking connect semantics *deterministically*,
// with NO outbound network required. Before Phase 86b the inet TCP `connect()`
// busy-blocked until the handshake completed regardless of `O_NONBLOCK`; this
// smoke pins the corrected behaviour that unblocks the git-over-SSH client.
//
// Core assertions (the load-bearing PASS — need no internet, run against the
// RFC 5737 TEST-NET-1 address 192.0.2.1 whose SYN goes nowhere):
//   1. socket(SOCK_STREAM|SOCK_NONBLOCK) + connect() returns -1/EINPROGRESS
//      *synchronously* — the kernel no longer parks a non-blocking connect.
//   2. poll(POLLOUT, timeout=0) reports the socket NOT yet ready while the SYN
//      is in flight (no spurious writable / error before completion).
//   3. getsockopt(SO_ERROR) reports 0 (no pending error) while connecting.
//   4. a second connect() on the same fd returns -1/EALREADY — the re-issue
//      guard that stops poll-driven clients leaking TCP slots on retry.
//   5. setsockopt(SO_KEEPALIVE) + setsockopt(TCP_KEEPIDLE/INTVL/CNT) all
//      return 0 (Phase 90b ABI-conformance guard): libuv sets these on every
//      client socket and a hard ENOPROTOOPT there made Node fetch/undici and
//      Claude Code report "connect ... ENOPROTOOPT". getsockopt(TCP_KEEPIDLE)
//      round-trips the stored value. (The keepalive PROBE timer is deferred.)
//
// Best-effort completion leg (reported as INFO, never changes the verdict):
// connect non-blocking to a closed local port (10.0.2.2:9) and poll briefly;
// if QEMU SLIRP RSTs, this exercises the real EINPROGRESS -> poll-wake ->
// getsockopt(SO_ERROR)=ECONNREFUSED round trip end to end.
//
// Emits `CONNECT_SMOKE:PASS ...` and exits 0 when all four core asserts hold;
// emits `CONNECT_SMOKE:FAIL ...` and exits 1 otherwise. The smoke-runner gate
// requires both the exit code and the `CONNECT_SMOKE:PASS` marker.

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static int fail(const char *why, int detail) {
    printf("CONNECT_SMOKE:FAIL %s (detail=%d)\n", why, detail);
    fflush(stdout);
    return 1;
}

static int make_sockaddr(struct sockaddr_in *sa, const char *ip, int port) {
    memset(sa, 0, sizeof(*sa));
    sa->sin_family = AF_INET;
    sa->sin_port = htons((unsigned short)port);
    return inet_pton(AF_INET, ip, &sa->sin_addr);
}

int main(void) {
    // ---- Core: deterministic, no network required ----
    int fd = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    if (fd < 0) {
        return fail("socket(SOCK_NONBLOCK)", errno);
    }

    struct sockaddr_in dst;
    if (make_sockaddr(&dst, "192.0.2.1", 80) != 1) {
        return fail("inet_pton(192.0.2.1)", errno);
    }

    // 1. EINPROGRESS, returned synchronously.
    int rc = connect(fd, (struct sockaddr *)&dst, sizeof(dst));
    if (!(rc < 0 && errno == EINPROGRESS)) {
        int e = (rc < 0) ? errno : 0;
        close(fd);
        return fail("connect did not return EINPROGRESS", e);
    }

    // 2. Not spuriously ready while the SYN is in flight.
    struct pollfd pfd;
    pfd.fd = fd;
    pfd.events = POLLOUT;
    pfd.revents = 0;
    int pr = poll(&pfd, 1, 0);
    if (pr < 0) {
        int e = errno;
        close(fd);
        return fail("poll(timeout=0)", e);
    }
    if (pfd.revents & (POLLOUT | POLLERR | POLLHUP)) {
        close(fd);
        return fail("socket spuriously ready while connecting", pfd.revents);
    }

    // 3. SO_ERROR == 0 while the connect is still pending.
    int soerr = -1;
    socklen_t slen = sizeof(soerr);
    if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &slen) < 0) {
        int e = errno;
        close(fd);
        return fail("getsockopt(SO_ERROR)", e);
    }
    if (soerr != 0) {
        close(fd);
        return fail("SO_ERROR non-zero while pending", soerr);
    }

    // 4. A re-issued connect() must report EALREADY (no fresh TCP slot leaked).
    rc = connect(fd, (struct sockaddr *)&dst, sizeof(dst));
    if (!(rc < 0 && errno == EALREADY)) {
        int e = (rc < 0) ? errno : 0;
        close(fd);
        return fail("re-issued connect not EALREADY", e);
    }
    close(fd);

    // 5. setsockopt of standard TCP keepalive options must SUCCEED, not fail
    //    with ENOPROTOOPT. Phase 90b: libuv's uv__tcp_keepalive sets
    //    SO_KEEPALIVE then TCP_KEEPIDLE/INTVL/CNT on every client socket and
    //    treats *any* failure there as fatal — a hard ENOPROTOOPT made Node's
    //    fetch/undici (and thus Claude Code) report
    //    "connect ... ENOPROTOOPT" against api.anthropic.com and abort. The
    //    kernel now accepts + stores these; the probe TIMER is deferred (see
    //    docs/roadmap/90b-claude-code.md). No network required — this is the
    //    always-on regression guard for that ABI-conformance fix.
    int kfd = socket(AF_INET, SOCK_STREAM, 0);
    if (kfd < 0) {
        return fail("socket(keepalive-opts)", errno);
    }
    int on = 1;
    if (setsockopt(kfd, SOL_SOCKET, SO_KEEPALIVE, &on, sizeof(on)) != 0) {
        int e = errno;
        close(kfd);
        return fail("setsockopt(SO_KEEPALIVE)", e);
    }
    int kidle = 60, kintvl = 10, kcnt = 4;
    if (setsockopt(kfd, IPPROTO_TCP, TCP_KEEPIDLE, &kidle, sizeof(kidle)) != 0) {
        int e = errno;
        close(kfd);
        return fail("setsockopt(TCP_KEEPIDLE) - ABI regression", e);
    }
    if (setsockopt(kfd, IPPROTO_TCP, TCP_KEEPINTVL, &kintvl, sizeof(kintvl)) != 0) {
        int e = errno;
        close(kfd);
        return fail("setsockopt(TCP_KEEPINTVL)", e);
    }
    if (setsockopt(kfd, IPPROTO_TCP, TCP_KEEPCNT, &kcnt, sizeof(kcnt)) != 0) {
        int e = errno;
        close(kfd);
        return fail("setsockopt(TCP_KEEPCNT)", e);
    }
    // getsockopt coherence: the stored value must round-trip. Split the syscall
    // failure from the value mismatch so the FAIL detail is unambiguous — a
    // syscall error reports errno; a mismatch reports the actual value read back
    // (expected = kidle, stated in the message).
    int rb = 0;
    socklen_t rl = sizeof(rb);
    if (getsockopt(kfd, IPPROTO_TCP, TCP_KEEPIDLE, &rb, &rl) != 0) {
        int e = errno;
        close(kfd);
        return fail("getsockopt(TCP_KEEPIDLE)", e);
    }
    if (rb != kidle) {
        close(kfd);
        return fail("getsockopt(TCP_KEEPIDLE) round-trip mismatch (expected 60, got)", rb);
    }
    close(kfd);

    // ---- Best-effort completion leg (INFO only, never fails the verdict) ----
    // If SLIRP RSTs a closed local port, prove the full
    // EINPROGRESS -> poll-wake -> getsockopt(SO_ERROR) failure round trip.
    int cfd = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    if (cfd >= 0) {
        struct sockaddr_in closed;
        if (make_sockaddr(&closed, "10.0.2.2", 9) == 1) {
            int crc = connect(cfd, (struct sockaddr *)&closed, sizeof(closed));
            if (crc < 0 && errno == EINPROGRESS) {
                struct pollfd cp;
                cp.fd = cfd;
                cp.events = POLLOUT;
                cp.revents = 0;
                int cpr = poll(&cp, 1, 1500);
                if (cpr > 0 && (cp.revents & (POLLOUT | POLLERR | POLLHUP))) {
                    int cerr = 0;
                    socklen_t cl = sizeof(cerr);
                    getsockopt(cfd, SOL_SOCKET, SO_ERROR, &cerr, &cl);
                    printf("CONNECT_SMOKE:INFO completion revents=0x%x so_error=%d\n",
                           cp.revents, cerr);
                } else {
                    printf("CONNECT_SMOKE:INFO completion poll rc=%d (no RST in harness)\n",
                           cpr);
                }
            } else {
                printf("CONNECT_SMOKE:INFO closed-port connect rc=%d errno=%d\n", crc, errno);
            }
        }
        close(cfd);
    }
    fflush(stdout);

    printf("CONNECT_SMOKE:PASS nonblock connect EINPROGRESS/poll/SO_ERROR/EALREADY"
           " + keepalive setsockopt ok\n");
    fflush(stdout);
    return 0;
}
