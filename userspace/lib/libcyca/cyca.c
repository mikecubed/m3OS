/*
 * libcyca — Phase 76b F1.4 cycle-test library A.
 *
 * Final-build form: defines `cyca_func` and references `cycb_func`
 * via `DT_NEEDED = libcycb.so`. At runtime the bring-up linker
 * detects the libcyca ↔ libcycb cycle and exits with `exit(80)`
 * (ELIBBAD).
 *
 * Build sequence: a stub libcyca.so with no DT_NEEDED is built
 * first so libcycb.so can link against it. Then libcycb.so is
 * built. Finally THIS source is rebuilt linking against libcycb.so
 * so its DT_NEEDED records the back-edge that closes the cycle.
 */

extern int cycb_func(void);

int cyca_func(void) {
    return cycb_func() + 1;
}
