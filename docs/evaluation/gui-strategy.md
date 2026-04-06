# GUI Strategy: From Framebuffer Console to a Redox-Like Desktop

## Bottom line

If the goal is "something like Redox", the right target is **not** "map the framebuffer into apps and stop there." The right target is a **userspace display server/compositor with a small kernel graphics/input substrate**.

The current repo is closer to "graphics-capable kernel substrate" than to "desktop system":

- `kernel/src/fb/mod.rs` implements a framebuffer text console
- `docs/09-framebuffer-and-shell.md` already hints at moving framebuffer ownership to a dedicated display server later
- `docs/roadmap/47-doom.md` proposes raw framebuffer access for a single graphical app
- `docs/roadmap/48-mouse-input.md` and `docs/roadmap/49-audio.md` are still planned, not integrated

## Why this needs detailed planning

A GUI effort here is not "just add windows." A serious desktop path touches multiple layers at once:

| Layer | What is needed |
|---|---|
| Display ownership | one process or service controls presentation |
| Buffer exchange | apps need a way to hand pixels to the compositor efficiently |
| Input model | keyboard and mouse events need focus-aware routing |
| Session/lifecycle | graphical login, launcher, and shutdown semantics |
| Text/fonts | terminal emulation, rasterization, and text layout |
| Toolkit/widgets | buttons, lists, menus, dialogs, scrolling, focus |
| App conventions | clipboard, file chooser, settings, notifications, window controls |
| Packaging/runtime | enough ecosystem to ship graphical apps without heroic per-app effort |

## Why GUI work is also microkernel work

The display stack is not just a user-facing feature area. For m3OS, it is also the most natural place to **start enforcing the microkernel boundary more seriously**.

Why:

1. a display server is naturally a userspace service
2. focus, composition, and window policy obviously do not belong in ring 0
3. the graphics path forces the project to solve real shared-buffer and input-routing problems instead of relying on kernel-pointer shortcuts
4. the same infrastructure needed for a real display server is useful for other serverized subsystems

That means GUI work is not separate from microkernel work. It is one of the cleanest paths into it. The broader migration context is in [microkernel-path.md](./microkernel-path.md).

## Starting point

```mermaid
flowchart LR
    subgraph Today["Today"]
        KFB["Kernel framebuffer text console"]
        KBD["Keyboard path"]
        APPS["Text-mode apps"]
    end

    APPS --> KFB
    KBD --> APPS
```

That is enough for:

- framebuffer text UI
- a DOOM-style "one app owns the screen" milestone

It is not enough for:

- multiple windows
- focus management
- mouse routing
- clipping and damage tracking
- compositing
- a graphical login/session

## Option space

| Option | What it is | Good for | Main downside | Recommendation |
|---|---|---|---|---|
| Raw framebuffer mmap | Map the linear framebuffer directly into one app | DOOM, demos, bring-up | Becomes an ABI dead end for multi-app graphics | Use only as a short-term proof point |
| `fbdev`-like `/dev/fb0` | Device API plus mmap/ioctl-style control | Cleaner than one-off syscalls; easier for ports | Still fundamentally single-client | Better than raw mmap, but still not a desktop model |
| Orbital-like display server | One userspace server owns display, input focus, and composition | Best fit for m3OS and closest to Redox | Requires protocol design and software composition | **Recommended direction** |
| Wayland compositor first | Adopt a modern ecosystem protocol immediately | Future ecosystem alignment | Massive dependency and toolchain burden too early | Too early for m3OS right now |

## Recommended target architecture

```mermaid
flowchart LR
    subgraph Kernel["Kernel substrate"]
        FB["Framebuffer device"]
        IN["Input event queue"]
        SHM["Shared-memory / mmap buffers"]
        T["Timers"]
    end

    subgraph User["Userspace graphics stack"]
        DS["display-server / compositor"]
        WM["window + focus policy"]
        TK["minimal GUI toolkit"]
        APPS["GUI applications"]
    end

    APPS <-->|Unix sockets + shared buffers| DS
    WM --> DS
    IN --> DS
    DS --> FB
    T --> DS
    TK --> APPS
```

This matches m3OS better than a Wayland-first approach because:

1. Unix domain sockets already exist (`docs/roadmap/39-unix-domain-sockets.md`).
2. `mmap`, `munmap`, and related VM machinery already exist (`docs/33-kernel-memory.md`, `docs/36-expanded-memory.md`).
3. The project already documents a microkernel philosophy where high-level services belong outside the kernel.
4. The display server is one of the easiest places to turn that philosophy into an enforced boundary.
5. A Redox-like path is philosophically closer to a small custom display server than to pulling in a large Linux graphics stack early.

## Practical staged plan

### Phase A: single-app graphics proof

Purpose: prove that graphical applications can run at all.

Suggested scope:

- expose framebuffer access through a device-style API rather than a custom long-term syscall contract
- expose raw keyboard input for one foreground graphical client
- ship the DOOM milestone as a proof of graphical capability, not as the desktop architecture

### Phase B: input and event model

Purpose: stop treating input as a one-off keyboard shortcut path.

Suggested scope:

- finish PS/2 mouse support for QEMU
- define a unified input event format early, even if it is initially minimal
- route focus and input ownership through one foreground graphics service

### Phase C: display server

Purpose: enable more than one graphical client.

Suggested scope:

- one display server process owns the framebuffer
- clients submit buffers or damage rectangles
- the server composites and presents
- basic focus, stacking order, and pointer routing live in userspace
- the shared-buffer path should be designed in a way that can later serve storage and networking too, rather than becoming a graphics-only special case

### Phase D: desktop usability layer

Purpose: make the GUI feel like a system instead of a demo.

Suggested scope:

- terminal emulator
- graphical launcher or session menu
- graphical editor or file browser
- clipboard, font rendering, icons, and image decoding
- graphical login/session startup

### Phase E: ecosystem bridge

Purpose: make the GUI stack worth investing in for more than one showcase app.

Suggested scope:

- stable client protocol for surfaces/windows/input
- at least one reusable toolkit layer
- packaging story for graphical applications
- a clear stance on whether m3OS wants a custom GUI ecosystem first or compatibility bridges later

## What Redox has that m3OS does not

Redox already has the key "middle layer" m3OS lacks:

- a working desktop/windowing story
- a display server/window manager instead of raw-fb-only demos
- enough graphics-facing ecosystem support to run real GUI applications

That does **not** mean m3OS needs to clone Redox exactly. It does mean the shortest path to "GUI like Redox" is to copy the **shape** of the solution:

- small kernel substrate
- userspace display ownership
- a protocol for windows/surfaces/input

## What not to do

1. **Do not freeze a raw framebuffer syscall as the long-term API.**
2. **Do not jump straight to Wayland or wlroots before the basic graphics substrate exists.**
3. **Do not treat DOOM as the desktop architecture.** It is a milestone, not the windowing model.
4. **Do not leave session management and service supervision out of the graphics plan.** GUI systems need those just as much as headless ones.

## Recommendation

The best path is:

1. use the Phase 47 DOOM work as a **graphics bring-up milestone**
2. immediately follow with **input abstraction**
3. then design and build a **userspace display server/compositor**
4. only after that consider higher-level compatibility layers or larger GUI toolkits

That path preserves m3OS's architectural clarity, stays close to the repo's own instincts, and gives the project the cleanest route toward a Redox-like GUI without pretending it is one short patch away.
