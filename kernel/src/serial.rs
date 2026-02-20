use spin::Mutex;
use uart_16550::SerialPort;

const COM1_PORT: u16 = 0x3F8;

static SERIAL1: Mutex<Option<SerialPort>> = Mutex::new(None);

pub fn init() {
    let mut serial_port = unsafe { SerialPort::new(COM1_PORT) };
    serial_port.init();
    *SERIAL1.lock() = Some(serial_port);
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    use core::fmt::Write;
    if let Some(ref mut serial) = *SERIAL1.lock() {
        serial.write_fmt(args).expect("serial write failed");
    }
}

/// Write to serial without risking deadlock. Used by the panic handler.
/// Falls back to a fresh port if the mutex is already held.
#[doc(hidden)]
pub fn _panic_print(args: core::fmt::Arguments) {
    use core::fmt::Write;
    if let Some(mut guard) = SERIAL1.try_lock() {
        if let Some(ref mut serial) = *guard {
            let _ = serial.write_fmt(args);
            return;
        }
    }
    let mut serial = unsafe { SerialPort::new(COM1_PORT) };
    serial.init();
    let _ = serial.write_fmt(args);
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*))
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
