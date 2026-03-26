use crate::{serial_print, serial_println};
use x86_64::instructions::port::Port;

/// Exit codes for the QEMU ISA debug exit device (I/O port 0xf4).
///
/// QEMU computes the process exit code as `(value << 1) | 1`:
///   - `Success` (0x10) → QEMU exits with code 0x21
///   - `Failure` (0x11) → QEMU exits with code 0x23
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failure = 0x11,
}

/// Write to the ISA debug exit device to terminate QEMU immediately.
pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
    // QEMU exits before reaching here; loop as a safety net.
    loop {
        x86_64::instructions::hlt();
    }
}

/// Wrapper trait that prints the test function name before/after execution.
pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

/// Custom test runner invoked by `#[test_runner]`.
///
/// Iterates all `#[test_case]` functions, runs each via the `Testable` trait,
/// then exits QEMU with success if none panicked.
pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

/// Panic handler used during test builds.
///
/// Prints the failure marker and panic info to serial, then exits QEMU with
/// the failure code so that `cargo xtask test` can detect the error.
pub fn test_panic_handler(info: &core::panic::PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failure);
}
