#![no_std]
#![no_main]

mod serial;

use bootloader_api::{entry_point, BootInfo};

entry_point!(kernel_main);

fn kernel_main(_boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    serial::init_logger();

    serial_println!("[ostest] Hello from kernel!");
    log::info!("Kernel initialized");

    hlt_loop();
}

fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    if let Some(location) = info.location() {
        serial_println!(
            "KERNEL PANIC at {}:{}",
            location.file(),
            location.line()
        );
    } else {
        serial_println!("KERNEL PANIC at unknown location");
    }
    serial_println!("  {}", info.message());
    hlt_loop();
}
