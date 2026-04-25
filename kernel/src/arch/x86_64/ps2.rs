//! Phase 56 Track B.2 — PS/2 controller wiring (keyboard + AUX/mouse).
//!
//! This module owns the kernel side of the PS/2 AUX port: 8042 controller
//! initialization (enable AUX, attempt IntelliMouse handshake, enable
//! streaming), the lock-free `MousePacket` ring buffer fed by the IRQ12 ISR,
//! and the `Ps2MouseDecoder` instance that frames raw bytes into packets in
//! interrupt context.
//!
//! The decoder is the pure-logic core declared in
//! [`kernel_core::input::mouse`] (Phase 56 PR #122). This file is the thin
//! kernel-side wiring around it: port I/O, IRQ glue, ring buffer, and the
//! `sys_read_mouse_packet` consumption point.
//!
//! # Concurrency
//!
//! - `MOUSE_PACKET_RING` is a single-producer / single-consumer ring with a
//!   power-of-two capacity. The ISR is the producer; the syscall handler is
//!   the consumer. Atomic head/tail with Acquire/Release ordering keeps it
//!   ISR-safe without taking a lock from the handler.
//! - `MOUSE_DECODER` lives behind a `spin::Mutex` because a one-shot
//!   `feed_byte_isr` call from the IRQ handler reads-modifies-writes the
//!   decoder state. The lock is uncontended in practice (the IRQ handler is
//!   the only writer) but the mutex keeps the API clean and matches the
//!   `RAW_INPUT_ROUTER` precedent in `interrupts.rs`.
//!
//! # Failure modes
//!
//! Every controller command is gated by a bounded busy-wait on the status
//! register's input/output buffer bits. A timeout returns a typed error and
//! the caller logs a structured warning; the kernel keeps booting without a
//! mouse.

use core::sync::atomic::{AtomicUsize, Ordering};

use kernel_core::input::mouse::{DecoderEvent, MousePacket, Ps2MouseDecoder};
pub use kernel_core::input::mouse::{MOUSE_PACKET_WIRE_SIZE, encode_packet};
use spin::Mutex;
use x86_64::instructions::port::Port;

// ---------------------------------------------------------------------------
// 8042 PS/2 controller register layout
// ---------------------------------------------------------------------------

/// Data port (read scancode / write byte to controller or device).
pub const PS2_DATA: u16 = 0x60;
/// Status (read) / Command (write) port.
pub const PS2_STATUS: u16 = 0x64;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;

// 8042 controller commands (written to PS2_STATUS).
const CMD_ENABLE_AUX: u8 = 0xA8;
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_WRITE_TO_AUX: u8 = 0xD4;

// 8042 controller config byte bits (read/written via 0x20 / 0x60).
const CONFIG_AUX_IRQ: u8 = 1 << 1;
const CONFIG_AUX_DISABLE: u8 = 1 << 5;

// Mouse (AUX device) commands (written via 0xD4 prefix).
const MOUSE_CMD_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_CMD_ENABLE_STREAMING: u8 = 0xF4;
const MOUSE_CMD_SET_SAMPLE_RATE: u8 = 0xF3;
const MOUSE_CMD_GET_DEVICE_ID: u8 = 0xF2;

const MOUSE_RESPONSE_ACK: u8 = 0xFA;

/// Bounded retry loop for status-register polling.
///
/// The 8042 is famously slow on real hardware; QEMU answers in nanoseconds.
/// 100k spins is well above the worst-case real-hardware delay observed in
/// kernel.org's i8042 driver and below the budget for getting stuck during
/// init.
const POLL_BUDGET: u32 = 100_000;

// ---------------------------------------------------------------------------
// Mouse packet ring buffer (single-producer, single-consumer)
// ---------------------------------------------------------------------------

/// Ring capacity. 64 packets at 8 bytes each = 512 bytes of static storage,
/// enough headroom for typical mouse motion bursts (~125 packets/sec) with
/// userspace polling at sub-millisecond cadence.
const MOUSE_RING_CAPACITY: usize = 64;
const _: () = assert!(
    MOUSE_RING_CAPACITY.is_power_of_two(),
    "MOUSE_RING_CAPACITY must be a power of two for bitmask wraparound",
);

/// One slot in the ring. `Default` initializes to a zeroed packet, which is a
/// valid `MousePacket` (no buttons, no motion, no overflow).
static mut MOUSE_PACKET_RING: [MousePacket; MOUSE_RING_CAPACITY] = [MousePacket {
    dx: 0,
    dy: 0,
    wheel: 0,
    left: false,
    right: false,
    middle: false,
    x_overflow: false,
    y_overflow: false,
}; MOUSE_RING_CAPACITY];

static MOUSE_RING_HEAD: AtomicUsize = AtomicUsize::new(0);
static MOUSE_RING_TAIL: AtomicUsize = AtomicUsize::new(0);

/// Decoder state — fed from the IRQ12 handler.
static MOUSE_DECODER: Mutex<Ps2MouseDecoder> = Mutex::new(Ps2MouseDecoder::new());

/// Records successful AUX-port initialization. **Informational only — it
/// does not gate the IRQ12 path.** The actual gate is the PIC mask: until
/// [`init_mouse`] has unmasked IRQ12 (and the BIOS / firmware hasn't left
/// it unmasked), no IRQ12 fires. If a stray IRQ12 *does* fire before init,
/// `mouse_handler` and [`feed_byte_isr`] still read, decode, and queue the
/// byte through the normal path. The [`is_ready`] accessor exposes this
/// flag for diagnostics (e.g. a future `mouse_server` health check).
static MOUSE_READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Pop the next decoded packet from the ring, or `None` if empty.
///
/// Called from the syscall path (process context) — never from an ISR.
pub fn read_mouse_packet() -> Option<MousePacket> {
    let head = MOUSE_RING_HEAD.load(Ordering::Acquire);
    let tail = MOUSE_RING_TAIL.load(Ordering::Acquire);
    if head == tail {
        return None;
    }
    // SAFETY: the ring lives for 'static, the head index is valid, and we
    // are the sole consumer (single-consumer model).
    let packet = unsafe {
        let ptr = (&raw const MOUSE_PACKET_RING) as *const MousePacket;
        ptr.add(head).read()
    };
    let next = (head + 1) & (MOUSE_RING_CAPACITY - 1);
    MOUSE_RING_HEAD.store(next, Ordering::Release);
    Some(packet)
}

/// True when the ring has at least one packet pending.
#[allow(dead_code)]
pub fn has_mouse_packet() -> bool {
    let head = MOUSE_RING_HEAD.load(Ordering::Acquire);
    let tail = MOUSE_RING_TAIL.load(Ordering::Acquire);
    head != tail
}

/// Feed one byte from the IRQ handler. Pushes a complete packet to the ring
/// when one assembles. Drops packets silently if the ring is full (prefer
/// losing motion over blocking the ISR).
///
/// Returns true when a packet was produced — the caller uses this to know
/// whether to signal the userspace mouse-server notification.
pub fn feed_byte_isr(byte: u8) -> bool {
    // Lock contention is impossible in practice: the IRQ handler is the
    // only caller, and IRQ12 is masked while this runs.
    let mut decoder = MOUSE_DECODER.lock();
    match decoder.feed(byte) {
        Some(DecoderEvent::Packet(packet)) => {
            push_packet_isr(packet);
            true
        }
        Some(DecoderEvent::Resync) | None => false,
    }
}

fn push_packet_isr(packet: MousePacket) {
    let tail = MOUSE_RING_TAIL.load(Ordering::Relaxed);
    let next = (tail + 1) & (MOUSE_RING_CAPACITY - 1);
    if next == MOUSE_RING_HEAD.load(Ordering::Acquire) {
        // Ring full — drop oldest by advancing head, then overwrite.
        // Lossy: prefer dropping a stale motion packet over blocking the ISR
        // or losing the most recent one. A fresh PointerEvent encodes
        // *delta*, so userspace will eventually catch up to the cursor's
        // true position even with a few dropped packets.
        let head = MOUSE_RING_HEAD.load(Ordering::Acquire);
        let new_head = (head + 1) & (MOUSE_RING_CAPACITY - 1);
        MOUSE_RING_HEAD.store(new_head, Ordering::Release);
    }
    // SAFETY: tail is valid, this is the sole producer.
    unsafe {
        let ptr = (&raw mut MOUSE_PACKET_RING) as *mut MousePacket;
        ptr.add(tail).write(packet);
    }
    MOUSE_RING_TAIL.store(next, Ordering::Release);
}

// ---------------------------------------------------------------------------
// 8042 controller helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ps2Error {
    /// Bounded busy-wait expired waiting for the input or output buffer.
    Timeout,
    /// AUX port did not acknowledge a command (returned a byte other than 0xFA).
    NotAcked,
}

fn wait_input_clear() -> Result<(), Ps2Error> {
    let mut status: Port<u8> = Port::new(PS2_STATUS);
    for _ in 0..POLL_BUDGET {
        // SAFETY: PS/2 status port is read-only from this perspective.
        let s = unsafe { status.read() };
        if s & STATUS_INPUT_FULL == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(Ps2Error::Timeout)
}

fn wait_output_full() -> Result<(), Ps2Error> {
    let mut status: Port<u8> = Port::new(PS2_STATUS);
    for _ in 0..POLL_BUDGET {
        // SAFETY: status port read.
        let s = unsafe { status.read() };
        if s & STATUS_OUTPUT_FULL != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(Ps2Error::Timeout)
}

fn write_command(cmd: u8) -> Result<(), Ps2Error> {
    wait_input_clear()?;
    let mut status: Port<u8> = Port::new(PS2_STATUS);
    // SAFETY: writing a command byte to the controller; documented in the
    // i8042 spec.
    unsafe { status.write(cmd) };
    Ok(())
}

fn write_data(byte: u8) -> Result<(), Ps2Error> {
    wait_input_clear()?;
    let mut data: Port<u8> = Port::new(PS2_DATA);
    // SAFETY: writing data byte to the data port.
    unsafe { data.write(byte) };
    Ok(())
}

fn read_data() -> Result<u8, Ps2Error> {
    wait_output_full()?;
    let mut data: Port<u8> = Port::new(PS2_DATA);
    // SAFETY: status reports OBF set, byte is available.
    Ok(unsafe { data.read() })
}

fn write_to_aux(byte: u8) -> Result<u8, Ps2Error> {
    write_command(CMD_WRITE_TO_AUX)?;
    write_data(byte)?;
    let response = read_data()?;
    if response != MOUSE_RESPONSE_ACK {
        return Err(Ps2Error::NotAcked);
    }
    Ok(response)
}

fn write_to_aux_with_arg(cmd: u8, arg: u8) -> Result<(), Ps2Error> {
    write_to_aux(cmd)?;
    // The mouse acks each byte separately; argument needs its own AUX wrap.
    write_command(CMD_WRITE_TO_AUX)?;
    write_data(arg)?;
    let response = read_data()?;
    if response != MOUSE_RESPONSE_ACK {
        return Err(Ps2Error::NotAcked);
    }
    Ok(())
}

/// Drain any pending bytes from the AUX output buffer.
///
/// The 8042 may have leftover state from BIOS / firmware probing. We discard
/// up to a small bounded count so init starts from a known-empty stream.
fn drain_output() {
    let mut status: Port<u8> = Port::new(PS2_STATUS);
    let mut data: Port<u8> = Port::new(PS2_DATA);
    for _ in 0..16 {
        // SAFETY: status read, then data read if OBF is set.
        let s = unsafe { status.read() };
        if s & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let _ = unsafe { data.read() };
    }
}

// ---------------------------------------------------------------------------
// IntelliMouse "magic knock" handshake
// ---------------------------------------------------------------------------
//
// Setting the sample rate in the sequence 200, 100, 80 then querying the
// device ID is the standard probe — IntelliMouse-capable devices return ID
// 0x03 (3-button + wheel), legacy 3-button devices return 0x00. On QEMU the
// default `-device i8042` is IntelliMouse-capable when started with `-mouse
// usb-mouse` or with the default PS/2 mouse. We accept either result and
// configure the decoder accordingly.

fn try_intellimouse_handshake() -> Result<bool, Ps2Error> {
    write_to_aux_with_arg(MOUSE_CMD_SET_SAMPLE_RATE, 200)?;
    write_to_aux_with_arg(MOUSE_CMD_SET_SAMPLE_RATE, 100)?;
    write_to_aux_with_arg(MOUSE_CMD_SET_SAMPLE_RATE, 80)?;
    write_to_aux(MOUSE_CMD_GET_DEVICE_ID)?;
    let id = read_data()?;
    Ok(id == 0x03)
}

// ---------------------------------------------------------------------------
// Public init entry point
// ---------------------------------------------------------------------------

/// Initialize the AUX port (mouse).
///
/// Returns `Ok(())` after the device is enabled and producing packets, or an
/// error if any step times out / fails. On error the system continues to
/// boot without a mouse — the caller logs a structured warning.
///
/// # Safety
///
/// Performs port I/O and modifies the 8042 controller config byte. Must be
/// called from kernel init context with interrupts disabled (or before the
/// PIC is unmasked for IRQ12).
pub unsafe fn init_mouse() -> Result<(), Ps2Error> {
    // Reset decoder + clear any leftover state from BIOS.
    MOUSE_DECODER.lock().resync();
    drain_output();

    // Step 1 — enable the AUX device port at the controller.
    write_command(CMD_ENABLE_AUX)?;

    // Step 2 — read controller config byte, clear AUX_DISABLE, set AUX_IRQ.
    write_command(CMD_READ_CONFIG)?;
    let mut config = read_data()?;
    config &= !CONFIG_AUX_DISABLE;
    config |= CONFIG_AUX_IRQ;
    write_command(CMD_WRITE_CONFIG)?;
    write_data(config)?;

    // Step 3 — set defaults on the mouse to start from a known state.
    let _ = write_to_aux(MOUSE_CMD_SET_DEFAULTS);

    // Step 4 — IntelliMouse magic-knock; if it fails we fall back silently.
    let wheel = try_intellimouse_handshake().unwrap_or(false);
    if wheel {
        MOUSE_DECODER.lock().enable_wheel_mode();
    }

    // Step 5 — enable streaming. From here, IRQ12 fires on each packet.
    write_to_aux(MOUSE_CMD_ENABLE_STREAMING)?;

    // Drain any acks/leftover bytes the device may have queued.
    drain_output();
    MOUSE_READY.store(true, Ordering::Release);
    Ok(())
}

/// True after [`init_mouse`] has succeeded.
#[allow(dead_code)] // Phase 56 D.2 will read this from the userspace mouse_server health check.
pub fn is_ready() -> bool {
    MOUSE_READY.load(Ordering::Acquire)
}

// Wire-encoding (`encode_packet` + `MOUSE_PACKET_WIRE_SIZE`) lives in
// `kernel_core::input::mouse` so it's host-testable; the kernel side
// re-exports those symbols at the top of this module.
