#![no_std]
#![no_main]

use syscall_lib::{
    AF_INET, SOCK_DGRAM, STDOUT_FILENO, SockaddrIn, bind, close, connect, exit, socket, write,
    write_str,
};

const LOCAL_PORT: u16 = 40123;
const REMOTE_PORT: u16 = 9999;
const REMOTE_IP: [u8; 4] = [10, 0, 2, 2];

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    let fd = socket(AF_INET as i32, SOCK_DGRAM as i32, 0);
    if fd < 0 {
        write_str(STDOUT_FILENO, "udp-smoke: socket() failed\n");
        return 1;
    }
    let fd = fd as i32;

    let local = SockaddrIn::new([0, 0, 0, 0], LOCAL_PORT);
    if bind(fd, &local) < 0 {
        write_str(STDOUT_FILENO, "udp-smoke: bind() failed\n");
        close(fd);
        return 2;
    }

    let remote = SockaddrIn::new(REMOTE_IP, REMOTE_PORT);
    if connect(fd, &remote) < 0 {
        write_str(STDOUT_FILENO, "udp-smoke: connect() failed\n");
        close(fd);
        return 3;
    }

    let payload = b"phase54-udp-smoke";
    if write(fd, payload) != payload.len() as isize {
        write_str(STDOUT_FILENO, "udp-smoke: write() failed\n");
        close(fd);
        return 4;
    }

    write_str(STDOUT_FILENO, "udp-smoke: PASS\n");
    close(fd);
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(101)
}
