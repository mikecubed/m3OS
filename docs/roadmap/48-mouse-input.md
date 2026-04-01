# Phase 48 - Mouse Input

## Milestone Goal

The OS supports PS/2 mouse input. Userspace programs can read mouse movement deltas
and button state via a device file or syscall. The mouse is usable in the text editor
for cursor positioning and in graphical programs like DOOM for aiming.

## Learning Goals

- Understand the PS/2 mouse protocol: 3-byte packets, movement deltas, button bits.
- Learn how the auxiliary PS/2 port (IRQ 12) differs from the keyboard port (IRQ 1).
- See how input devices are abstracted behind a unified event model.
- Understand relative vs absolute pointing devices.

## Feature Scope

### PS/2 Mouse Driver

The PS/2 controller has two ports: port 1 (keyboard, IRQ 1) and port 2 (mouse, IRQ 12).
The mouse sends 3-byte packets:

| Byte | Content |
|---|---|
| 0 | Flags: Y overflow, X overflow, Y sign, X sign, always 1, middle btn, right btn, left btn |
| 1 | X movement delta (signed with bit from byte 0) |
| 2 | Y movement delta (signed with bit from byte 0) |

**Initialization sequence:**
1. Enable the auxiliary port on the PS/2 controller (command 0xA8).
2. Enable IRQ 12 in the APIC/IOAPIC.
3. Send 0xF4 (Enable Reporting) to the mouse via the auxiliary port.
4. Optionally enable scroll wheel (Intellimouse protocol: 4-byte packets).

**Interrupt handler (IRQ 12):**
- Read byte from port 0x60.
- Accumulate into 3-byte packets (state machine).
- Push complete packets to a ring buffer.

### Userspace Interface

**Option A: `/dev/mouse` device file**
- Open and read mouse events as structured data.
- Each read returns: `{ dx: i16, dy: i16, buttons: u8 }`.

**Option B: `/dev/input/event0` (evdev-like)**
- Unified input events: `{ type, code, value }`.
- Mouse movement: type=REL, code=REL_X/REL_Y.
- Mouse buttons: type=KEY, code=BTN_LEFT/BTN_RIGHT/BTN_MIDDLE.

**Option C: `sys_read_mouse` syscall (simplest)**
- Returns dx, dy, and button state directly.
- Non-blocking: returns zeros if no movement.

### Integration with poll/select

Mouse fd should work with `poll()` and future `epoll()` — report `POLLIN` when
mouse packets are available.

### Cursor Support (Stretch Goal)

- Software cursor rendered to the framebuffer.
- Kernel tracks cursor position (accumulating deltas).
- Text-mode cursor positioning for the editor.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 15 (Hardware Discovery) | APIC/IOAPIC for IRQ 12 routing |
| Phase 3 (Interrupts) | Interrupt handler infrastructure |

## Implementation Outline

1. Initialize PS/2 auxiliary port and enable mouse reporting.
2. Add IRQ 12 handler that accumulates 3-byte packets.
3. Add mouse packet ring buffer (similar to keyboard scancode buffer).
4. Implement userspace interface (device file or syscall).
5. Write a mouse test program that prints movement and button events.
6. Integrate with DOOM platform layer for mouse aiming.
7. Optionally add software cursor for framebuffer mode.

## Acceptance Criteria

- Moving the mouse in QEMU generates events readable by userspace.
- Button clicks (left, right, middle) are detected.
- A test program prints continuous dx/dy values as the mouse moves.
- DOOM can use mouse input for aiming (if Phase 47 is complete).
- Mouse input does not interfere with keyboard input.
- All existing tests pass without regression.

## Companion Task List

- Phase 48 Task List — *not yet created*

## How Real OS Implementations Differ

Real systems use:
- **evdev** — unified input subsystem for keyboard, mouse, touchpad, gamepad, etc.
- **libinput** — userspace library for pointer acceleration, gesture recognition.
- **USB HID** — most modern mice are USB, not PS/2 (QEMU emulates PS/2).
- **Touchpad drivers** — Synaptics, ALPS protocols for multitouch.
- **Pointer acceleration curves** — smooth, configurable acceleration.
- **Cursor compositor** — hardware cursor plane on the GPU.

Our approach handles only PS/2 mouse (which QEMU provides) with raw deltas.

## Deferred Until Later

- USB HID mouse driver
- Pointer acceleration
- Hardware cursor
- Multi-touch / touchpad
- Gamepad / joystick input
- evdev unified input layer
