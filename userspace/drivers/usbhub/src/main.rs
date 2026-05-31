//! Ring-3 USB hub class driver — Phase 78b Track B.
//!
//! This daemon is responsible for enumerating USB hubs detected by the xHCI
//! host-controller driver, applying `SET_FEATURE(PORT_POWER)` to bring each
//! downstream port out of power-off, watching for `PORT_RESET` completions,
//! and reporting newly-attached devices back to the kernel's device-host
//! substrate via the `usb-core` IPC protocol.
//!
//! # Phase status
//!
//! **Phase 78b Track B** implements the hub class logic in
//! `kernel_core::usb::hub` (descriptor parsing, `PortId` topology tree,
//! route-string computation) and wires this daemon into the build + service
//! machinery. The xHCI server now publishes the `usb` service (Phase 78c), but
//! it only classifies + enumerates ROOT-hub HID devices; **live external-hub
//! enumeration (devices behind a `usb-hub`) is deferred to Phase 90 (USB Class
//! Expansion)**, which adds hub-class publishing + the `SET_FEATURE`
//! PORT_POWER/PORT_RESET child-device path. Until then the daemon starts, logs
//! its presence, exercises the hub logic at a call site so it is verified at
//! build time, and exits cleanly — exactly as `xhci_driver` exits 0 when no
//! controller is present, so the service manager marks the service stopped
//! without burning its restart budget.
//!
//! # Live enumeration path (Phase 90, deferred)
//!
//! Once the xHCI server publishes hub-class `AttachNotice`s and serves
//! `GetDescriptors`/`ControlRequest` for hubs:
//!
//! 1. Receive `AttachNotice { interface_class: CLASS_HUB, … }`.
//! 2. Issue `GET_DESCRIPTOR(Hub)` via the `UsbClient` request channel.
//! 3. Parse the `HubDescriptor` and call `PortTopology::add_root_port` (or
//!    `add_child_port` for nested hubs) to register the hub in the tree.
//! 4. For each downstream port, send `SET_FEATURE(PORT_POWER)` then wait for
//!    `PORT_CONNECTION` status change; on connect, send `SET_FEATURE(PORT_RESET)`.
//! 5. On `PORT_RESET` completion, report the new device to the kernel.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "usbhub: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "usbhub: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

// ---------------------------------------------------------------------------
// Public constants (load-bearing: asserted in the smoke test below)
// ---------------------------------------------------------------------------

/// Boot-log marker written when the hub daemon starts.
///
/// The `xhci-bringup-smoke` / future `usbhub-smoke` gate asserts this line
/// appears in serial output so the test infrastructure can confirm the daemon
/// reached `program_main`.
pub const BOOT_LOG_MARKER: &str = "usbhub: spawned\n";

// ---------------------------------------------------------------------------
// Hub-class interface classifier (exercises kernel_core::usb::hub at the
// usbhub build site, confirming the link is live even while the IPC path
// is dormant in 78b).
// ---------------------------------------------------------------------------

/// Returns `true` if `b_interface_class` identifies a USB Hub interface.
///
/// Delegates to [`kernel_core::usb::hub::is_hub_interface`].  Called here to
/// give the build a real reference into the hub logic crate — the compiler
/// verifies the symbol resolves and the ABI matches even when the live IPC
/// path is not yet wired up.
pub fn classify_hub_interface(b_interface_class: u8) -> bool {
    kernel_core::usb::hub::is_hub_interface(b_interface_class)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Hub daemon main — Phase 78b.
///
/// Logs [`BOOT_LOG_MARKER`], exercises the hub classifier (verifying the link
/// into `kernel_core::usb::hub`) and exits 0.  Live external-hub enumeration via
/// the `usb` service lands in Phase 90 (USB Class Expansion).
#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    // Confirm the hub classifier is reachable from this daemon's build — any
    // broken import or ABI mismatch would cause a link error here.
    let _ = classify_hub_interface(0x09); // CLASS_HUB

    // Phase 90: receive hub AttachNotices, enumerate downstream ports, apply
    // PORT_POWER / PORT_RESET, report child devices. For now, exit cleanly so
    // init's `on-failure` policy marks the service stopped rather than looping.
    0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{BOOT_LOG_MARKER, classify_hub_interface};

    #[test]
    fn boot_log_marker_correct() {
        assert_eq!(BOOT_LOG_MARKER, "usbhub: spawned\n");
    }

    #[test]
    fn classify_hub_interface_hub_class() {
        // CLASS_HUB = 0x09
        assert!(classify_hub_interface(0x09));
    }

    #[test]
    fn classify_hub_interface_non_hub_class() {
        // CLASS_HID = 0x03, not a hub.
        assert!(!classify_hub_interface(0x03));
        assert!(!classify_hub_interface(0x00));
        assert!(!classify_hub_interface(0xFF));
    }
}
