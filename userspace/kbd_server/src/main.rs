//! Userspace keyboard service for m3OS.
//!
//! Phase 52 baseline: handle `KBD_READ` IPC requests by draining one
//! scancode at a time from the kernel's PS/2 ring and returning it as the
//! reply label. This is the line-oriented path used by `ion`, `login`,
//! and any other text-mode consumer.
//!
//! Phase 56 Track D.1 extension: optionally drain the same scancode
//! stream through `kernel_core::input::keymap` to produce
//! [`kernel_core::input::events::KeyEvent`] messages — the GUI-class
//! event shape consumed by `display_server` (D.3) and any future
//! GUI-aware client. The new label is [`KBD_EVENT_PULL = 2`] on the
//! existing `kbd` service endpoint; the legacy `KBD_READ = 1` path is
//! kept fully intact so text-mode consumers still see raw scancode
//! bytes.
//!
//! ## Endpoint design choice
//!
//! Phase 56 D.1 picks the **second-label-on-existing-endpoint** option:
//! `kbd_server` registers exactly one IPC endpoint as service `"kbd"`
//! and dispatches by label. Rationale (documented in the track report):
//!   * The IPC model is request/reply; the `kbd-events` "push" pattern
//!     is naturally implemented as "client pulls next event, server
//!     replies when one is available," which is just another label on
//!     the same endpoint.
//!   * One service registration → one lookup at the consumer side; D.4
//!     and D.3 don't have to coordinate two services.
//!   * The legacy `KBD_READ` path stays bit-for-bit identical, so
//!     `ion` / `login` cannot regress.
//!
//! Each request consumes one byte (KBD_READ) or one KeyEvent
//! (KBD_EVENT_PULL) from the kernel-driven scancode stream. Mixing the
//! two is supported but only one mode is expected to be active at a
//! time (TTY ownership vs GUI ownership, exactly like the kernel's
//! existing scancode-router split).
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use kernel_core::input::events::{KEY_EVENT_WIRE_SIZE, KeyEvent};
use kernel_core::input::keymap::{
    DecodedScancode, KeyRepeatScheduler, Keymap, KeymapError, ModifierTracker, ScancodeDecoder,
};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "kbd_server: alloc error\n");
    syscall_lib::exit(99)
}

// ---------------------------------------------------------------------------
// Wire labels
// ---------------------------------------------------------------------------

/// Legacy text-mode label. Reply label is the raw scancode byte.
const KBD_READ: u64 = 1;

/// Phase 56 Track D.1 label. Reply carries a 19-byte
/// `KeyEvent` wire payload as bulk data (zero-byte data0/label).
/// The `display_server` (D.3) is the expected consumer; D.4 wires
/// the lookup at startup. Documented in the track-D.1 report.
const KBD_EVENT_PULL: u64 = 2;

/// Reply-cap slot is fixed at 1 by the kernel's IPC ABI.
const REPLY_CAP_HANDLE: u32 = 1;

/// Polling interval used while waiting for a scancode to arrive
/// (matches the legacy 5 ms cadence).
const POLL_INTERVAL_NS: u32 = 5_000_000;

/// Phase 56 close-out — the pull pattern is now non-blocking on the
/// server side. kbd_server checks the scancode buffer once and
/// replies immediately: an event if queued, `u64::MAX` (timeout
/// sentinel) otherwise. display_server's main loop drives the wait by
/// polling every 1 ms via its multiplex (`SYS_IPC_TRY_RECV_MSG`).
///
/// Pre-close-out this was 6_000 × 5 ms = 30 s. That pinned
/// display_server's main loop on every drain pass and blocked the
/// control endpoint. Moving the wait to the caller side restores the
/// multiplex.
const MAX_PULL_POLLS: u32 = 1;

// ---------------------------------------------------------------------------
// Pipeline state
// ---------------------------------------------------------------------------

/// Owns the four pure-logic types from `kernel-core::input::keymap` and
/// drives them in lock-step from the raw scancode stream. Single-threaded;
/// shared across requests so modifier state and held-key tracking persist
/// across multiple `KBD_EVENT_PULL` calls.
struct KeyboardPipeline {
    decoder: ScancodeDecoder,
    tracker: ModifierTracker,
    keymap: Keymap,
    scheduler: KeyRepeatScheduler,
}

impl KeyboardPipeline {
    fn new() -> Self {
        Self {
            decoder: ScancodeDecoder::new(),
            tracker: ModifierTracker::new(),
            keymap: Keymap::us_qwerty(),
            scheduler: KeyRepeatScheduler::new(),
        }
    }

    /// Feed one raw scancode byte. If a complete `(Keycode, KeyKind)`
    /// edge is decoded, it is run through the modifier tracker and the
    /// keymap and a fully-stamped [`KeyEvent`] is returned.
    ///
    /// Returns `None` if the byte is mid-sequence or was discarded for
    /// resync. Modifier-key edges return `None` from this function —
    /// they update the tracker but do not surface as `KeyEvent`s on the
    /// pull path (the modifier snapshot is folded into the next non-
    /// modifier event). This keeps the wire shape minimal for the GUI
    /// path while preserving correct chord behavior.
    fn feed_byte(&mut self, byte: u8, timestamp_ms: u64) -> Option<KeyEvent> {
        let DecodedScancode::Edge { keycode, kind } = self.decoder.feed(byte) else {
            return None;
        };

        // Update modifier tracker. Note: tracker.apply must run BEFORE
        // we look up the symbol so chords that include a just-pressed
        // modifier resolve against the post-edge state.
        let mods = self.tracker.apply(keycode, kind);

        // Inform the scheduler of the edge. Surface the structured
        // overflow warning to serial so the operator sees a stuck-key
        // chord immediately, but never fail the dispatch — we still
        // want to deliver the event.
        match self.scheduler.observe(keycode, kind, mods, timestamp_ms) {
            Ok(()) => {}
            Err(KeymapError::HeldKeyTableOverflow) => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "kbd_server: warn: held-key table overflow; oldest key dropped\n",
                );
            }
            Err(_) => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "kbd_server: warn: scheduler observe returned unknown error\n",
                );
            }
        }

        // Modifier and lock keys do not produce stand-alone KeyEvents;
        // their effect is folded into the snapshot delivered with the
        // next non-modifier event.
        let symbol = match self.keymap.lookup(keycode, mods) {
            Some(s) => s.0,
            None => {
                // Modifier / lock key — skip emission.
                return None;
            }
        };

        Some(KeyEvent {
            timestamp_ms,
            keycode: keycode.0,
            symbol,
            modifiers: mods,
            kind,
        })
    }

    /// Drive the repeat scheduler at the current monotonic timestamp.
    /// Returns at most one repeat event per call; callers should poll
    /// repeatedly to drain queued repeats.
    fn tick_repeat(&mut self, timestamp_ms: u64) -> Option<KeyEvent> {
        let mut ev = self.scheduler.tick(timestamp_ms)?;
        // The scheduler can't see the keymap; resolve the symbol now so
        // the wire payload always carries a meaningful KeySym for repeats
        // of printable keys.
        let mods = ev.modifiers;
        if let Some(sym) = self
            .keymap
            .lookup(kernel_core::input::keymap::Keycode(ev.keycode), mods)
        {
            ev.symbol = sym.0;
        }
        Some(ev)
    }
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

/// Read the monotonic clock and return the time in milliseconds.
/// Wraps `clock_gettime(CLOCK_MONOTONIC)` and folds (sec, nsec) into a
/// single `u64` ms count.
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

/// Handle the legacy `KBD_READ` request.
///
/// Polls `read_kbd_scancode` until a non-zero byte arrives, then replies
/// with the byte as the reply label (preserving Phase 52 contract).
fn handle_kbd_read() {
    let scancode = loop {
        let sc = syscall_lib::read_kbd_scancode();
        if sc != 0 {
            break sc;
        }
        let _ = syscall_lib::nanosleep_for(0, POLL_INTERVAL_NS);
    };
    syscall_lib::ipc_reply(REPLY_CAP_HANDLE, scancode as u64, 0);
}

/// Handle a `KBD_EVENT_PULL` request.
///
/// Polls scancodes through the keymap pipeline until either a fully-
/// formed `KeyEvent` is produced, the key-repeat scheduler emits a
/// repeat, or the request times out. On success, encodes the
/// 19-byte wire payload, stages it as the reply bulk, and replies
/// with `label = KBD_EVENT_PULL` and `data0 = 0`.
///
/// On timeout, replies with `label = u64::MAX` and no bulk so the
/// caller can distinguish a bounded-wait expiry from a real event.
fn handle_kbd_event_pull(pipeline: &mut KeyboardPipeline) {
    for _ in 0..MAX_PULL_POLLS {
        // Drain as many scancode bytes as the kernel has buffered,
        // emitting at most one KeyEvent per pull (the scheduler picks
        // the next due event on subsequent ticks).
        loop {
            let sc = syscall_lib::read_kbd_scancode();
            if sc == 0 {
                break;
            }
            let now = monotonic_ms();
            if let Some(ev) = pipeline.feed_byte(sc, now) {
                emit_key_event(&ev);
                return;
            }
        }

        // No fresh edge — see if the scheduler has a repeat ready.
        let now = monotonic_ms();
        if let Some(ev) = pipeline.tick_repeat(now) {
            emit_key_event(&ev);
            return;
        }

        let _ = syscall_lib::nanosleep_for(0, POLL_INTERVAL_NS);
    }

    // Bounded timeout — surface as a typed error reply (label = u64::MAX,
    // matching the convention used by other syscall-lib paths). The bulk
    // slot is left empty so the caller's recv_msg returns a zero-byte
    // payload alongside the sentinel label.
    syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
}

/// Encode the `KeyEvent` and reply with it as bulk data.
///
/// The 19-byte wire layout is defined by `kernel_core::input::events`.
/// If encoding fails (which it cannot under the stable wire size, but
/// we handle the typed error anyway), surface a typed error to the
/// caller via `label = u64::MAX`.
fn emit_key_event(ev: &KeyEvent) {
    let mut buf = [0u8; KEY_EVENT_WIRE_SIZE];
    match ev.encode(&mut buf) {
        Ok(_) => {
            // Stage the bulk and reply.
            let _ = syscall_lib::ipc_store_reply_bulk(&buf);
            syscall_lib::ipc_reply(REPLY_CAP_HANDLE, KBD_EVENT_PULL, 0);
        }
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "kbd_server: error: KeyEvent encode failed; replying with sentinel\n",
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
        "kbd_server: starting (Phase 56 D.1 — KeyEvent pipeline online)\n",
    );

    // 1. Create the IPC endpoint that backs the `kbd` service.
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "kbd_server: failed to create endpoint\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    // 2. Register as `"kbd"` so both legacy and GUI consumers can find us
    //    via a single service lookup. Label-based dispatch decides the
    //    request shape.
    let ret = syscall_lib::ipc_register_service(ep_handle, "kbd");
    if ret == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "kbd_server: failed to register 'kbd'\n");
        return 1;
    }

    syscall_lib::write_str(STDOUT_FILENO, "kbd_server: ready\n");

    let mut pipeline = KeyboardPipeline::new();

    // Service loop: dispatch by label. Each branch must end in an
    // `ipc_reply` so the reply-cap slot is freed before the next recv.
    let mut label = syscall_lib::ipc_recv(ep_handle);

    loop {
        match label {
            KBD_READ => handle_kbd_read(),
            KBD_EVENT_PULL => handle_kbd_event_pull(&mut pipeline),
            _ => {
                // Unknown label — typed error, observable to clients.
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "kbd_server: warn: unknown IPC label; replying with sentinel\n",
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
    syscall_lib::write_str(STDOUT_FILENO, "kbd_server: PANIC\n");
    syscall_lib::exit(101)
}
