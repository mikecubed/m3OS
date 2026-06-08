// Phase 93 — ext2 cross-process read-coherence regression (Bug B).
//
// Reproduces the dual-engine ext2 read-incoherence hazard: writes a file on the
// ext2 root through the kernel write path, churns unrelated ext2 metadata, then
// reads the SAME file back from a FRESHLY fork/exec'd process. The fresh reader
// must observe the bytes just written — not a stale snapshot from a second,
// uncoordinated ext2 engine.
//
//   - Before the fix: ext2 READS were served by the ring-3 vfs_server (its own
//     `Ext2State`) while WRITES went to the in-kernel `EXT2_VOLUME` engine. A
//     write mutated one engine's view; the fresh-process read was answered by
//     the other from a stale view → the reader saw old content.
//   - After the fix: vfs_server is the single ext2 owner (reads AND writes), so
//     the write-then-fresh-read is the same engine reading its own state →
//     coherent by construction.
//
// Roles (single self-exec binary):
//   * orchestrator (no extra args): pick a per-run nonce, write the marker,
//     churn unrelated ext2 files, then fork+exec the reader role on itself and
//     gate on its exit. Prints EXT2_COHERENCE:PASS / :FAIL.
//   * reader  (argv: "reader" <path> <expected-marker>): a fresh process image
//     that opens <path> O_RDONLY and exits 0 iff its content == expected.
//
// Built as a full-musl static binary (open/read/write/fork/execve/printf), so a
// FAIL fails the smoke gate. Exits 0 on PASS, non-zero on FAIL.

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

#define TARGET "/root/ext2-coh.txt"
#define CHURN_PREFIX "/root/ext2-coh-churn-"
#define CHURN_COUNT 6
#define MARKER_MAX 96
// Absolute path used to re-exec ourselves as the reader role. argv[0] is only
// the basename when the smoke-runner launches us, so exec by full path.
#define SELF_PATH "/bin/ext2-coherence-smoke"

// Write `data` to `path`, truncating any prior content. Returns 0 on success.
static int write_file(const char *path, const char *data, size_t len) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        return -1;
    }
    size_t off = 0;
    while (off < len) {
        ssize_t n = write(fd, data + off, len - off);
        if (n <= 0) {
            close(fd);
            return -1;
        }
        off += (size_t)n;
    }
    close(fd);
    return 0;
}

// Read up to buf_len-1 bytes of `path` into buf (NUL-terminated). Returns the
// byte count, or -1 on error.
static ssize_t read_file(const char *path, char *buf, size_t buf_len) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    size_t off = 0;
    while (off < buf_len - 1) {
        ssize_t n = read(fd, buf + off, buf_len - 1 - off);
        if (n < 0) {
            close(fd);
            return -1;
        }
        if (n == 0) {
            break;
        }
        off += (size_t)n;
    }
    buf[off] = '\0';
    close(fd);
    return (ssize_t)off;
}

// Reader role: a fresh process that must see the bytes the orchestrator wrote.
static int run_reader(const char *path, const char *expected) {
    char got[MARKER_MAX + 16];
    ssize_t n = read_file(path, got, sizeof(got));
    if (n < 0) {
        printf("EXT2_COHERENCE:reader open/read failed\n");
        return 2;
    }
    if (strcmp(got, expected) != 0) {
        // The decisive line: a fresh reader saw stale content.
        printf("EXT2_COHERENCE:reader STALE got=\"%s\" expected=\"%s\"\n", got,
               expected);
        return 3;
    }
    return 0;
}

// Writer role: a fresh process that overwrites `path` (O_TRUNC) with `data`.
// Used by sub-test 2 to mutate the file from a DIFFERENT process while the
// orchestrator holds an O_RDWR fd whose in-kernel block cache must not go stale.
static int run_writer(const char *path, const char *data) {
    if (write_file(path, data, strlen(data)) != 0) {
        printf("EXT2_COHERENCE:writer write failed\n");
        return 4;
    }
    return 0;
}

// Create + write + delete unrelated ext2 files to force inode/block-bitmap and
// directory mutations BETWEEN the write and the cross-process read — the
// "intervening unrelated ext2 write churn" the regression requires.
static void churn(void) {
    for (int i = 0; i < CHURN_COUNT; i++) {
        char p[64];
        snprintf(p, sizeof(p), CHURN_PREFIX "%d", i);
        char body[64];
        int blen = snprintf(body, sizeof(body), "churn-%d-payload-data\n", i);
        if (write_file(p, body, (size_t)blen) == 0) {
            // Read it back (exercises the read path too), then unlink it.
            char tmp[80];
            (void)read_file(p, tmp, sizeof(tmp));
            unlink(p);
        }
    }
}

// Fork+exec a fresh image of ourselves in the given role; wait; return its
// exit code (or -1 on fork/wait failure).
static int spawn_self(const char *role, const char *arg2, const char *arg3) {
    pid_t pid = fork();
    if (pid < 0) {
        return -1;
    }
    if (pid == 0) {
        char *rargv[] = {(char *)SELF_PATH, (char *)role, (char *)arg2,
                         (char *)arg3, NULL};
        execve(SELF_PATH, rargv, environ);
        _exit(127);
    }
    int status = 0;
    if (waitpid(pid, &status, 0) < 0) {
        return -1;
    }
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

// Sub-test 1 — fresh-process read-back: a write made by the orchestrator must
// be visible to a FRESHLY fork/exec'd reader process, even after intervening
// unrelated ext2 write churn.
static int subtest_fresh_read(const char *marker, size_t mlen) {
    if (write_file(TARGET, marker, mlen) != 0) {
        printf("EXT2_COHERENCE:FAIL t1 initial write\n");
        return 1;
    }
    char self_got[MARKER_MAX + 16];
    if (read_file(TARGET, self_got, sizeof(self_got)) < 0 ||
        strcmp(self_got, marker) != 0) {
        printf("EXT2_COHERENCE:FAIL t1 same-process read-back\n");
        return 1;
    }
    churn(); // intervening unrelated ext2 write churn
    if (write_file(TARGET, marker, mlen) != 0) {
        printf("EXT2_COHERENCE:FAIL t1 post-churn write\n");
        return 1;
    }
    int rc = spawn_self("reader", TARGET, marker);
    if (rc != 0) {
        printf("EXT2_COHERENCE:FAIL t1 reader rc=%d\n", rc);
        return 1;
    }
    return 0;
}

// Sub-test 2 — cross-process write must invalidate the in-kernel Ext2Disk read
// cache. The orchestrator opens TARGET O_RDWR (the in-kernel `Ext2Disk` backend
// whose block cache is the historical stale-snapshot), reads it once to PRIME
// that cache, then a FRESH writer process overwrites the file. A subsequent
// read through the orchestrator's still-open O_RDWR fd must see the writer's new
// bytes — not the primed-but-now-stale cached block. This is the path the
// single-owner fix makes coherent (it routes Ext2Disk reads through vfs_server
// and invalidates the kernel cache after each routed write).
static int subtest_cache_coherence(const char *v1, const char *v2, size_t v2len) {
    int fd = open(TARGET, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        printf("EXT2_COHERENCE:FAIL t2 open rdwr\n");
        return 1;
    }
    // Seed V1 through the O_RDWR fd, then read it back to prime the in-kernel
    // Ext2Disk block cache with V1's content block.
    size_t v1len = strlen(v1);
    if (write(fd, v1, v1len) != (ssize_t)v1len) {
        close(fd);
        printf("EXT2_COHERENCE:FAIL t2 seed write\n");
        return 1;
    }
    char prime[MARKER_MAX + 16];
    if (lseek(fd, 0, SEEK_SET) != 0) {
        close(fd);
        printf("EXT2_COHERENCE:FAIL t2 lseek prime\n");
        return 1;
    }
    ssize_t pn = read(fd, prime, sizeof(prime) - 1);
    if (pn < 0) {
        close(fd);
        printf("EXT2_COHERENCE:FAIL t2 prime read\n");
        return 1;
    }
    prime[pn] = '\0';
    if (strcmp(prime, v1) != 0) {
        close(fd);
        printf("EXT2_COHERENCE:FAIL t2 prime mismatch got=\"%s\"\n", prime);
        return 1;
    }

    // A FRESH process overwrites the file (O_TRUNC) with V2.
    int rc = spawn_self("writer", TARGET, v2);
    if (rc != 0) {
        close(fd);
        printf("EXT2_COHERENCE:FAIL t2 writer rc=%d\n", rc);
        return 1;
    }

    // Re-read through the orchestrator's O_RDWR fd. Must observe V2, not the
    // primed-stale V1 cache block.
    char got[MARKER_MAX + 16];
    if (lseek(fd, 0, SEEK_SET) != 0) {
        close(fd);
        printf("EXT2_COHERENCE:FAIL t2 lseek reread\n");
        return 1;
    }
    ssize_t gn = read(fd, got, sizeof(got) - 1);
    close(fd);
    if (gn < 0) {
        printf("EXT2_COHERENCE:FAIL t2 reread\n");
        return 1;
    }
    got[gn] = '\0';
    if (strncmp(got, v2, v2len) != 0) {
        // The decisive line: the O_RDWR re-read served a stale cached block.
        printf("EXT2_COHERENCE:t2 STALE got=\"%s\" expected=\"%s\"\n", got, v2);
        return 1;
    }
    return 0;
}

// Sub-test 3 — LARGE-file write + cross-process read-back. Writes a 200 KB file
// (>64 KB single-write cap → multiple sys_write calls; >12 KB → ext2 indirect
// blocks; full-4096-byte data chunks → the vfs_server bulk-buffer path+data
// path) with a deterministic per-offset byte pattern, then a FRESH process
// re-reads and verifies every byte. This is the coverage the original Bug-B fix
// LACKED: its tests wrote only tiny markers, so the large-write path (where a
// full data chunk plus the path overflowed vfs_server's recv_buf and the write
// was rejected with EINVAL) shipped untested. A FAIL here is a hard gate fail.
#define LARGE_PATH "/root/ext2-coh-large.bin"
#define LARGE_SIZE (200u * 1024u)

// Deterministic byte for file offset `i` — recomputable in a fresh process, so
// the reader needs no stored copy of the 200 KB content.
static unsigned char pat(size_t i) {
    return (unsigned char)(((i * 31u) + 7u) & 0xFFu);
}

// Big-reader role: a fresh process that re-reads LARGE_PATH and asserts every
// byte matches pat(offset) and the total length is exactly LARGE_SIZE.
static int run_bigreader(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        printf("EXT2_COHERENCE:bigreader open failed\n");
        return 5;
    }
    unsigned char buf[8192];
    size_t off = 0;
    for (;;) {
        ssize_t r = read(fd, buf, sizeof(buf));
        if (r < 0) {
            close(fd);
            printf("EXT2_COHERENCE:bigreader read err off=%zu\n", off);
            return 6;
        }
        if (r == 0) {
            break;
        }
        for (ssize_t k = 0; k < r; k++) {
            if (buf[k] != pat(off + (size_t)k)) {
                close(fd);
                printf("EXT2_COHERENCE:bigreader MISMATCH off=%zu\n", off + (size_t)k);
                return 7;
            }
        }
        off += (size_t)r;
    }
    close(fd);
    if (off != LARGE_SIZE) {
        printf("EXT2_COHERENCE:bigreader SHORT total=%zu want=%u\n", off, LARGE_SIZE);
        return 8;
    }
    return 0;
}

static int subtest_large_write(void) {
    int fd = open(LARGE_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        printf("EXT2_COHERENCE:FAIL t3 open\n");
        return 1;
    }
    unsigned char buf[8192];
    size_t off = 0;
    while (off < LARGE_SIZE) {
        size_t n = LARGE_SIZE - off;
        if (n > sizeof(buf)) {
            n = sizeof(buf);
        }
        for (size_t k = 0; k < n; k++) {
            buf[k] = pat(off + k);
        }
        size_t w = 0;
        while (w < n) {
            ssize_t r = write(fd, buf + w, n - w);
            if (r <= 0) {
                close(fd);
                printf("EXT2_COHERENCE:FAIL t3 write off=%zu r=%zd\n", off + w, r);
                return 1;
            }
            w += (size_t)r;
        }
        off += n;
    }
    close(fd);
    churn(); // intervening unrelated ext2 churn before the fresh-process read
    int rc = spawn_self("bigreader", LARGE_PATH, "");
    if (rc != 0) {
        printf("EXT2_COHERENCE:FAIL t3 bigreader rc=%d\n", rc);
        return 1;
    }
    unlink(LARGE_PATH);
    return 0;
}

int main(int argc, char **argv) {
    // Reader / writer roles (fresh process images spawned by the orchestrator).
    if (argc >= 4 && strcmp(argv[1], "reader") == 0) {
        return run_reader(argv[2], argv[3]);
    }
    if (argc >= 4 && strcmp(argv[1], "writer") == 0) {
        return run_writer(argv[2], argv[3]);
    }
    if (argc >= 3 && strcmp(argv[1], "bigreader") == 0) {
        return run_bigreader(argv[2]);
    }

    // Orchestrator role.
    // Per-run nonce so a stale read of a *previous boot's* content is caught as
    // a mismatch (defends the regression against disk persistence across runs).
    unsigned long nonce = (unsigned long)getpid() * 2654435761UL;
    nonce ^= (unsigned long)(size_t)&argc; // a little stack-address entropy

    char marker[MARKER_MAX];
    int mlen = snprintf(marker, sizeof(marker),
                        "EXT2-COHERENT-V2-nonce-%lu-end\n", nonce);
    if (mlen <= 0 || (size_t)mlen >= sizeof(marker)) {
        printf("EXT2_COHERENCE:FAIL marker build\n");
        return 1;
    }
    if (subtest_fresh_read(marker, (size_t)mlen) != 0) {
        return 1;
    }

    char v1[MARKER_MAX];
    char v2[MARKER_MAX];
    int v1len = snprintf(v1, sizeof(v1), "EXT2-V1-OLD-nonce-%lu-end\n", nonce);
    int v2len = snprintf(v2, sizeof(v2), "EXT2-V2-NEW-nonce-%lu-end\n", nonce);
    if (v1len <= 0 || v2len <= 0) {
        printf("EXT2_COHERENCE:FAIL v1/v2 build\n");
        return 1;
    }
    if (subtest_cache_coherence(v1, v2, (size_t)v2len) != 0) {
        return 1;
    }

    // Sub-test 3 — large-file write + fresh-process verify (indirect blocks +
    // the vfs_server bulk path+data chunk path). No stored marker needed; the
    // reader recomputes the per-offset pattern.
    if (subtest_large_write() != 0) {
        return 1;
    }

    // Best-effort cleanup of the target.
    unlink(TARGET);

    printf("EXT2_COHERENCE:PASS\n");
    return 0;
}
