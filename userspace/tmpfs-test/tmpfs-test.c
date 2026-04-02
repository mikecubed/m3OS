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
#include <fcntl.h>
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

/* Clean up test files */
static void cleanup(void) {
    unlink("/tmp/test.txt");
    unlink("/tmp/todelete.txt");
    unlink("/tmp/trunc.txt");
    unlink("/tmp/append.txt");
    unlink("/tmp/link.txt");
    unlink("/tmp/target.txt");
}

int main(void) {
    printf("[tmpfs-test] starting Phase 13 validation\n"); /* DevSkim: ignore DS154189 — format string is a literal */

    test_write_read_roundtrip();
    test_mkdir_rmdir();
    test_unlink();
    test_truncate();
    test_sequential_write();
    test_symlink_semantics();
    cleanup();

    printf("[tmpfs-test] results: %d passed, %d failed\n", /* DevSkim: ignore DS154189 — format string is a literal */
           tests_passed, tests_failed);

    return tests_failed > 0 ? 1 : 0;
}
