# less Patches

No patches are required against less 668 for the m3OS musl cross-toolchain.
The upstream tarball builds cleanly with `--with-regex=posix --without-shared`
and the m3OS POSIX termios contract is sufficient for less's raw-mode setup.
