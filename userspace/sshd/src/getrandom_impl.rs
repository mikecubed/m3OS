//! Custom `getrandom` backend that delegates to the kernel's getrandom syscall.

use getrandom::{Error, register_custom_getrandom};

fn kernel_getrandom(dest: &mut [u8]) -> Result<(), Error> {
    let ret = syscall_lib::getrandom(dest);
    if ret == dest.len() as isize {
        Ok(())
    } else {
        Err(Error::UNEXPECTED)
    }
}

register_custom_getrandom!(kernel_getrandom);
