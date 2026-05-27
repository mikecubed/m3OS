/*
 * dlopen_test — Phase 76c end-to-end smoke binary.
 *
 * Built as a dynamic ELF that:
 *   - carries `PT_INTERP = /lib/ld-musl-x86_64.so.1`
 *   - has `DT_NEEDED = libdl.so` (resolved by `-ldl` against the
 *     Phase 76c stub at `/usr/lib/libdl.so`; the real implementations
 *     are exported by the dynamic linker, which self-injects at the
 *     front of the SysV symbol search scope so its `dlopen` /
 *     `dlsym` / `dlclose` / `dlerror` shadow the libdl.so stubs.)
 *
 * Positive path:
 *   1. dlopen("/usr/lib/libhello.so", RTLD_NOW)   → non-NULL handle
 *   2. dlsym(handle, "hello_str")                  → non-NULL fn pointer
 *   3. call ptr() → must return "HELLO_FROM_SHARED_LIB:OK"
 *   4. Open libhello_fini.so to exercise the destructor pipeline
 *   5. Print "DLOPEN_TEST:FINI_PENDING" before closing libhello_fini
 *   6. dlclose(libhello_fini_handle) → must return 0; the destructor
 *      writes "LIBHELLO_FINI:RAN\n" to stderr
 *   7. dlclose(libhello_handle) → must return 0
 *   8. Print "DLOPEN_TEST:PASS"
 *
 * Negative paths (all must populate dlerror() and return NULL / -1):
 *   - missing library:        dlopen("/usr/lib/libnope.so", RTLD_NOW)
 *   - missing symbol:         dlsym(handle, "nonexistent_symbol")
 *   - double-close:           dlclose(handle) twice
 *   - close of never-opened:  dlclose(0xDEADBEEF)
 *
 * Built (see xtask::build_dlopen_test) with:
 *   musl-gcc -nostdlib -nostartfiles -fPIC -Wl,-pie \
 *     -Wl,-dynamic-linker=/lib/ld-musl-x86_64.so.1 \
 *     -Wl,--hash-style=sysv -Wl,-rpath,/usr/lib \
 *     dlopen_test.c <libdl.so> -o dlopen_test
 *
 * The binary uses no libc — every syscall goes through inline-asm
 * `syscall` so the only DT_NEEDED is `libdl.so` (which resolves to
 * the linker's exports at runtime).
 */

#define RTLD_NOW 2

/* ----------------------------------------------------------------- */
/* Inline-asm syscall stubs (no libc available).                     */
/* ----------------------------------------------------------------- */

static long sys_write(long fd, const char *buf, long len) {
    long ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "0"(1), "D"(fd), "S"(buf), "d"(len)
        : "rcx", "r11", "memory");
    return ret;
}

__attribute__((noreturn))
static void sys_exit(long code) {
    __asm__ volatile(
        "syscall"
        :
        : "a"(60), "D"(code)
        : "rcx", "r11", "memory");
    __builtin_unreachable();
}

/* ----------------------------------------------------------------- */
/* libdl prototypes — resolved at runtime against the linker's exports. */
/* ----------------------------------------------------------------- */

extern void *dlopen(const char *path, int flags);
extern void *dlsym(void *handle, const char *name);
extern int dlclose(void *handle);
extern char *dlerror(void);

/* ----------------------------------------------------------------- */
/* String / printing helpers — no libc, all hand-rolled.             */
/* ----------------------------------------------------------------- */

static long c_strlen(const char *s) {
    long n = 0;
    while (s[n] != '\0') n++;
    return n;
}

static int c_streq(const char *a, const char *b) {
    long i = 0;
    while (a[i] == b[i]) {
        if (a[i] == '\0') return 1;
        i++;
    }
    return 0;
}

static void puts1(const char *s) {
    sys_write(1, s, c_strlen(s));
    sys_write(1, "\n", 1);
}

/* "EXPECT:<name> got=NULL" / "EXPECT:<name> got=<msg>" diagnostic
 * for negative paths. The smoke gate doesn't assert on these lines
 * but a developer reading the serial log can find out what went
 * wrong. */
static void diag_label_msg(const char *label, const char *msg) {
    sys_write(2, label, c_strlen(label));
    if (msg) {
        sys_write(2, ": ", 2);
        sys_write(2, msg, c_strlen(msg));
    } else {
        sys_write(2, ": (no dlerror message)", 22);
    }
    sys_write(2, "\n", 1);
}

/* ----------------------------------------------------------------- */
/* Test entry.                                                       */
/* ----------------------------------------------------------------- */

void _start(void) {
    /* ----- positive path: open libhello, call hello_str, close ---- */
    void *h = dlopen("/usr/lib/libhello.so", RTLD_NOW);
    if (h == 0) {
        puts1("DLOPEN_TEST:FAIL libhello dlopen returned NULL");
        diag_label_msg("dlopen libhello", dlerror());
        sys_exit(1);
    }
    typedef const char *(*hello_fn)(void);
    hello_fn fn = (hello_fn) dlsym(h, "hello_str");
    if (fn == 0) {
        puts1("DLOPEN_TEST:FAIL dlsym(hello_str) returned NULL");
        diag_label_msg("dlsym hello_str", dlerror());
        sys_exit(1);
    }
    const char *got = fn();
    if (got == 0 || !c_streq(got, "HELLO_FROM_SHARED_LIB:OK")) {
        puts1("DLOPEN_TEST:FAIL hello_str returned wrong string");
        sys_exit(1);
    }

    /* Repeat dlopen of same SONAME — must succeed (refcount path). */
    void *h2 = dlopen("/usr/lib/libhello.so", RTLD_NOW);
    if (h2 == 0) {
        puts1("DLOPEN_TEST:FAIL repeat-open of libhello returned NULL");
        diag_label_msg("dlopen libhello (refcount)", dlerror());
        sys_exit(1);
    }

    /* ----- negative: missing symbol --------------------------------- */
    void *bad_sym = dlsym(h, "this_symbol_does_not_exist");
    if (bad_sym != 0) {
        puts1("DLOPEN_TEST:FAIL dlsym of missing symbol returned non-NULL");
        sys_exit(1);
    }
    char *err1 = dlerror();
    if (err1 == 0) {
        puts1("DLOPEN_TEST:FAIL dlerror() after missing-symbol returned NULL");
        sys_exit(1);
    }
    /* dlerror is read-and-clear; second call without intervening
     * failure must return NULL. */
    char *err1_again = dlerror();
    if (err1_again != 0) {
        puts1("DLOPEN_TEST:FAIL dlerror() did not clear after first read");
        sys_exit(1);
    }

    /* ----- negative: missing library -------------------------------- */
    void *missing = dlopen("/usr/lib/libdoesnotexist.so", RTLD_NOW);
    if (missing != 0) {
        puts1("DLOPEN_TEST:FAIL dlopen of missing library returned non-NULL");
        sys_exit(1);
    }
    char *err2 = dlerror();
    if (err2 == 0) {
        puts1("DLOPEN_TEST:FAIL dlerror() after missing-library returned NULL");
        sys_exit(1);
    }

    /* ----- negative: close of never-opened handle ------------------ */
    int bad_close = dlclose((void *) 0xDEADBEEFul);
    if (bad_close == 0) {
        puts1("DLOPEN_TEST:FAIL dlclose of bogus handle returned 0");
        sys_exit(1);
    }
    char *err3 = dlerror();
    if (err3 == 0) {
        puts1("DLOPEN_TEST:FAIL dlerror() after bad-handle close returned NULL");
        sys_exit(1);
    }

    /* ----- destructor pipeline: libhello_fini ---------------------- */
    void *hf = dlopen("/usr/lib/libhello_fini.so", RTLD_NOW);
    if (hf == 0) {
        puts1("DLOPEN_TEST:FAIL libhello_fini dlopen returned NULL");
        diag_label_msg("dlopen libhello_fini", dlerror());
        sys_exit(1);
    }
    /* Pending sentinel goes out BEFORE the close so the smoke gate
     * can verify the ordering FINI_PENDING → LIBHELLO_FINI:RAN → PASS. */
    puts1("DLOPEN_TEST:FINI_PENDING");
    int rc_hf = dlclose(hf);
    if (rc_hf != 0) {
        puts1("DLOPEN_TEST:FAIL dlclose(libhello_fini) did not return 0");
        sys_exit(1);
    }

    /* ----- close libhello (refcount goes from 2 → 1 → 0 over the
     * two dlclose calls below) --------------------------------------- */
    int rc1 = dlclose(h2);
    if (rc1 != 0) {
        puts1("DLOPEN_TEST:FAIL first dlclose(libhello) did not return 0");
        sys_exit(1);
    }
    int rc2 = dlclose(h);
    if (rc2 != 0) {
        puts1("DLOPEN_TEST:FAIL second dlclose(libhello) did not return 0");
        sys_exit(1);
    }

    /* ----- negative: double-close ---------------------------------- */
    /* Both handles `h` and `h2` are now consumed; closing `h` a
     * third time must error. */
    int rc3 = dlclose(h);
    if (rc3 == 0) {
        puts1("DLOPEN_TEST:FAIL triple dlclose(libhello) returned 0");
        sys_exit(1);
    }
    char *err4 = dlerror();
    if (err4 == 0) {
        puts1("DLOPEN_TEST:FAIL dlerror() after triple-close returned NULL");
        sys_exit(1);
    }

    puts1("DLOPEN_TEST:PASS");
    sys_exit(0);
}
