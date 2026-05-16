# htop Patches

No patches are required against htop 3.4.0 for the m3OS musl
cross-toolchain. The upstream tarball builds cleanly with `--disable-hwloc
--enable-unicode --disable-affinity` against ncursesw 6.5.

The /proc reader in `linux/LinuxProcessList.c` degrades gracefully when
optional fields are missing — m3OS' partial /proc surface results in a
shorter process list but no parse errors.
