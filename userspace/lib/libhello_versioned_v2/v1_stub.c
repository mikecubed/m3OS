/*
 * Link-time stub library — provides a `hello_str@LIBHELLO_1.0`
 * symbol so the consumer (`dynlink_hello_versioned_mismatch`) can
 * be linked even though the REAL v2 library only defines
 * `LIBHELLO_2.0`. SONAME matches the real lib so the consumer's
 * `DT_NEEDED` resolves to the v2 lib at runtime.
 *
 * Never staged on disk — only feeds the static linker.
 */

const char *hello_str(void) {
    return "stub";
}
