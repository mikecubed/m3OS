/*
 * minizip - simple zlib compression test utility for m3OS
 *
 * Demonstrates linking against the zlib port's libz.a.
 * Compresses stdin to stdout using zlib deflate, or tests that
 * zlib is properly linked by printing the version.
 */

#include <stdio.h>
#include <string.h>
#include <zlib.h>

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "--version") == 0) {
        printf("minizip 1.0 (zlib %s)\n", zlibVersion());
        return 0;
    }

    /* Default: print zlib version to confirm linkage */
    printf("minizip: zlib %s linked successfully\n", zlibVersion());

    /* Simple compression test */
    const char *test_input = "Hello from m3OS ports system!";
    unsigned char compressed[256];
    unsigned char decompressed[256];
    unsigned long comp_len = sizeof(compressed);
    unsigned long decomp_len = sizeof(decompressed);

    int ret = compress(compressed, &comp_len,
                       (const unsigned char *)test_input, strlen(test_input) + 1);
    if (ret != Z_OK) {
        fprintf(stderr, "compression failed: %d\n", ret);
        return 1;
    }
    printf("compressed %lu bytes to %lu bytes\n",
           (unsigned long)(strlen(test_input) + 1), comp_len);

    ret = uncompress(decompressed, &decomp_len, compressed, comp_len);
    if (ret != Z_OK) {
        fprintf(stderr, "decompression failed: %d\n", ret);
        return 1;
    }

    if (strcmp((char *)decompressed, test_input) == 0) {
        printf("round-trip OK: %s\n", (char *)decompressed);
    } else {
        fprintf(stderr, "round-trip mismatch!\n");
        return 1;
    }

    return 0;
}
