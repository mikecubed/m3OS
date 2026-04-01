# Phase 52 - Node.js

## Milestone Goal

Node.js runs natively inside m3OS. The V8 JavaScript engine, libuv event loop, and
core Node.js modules (`fs`, `path`, `console`, `process`, `setTimeout`) work. With
TLS and DNS from Phase 51, the `https` and `net` modules also work, enabling npm
package installation.

## Learning Goals

- Understand how V8's JIT compiler uses `mmap(PROT_EXEC)` and `mprotect()` to emit
  and execute machine code at runtime.
- Learn how libuv's event loop wraps `epoll` for async I/O.
- See how a Go-runtime-style M:N threading model (goroutines) compares to libuv's
  thread pool model.

## Feature Scope

### Node.js Binary

Cross-compile Node.js with musl, fully static:

```bash
CC=x86_64-linux-musl-gcc CXX=x86_64-linux-musl-g++ LDFLAGS="-static" \
./configure --fully-static --without-npm --without-inspector --without-intl \
  --prefix=/usr
make -j$(nproc) && strip out/Release/node
```

**Binary:** ~50-80 MB static (no ICU, no inspector).

### Stage 1: REPL and Local Modules

With `--without-ssl` and `--without-cares`:
- Node.js REPL works
- `fs.readFileSync` / `fs.writeFileSync` work
- `setTimeout` / `setInterval` work (event loop functional)
- `process.platform`, `process.arch`, `process.version` work
- `JSON`, `Buffer`, `console`, `path`, `os` modules work

### Stage 2: Networking and npm

Rebuild with OpenSSL/BoringSSL and c-ares:
- `https.get()` works
- `net.connect()` works
- DNS resolution via c-ares
- npm bundled on disk (~50 MB)
- `npm install` works

### npm and Claude Code Preparation

With npm working, `npm install -g @anthropic-ai/claude-code` becomes possible,
which is the final prerequisite for Phase 53 (Claude Code).

See [Node.js roadmap](../nodejs-roadmap.md) for full details including V8/libuv
architecture diagrams and OS requirement comparisons.

## Dependencies

- **Phase 36** (Expanded Memory) — demand paging, `mprotect()` for V8 JIT
- **Phase 37** (I/O Multiplexing) — `epoll` for libuv event loop (**hard blocker**)
- **Phase 38** (Filesystem Enhancements) — `/proc/self/exe`, `/dev/null`, `/dev/urandom`
- **Phase 40** (Threading Primitives) — `clone(CLONE_THREAD)`, `futex` for libuv thread pool
- **Phase 42** (Crypto and TLS) — for Stage 2 (OpenSSL/BoringSSL rebuild)
- **Phase 51** (Networking and GitHub) — DNS resolution, `getrandom()` for Stage 2

## Acceptance Criteria

### Stage 1
- [ ] `node -e "console.log('hello from m3OS')"` works.
- [ ] `node -e "require('fs').writeFileSync('/tmp/test', 'ok')"` works.
- [ ] `node -e "setTimeout(() => console.log('timer!'), 100)"` fires the timer.
- [ ] `node -e "console.log(process.platform, process.arch)"` prints `linux x64`.

### Stage 2
- [ ] `node -e "require('https').get('https://httpbin.org/get', ...)"` completes.
- [ ] npm is bundled and `npm --version` works.
- [ ] `npm install -g` installs a package to `/usr/lib/node_modules/`.

## Deferred Items

- **Native addons (node-gyp)** — requires a C++ compiler on the OS; Clang from Phase 50
  could enable this eventually.
- **V8 inspector/debugger** — Chrome DevTools protocol; not needed.
- **ICU internationalization** — ~25 MB of data; not needed.
- **Worker threads** — `worker_threads` module; would need multiple V8 isolates.
