pub mod x86_64;

pub use x86_64::{enable_interrupts, init};
// enter_userspace is part of the Phase 5 API; no caller yet in the kernel binary.
#[allow(unused_imports)]
pub use x86_64::enter_userspace;
