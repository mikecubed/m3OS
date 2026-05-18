# tmux Patches

No patches are required against tmux 3.5a for the m3OS musl
cross-toolchain.  The upstream build supports the following configure
flags, and the Phase 22/29 PTY layer plus the Phase 69a termios contract
are sufficient for tmux's nested-PTY behaviour:

```
--enable-utempter=no --enable-systemd=no --disable-debug
```
