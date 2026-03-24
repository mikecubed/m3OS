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
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

static int tests_passed = 0;
static int tests_failed = 0;

static void pass(const char *name) {
    printf("  PASS: %s\n", name);
    tests_passed++;
}

static void fail(const char *name, const char *reason) {
    printf("  FAIL: %s — %s\n", name, reason);
    tests_failed++;
}

/* Test 1: create, write, close, reopen, read back */
static void test_write_read_roundtrip(void) {
    const char *path = "/tmp/test.txt";
    const char *msg = "Hello from tmpfs!";
    size_t msg_len = strlen(msg);

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
    write(fd, "x", 1);
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
    write(fd, "abcdefghij", 10);
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

/* Test 5: write appends at correct offset */
static void test_append(void) {
    const char *path = "/tmp/append.txt";
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fail("append: create", "open returned < 0");
        return;
    }
    write(fd, "AAA", 3);
    write(fd, "BBB", 3);
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
    pass("sequential write (append)");
}

/* Clean up test files */
static void cleanup(void) {
    unlink("/tmp/test.txt");
    unlink("/tmp/todelete.txt");
    unlink("/tmp/trunc.txt");
    unlink("/tmp/append.txt");
}

int main(void) {
    printf("[tmpfs-test] starting Phase 13 validation\n");

    test_write_read_roundtrip();
    test_mkdir_rmdir();
    test_unlink();
    test_truncate();
    test_append();
    cleanup();

    printf("[tmpfs-test] results: %d passed, %d failed\n",
           tests_passed, tests_failed);

    return tests_failed > 0 ? 1 : 0;
}
