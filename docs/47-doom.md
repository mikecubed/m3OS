# DOOM Port

**Aligned Roadmap Phase:** Phase 47
**Status:** Complete
**Source Ref:** phase-47
**Supersedes Legacy Doc:** (none - new content)

## Overview

Phase 47 is where m3OS stops proving isolated subsystems and starts carrying a real
graphical application end to end. It adds framebuffer mapping, raw scancode input,
doomgeneric build integration, shareware WAD delivery on the ext2 image, and the
debugging work needed to keep a real game responsive under QEMU. The end result is a
working DOOM port with known remaining performance headroom.

## What This Doc Covers

- Framebuffer ownership, userspace mapping, and software blitting
- Raw PS/2 scancode delivery for gameplay input
- doomgeneric integration and the patch-overlay build flow
- The real bugs uncovered by the port: timing granularity, WAD I/O pressure, hold-key
  freezes, and stuck-key releases
- What remains for later performance and UX work

## Core Implementation

### Framebuffer takeover and userspace rendering

The kernel exposes the UEFI framebuffer to userspace through custom syscalls and a
simple ownership model. A graphical program can query framebuffer geometry, map the
pixel memory into its own address space, and temporarily take ownership of the
display. While DOOM owns the framebuffer, the normal text console yields and serial
or telnet remain the fallback control paths.

The m3OS platform layer in `userspace/doom/dg_m3os.c` renders DOOM's software frame
into the mapped framebuffer. This phase deliberately stays simple: software scaling,
software blitting, and raw pixel writes, with no window system or GPU acceleration.

### Raw keyboard input path

DOOM needs make/break semantics, not cooked terminal input, so the kernel exposes raw
PS/2 scancodes through a dedicated game-input path. When a process owns the
framebuffer, keyboard IRQs route bytes into `RAW_SCANCODE_BUF`; userspace polls
`sys_read_scancode()` and `DG_GetKey()` translates set-1 scancodes into DOOM key
events.

This looks straightforward on paper, but it turned out to be the hardest part of the
phase because small contract mismatches at either end of the pipeline immediately show
up as frozen movement or stuck releases during play.

### doomgeneric build and patch overlay

`xtask` clones doomgeneric into `target/doomgeneric-src/doomgeneric/` during the
build, then copies any files from `userspace/doom/patches/` over the upstream tree
before compiling. That overlay mechanism is the persistent way to carry small local
engine fixes without maintaining a full fork in the repository. The final DOOM binary
is embedded as `kernel/initrd/doom`, while the shareware `doom1.wad` is placed on the
ext2 data disk at `/usr/share/doom/doom1.wad`.

### What actually broke during bring-up

Porting a real game exposed failure modes that unit tests and small shell programs did
not:

1. **Timing and frame pacing:** coarse timing paths and scheduler-dependent short
   sleeps produced visible hitching. The final kernel uses TSC-backed wall-clock time
   and short-sleep handling in `kernel/src/arch/x86_64/syscall.rs`, which is good
   enough for DOOM's 35 Hz tic loop.
2. **WAD I/O pressure:** large WAD reads hit ext2 hot paths hard enough to expose
   cache pressure and extra copy/allocation work. `kernel/src/fs/ext2.rs` now keeps a
   larger block cache and provides `read_block_into_slice()` for the hot file-data
   path.
3. **Hold-key freeze:** `DG_GetKey()` originally returned `0` for filtered bytes such
   as PS/2 `0xE0` prefixes and typematic repeats. DOOM treats `0` as "queue empty", so
   the drain loop stopped early and gameplay could freeze while a key was held. The
   final implementation drains filtered bytes internally and returns `0` only when the
   raw queue is truly empty.
4. **Stuck keys after release:** two separate release bugs remained even after the
   drain-loop fix. `userspace/doom/patches/i_input.c` removes an upstream
   key-up-only `break` that delayed later release events, and
   `kernel/src/arch/x86_64/interrupts.rs` now drains all pending i8042 bytes per
   keyboard interrupt so extended break codes are not stranded by a single-byte read.

One useful lesson from this phase is that heuristic recovery can make the wrong bug
harder to see. A timestamp-based userspace stuck-key workaround was tried during
debugging and then removed because it fought the real producer/consumer contract bugs
instead of fixing them.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/arch/x86_64/syscall.rs` | Framebuffer syscalls, raw scancode syscall, TSC-backed `gettimeofday()` and short `nanosleep()` behavior |
| `kernel/src/arch/x86_64/interrupts.rs` | Keyboard IRQ handling, raw scancode buffers, bounded i8042 drain loop |
| `kernel/src/fs/ext2.rs` | WAD read hot path, enlarged block cache, zero-copy `read_block_into_slice()` helper |
| `kernel/src/fb/mod.rs` | Framebuffer ownership, console yield/restore path |
| `userspace/doom/dg_m3os.c` | m3OS doomgeneric platform layer: framebuffer init, blit path, key translation, timing wrappers |
| `userspace/doom/patches/i_input.c` | Persistent local patch removing the upstream key-up drain asymmetry |
| `xtask/src/main.rs` | doomgeneric clone/build flow, patch overlay, WAD population on the ext2 image |

## How This Phase Differs From Later Work

- Phase 47 uses direct framebuffer mapping and software blits. Later graphics work can
  add mouse input, audio, better blit strategies, or eventually a richer graphics
  stack.
- Input is raw PS/2 polling. Later phases can add higher-level device abstractions
  rather than exposing scancodes directly to applications.
- The current result is functionally playable, but not performance-tuned. Later work
  should add instrumentation first, then optimize with data instead of ad-hoc tweaks.

## Related Roadmap Docs

- [Phase 47 roadmap doc](./roadmap/47-doom.md)
- [Phase 47 task doc](./roadmap/tasks/47-doom-tasks.md)

## Deferred or Later-Phase Topics

- Mouse input for graphical programs (Phase 48)
- Audio output and DOOM sound/music support (Phase 49)
- Frame/tic/blit instrumentation for performance tuning
- Write-combining framebuffer mappings or smaller dirty-rectangle blits
- User-supplied IWAD workflow and broader WAD compatibility
