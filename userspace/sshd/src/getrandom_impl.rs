//! Custom `getrandom` backend that delegates to the kernel's getrandom syscall.

use getrandom::{Error, register_custom_getrandom};

fn kernel_getrandom(dest: &mut [u8]) -> Result<(), Error> {
    let ret = syscall_lib::getrandom(dest);
    if ret < 0 {
        Err(Error::UNEXPECTED)
    } else {
        Ok(())
    }
}

register_custom_getrandom!(kernel_getrandom);
