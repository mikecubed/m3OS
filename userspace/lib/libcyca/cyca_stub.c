/*
 * libcyca_stub — Phase 76b F1.4 cycle-test build helper.
 *
 * Empty stub form of `cyca_func` that lets libcycb.so link against
 * libcyca.so before the real (cycle-closing) libcyca.so is built.
 * The stub is never staged to disk; it exists only as a link-time
 * placeholder.
 */

int cyca_func(void) {
    return 0;
}
