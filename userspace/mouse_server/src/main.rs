//! Userspace mouse service for m3OS (Phase 56 Track D.2).
//!
//! Drains the kernel PS/2 mouse-packet ring (via `SYS_READ_MOUSE_PACKET =
//! 0x1015`, exposed as `syscall_lib::read_mouse_packet`) and lifts each 8-byte
//! wire packet into a `kernel_core::input::events::PointerEvent` with stable
//! relative deltas, button-edge tracking via the pure-logic `ButtonTracker`,
//! and (when IntelliMouse mode is active in the kernel) wheel deltas.
//!
//! ## Endpoint design choice
//!
//! Phase 56 D.2 mirrors D.1's **second-label-on-existing-endpoint** approach:
//! `mouse_server` registers exactly one IPC endpoint as service `"mouse"` and
//! dispatches by label. Today the only label is [`MOUSE_EVENT_PULL = 1`] —
//! clients pull, the server replies with `label = MOUSE_EVENT_PULL` and a
//! 37-byte `PointerEvent` wire payload as bulk data, or with the sentinel
//! `label = u64::MAX` on bounded-wait expiry. The label space leaves room
//! for follow-up labels (e.g. `MOUSE_GRAB`, `MOUSE_UNGRAB`) without forcing
//! a separate endpoint.
//!
//! ## Pipeline shape
//!
//! 1. `read_mouse_packet` drains one 8-byte wire packet from the kernel ring.
//!    `-EAGAIN` (-11) means the ring is empty; we sleep and retry up to
//!    `MAX_PULL_POLLS` cycles before timing the request out.
//! 2. `kernel_core::input::mouse::decode_packet` lifts the wire bytes into a
//!    `MousePacket` (relative dx/dy in PS/2 9-bit space, signed wheel byte,
//!    three button bits, two overflow bits).
//! 3. The packet's button bits feed `ButtonTracker::update`, which emits a
//!    deterministic stream of `Down(idx)` / `Up(idx)` edges in stable
//!    left-right-middle order.
//! 4. The first edge (or, if no edges, the motion-only event) is encoded as a
//!    `PointerEvent` with relative dx/dy, optional wheel delta, and the
//!    captured button transition. Any remaining button edges are queued in a
//!    `PendingEdges` struct so subsequent `MOUSE_EVENT_PULL` requests drain
//!    them one at a time without re-polling the kernel ring.
//!
//! ## Y-axis convention
//!
//! PS/2 reports +dy = up, screen coordinates use +dy = down. We flip dy at
//! the boundary so `display_server` clients see the screen-native sign
//! immediately and don't have to know about PS/2 quirks. Wheel sign is
//! preserved as-is — wheel up is +1.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use kernel_core::input::events::{POINTER_EVENT_WIRE_SIZE, PointerButton, PointerEvent};
use kernel_core::input::mouse::{
    BUTTON_INDEX_LEFT, BUTTON_INDEX_MIDDLE, BUTTON_INDEX_RIGHT, ButtonState, ButtonTracker,
    ButtonTransition, MOUSE_PACKET_WIRE_SIZE, MousePacket, decode_packet,
};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "mouse_server: alloc error\n");
    syscall_lib::exit(99)
}

// ---------------------------------------------------------------------------
// Wire labels and constants (matched to D.1's pattern)
// ---------------------------------------------------------------------------

/// Phase 56 Track D.2 label. Reply carries a 37-byte
/// `PointerEvent` wire payload as bulk data (zero-byte data0/label).
/// The `display_server` (D.3) is the expected consumer.
const MOUSE_EVENT_PULL: u64 = 1;

/// Reply-cap slot is fixed at 1 by the kernel's IPC ABI.
const REPLY_CAP_HANDLE: u32 = 1;

/// Polling interval used while waiting for a packet to arrive (matches D.1's
/// 5 ms cadence for kbd_server). Keeps the wake-rate bounded so a stuck
/// reader does not pin a CPU.
const POLL_INTERVAL_NS: u32 = 5_000_000;

/// Best-effort upper bound on total wait time for a single
/// `MOUSE_EVENT_PULL` request. Identical to kbd_server's bound: at 5 ms per
/// cycle this is ~30 s of patience, more than enough for interactive input
/// but bounded so cancelled pulls do not pin the server forever.
const MAX_PULL_POLLS: u32 = 6_000;

/// `read_mouse_packet` returns this when the AUX ring is empty.
/// Mirrors `-libc::EAGAIN` (-11) — the kernel returns the value as a signed
/// errno cast back to `isize`.
const ERRNO_EAGAIN: isize = -11;

// ---------------------------------------------------------------------------
// Pending-edge queue
// ---------------------------------------------------------------------------

/// At most three button edges + one wheel/motion event can be produced from
/// one PS/2 packet. We deliver one event per `MOUSE_EVENT_PULL`, so any
/// extras must be buffered until the next pull. The queue is a fixed
/// 4-slot array; allocation is forbidden on the hot input path.
#[derive(Clone, Copy, Debug, Default)]
struct PendingEdges {
    /// Slots filled left-to-right by `enqueue` and drained left-to-right by
    /// `dequeue`. Each slot is an already-encoded `PointerEvent`.
    slots: [Option<PointerEvent>; 4],
}

impl PendingEdges {
    const fn new() -> Self {
        Self {
            slots: [None, None, None, None],
        }
    }

    /// Buffer one event for delivery on a subsequent pull. Returns `false`
    /// if the queue is full (which should never happen in practice — a
    /// single PS/2 packet emits at most 3 button edges + 1 motion = 4
    /// events, exactly the queue capacity).
    fn enqueue(&mut self, ev: PointerEvent) -> bool {
        for slot in self.slots.iter_mut() {
            if slot.is_none() {
                *slot = Some(ev);
                return true;
            }
        }
        false
    }

    /// Pop and return the oldest buffered event, if any.
    fn dequeue(&mut self) -> Option<PointerEvent> {
        let head = self.slots[0]?;
        // Shift the queue left by one slot.
        for i in 0..(self.slots.len() - 1) {
            self.slots[i] = self.slots[i + 1];
        }
        let last = self.slots.len() - 1;
        self.slots[last] = None;
        Some(head)
    }
}

// ---------------------------------------------------------------------------
// Mouse pipeline state
// ---------------------------------------------------------------------------

/// Owns the per-server state that persists across `MOUSE_EVENT_PULL`
/// requests: the button-edge tracker and the pending-edge queue. The
/// `Ps2MouseDecoder` is *not* part of this struct because the kernel-side
/// decoder already produces fully-formed `MousePacket`s; we only need to
/// handle the wire packet → typed packet boundary.
struct MousePipeline {
    buttons: ButtonTracker,
    pending: PendingEdges,
}

impl MousePipeline {
    const fn new() -> Self {
        Self {
            buttons: ButtonTracker::new(),
            pending: PendingEdges::new(),
        }
    }

    /// Lift one decoded `MousePacket` into one or more `PointerEvent`s.
    ///
    /// Returns `Some(primary)` to deliver to the client, with any additional
    /// events (extra button edges) buffered in `self.pending` for the next
    /// pull. Returns `None` when the packet carries no meaningful change
    /// (zero motion, zero wheel, zero button edges); the caller continues
    /// draining the kernel ring without producing an IPC reply.
    ///
    /// Wheel deltas are passed through only when `packet.wheel != 0` (i.e.
    /// when the kernel has IntelliMouse mode active and the user actually
    /// scrolled). Spec D.2 §7.
    fn ingest_packet(&mut self, packet: MousePacket, timestamp_ms: u64) -> Option<PointerEvent> {
        // Phase 56 D.2 acceptance §6: deliver dx/dy as relative deltas.
        // PS/2 reports +Y up, screen coordinates use +Y down — flip here.
        let dx = packet.dx as i32;
        let dy = -(packet.dy as i32);

        // Wheel: spec §7 — only emit non-zero wheel deltas.
        // Y-axis convention: PS/2 reports wheel-up as positive 1; screen
        // wheel-up is conventionally also +1, so we preserve sign.
        let wheel_dy = packet.wheel as i32;
        let wheel_dx = 0; // PS/2 IntelliMouse has no horizontal wheel.

        // Compute button transitions.
        let new_state = ButtonState::from_packet(&packet);
        let transitions = self.buttons.update(new_state);

        // Strategy: emit (motion+wheel) as the primary event, and queue
        // extra button-edge events as motion-zero follow-ups. If there are
        // button edges and no motion+wheel, the first edge becomes the
        // primary and subsequent edges queue. If the packet carries
        // *nothing*, return None so the caller keeps polling.
        let has_motion_or_wheel = dx != 0 || dy != 0 || wheel_dy != 0;
        let mut edges_iter = transitions.iter();
        let first_edge = edges_iter.next();

        let primary = if has_motion_or_wheel {
            Some(PointerEvent {
                timestamp_ms,
                dx,
                dy,
                abs_position: None,
                button: PointerButton::None,
                wheel_dx,
                wheel_dy,
                modifiers: Default::default(),
            })
        } else {
            first_edge.map(|edge| PointerEvent {
                timestamp_ms,
                dx: 0,
                dy: 0,
                abs_position: None,
                button: encode_button(edge),
                wheel_dx: 0,
                wheel_dy: 0,
                modifiers: Default::default(),
            })
        };

        // Queue any remaining button edges. If the primary was the
        // motion+wheel event, ALL edges (including first_edge) need to be
        // queued as follow-ups so the client sees them.
        if has_motion_or_wheel && let Some(edge) = first_edge {
            let _ = self.pending.enqueue(button_only_event(edge, timestamp_ms));
        }
        for edge in edges_iter {
            if !self.pending.enqueue(button_only_event(edge, timestamp_ms)) {
                // Queue overflow — should not happen given the 4-slot
                // capacity, but log it instead of panicking.
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "mouse_server: warn: pending-edge queue full; edge dropped\n",
                );
            }
        }

        primary
    }
}

/// Translate one `ButtonTransition` into a `PointerButton` enum.
fn encode_button(edge: ButtonTransition) -> PointerButton {
    match edge {
        ButtonTransition::Down(idx) => match idx {
            BUTTON_INDEX_LEFT | BUTTON_INDEX_RIGHT | BUTTON_INDEX_MIDDLE => {
                PointerButton::Down(idx)
            }
            other => PointerButton::Down(other),
        },
        ButtonTransition::Up(idx) => match idx {
            BUTTON_INDEX_LEFT | BUTTON_INDEX_RIGHT | BUTTON_INDEX_MIDDLE => PointerButton::Up(idx),
            other => PointerButton::Up(other),
        },
    }
}

/// Construct a button-only `PointerEvent` (no motion, no wheel) for a
/// single edge. Used when buffering follow-up edges after motion+wheel.
fn button_only_event(edge: ButtonTransition, timestamp_ms: u64) -> PointerEvent {
    PointerEvent {
        timestamp_ms,
        dx: 0,
        dy: 0,
        abs_position: None,
        button: encode_button(edge),
        wheel_dx: 0,
        wheel_dy: 0,
        modifiers: Default::default(),
    }
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

/// Read the monotonic clock and return the time in milliseconds.
/// Wraps `clock_gettime(CLOCK_MONOTONIC)` and folds (sec, nsec) into a
/// single `u64` ms count. Mirrors kbd_server's `monotonic_ms` helper so
/// timestamps in `KeyEvent` and `PointerEvent` are on the same clock.
fn monotonic_ms() -> u64 {
    let (sec, nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    if sec < 0 {
        return 0;
    }
    let sec_ms = (sec as u64).saturating_mul(1_000);
    let nsec_ms = (nsec as u64) / 1_000_000;
    sec_ms.saturating_add(nsec_ms)
}

// ---------------------------------------------------------------------------
// Request handlers
// ---------------------------------------------------------------------------

/// Handle a `MOUSE_EVENT_PULL` request.
///
/// 1. If a buffered event is queued from a previous packet, deliver it
///    immediately.
/// 2. Otherwise, poll `read_mouse_packet` until a fresh packet arrives or
///    the bounded-wait timeout expires.
/// 3. Decode the wire packet, compute button edges, and reply with the
///    primary `PointerEvent`. Extra edges are queued for the next pull.
///
/// On timeout, replies with `label = u64::MAX` and no bulk so the caller
/// can distinguish a bounded-wait expiry from a real event (matches D.1).
fn handle_mouse_event_pull(pipeline: &mut MousePipeline) {
    // Drain queued events first — keep latency low for chord traffic.
    if let Some(ev) = pipeline.pending.dequeue() {
        emit_pointer_event(&ev);
        return;
    }

    let mut buf = [0u8; MOUSE_PACKET_WIRE_SIZE];
    for _ in 0..MAX_PULL_POLLS {
        let rc = syscall_lib::read_mouse_packet(&mut buf);
        match rc {
            0 => {
                // Successful read — decode and lift to PointerEvent. If the
                // packet carries no meaningful change (idle PS/2 chatter),
                // continue draining without replying.
                let packet = decode_packet(&buf);
                let now = monotonic_ms();
                if let Some(ev) = pipeline.ingest_packet(packet, now) {
                    emit_pointer_event(&ev);
                    return;
                }
                // Idle packet — drain the next one if available without
                // sleeping (avoid wasting a 5 ms cycle on a no-op).
                continue;
            }
            ERRNO_EAGAIN => {
                // Empty ring — sleep and retry.
                let _ = syscall_lib::nanosleep_for(0, POLL_INTERVAL_NS);
                continue;
            }
            other => {
                // Any other error is unexpected (EINVAL, EFAULT, …). Log
                // a structured warning and surface the timeout sentinel
                // so the client doesn't hang.
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "mouse_server: warn: read_mouse_packet returned unexpected errno\n",
                );
                let _ = other;
                syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
                return;
            }
        }
    }

    // Bounded timeout — surface as a typed error reply (label = u64::MAX),
    // matching D.1's MAX_PULL_POLLS expiry shape so display_server can
    // share its bounded-wait machinery across input services.
    syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
}

/// Encode the `PointerEvent` and reply with it as bulk data.
///
/// The 37-byte wire layout is defined by `kernel_core::input::events`. If
/// encoding fails (which the codec does only when the modifiers bitmask
/// has unknown bits set, which we never do) we surface a typed error to
/// the caller via `label = u64::MAX`.
fn emit_pointer_event(ev: &PointerEvent) {
    let mut buf = [0u8; POINTER_EVENT_WIRE_SIZE];
    match ev.encode(&mut buf) {
        Ok(_) => {
            let _ = syscall_lib::ipc_store_reply_bulk(&buf);
            syscall_lib::ipc_reply(REPLY_CAP_HANDLE, MOUSE_EVENT_PULL, 0);
        }
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "mouse_server: error: PointerEvent encode failed; replying with sentinel\n",
            );
            syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(
        STDOUT_FILENO,
        "mouse_server: starting (Phase 56 D.2 — PointerEvent pipeline online)\n",
    );

    // Phase 56 F.1 acceptance: input services emit a one-time log on startup
    // identifying which input source they will target. PS/2 AUX is IRQ 12 in
    // the legacy 8259 / IOAPIC mapping; the kernel's ps2.rs handler drains
    // bytes into the ring this server reads via `read_mouse_packet`.
    syscall_lib::write_str(
        STDOUT_FILENO,
        "mouse_server: attached to PS/2 AUX (IRQ 12) — kernel-decoded packets\n",
    );

    // 1. Create the IPC endpoint that backs the `mouse` service.
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "mouse_server: failed to create endpoint\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    // 2. Register as `"mouse"` so display_server can find us via a single
    //    service lookup. Label-based dispatch decides the request shape.
    let ret = syscall_lib::ipc_register_service(ep_handle, "mouse");
    if ret == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "mouse_server: failed to register 'mouse'\n");
        return 1;
    }

    syscall_lib::write_str(STDOUT_FILENO, "mouse_server: ready\n");

    let mut pipeline = MousePipeline::new();

    // Service loop: dispatch by label. Each branch must end in an
    // `ipc_reply` so the reply-cap slot is freed before the next recv.
    let mut label = syscall_lib::ipc_recv(ep_handle);

    loop {
        match label {
            MOUSE_EVENT_PULL => handle_mouse_event_pull(&mut pipeline),
            _ => {
                // Unknown label — typed error, observable to clients.
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "mouse_server: warn: unknown IPC label; replying with sentinel\n",
                );
                syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
            }
        }
        label = syscall_lib::ipc_recv(ep_handle);
    }
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "mouse_server: PANIC\n");
    syscall_lib::exit(101)
}
