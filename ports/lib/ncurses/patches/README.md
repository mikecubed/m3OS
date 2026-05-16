# ncurses Patches

No patches are required against ncurses 6.5 to build against the m3OS musl
cross-toolchain. The upstream autoconf macros detect musl correctly and the
m3OS-side termios contract (Phase 69a) is fully POSIX-conformant for every
ioctl ncurses issues.

If a patch becomes necessary in the future, drop a numbered `.patch` file in
this directory; the `cargo xtask port build ncurses` driver applies them in
sorted order with `patch -p1` after extracting the upstream tarball.
