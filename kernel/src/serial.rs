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
        .map(|()| log::set_max_level(log::LevelFilter::Trace))
        .expect("logger already set");
}
