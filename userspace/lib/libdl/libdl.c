/*
 * libdl.so — Phase 76c POSIX-libdl link-time stub.
 *
 * Real m3OS programs that call `dlopen` / `dlsym` / `dlclose` /
 * `dlerror` route through the dynamic linker (`ld-musl-x86_64.so.1`),
 * which exports those names through its own DT_HASH/DT_SYMTAB. The
 * linker self-injects into the bring-up DSO scope at slot 1 BEFORE
 * any DT_NEEDED libraries are loaded, so SysV symbol search resolves
 * `dlopen` etc. against the linker rather than against this stub.
 *
 * This file exists for the GNU ld link-time step only: a consumer
 * built with `-ldl` needs SOMETHING under `/usr/lib/libdl.so` that
 * advertises the four names. At runtime these stub implementations
 * are shadowed by the linker (because the linker is earlier in the
 * SysV first-found-wins search order); they are never called.
 *
 * No DT_NEEDED entries — built with `-nostdlib` so the bring-up
 * linker never has to chase a transitive dependency from this file.
 * `DT_SONAME = libdl.so` so consumers' DT_NEEDED resolves to that
 * name (and the runtime linker looks for /usr/lib/libdl.so).
 *
 * Hash style: SysV (`--hash-style=sysv`) so the dynamic linker's
 * DT_HASH walker (Phase 76b/c only — DT_GNU_HASH ships in 76d) can
 * still see these symbols if it ever needs to fall back to the
 * stub for any reason (paranoia bound — should never happen).
 */

/* If the linker's `dlopen` ever fails to override these stubs, the
 * call would land here with the same calling convention. Each stub
 * is "if you see this serial banner, the linker's self-injection
 * regressed" — but for the smoke gate to keep going we silently
 * return failure values rather than aborting. Phase 76c's smoke
 * test asserts the positive path, so a regression would manifest
 * as a `DLOPEN_TEST:FAIL` rather than a hang. */

void *dlopen(const char *path, int flags) {
    (void) path;
    (void) flags;
    return 0;
}

void *dlsym(void *handle, const char *name) {
    (void) handle;
    (void) name;
    return 0;
}

int dlclose(void *handle) {
    (void) handle;
    return -1;
}

char *dlerror(void) {
    return (char *) 0;
}
