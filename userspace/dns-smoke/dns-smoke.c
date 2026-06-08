// Phase 77 Track D.1 — DNS resolution smoke test.
//
// Exercises the prebuilt musl resolver end to end against m3OS's
// `socket(AF_INET, SOCK_DGRAM)` / `udp::bind` path and the staged
// `/etc/resolv.conf` (nameserver 10.0.2.3 — QEMU SLIRP's virtual DNS).
//
// Resolves the SAME name TWICE back-to-back. This is a regression guard for the
// Phase 86b resolver bug where the *second* consecutive `getaddrinfo` failed
// with EAI_SYSTEM/EADDRINUSE: musl `bind`s the query socket to port 0
// (wildcard), and net_server reserved a literal port-0 entry that `handle_close`
// never freed, so the second wildcard bind collided. git's ssh transport runs
// the client twice during a clone, so this hit the live `git clone`.
//
// Verdicts (gate accepts the `DNS_SMOKE:` prefix; exit 0 except on FAIL):
//   - both resolve            -> `DNS_SMOKE:PASS` (exit 0)
//   - first OK, second fails  -> `DNS_SMOKE:FAIL` (exit 1) — the resolver bug
//   - neither resolves        -> `DNS_SMOKE:SKIP` (exit 0) — no outbound DNS

#include <arpa/inet.h>
#include <errno.h>
#include <netdb.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;

    int rc1 = -1, rc2 = -1, e2 = 0;
    char ip1[INET_ADDRSTRLEN] = "?";

    for (int attempt = 1; attempt <= 2; attempt++) {
        struct addrinfo *res = NULL;
        errno = 0;
        int rc = getaddrinfo("github.com", "80", &hints, &res);
        if (attempt == 1) {
            rc1 = rc;
            if (rc == 0 && res != NULL) {
                struct sockaddr_in *sin = (struct sockaddr_in *)res->ai_addr;
                inet_ntop(AF_INET, &sin->sin_addr, ip1, sizeof(ip1));
            }
        } else {
            rc2 = rc;
            e2 = errno;
        }
        if (res != NULL) {
            freeaddrinfo(res);
        }
    }

    if (rc1 == 0 && rc2 == 0) {
        printf("DNS_SMOKE:PASS %s (2x)\n", ip1);
        fflush(stdout);
        return 0;
    }
    if (rc1 == 0 && rc2 != 0) {
        // First resolved, second failed: the back-to-back resolver regression.
        printf("DNS_SMOKE:FAIL second getaddrinfo rc=%d (%s) errno=%d (%s)\n", rc2,
               gai_strerror(rc2), e2, strerror(e2));
        fflush(stdout);
        return 1;
    }
    printf("DNS_SMOKE:SKIP getaddrinfo rc1=%d rc2=%d\n", rc1, rc2);
    fflush(stdout);
    return 0;
}
