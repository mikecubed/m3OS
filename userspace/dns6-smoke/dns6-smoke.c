// Phase 91 Track D — AAAA resolution + RFC 6724 dual-stack selection smoke test.
//
// Two arms, both through musl's `getaddrinfo` (the m3OS work is supplying the
// AF_INET6 socket transport + source-address inputs the musl RFC 3484/6724 sort
// consumes — m3OS does not re-implement the sort):
//
//   RFC 6724 (CI-deterministic, no network): `getaddrinfo("localhost",
//   AF_UNSPEC)` reads the staged dual-stack `/etc/hosts` (`127.0.0.1` + `::1`)
//   and MUST return BOTH families; we assert that and report the musl-sorted
//   leading family. -> DNS6_SMOKE:rfc6724:ok
//
//   AAAA (opt-in / soft, real internet): `getaddrinfo("github.com", AF_UNSPEC)`
//   and check for an AF_INET6 answer. Soft by design — with no v6 route / no
//   outbound DNS it reports `aaaa:skip` rather than failing (mirroring
//   `dns-smoke`'s SKIP discipline). -> DNS6_SMOKE:aaaa:ok | :skip
//
// Verdicts (gate accepts the `DNS6_SMOKE:` prefix; exit 0 except on FAIL):
//   - localhost dual-stack resolves -> `DNS6_SMOKE:PASS` (exit 0)
//   - localhost not dual-stack       -> `DNS6_SMOKE:FAIL` (exit 1)
//   - localhost cannot resolve       -> `DNS6_SMOKE:SKIP` (exit 0)

#include <arpa/inet.h>
#include <netdb.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;

    // --- RFC 6724 arm: dual-stack localhost from /etc/hosts ---
    struct addrinfo *res = NULL;
    int rc = getaddrinfo("localhost", "80", &hints, &res);
    if (rc != 0 || res == NULL) {
        printf("DNS6_SMOKE:SKIP localhost getaddrinfo rc=%d (%s)\n", rc,
               gai_strerror(rc));
        fflush(stdout);
        return 0;
    }
    int have4 = 0, have6 = 0, first_family = 0;
    for (struct addrinfo *ai = res; ai != NULL; ai = ai->ai_next) {
        if (first_family == 0) {
            first_family = ai->ai_family;
        }
        if (ai->ai_family == AF_INET) {
            have4 = 1;
        }
        if (ai->ai_family == AF_INET6) {
            have6 = 1;
        }
    }
    freeaddrinfo(res);
    if (!have4 || !have6) {
        printf("DNS6_SMOKE:FAIL localhost not dual-stack (v4=%d v6=%d)\n", have4,
               have6);
        fflush(stdout);
        return 1;
    }
    printf("DNS6_SMOKE:rfc6724:ok both-families first=%s\n",
           first_family == AF_INET6 ? "v6" : "v4");

    // --- AAAA arm: a real name (soft / opt-in real internet) ---
    res = NULL;
    rc = getaddrinfo("github.com", "443", &hints, &res);
    int aaaa = 0, a = 0;
    char ip6[INET6_ADDRSTRLEN] = "?";
    if (rc == 0 && res != NULL) {
        for (struct addrinfo *ai = res; ai != NULL; ai = ai->ai_next) {
            if (ai->ai_family == AF_INET6) {
                aaaa = 1;
                struct sockaddr_in6 *s6 = (struct sockaddr_in6 *)ai->ai_addr;
                inet_ntop(AF_INET6, &s6->sin6_addr, ip6, sizeof(ip6));
            }
            if (ai->ai_family == AF_INET) {
                a = 1;
            }
        }
        freeaddrinfo(res);
    }
    if (aaaa) {
        printf("DNS6_SMOKE:aaaa:ok github.com AAAA=%s (A=%d)\n", ip6, a);
    } else {
        printf("DNS6_SMOKE:aaaa:skip no AAAA (A=%d rc=%d — no v6 route/DNS)\n", a,
               rc);
    }

    printf("DNS6_SMOKE:PASS\n");
    fflush(stdout);
    return 0;
}
