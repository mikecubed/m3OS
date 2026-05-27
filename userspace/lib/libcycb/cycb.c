/*
 * libcycb — Phase 76b F1.4 cycle-test library B.
 *
 * Defines `cycb_func` and references `cyca_func` via
 * `DT_NEEDED = libcyca.so`. Built once, against the libcyca_stub
 * intermediate, so its DT_NEEDED records the half of the cycle the
 * runtime will walk.
 */

extern int cyca_func(void);

int cycb_func(void) {
    return cyca_func() + 2;
}
