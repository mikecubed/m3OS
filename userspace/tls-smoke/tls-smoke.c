// Phase 77 Track C — multi-threaded thread-local-storage smoke test.
//
// Proves the PT_TLS template the ELF loader now recognises is wired correctly
// end to end: each pthread must see its OWN copy of a `__thread` variable, and
// the main thread's copy must be untouched. Prints `TLS_SMOKE:PASS` on success
// or `TLS_SMOKE:FAIL <detail>` on any cross-thread bleed, exiting 0 / non-zero.
//
// Built as a full-musl static binary (musl folds libpthread into libc), so it
// also exercises clone(), per-thread FS-base (TLS), and futex-backed
// pthread_create/join on m3OS.

#include <pthread.h>
#include <stdio.h>
#include <unistd.h>

#define NTHREADS 4

// The TLS variable under test. The initialized value (42) comes from the
// PT_TLS `.tdata` image; each thread gets its own copy.
static __thread int tls_x = 42;

// Per-thread observations, indexed by thread id; written by each worker.
static int observed[NTHREADS];
// Each worker also records the value of `tls_x` it saw at entry (must be the
// initializer 42 — proving the .tdata template was copied into the new thread).
static int initial_seen[NTHREADS];

static void *worker(void *arg) {
    int idx = (int)(long)arg;
    initial_seen[idx] = tls_x; // should be the .tdata initializer (42)
    tls_x = 1000 + idx;        // write this thread's private copy
    // Spin a little so the threads genuinely overlap and any shared-storage
    // bug would corrupt a neighbour's value before we read it back.
    for (volatile long i = 0; i < 200000; i++) {
    }
    observed[idx] = tls_x; // read back this thread's private copy
    return NULL;
}

int main(void) {
    pthread_t t[NTHREADS];
    for (int i = 0; i < NTHREADS; i++) {
        if (pthread_create(&t[i], NULL, worker, (void *)(long)i) != 0) {
            printf("TLS_SMOKE:FAIL pthread_create thread %d\n", i);
            fflush(stdout);
            return 1;
        }
    }
    for (int i = 0; i < NTHREADS; i++) {
        pthread_join(t[i], NULL);
    }

    int ok = (tls_x == 42); // main thread's copy must be untouched
    for (int i = 0; i < NTHREADS; i++) {
        if (initial_seen[i] != 42) {
            ok = 0;
        }
        if (observed[i] != 1000 + i) {
            ok = 0;
        }
    }

    if (ok) {
        printf("TLS_SMOKE:PASS\n");
    } else {
        printf(
            "TLS_SMOKE:FAIL main_x=%d init=[%d,%d,%d,%d] obs=[%d,%d,%d,%d]\n",
            tls_x, initial_seen[0], initial_seen[1], initial_seen[2],
            initial_seen[3], observed[0], observed[1], observed[2], observed[3]);
    }
    fflush(stdout);
    return ok ? 0 : 1;
}
