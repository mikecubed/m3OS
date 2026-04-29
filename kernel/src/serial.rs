extern crate alloc;

use alloc::vec::Vec;
use core::fmt::{self, Write};
use kernel_core::log_ring::LogRing;
use spin::Mutex;
use uart_16550::SerialPort;

const COM1_PORT: u16 = 0x3F8;
const DMESG_RING_SIZE: usize = 64 * 1024;

static SERIAL1: Mutex<Option<SerialPort>> = Mutex::new(None);
static DMESG_RING: Mutex<LogRing<DMESG_RING_SIZE>> = Mutex::new(LogRing::new());

pub fn init() {
    let mut serial_port = unsafe { SerialPort::new(COM1_PORT) };
    serial_port.init();
    *SERIAL1.lock() = Some(serial_port);
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    let mut serial = SERIAL1.lock();
    if let Some(ref mut serial_port) = *serial {
        serial_port.write_fmt(args).expect("serial write failed");
    }
}

#[doc(hidden)]
pub fn _kernel_print(args: core::fmt::Arguments) {
    let mut ring = DMESG_RING.lock();
    let mut serial = SERIAL1.lock();
    if let Some(ref mut serial_port) = *serial {
        let mut writer = SerialRingWriter {
            serial: Some(serial_port),
            ring: &mut ring,
        };
        writer.write_fmt(args).expect("serial write failed");
    } else {
        let mut writer = SerialRingWriter {
            serial: None,
            ring: &mut ring,
        };
        writer.write_fmt(args).expect("ring write failed");
    }
}

/// Write to serial without risking deadlock. Used by the panic handler.
/// Falls back to a fresh port if the mutex is already held.
#[doc(hidden)]
pub fn _panic_print(args: core::fmt::Arguments) {
    if let Some(mut guard) = SERIAL1.try_lock()
        && let Some(ref mut serial) = *guard
    {
        if let Some(mut ring) = DMESG_RING.try_lock() {
            let mut writer = SerialRingWriter {
                serial: Some(serial),
                ring: &mut ring,
            };
            let _ = writer.write_fmt(args);
        } else {
            let _ = serial.write_fmt(args);
        }
        return;
    }
    let mut serial = unsafe { SerialPort::new(COM1_PORT) };
    serial.init();
    if let Some(mut ring) = DMESG_RING.try_lock() {
        let mut writer = SerialRingWriter {
            serial: Some(&mut serial),
            ring: &mut ring,
        };
        let _ = writer.write_fmt(args);
    } else {
        let _ = serial.write_fmt(args);
    }
}

struct SerialRingWriter<'a> {
    serial: Option<&'a mut SerialPort>,
    ring: &'a mut LogRing<DMESG_RING_SIZE>,
}

impl Write for SerialRingWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if let Some(serial) = self.serial.as_mut() {
            serial.write_str(s)?;
        }
        self.ring.push_bytes(s.as_bytes());
        Ok(())
    }
}

pub fn dmesg_snapshot() -> Vec<u8> {
    let mut out = Vec::with_capacity(DMESG_RING_SIZE);
    DMESG_RING.lock().snapshot_into(&mut out);
    out
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_kernel_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}

// Log crate backend
struct SerialLogger;

impl log::Log for SerialLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            serial_println!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

static LOGGER: SerialLogger = SerialLogger;

pub fn init_logger() {
    log::set_logger(&LOGGER)
        .map(|()| log::set_max_level(log::LevelFilter::Info))
        .expect("logger already set");
}

// ---------------------------------------------------------------------------
// IRQ-driven serial RX ring buffer (lock-free, ISR-safe)
// ---------------------------------------------------------------------------
// Uses atomic head/tail indices (same pattern as the keyboard scancode
// buffers) so the IRQ handler never takes a mutex. Single-producer (IRQ)
// single-consumer (serial feeder task).
// ---------------------------------------------------------------------------

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const SERIAL_BUF_SIZE: usize = 256; // must be power of 2
const SERIAL_BUF_MASK: usize = SERIAL_BUF_SIZE - 1;

static mut SERIAL_RX_RAW: [u8; SERIAL_BUF_SIZE] = [0u8; SERIAL_BUF_SIZE];
static SERIAL_RX_HEAD: AtomicUsize = AtomicUsize::new(0);
static SERIAL_RX_TAIL: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Notification-based feeder wake infrastructure (Phase 57a H.1)
// ---------------------------------------------------------------------------
//
// Mirrors the pattern used by `net::virtio_net` for the network task:
//   - `STDIN_FEEDER_TASK_ID` stores the TaskId of the feeder so the ISR can
//     call `wake_task_v2` without taking any lock.
//   - `STDIN_FEEDER_WOKEN` is the `AtomicBool` that `serial_stdin_feeder_task`
//     parks on via `block_current_until`.  The ISR sets it; the feeder clears
//     it (via `swap`) at the top of each drain iteration.
//
// Using 0 as the "not yet registered" sentinel is safe because Task IDs are
// allocated starting at 1 in `Task::new`.

/// TaskId of the serial-stdin feeder task.  Registered by the task itself
/// before it first parks.  The COM1 RX ISR reads this to issue a
/// `wake_task_v2` after writing bytes to the ring buffer.
pub static STDIN_FEEDER_TASK_ID: AtomicU64 = AtomicU64::new(0);

/// Unified wake flag for the serial-stdin feeder task.
///
/// The COM1 RX ISR sets this (via [`wake_feeder_task`]) when it drains at
/// least one byte into `SERIAL_RX_RAW`.  The feeder task parks on it via
/// `block_current_until(&STDIN_FEEDER_WOKEN, None)` and clears it with a
/// `swap(false, …)` at the top of each drain loop — identical to the
/// `net::NIC_WOKEN` pattern in `net_task`.
pub static STDIN_FEEDER_WOKEN: AtomicBool = AtomicBool::new(false);

/// Register the feeder task's [`TaskId`] so the COM1 RX ISR can wake it.
///
/// Called once from `serial_stdin_feeder_task` before it first parks.
pub fn set_feeder_task_id(id: crate::task::TaskId) {
    STDIN_FEEDER_TASK_ID.store(id.0, Ordering::Release);
    log::info!("[serial-stdin] registered feeder task id={}", id.0);
}

/// Set `STDIN_FEEDER_WOKEN` and issue a `wake_task_v2` IPI to the feeder.
///
/// Called from the COM1 RX ISR after bytes have been pushed into the ring
/// buffer.  Must be ISR-safe: no allocation, no mutex, no blocking.
pub fn wake_feeder_task() {
    STDIN_FEEDER_WOKEN.store(true, Ordering::Release);
    let raw = STDIN_FEEDER_TASK_ID.load(Ordering::Acquire);
    if raw != 0 {
        let _ = crate::task::scheduler::wake_task_v2(crate::task::TaskId(raw));
    } else {
        // Task not yet registered — fall back to a generic reschedule hint so
        // other cores notice the flag on their next tick.
        crate::task::scheduler::signal_reschedule();
    }
}

/// Pop one byte from the serial RX ring buffer, or `None` if empty.
/// Single-consumer: only called from the serial feeder task.
pub fn serial_rx_pop() -> Option<u8> {
    let head = SERIAL_RX_HEAD.load(Ordering::Acquire);
    let tail = SERIAL_RX_TAIL.load(Ordering::Acquire);
    if head == tail {
        return None;
    }
    // Safety: single consumer; head is only advanced here.
    let byte = unsafe { SERIAL_RX_RAW[head] };
    SERIAL_RX_HEAD.store((head + 1) & SERIAL_BUF_MASK, Ordering::Release);
    Some(byte)
}

/// Called from the serial IRQ handler. Drains all available bytes from the
/// UART FIFO into the lock-free ring buffer. No mutex is taken — safe to
/// call from interrupt context.
pub fn handle_serial_irq() {
    let mut got_data = false;
    loop {
        let lsr: u8 = unsafe { x86_64::instructions::port::Port::new(0x3FDu16).read() };
        if lsr & 1 == 0 {
            break;
        }
        let byte: u8 = unsafe { x86_64::instructions::port::Port::new(0x3F8u16).read() };
        let tail = SERIAL_RX_TAIL.load(Ordering::Relaxed);
        let next = (tail + 1) & SERIAL_BUF_MASK;
        if next != SERIAL_RX_HEAD.load(Ordering::Acquire) {
            // Safety: single producer (IRQ handler); tail only advanced here.
            unsafe { SERIAL_RX_RAW[tail] = byte };
            SERIAL_RX_TAIL.store(next, Ordering::Release);
        }
        // else: buffer full — drop byte (prefer losing data over blocking ISR)
        got_data = true;
    }
    if got_data {
        // Set the legacy pending flag (still read during feeder startup before
        // the task ID is registered, and harmless to keep for diagnostics).
        SERIAL_RX_PENDING.store(true, Ordering::Release);
        // H.1: wake the feeder task via the notification-based protocol.
        // This calls wake_task_v2 which issues a cross-core IPI if the feeder
        // is parked on another CPU — same mechanism as virtio-net.
        wake_feeder_task();
    }
}

/// Atomic flag set by the IRQ handler when new data is available.
/// The feeder task clears it under disabled interrupts to avoid lost wakeups.
pub static SERIAL_RX_PENDING: AtomicBool = AtomicBool::new(false);
