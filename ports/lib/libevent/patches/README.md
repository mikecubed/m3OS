# libevent Patches

No patches are required against libevent 2.1.12-stable for the m3OS musl
cross-toolchain. The upstream build supports `--disable-openssl
--disable-samples --disable-debug-mode` and the m3OS userspace exposes
epoll, select, and pipe — sufficient for libevent's portable backend.
