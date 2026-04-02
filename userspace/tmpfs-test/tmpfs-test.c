/* tmpfs-test.c — Phase 13 tmpfs validation.
 *
 * Exercises the writable tmpfs mounted at /tmp:
 *   - create + write + close + reopen + read back (round-trip)
 *   - mkdir + rmdir
 *   - unlink
 *   - truncate
 *
 * Compiled with musl-gcc -static and run as a userspace ELF binary.
 * Exit code 0 = all tests passed; non-zero = failure.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <dirent.h>
#include <fcntl.h>
#include <sys/wait.h>
#include <unistd.h>
#include <sys/stat.h>

static int tests_passed = 0;
static int tests_failed = 0;

static void pass(const char *name) {
    printf("  PASS: %s\n", name); /* DevSkim: ignore DS154189 — format string is a literal */
    tests_passed++;
}

static void fail(const char *name, const char *reason) {
    printf("  FAIL: %s — %s\n", name, reason); /* DevSkim: ignore DS154189 — format string is a literal */
    tests_failed++;
}

static int read_file_into(const char *path, char *buf, size_t buf_size, ssize_t *out_len) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t n = read(fd, buf, buf_size);
    close(fd);
    if (n < 0) {
        return -1;
    }
    if (out_len) {
        *out_len = n;
    }
    if ((size_t)n < buf_size) {
        buf[n] = '\0';
    }
    return 0;
}

/* Test 1: create, write, close, reopen, read back */
static void test_write_read_roundtrip(void) {
    const char *path = "/tmp/test.txt";
    const char *msg = "Hello from tmpfs!";
    size_t msg_len = strlen(msg); /* DevSkim: ignore DS140021 — string literal */

    /* Create and write */
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fail("write-read: open for write", "open returned < 0");
        return;
    }
    ssize_t written = write(fd, msg, msg_len);
    if (written < 0 || (size_t)written != msg_len) {
        fail("write-read: write", "short write");
        close(fd);
        return;
    }
    close(fd);

    /* Reopen and read back */
    fd = open(path, O_RDONLY);
    if (fd < 0) {
        fail("write-read: open for read", "open returned < 0");
        return;
    }
    char buf[64];
    memset(buf, 0, sizeof(buf));
    ssize_t nread = read(fd, buf, sizeof(buf));
    close(fd);

    if (nread < 0 || (size_t)nread != msg_len) {
        fail("write-read: read length", "wrong byte count");
        return;
    }
    if (memcmp(buf, msg, msg_len) != 0) {
        fail("write-read: content", "data mismatch");
        return;
    }
    pass("write-read roundtrip");
}

/* Test 2: mkdir + rmdir */
static void test_mkdir_rmdir(void) {
    if (mkdir("/tmp/testdir", 0755) != 0) {
        fail("mkdir", "mkdir returned non-zero");
        return;
    }
    /* rmdir should succeed on empty dir */
    if (rmdir("/tmp/testdir") != 0) {
        fail("rmdir", "rmdir returned non-zero");
        return;
    }
    pass("mkdir + rmdir");
}

/* Test 3: unlink */
static void test_unlink(void) {
    const char *path = "/tmp/todelete.txt";
    int fd = open(path, O_WRONLY | O_CREAT, 0644);
    if (fd < 0) {
        fail("unlink: create", "open returned < 0");
        return;
    }
    ssize_t wr = write(fd, "x", 1);
    if (wr != 1) {
        fail("unlink: write", "short write or error");
        close(fd);
        return;
    }
    close(fd);

    if (unlink(path) != 0) {
        fail("unlink", "unlink returned non-zero");
        return;
    }

    /* Verify the file is gone */
    fd = open(path, O_RDONLY);
    if (fd >= 0) {
        fail("unlink: verify", "file still exists after unlink");
        close(fd);
        return;
    }
    pass("unlink");
}

/* Test 4: ftruncate */
static void test_truncate(void) {
    const char *path = "/tmp/trunc.txt";
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fail("truncate: create", "open returned < 0");
        return;
    }
    ssize_t nw = write(fd, "abcdefghij", 10);
    if (nw != 10) {
        fail("truncate: write", "write failed or short");
        close(fd);
        return;
    }
    /* Truncate to 5 bytes */
    if (ftruncate(fd, 5) != 0) {
        fail("truncate: ftruncate", "ftruncate returned non-zero");
        close(fd);
        return;
    }
    close(fd);

    /* Read back and check length */
    fd = open(path, O_RDONLY);
    if (fd < 0) {
        fail("truncate: reopen", "open returned < 0");
        return;
    }
    char buf[64];
    ssize_t nread = read(fd, buf, sizeof(buf));
    close(fd);

    if (nread != 5) {
        fail("truncate: length", "expected 5 bytes");
        return;
    }
    if (memcmp(buf, "abcde", 5) != 0) {
        fail("truncate: content", "data mismatch after truncate");
        return;
    }
    pass("ftruncate");
}

/* Test 5: sequential writes advance the fd offset correctly */
static void test_sequential_write(void) {
    const char *path = "/tmp/append.txt";
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fail("append: create", "open returned < 0");
        return;
    }
    ssize_t n1 = write(fd, "AAA", 3);
    if (n1 != 3) {
        fail("append: first write", "write did not return 3");
        close(fd);
        return;
    }
    ssize_t n2 = write(fd, "BBB", 3);
    if (n2 != 3) {
        fail("append: second write", "write did not return 3");
        close(fd);
        return;
    }
    close(fd);

    fd = open(path, O_RDONLY);
    if (fd < 0) {
        fail("append: reopen", "open returned < 0");
        return;
    }
    char buf[64];
    ssize_t nread = read(fd, buf, sizeof(buf));
    close(fd);

    if (nread != 6) {
        fail("append: length", "expected 6 bytes");
        return;
    }
    if (memcmp(buf, "AAABBB", 6) != 0) {
        fail("append: content", "sequential writes not contiguous");
        return;
    }
    pass("sequential write");
}

/* Test 6: symlink creation, readlink, and stat/lstat semantics */
static void test_symlink_semantics(void) {
    const char *target_path = "/tmp/target.txt";
    const char *link_path = "/tmp/link.txt";
    const char *target_text = "through symlink";
    char link_buf[128];
    struct stat st;

    int fd = open(target_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fail("symlink: create target", "open returned < 0");
        return;
    }
    if (write(fd, target_text, strlen(target_text)) != (ssize_t)strlen(target_text)) {
        fail("symlink: write target", "short write");
        close(fd);
        return;
    }
    close(fd);

    if (symlink(target_path, link_path) != 0) {
        fail("symlink: create", "symlink returned non-zero");
        return;
    }

    ssize_t n = readlink(link_path, link_buf, sizeof(link_buf));
    if (n < 0) {
        fail("symlink: readlink", "readlink returned < 0");
        return;
    }
    if ((size_t)n != strlen(target_path)) {
        fail("symlink: readlink length", "wrong target length");
        return;
    }
    link_buf[n] = '\0';
    if (strcmp(link_buf, target_path) != 0) {
        fail("symlink: readlink content", "target mismatch");
        return;
    }

    if (lstat(link_path, &st) != 0) {
        fail("symlink: lstat", "lstat returned non-zero");
        return;
    }
    if (!S_ISLNK(st.st_mode)) {
        fail("symlink: lstat mode", "path is not reported as a symlink");
        return;
    }
    if ((size_t)st.st_size != strlen(target_path)) {
        fail("symlink: lstat size", "symlink size is not target length");
        return;
    }

    if (stat(link_path, &st) != 0) {
        fail("symlink: stat", "stat returned non-zero");
        return;
    }
    if (!S_ISREG(st.st_mode)) {
        fail("symlink: stat mode", "stat did not follow symlink to file");
        return;
    }
    if ((size_t)st.st_size != strlen(target_text)) {
        fail("symlink: stat size", "stat did not report target file size");
        return;
    }

    fd = open(link_path, O_RDONLY);
    if (fd < 0) {
        fail("symlink: open link", "open through symlink returned < 0");
        return;
    }
    memset(link_buf, 0, sizeof(link_buf));
    n = read(fd, link_buf, sizeof(link_buf));
    close(fd);
    if (n != (ssize_t)strlen(target_text)) {
        fail("symlink: open link length", "wrong byte count");
        return;
    }
    if (memcmp(link_buf, target_text, strlen(target_text)) != 0) {
        fail("symlink: open link content", "did not read target file data");
        return;
    }

    pass("symlink create + readlink + stat");
}

/* Test 7: multi-hop symlink resolution and loop detection */
static void test_symlink_chain_and_loops(void) {
    const char *target_path = "/tmp/chain-target.txt";
    char buf[128];
    int fd = open(target_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fail("symlink-chain: create target", "open returned < 0");
        return;
    }
    if (write(fd, "chain", 5) != 5) {
        fail("symlink-chain: write target", "short write");
        close(fd);
        return;
    }
    close(fd);

    if (symlink(target_path, "/tmp/chain-c") != 0 ||
        symlink("/tmp/chain-c", "/tmp/chain-b") != 0 ||
        symlink("/tmp/chain-b", "/tmp/chain-a") != 0) {
        fail("symlink-chain: create", "failed to create chain");
        return;
    }

    fd = open("/tmp/chain-a", O_RDONLY);
    if (fd < 0) {
        fail("symlink-chain: open", "open returned < 0");
        return;
    }
    memset(buf, 0, sizeof(buf));
    if (read(fd, buf, sizeof(buf)) != 5 || memcmp(buf, "chain", 5) != 0) {
        fail("symlink-chain: content", "chain did not resolve to target");
        close(fd);
        return;
    }
    close(fd);

    if (symlink("/tmp/loop-b", "/tmp/loop-a") != 0 ||
        symlink("/tmp/loop-a", "/tmp/loop-b") != 0) {
        fail("symlink-loop: create", "failed to create loop");
        return;
    }
    if (open("/tmp/loop-a", O_RDONLY) >= 0) {
        fail("symlink-loop: detect", "loop opened successfully");
        return;
    }

    if (symlink("/tmp/self-loop", "/tmp/self-loop") != 0) {
        fail("symlink-self-loop: create", "failed to create self loop");
        return;
    }
    if (open("/tmp/self-loop", O_RDONLY) >= 0) {
        fail("symlink-self-loop: detect", "self loop opened successfully");
        return;
    }

    for (int i = 0; i < 41; i++) {
        char name[32];
        char target[32];
        snprintf(name, sizeof(name), "/tmp/hop-%02d", i);
        if (i == 40) {
            strcpy(target, target_path); /* DevSkim: ignore DS148264 — bounded copy into fixed buffer */
        } else {
            snprintf(target, sizeof(target), "/tmp/hop-%02d", i + 1);
        }
        if (symlink(target, name) != 0) {
            fail("symlink-hop-limit: create", "failed to create hop chain");
            return;
        }
    }
    if (open("/tmp/hop-00", O_RDONLY) >= 0) {
        fail("symlink-hop-limit: detect", "41-hop chain opened successfully");
        return;
    }

    pass("symlink chain + loop detection");
}

/* Test 8: ext2 hard link semantics */
static void test_hard_links(void) {
    const char *path_a = "/hard-a.txt";
    const char *path_b = "/hard-b.txt";
    const char *payload = "linked data";
    char buf[64];
    struct stat st_a;
    struct stat st_b;

    int fd = open(path_a, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fail("hard-link: create", "open returned < 0");
        return;
    }
    if (write(fd, payload, strlen(payload)) != (ssize_t)strlen(payload)) {
        fail("hard-link: write", "short write");
        close(fd);
        return;
    }
    close(fd);

    if (link(path_a, path_b) != 0) {
        fail("hard-link: create link", "link returned non-zero");
        return;
    }
    if (stat(path_a, &st_a) != 0 || stat(path_b, &st_b) != 0) {
        fail("hard-link: stat", "stat returned non-zero");
        return;
    }
    if (st_a.st_ino != st_b.st_ino || st_a.st_nlink != 2 || st_b.st_nlink != 2) {
        fail("hard-link: inode/link count", "hard links do not share inode metadata");
        return;
    }

    if (unlink(path_a) != 0) {
        fail("hard-link: unlink first", "unlink returned non-zero");
        return;
    }
    fd = open(path_b, O_RDONLY);
    if (fd < 0) {
        fail("hard-link: survivor open", "remaining link could not be opened");
        return;
    }
    memset(buf, 0, sizeof(buf));
    if (read(fd, buf, sizeof(buf)) != (ssize_t)strlen(payload) ||
        memcmp(buf, payload, strlen(payload)) != 0) {
        fail("hard-link: survivor content", "remaining link lost file contents");
        close(fd);
        return;
    }
    close(fd);

    if (unlink(path_b) != 0) {
        fail("hard-link: unlink last", "unlink returned non-zero");
        return;
    }
    if (open(path_b, O_RDONLY) >= 0) {
        fail("hard-link: final removal", "last link still opens after unlink");
        return;
    }

    pass("hard link semantics");
}

/* Test 9: procfs files and device nodes */
static void test_procfs_and_devices(void) {
    char buf[4097];
    ssize_t n = 0;

    if (read_file_into("/proc/self/status", buf, sizeof(buf) - 1, &n) != 0 ||
        strstr(buf, "Pid:\t") == NULL || strstr(buf, "Name:\t") == NULL) {
        fail("procfs: status", "missing status fields");
        return;
    }

    if (read_file_into("/proc/meminfo", buf, sizeof(buf) - 1, &n) != 0 ||
        strstr(buf, "MemTotal:") == NULL || strstr(buf, "MemFree:") == NULL) {
        fail("procfs: meminfo", "missing meminfo lines");
        return;
    }

    if (read_file_into("/proc/self/maps", buf, sizeof(buf) - 1, &n) != 0 ||
        strstr(buf, "[stack]") == NULL) {
        fail("procfs: maps", "maps output missing stack mapping");
        return;
    }

    if (read_file_into("/proc/stat", buf, sizeof(buf) - 1, &n) != 0 ||
        strncmp(buf, "cpu ", 4) != 0) {
        fail("procfs: stat", "missing aggregate CPU line");
        return;
    }

    if (read_file_into("/proc/version", buf, sizeof(buf) - 1, &n) != 0 ||
        strstr(buf, "m3OS version") == NULL) {
        fail("procfs: version", "missing version string");
        return;
    }

    if (read_file_into("/proc/mounts", buf, sizeof(buf) - 1, &n) != 0 ||
        strstr(buf, "/proc") == NULL) {
        fail("procfs: mounts", "missing proc mount");
        return;
    }

    if (read_file_into("/proc/uptime", buf, sizeof(buf) - 1, &n) != 0 ||
        strchr(buf, '.') == NULL) {
        fail("procfs: uptime", "uptime did not contain a decimal value");
        return;
    }

    memset(buf, 0, sizeof(buf));
    n = readlink("/proc/self/exe", buf, sizeof(buf) - 1);
    if (n <= 0 || strstr(buf, "tmpfs-test") == NULL) {
        fail("procfs: exe", "readlink did not return running binary path");
        return;
    }

    memset(buf, 0, sizeof(buf));
    n = readlink("/proc/self/fd/1", buf, sizeof(buf) - 1);
    if (n <= 0) {
        fail("procfs: fd symlink", "readlink on stdout fd failed");
        return;
    }

    DIR *dir = opendir("/proc");
    if (dir == NULL) {
        fail("procfs: opendir", "could not open /proc");
        return;
    }
    int saw_self = 0;
    int saw_pid = 0;
    char pid_buf[16];
    snprintf(pid_buf, sizeof(pid_buf), "%d", getpid());
    for (struct dirent *ent = readdir(dir); ent != NULL; ent = readdir(dir)) {
        if (strcmp(ent->d_name, "self") == 0) {
            saw_self = 1;
        }
        if (strcmp(ent->d_name, pid_buf) == 0) {
            saw_pid = 1;
        }
    }
    closedir(dir);
    if (!saw_self || !saw_pid) {
        fail("procfs: directory listing", "missing self or pid entry");
        return;
    }

    int fd = open("/dev/zero", O_RDONLY);
    if (fd < 0) {
        fail("device: /dev/zero open", "open returned < 0");
        return;
    }
    unsigned char zeros[4096];
    if (read(fd, zeros, sizeof(zeros)) != (ssize_t)sizeof(zeros)) {
        fail("device: /dev/zero read", "short read");
        close(fd);
        return;
    }
    close(fd);
    for (size_t i = 0; i < sizeof(zeros); i++) {
        if (zeros[i] != 0) {
            fail("device: /dev/zero content", "non-zero byte observed");
            return;
        }
    }

    fd = open("/dev/urandom", O_RDONLY);
    if (fd < 0) {
        fail("device: /dev/urandom open", "open returned < 0");
        return;
    }
    unsigned char random_bytes[64];
    if (read(fd, random_bytes, sizeof(random_bytes)) != (ssize_t)sizeof(random_bytes)) {
        fail("device: /dev/urandom read", "short read");
        close(fd);
        return;
    }
    close(fd);
    int all_zero = 1;
    for (size_t i = 0; i < sizeof(random_bytes); i++) {
        if (random_bytes[i] != 0) {
            all_zero = 0;
            break;
        }
    }
    if (all_zero) {
        fail("device: /dev/urandom content", "buffer was all zeroes");
        return;
    }

    fd = open("/dev/full", O_WRONLY);
    if (fd < 0) {
        fail("device: /dev/full open", "open returned < 0");
        return;
    }
    errno = 0;
    if (write(fd, "x", 1) >= 0 || errno != ENOSPC) {
        fail("device: /dev/full write", "write did not fail with ENOSPC");
        close(fd);
        return;
    }
    close(fd);

    fd = open("/dev/null", O_WRONLY);
    if (fd < 0 || write(fd, "test", 4) != 4) {
        fail("device: /dev/null write", "write failed");
        if (fd >= 0) {
            close(fd);
        }
        return;
    }
    close(fd);

    pass("procfs + device nodes");
}

/* Test 10: DAC enforcement and umask */
static void test_permissions_and_umask(void) {
    const char *secret_path = "/tmp/root-only.txt";
    const char *dir_path = "/tmp/root-owned";
    struct stat st;

    int fd = open(secret_path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        fail("permissions: create root-only", "open returned < 0");
        return;
    }
    write(fd, "secret", 6);
    close(fd);

    if (mkdir(dir_path, 0555) != 0) {
        fail("permissions: mkdir", "mkdir returned non-zero");
        return;
    }

    pid_t child = fork();
    if (child < 0) {
        fail("permissions: fork", "fork returned < 0");
        return;
    }
    if (child == 0) {
        if (setuid(1000) != 0) {
            _exit(10);
        }
        if (open(secret_path, O_RDONLY) >= 0) {
            _exit(11);
        }
        if (open("/tmp/root-owned/child.txt", O_WRONLY | O_CREAT, 0644) >= 0) {
            _exit(12);
        }
        _exit(0);
    }

    int status = 0;
    if (waitpid(child, &status, 0) < 0 || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fail("permissions: non-root DAC", "non-root access was not denied");
        return;
    }

    fd = open(secret_path, O_RDONLY);
    if (fd < 0) {
        fail("permissions: root bypass", "root could not reopen protected file");
        return;
    }
    close(fd);

    mode_t old_mask = umask(0077);
    if (mkdir("/tmp/umask-private", 0777) != 0) {
        umask(old_mask);
        fail("umask: mkdir", "mkdir returned non-zero");
        return;
    }
    umask(old_mask);
    if (stat("/tmp/umask-private", &st) != 0) {
        fail("umask: stat", "stat returned non-zero");
        return;
    }
    if ((st.st_mode & 0777) != 0700) {
        fail("umask: mode", "mkdir did not apply 077 umask");
        return;
    }

    pass("permissions + umask");
}

/* Clean up test files */
static void cleanup(void) {
    unlink("/tmp/test.txt");
    unlink("/tmp/todelete.txt");
    unlink("/tmp/trunc.txt");
    unlink("/tmp/append.txt");
    unlink("/tmp/link.txt");
    unlink("/tmp/target.txt");
    unlink("/tmp/chain-target.txt");
    unlink("/tmp/chain-a");
    unlink("/tmp/chain-b");
    unlink("/tmp/chain-c");
    unlink("/tmp/loop-a");
    unlink("/tmp/loop-b");
    unlink("/tmp/self-loop");
    unlink("/hard-a.txt");
    unlink("/hard-b.txt");
    unlink("/tmp/root-only.txt");
    for (int i = 0; i < 41; i++) {
        char name[32];
        snprintf(name, sizeof(name), "/tmp/hop-%02d", i);
        unlink(name);
    }
    unlink("/tmp/umask-private");
    unlink("/tmp/root-owned/child.txt");
    rmdir("/tmp/umask-private");
    rmdir("/tmp/root-owned");
}

int main(void) {
    printf("[tmpfs-test] starting Phase 38 validation\n"); /* DevSkim: ignore DS154189 — format string is a literal */

    test_write_read_roundtrip();
    test_mkdir_rmdir();
    test_unlink();
    test_truncate();
    test_sequential_write();
    test_symlink_semantics();
    test_symlink_chain_and_loops();
    test_hard_links();
    test_procfs_and_devices();
    test_permissions_and_umask();
    cleanup();

    printf("[tmpfs-test] results: %d passed, %d failed\n", /* DevSkim: ignore DS154189 — format string is a literal */
           tests_passed, tests_failed);

    return tests_failed > 0 ? 1 : 0;
}
