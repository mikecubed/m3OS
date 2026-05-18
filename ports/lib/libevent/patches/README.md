# libevent Patches

No patches are required against libevent 2.1.12-stable for the m3OS
musl cross-toolchain.  The upstream build supports the following
configure flags, and the m3OS userspace exposes epoll, select, and
pipe — sufficient for libevent's portable backend:

```
--disable-openssl --disable-samples --disable-debug-mode
```
