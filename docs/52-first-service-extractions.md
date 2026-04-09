# First Service Extractions

**Aligned Roadmap Phase:** Phase 52
**Status:** In Progress
**Source Ref:** phase-52
**Supersedes Legacy Doc:** (none -- new content)

> **Note:** This phase builds on Phase 50 (IPC Completion) and Phase 51 (Service
> Model Maturity) to move the first kernel-resident services into real ring-3
> processes. For the IPC transport model, see
> [docs/50-ipc-completion.md](./50-ipc-completion.md). For the service lifecycle
> model, see [docs/51-service-model-maturity.md](./51-service-model-maturity.md).

## Overview

Phase 52 proves that the m3OS microkernel architecture is real by extracting
console rendering and keyboard input translation from kernel-resident tasks into
supervised ring-3 services. After this phase, the system demonstrates that
user-visible subsystems can cross the ring-0 boundary, communicate through the
Phase 50 IPC contract, and be restarted by the Phase 51 service manager without
rebooting the machine.

## What This Doc Covers

- What stayed in the kernel vs. what moved to userspace
- Console service architecture and rendering ownership
- Keyboard service architecture and IPC-based scancode delivery
- Stdin feeder extraction and line discipline in userspace
- Restart and reconnection behavior
- TTY buffer dual-routing design
- Boundary measurements (TBD pending QEMU testing)
- Key design decisions and trade-offs

## What Stayed in the Kernel

The kernel retains only the minimal privileged substrate needed to mediate
hardware access:

| Component | Why it stays in ring 0 |
|---|---|
| Framebuffer mapping | Physical MMIO region must be mapped by the kernel; only the kernel can create page-table entries for device memory |
| IRQ handler for keyboard | ISR must run in ring 0 to read PS/2 port 0x60, acknowledge the interrupt, and send EOI |
| Scancode ring buffer | Populated by the ISR; drained by the kbd_server notification path |
| Stdin buffer (kernel-side) | Maintains a dual-routing path so that legacy stdin reads continue to work alongside the new IPC-based keyboard service |
| Notification objects | Kernel-owned; used to wake the ring-3 kbd_server when IRQ1 fires |

The key principle: the kernel does the minimum hardware-touching work and hands
everything else outward through notifications and IPC.

## What Moved to Userspace

### Console service (`console_server`)

The console service owns all rendering policy:

- Receives string payloads via IPC from clients
- Writes characters to the framebuffer through a mapped region (page grant from kernel)
- Manages cursor position, scroll behavior, and text layout
- Routes output to both framebuffer and serial (dual output)

The service registers as `"console"` in the service registry using the
owner-based registration model from Phase 50. Clients look up the endpoint by
name and send `CONSOLE_WRITE` messages with validated buffer payloads.

### Keyboard service (`kbd_server`)

The keyboard service owns input translation and event delivery:

- Waits on a kernel notification object bound to IRQ1
- Drains the scancode ring buffer when notified
- Translates raw PS/2 scancodes to key events
- Delivers events to subscribed clients via IPC
- Registers as `"kbd"` in the service registry

### Stdin feeder / line discipline

The stdin feeder bridges the keyboard IPC service with the legacy stdin path:

- Subscribes to kbd_server events
- Performs line discipline processing (echo, backspace, line buffering)
- Feeds processed input into the kernel stdin buffer for processes that read
  from fd 0 using traditional `sys_read`

This dual-routing design means existing programs (shell, coreutils, editors)
continue to work without modification while the system transitions to
IPC-native input consumption.

## TTY Buffer Dual-Routing

A key design decision is the TTY buffer dual-routing model:

```
IRQ1 → kernel scancode buffer → notification → kbd_server (ring 3)
                                                    ↓
                                            stdin feeder (ring 3)
                                                    ↓
                                            kernel stdin buffer ← legacy sys_read
```

This allows:

1. IPC-aware clients to receive key events directly from kbd_server
2. Legacy programs to read from stdin as before
3. The transition to be gradual -- no big-bang ABI break required

The stdin feeder is itself a supervised service, so it can be restarted
independently if it crashes.

## Restart and Reconnection Behavior

Both console_server and kbd_server are declared as supervised services in the
Phase 51 service manager:

- **Service definitions** in `/etc/services.d/` with `restart=always` policy
- **Crash recovery**: if a service crashes, the service manager restarts it
  using the Phase 51 backoff policy (immediate first restart, then exponential
  backoff with crash classification)
- **Re-registration**: the restarted service re-registers its endpoint in the
  registry using `replace_service()` from Phase 50, so future lookups resolve
  to the new endpoint.  Clients holding old endpoint capabilities must
  re-lookup the service to obtain a valid handle (existing handles point to
  the old, now-dead endpoint)
- **No reboot required**: the kernel continues running; only the crashed service
  restarts

### What happens during a console_server restart

1. Console output is lost during the restart window (typically < 100ms)
2. The service re-registers, re-maps the framebuffer grant, and resumes
3. Clients that were blocked on IPC to the dead endpoint receive an error and
   can retry
4. Serial output continues uninterrupted (the kernel serial driver is
   independent)

### What happens during a kbd_server restart

1. Scancodes accumulate in the kernel ring buffer during the restart window
2. The restarted kbd_server drains the buffer on startup
3. No keystrokes are lost unless the ring buffer overflows during the outage

## Boundary Measurements

> **TBD**: The following measurements will be collected during QEMU testing and
> filled in before the phase is marked complete.

| Metric | Value | Notes |
|---|---|---|
| Console write latency (IPC round-trip) | TBD | Measured from client send to reply received |
| Keyboard event latency (IRQ to client delivery) | TBD | Measured from ISR notification to kbd_server IPC delivery |
| Console restart recovery time | TBD | Time from crash detection to first successful client write |
| Keyboard restart recovery time | TBD | Time from crash detection to first event delivery |
| Kernel TCB reduction (lines of code) | TBD | Lines removed from kernel/ vs. lines added to userspace/ |

## Key Design Decisions

### Owner-based registry

Phase 50's owner-tracked registry with `replace_service()` is essential for
restart. Without it, a restarted service would need to deregister the dead
entry and re-register, creating a window where lookups fail. Owner-based
replacement makes restart atomic from the client's perspective.

### TTY buffer dual-routing over clean break

Rather than requiring all programs to switch to IPC-based input immediately,
the dual-routing design preserves backward compatibility. This is the
incremental migration strategy recommended in `docs/evaluation/microkernel-path.md`.

### Page grants for framebuffer access

The console service receives the framebuffer region as a page grant
(`Capability::Grant`) rather than using a shared-memory shortcut. This ensures
the boundary is real: the console service cannot access kernel memory beyond
what was explicitly granted.

### Minimal kernel mediation

The kernel's role is narrowed to: map hardware, handle IRQs, maintain ring
buffers, and deliver notifications. All policy decisions (rendering, input
translation, focus routing) live in userspace.

## Key Files

| File | Purpose |
|---|---|
| `userspace/console_server/src/main.rs` | Ring-3 console rendering service |
| `userspace/kbd_server/src/main.rs` | Ring-3 keyboard input translation service |
| `userspace/init/src/main.rs` | Service manager integration for extracted services |
| `kernel/src/main.rs` | Narrowed kernel-side mediation (FB mapping, IRQ, buffers) |
| `kernel/src/ipc/mod.rs` | Registry and capability infrastructure |
| `kernel/initrd/etc/services.d/console.conf` | Console service definition |
| `kernel/initrd/etc/services.d/kbd.conf` | Keyboard service definition |

## How This Phase Differs From Later Work

- This phase extracts only console and keyboard. Storage, networking, and
  display compositor extraction are deferred to Phase 54 (Deep Serverization).
- The framebuffer grant model is minimal: one contiguous region. A production
  display server would manage multiple surfaces and compositing.
- Input routing is single-consumer. Multi-seat or multi-session input
  multiplexing is deferred.
- Performance tuning of the IPC boundary is deferred; this phase prioritizes
  correctness and restartability over throughput.

## Related Roadmap Docs

- [Phase 52 roadmap doc](./roadmap/52-first-service-extractions.md)
- [Phase 52 task doc](./roadmap/tasks/52-first-service-extractions-tasks.md)
- [Phase 50 — IPC Completion](./50-ipc-completion.md)
- [Phase 51 — Service Model Maturity](./51-service-model-maturity.md)
- [Core Servers (Phase 7)](./07-core-servers.md)
- [Framebuffer and Shell (Phase 9)](./09-framebuffer-and-shell.md)

## Deferred or Later-Phase Topics

- Storage, namespace, and networking extraction (Phase 54)
- Rich multi-seat or multi-session input policy
- Fully graphical display ownership and compositor
- Broad performance tuning of the IPC boundary
- Display server as a first-class service
