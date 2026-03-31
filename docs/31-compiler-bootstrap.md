# Phase 31 -- Compiler Bootstrap

## Overview

Phase 31 adds a native C compiler (TCC -- Tiny C Compiler) to m3OS. A C source
file written with the text editor can be compiled and executed without leaving
the OS. The ultimate milestone is self-hosting: TCC compiles itself inside the
OS, producing a working compiler binary.

## Architecture

```
Host (build time)                     Guest (m3OS, run time)
+-------------------------------+     +------------------------------------------+
| cargo xtask image             |     |  Shell (sh0/ion)                         |
|   build_tcc()                 |     |    $ tcc /usr/src/hello.c -o /tmp/hello  |
|     git clone tinycc          |     |    $ /tmp/hello                          |
|     ./configure --prefix=/usr |     |    hello, world                          |
|     make (musl-gcc -static)   |     |                                          |
|   populate_tcc_files()        |     |  Self-hosting:                           |
|     /usr/bin/tcc              |     |    $ tcc /usr/src/tcc/tcc.c -o /tmp/tcc2 |
|     /usr/lib/libc.a,crt*.o   |     |    $ /tmp/tcc2 --version                 |
|     /usr/include/* (musl)     |     |    tcc version 0.9.28rc ...              |
|     /usr/lib/tcc/include/*   |     |                                          |
|     /usr/src/hello.c          |     |  Edit-compile-run cycle:                 |
|     /usr/src/tcc/*            |     |    $ edit /tmp/prog.c                    |
+-------------------------------+     |    $ tcc /tmp/prog.c -o /tmp/prog        |
                                      |    $ /tmp/prog                           |
                                      +------------------------------------------+
```

## TCC Cross-Compilation

TCC is cross-compiled on the host during `cargo xtask image` (or `cargo xtask
run`). The build system:

1. Clones TCC source from `https://repo.or.cz/tinycc.git` (mob branch) into
   `target/tcc-src/`.
2. Configures with `./configure --prefix=/usr --cc=x86_64-linux-musl-gcc
   --extra-cflags="-static" --cpu=x86_64 --triplet=x86_64-linux-musl`.
3. Builds with `make -j4`.
4. Strips the resulting binary and stages it at `target/tcc-staging/usr/bin/tcc`.

The `--prefix=/usr` flag is critical: it tells TCC where to find headers
(`/usr/include`) and libraries (`/usr/lib`) at runtime inside the OS.

## musl Library Packaging

The musl C library artifacts are collected from the host system
(`/usr/lib/x86_64-linux-musl/`, `/usr/include/x86_64-linux-musl/`) and staged
alongside TCC:

| Artifact | Location on disk | Purpose |
|---|---|---|
| `libc.a` | `/usr/lib/libc.a` | Static C library |
| `crt1.o` | `/usr/lib/crt1.o` | Program entry point (calls `main`) |
| `crti.o` | `/usr/lib/crti.o` | Init section prologue |
| `crtn.o` | `/usr/lib/crtn.o` | Init section epilogue |
| musl headers | `/usr/include/*` | Standard C headers (~1.1 MB) |

## TCC-Specific Headers

TCC ships its own versions of certain compiler-intrinsic headers that override
the system ones:

- `stdarg.h`, `stddef.h`, `stdbool.h`, `float.h`, `varargs.h`, `tcclib.h`

These are installed at `/usr/lib/tcc/include/`. TCC searches this path before
the system include path.

## Filesystem Layout

All TCC artifacts are placed on the ext2 data disk (128 MB):

```
/usr/
  bin/
    tcc               -- TCC compiler binary (static, ~300 KB stripped)
  lib/
    libc.a            -- musl static C library (~2.5 MB)
    crt1.o, crti.o, crtn.o  -- CRT startup objects
    tcc/
      include/        -- TCC-specific headers
  include/
    stdio.h, stdlib.h, string.h, ...  -- musl system headers
    sys/, bits/, arpa/, net/, netinet/, ...
  src/
    hello.c           -- test program
    tcc/
      tcc.c, tcc.h, libtcc.c, ...  -- TCC source for self-hosting
```

## Kernel Syscall Fixes

Several kernel syscalls were enhanced for TCC compatibility:

### sys_execve (loading from disk)

Previously `sys_execve` only loaded ELF binaries from the ramdisk. Phase 31
adds a `read_file_from_disk()` helper that falls back to ext2, tmpfs, and FAT32
when the ramdisk lookup fails. This allows executing binaries compiled by TCC
and written to the filesystem.

### sys_mmap (PROT_EXEC)

The `mmap` syscall now honours the `PROT_EXEC` flag. When set, pages are mapped
without the `NO_EXECUTE` bit, allowing code execution from mmap'd memory. This
is required for TCC's `-run` mode (JIT compilation).

### sys_open (O_TRUNC on FAT32)

FAT32 file open now supports `O_TRUNC`: the old cluster chain is freed and the
file size is reset to 0, allowing TCC to overwrite output files.

### sys_access (disk filesystems)

The `access` syscall now checks ext2 and FAT32 filesystems in addition to
ramdisk, tmpfs, and device nodes.

## TCC Syscall Requirements

TCC exercises these syscalls during compilation:

| Syscall | Purpose | Status |
|---|---|---|
| `open` | Open source files, headers, output files | Working |
| `close` | Close file descriptors | Working |
| `read` | Read source code and headers | Working |
| `write` | Write object code and executables | Working |
| `lseek` | Seek within files | Working |
| `stat`/`fstat` | Get file sizes for buffer allocation | Working |
| `brk` | Heap allocation (compiler data structures) | Working |
| `mmap` | Large allocations, executable memory | Working (Phase 31) |
| `munmap` | Free large allocations | Stub (returns success) |
| `execve` | Run compiled binaries | Working (Phase 31) |
| `fork` | Process creation | Working |
| `exit` | Process termination | Working |
| `unlink` | Delete temporary files | Working |
| `access` | Check file existence | Working (Phase 31) |

## Build Process

```bash
cargo xtask run          # builds TCC, creates disk image, boots in QEMU
cargo xtask image        # builds TCC, creates disk image only
```

The TCC build is cached in `target/tcc-staging/`. To force a rebuild, delete
`target/tcc-staging/usr/bin/tcc`.

The ext2 data disk is cached at `target/x86_64-unknown-none/release/disk.img`.
To regenerate it (required after first Phase 31 build), delete this file.

## Deferred Items

- **GCC/Clang** -- too complex to port; TCC is sufficient for bootstrapping
- **Dynamic linking** -- all binaries remain statically linked
- **Debugger support** -- useful but not needed for the bootstrap milestone
- **Multi-stage bootstrap** -- removing host-compiled binaries from the trust chain
- **C++ support** -- TCC is C-only
- **Make/build tools** -- Phase 32
- **Package manager** -- Phase 37
