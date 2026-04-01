/* mmap-leak-test.c — Phase 33: verify malloc/free (mmap/munmap) don't leak frames.
 *
 * Calls the kernel meminfo syscall (0x1001) to read the free frame count
 * before and after heavy allocation cycles. musl's malloc uses mmap for
 * large allocations (>= 128 KiB by default), and free calls munmap to
 * release them. If munmap correctly returns frames to the buddy allocator,
 * the free count should be restored (within a small tolerance for metadata).
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/syscall.h>

/* m3OS custom meminfo syscall number. */
#define SYS_MEMINFO 0x1001

/* Parse "free: <N>" from the meminfo output (Frames section). */
static long parse_free_frames(const char *buf, size_t len) {
    /* Look for "Frames" section header, then find "free: " within it. */
    const char *frames = NULL;
    for (size_t i = 0; i + 6 <= len; i++) {
        if (buf[i] == 'F' && buf[i+1] == 'r' && buf[i+2] == 'a' &&
            buf[i+3] == 'm' && buf[i+4] == 'e' && buf[i+5] == 's') {
            frames = &buf[i];
            break;
        }
    }
    if (!frames) return -1;

    /* Find "free: " after the Frames header. */
    size_t remaining = len - (size_t)(frames - buf);
    const char *p = NULL;
    for (size_t i = 0; i + 6 <= remaining; i++) {
        if (frames[i] == 'f' && frames[i+1] == 'r' && frames[i+2] == 'e' &&
            frames[i+3] == 'e' && frames[i+4] == ':' && frames[i+5] == ' ') {
            p = &frames[i + 6];
            break;
        }
    }
    if (!p) return -1;

    /* Parse the number. */
    long val = 0;
    while (*p >= '0' && *p <= '9') {
        val = val * 10 + (*p - '0');
        p++;
    }
    return val;
}

static long get_free_frames(void) {
    char buf[2048];
    long n = syscall(SYS_MEMINFO, buf, sizeof(buf));
    if (n <= 0) return -1;
    return parse_free_frames(buf, (size_t)n);
}

int main(void) {
    printf("mmap-leak-test: starting\n");

    /* Get baseline free frames. */
    long before = get_free_frames();
    if (before < 0) {
        printf("FAIL: could not read meminfo\n");
        return 1;
    }
    printf("free frames before: %ld\n", before);

    /* Allocate and free large blocks in cycles.
     * musl uses mmap for allocations >= 128 KiB (MMAP_THRESHOLD).
     * Each 256 KiB allocation should consume 64 frames via mmap,
     * and free should return them via munmap. */
    const int rounds = 5;
    const int allocs_per_round = 20;
    const size_t block_size = 256 * 1024; /* 256 KiB — well above MMAP_THRESHOLD */

    for (int r = 0; r < rounds; r++) {
        void *ptrs[20]; /* DevSkim: ignore DS161085 */
        for (int i = 0; i < allocs_per_round; i++) {
            ptrs[i] = malloc(block_size); /* DevSkim: ignore DS161085 */
            if (!ptrs[i]) {
                printf("FAIL: malloc returned NULL at round %d alloc %d\n", r, i);
                return 1;
            }
            /* Touch memory to ensure it's backed. */
            memset(ptrs[i], 0xAB, block_size); /* DevSkim: ignore DS154189 */
        }
        for (int i = 0; i < allocs_per_round; i++) {
            free(ptrs[i]);
        }
    }

    /* Get post-test free frames. */
    long after = get_free_frames();
    if (after < 0) {
        printf("FAIL: could not read meminfo after test\n");
        return 1;
    }
    printf("free frames after:  %ld\n", after);

    /* Allow a small tolerance for musl internal metadata and heap overhead.
     * musl retains a few mmap'd pages for its allocator state (thread-local
     * caches, malloc metadata, etc.). 32 frames = 128 KiB tolerance. */
    long leaked = before - after;
    printf("delta: %ld frames (%ld KiB)\n", leaked, leaked * 4);

    if (leaked > 32) {
        printf("FAIL: leaked %ld frames (> 32 tolerance)\n", leaked);
        return 1;
    }

    printf("PASS: no significant frame leak\n");
    return 0;
}
