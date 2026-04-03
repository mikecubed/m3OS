# Phase 47 - DOOM

## Milestone Goal

DOOM runs inside the OS. The shareware `doom1.wad` loads from disk, renders to the
framebuffer, and accepts keyboard input for gameplay. This is the "it runs DOOM"
milestone — the classic proof that a hobby OS has reached a meaningful level of
capability.

## Learning Goals

- Understand what a real graphical application needs from the OS: raw framebuffer
  access, input events, timing, and large file I/O.
- Learn how the DOOM rendering pipeline works: BSP traversal, palette-indexed 320x200
  software rendering, scaled blit to native resolution.
- See how a minimal platform abstraction layer bridges a large C codebase to a new OS.
- Experience porting a real program — the gap between "all syscalls pass tests" and
  "a real program actually works" teaches more than any unit test.

## Feature Scope

### Kernel: Framebuffer Access Syscall

The UEFI bootloader provides a raw pixel framebuffer (typically 1024x768 ARGB in QEMU
GUI mode). Currently only the kernel can write to it. Expose it to userspace:

**New syscall: `sys_framebuffer_info`**
- Returns framebuffer metadata to userspace: base physical address, width, height,
  pitch (bytes per row), bytes per pixel, pixel format (RGB/BGR).
- Alternatively, implement as an ioctl on `/dev/fb0`.

**New syscall: `sys_framebuffer_mmap`**
- Maps the framebuffer physical pages into the calling process's address space.
- Returns the virtual address of the mapped framebuffer.
- Alternatively, extend `mmap()` to support mapping `/dev/fb0`.

**Dual-mode console:**
- When a graphical program owns the framebuffer, the text console must yield.
- On program exit, restore the text console.
- Simplest approach: the graphical program takes over entirely; text console is
  only available via serial or telnet.

### Kernel: Raw Keyboard Input

The kernel captures PS/2 scancodes in a ring buffer (`SCANCODE_BUF` in `interrupts.rs`)
but only delivers cooked terminal input to userspace. DOOM needs key-down/key-up events.

**New syscall: `sys_read_scancode`** (or `/dev/input` device)
- Returns raw PS/2 scancodes (make/break codes).
- Non-blocking: returns 0 if no scancode available.
- DOOM's input loop polls this each frame.

**Scancode-to-key mapping:**
- DOOM uses its own key mapping; just deliver raw scancodes.
- Make codes (key down) and break codes (key up, 0x80 | make) are sufficient.

### Userspace: doomgeneric Platform Layer

[doomgeneric](https://github.com/ozkl/doomgeneric) is a portable DOOM source port
designed for exactly this use case. You implement 4 functions:

```c
void DG_Init();                          // open framebuffer, init input
void DG_DrawFrame();                     // blit 320x200 → native framebuffer
void DG_SleepMs(uint32_t ms);           // nanosleep wrapper
uint32_t DG_GetTicksMs();               // gettimeofday wrapper
```

Plus input handling via `DG_GetKey()` which returns key events.

**Platform implementation (~200 lines C):**
1. `DG_Init()` — call `sys_framebuffer_info`, `sys_framebuffer_mmap`, set up scaling.
2. `DG_DrawFrame()` — convert DOOM's 320x200 palette-indexed buffer to ARGB, scale
   to native resolution, copy to mapped framebuffer.
3. `DG_SleepMs()` — call `nanosleep()`.
4. `DG_GetTicksMs()` — call `gettimeofday()`, convert to milliseconds.
5. `DG_GetKey()` — call `sys_read_scancode()`, translate to DOOM key codes.

### WAD File on Disk

Place `doom1.wad` (shareware, ~4 MB) on the ext2 disk image at `/usr/share/doom/doom1.wad`.
The game opens and reads it via standard `open()`/`read()`/`lseek()`.

### Build Integration

- Cross-compile doomgeneric with `musl-gcc -static`.
- Add to the xtask build system alongside other C userspace programs.
- Binary goes to `kernel/initrd/doom.elf` or the ext2 disk image.

### Color Palette Conversion

DOOM uses a 256-color palette (VGA palette from the WAD file). The framebuffer is
32-bit ARGB. The platform layer must:
1. Read the PLAYPAL lump from the WAD (768 bytes: 256 RGB triplets).
2. Build a lookup table: `palette[256] → uint32_t ARGB`.
3. In `DG_DrawFrame()`, convert each pixel: `fb[y * pitch + x] = palette[doom_buf[y * 320 + x]]`.

### Scaling Strategy

DOOM renders 320x200. Native framebuffer is likely 1024x768.

**Option A: Nearest-neighbor 3x scale** → 960x600, centered in 1024x768.
**Option B: 2x scale** → 640x400, centered.
**Option C: Full-screen stretch** with integer scaling.

Nearest-neighbor is simplest and preserves the pixel art aesthetic.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 9 (Framebuffer) | Framebuffer exists in kernel |
| Phase 12 (POSIX Compat) | musl-linked C binary runs |
| Phase 24 (Persistent Storage) | WAD file on ext2 disk |

## Implementation Outline

1. Implement `sys_framebuffer_info` syscall — return dimensions, pitch, pixel format.
2. Implement `sys_framebuffer_mmap` syscall — map framebuffer into userspace.
3. Implement `sys_read_scancode` syscall — return raw PS/2 scancodes.
4. Clone doomgeneric source; write m3os platform layer (~200 lines).
5. Cross-compile with `musl-gcc -static`; add to xtask build.
6. Add `doom1.wad` to the ext2 disk image.
7. Boot with `cargo xtask run-gui` and test.
8. Tune scaling and input mapping.
9. Screenshot the running game for the README.

## Acceptance Criteria

- `doom` binary starts from the shell and displays the DOOM title screen.
- The framebuffer shows the game at a playable resolution.
- Keyboard input works: arrow keys move, Ctrl fires, Space opens doors, Enter selects menu items.
- The game runs at a playable frame rate (15+ FPS).
- WAD file loads from the ext2 disk without errors.
- Exiting DOOM (quit menu or Ctrl-C) returns to the shell.
- `cargo xtask run-gui` launches QEMU in GUI mode with the game playable.
- The shareware episode (E1M1 through E1M8) is completable.

## Companion Task List

- [Phase 47 Task List](./tasks/47-doom-tasks.md)

## How Real OS Implementations Differ

Real systems provide standardized graphics APIs:
- **Linux framebuffer** (`/dev/fb0`) with `fbdev` ioctls for mode setting.
- **DRM/KMS** (Direct Rendering Manager / Kernel Mode Setting) for modern GPU access.
- **X11/Wayland** compositing window managers for multi-application graphics.
- **OpenGL/Vulkan** hardware-accelerated 3D rendering.
- **ALSA/PulseAudio** for sound output.
- **evdev** (`/dev/input/event*`) for unified input events (keyboard, mouse, gamepad).

Our approach is much simpler: direct framebuffer mapping and raw scancodes. This is
closer to how DOS DOOM originally worked — direct VGA memory access at segment 0xA000
and BIOS keyboard interrupts.

## Deferred Until Later

- Sound output (PC speaker beeps, Sound Blaster emulation)
- Mouse input (PS/2 mouse driver)
- Network multiplayer (IPX/UDP)
- Save games to persistent storage (could work already via file I/O)
- DOOM II and commercial WAD support
- Higher-resolution rendering (GZDoom-style)
- Hardware-accelerated rendering
- Window manager / compositor for multi-app graphics
