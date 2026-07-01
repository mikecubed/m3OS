//! Phase 101 Tracks D/E — the thin platform-ACPI syscall surface the
//! ring-3 `acpid` binds.
//!
//! Three capabilities, all gated on the unforgeable `/drivers/` exec-path
//! (the `sys_device_pci_enumerate` policy — there is no PCI function to
//! claim for platform ACPI):
//!
//! - **Table fetch** ([`sys_acpi_table_get`]) — read-only copies of the
//!   firmware's FACP/DSDT/SSDT bytes. `acpid` parses them; the kernel
//!   never interprets AML.
//! - **SCI subscription** ([`sys_acpi_sci_subscribe`]) — routes the
//!   FADT's `SCI_INT` GSI to the demux ISR and hands back a
//!   `Capability::Notification` the ISR signals.
//! - **PM register access** ([`sys_acpi_pm_read`]/[`sys_acpi_pm_write`])
//!   — role-named PM1a/GPE0/`SMI_CMD` register I/O. Ring 3 names a
//!   register *selector*; the kernel resolves the FADT-declared port and
//!   width, so `acpid` can never address an arbitrary port.

use kernel_core::device_host::syscalls::{ACPI_PM_REG_SMI_CMD, NOTIFICATION_SENTINEL_NEW};
use kernel_core::ipc::Capability;
use x86_64::instructions::port::Port;

use crate::syscall::device_host::is_authorized_driver_process;
use crate::task::scheduler;

const NEG_EACCES: isize = -13;
const NEG_ESRCH: isize = -3;
const NEG_EFAULT: isize = -14;
const NEG_ENOENT: isize = -2;
const NEG_ENODEV: isize = -19;
const NEG_EINVAL: isize = -22;
const NEG_ENOMEM: isize = -12;
const NEG_EBUSY: isize = -16;

/// Common gate: resolve the calling PID and require a `/drivers/` binary.
fn gate() -> Result<(), isize> {
    let pid = crate::process::current_pid();
    if pid == 0 {
        return Err(NEG_ESRCH);
    }
    if !is_authorized_driver_process(pid) {
        return Err(NEG_EACCES);
    }
    Ok(())
}

/// `sys_acpi_table_get(sig_ptr, index, out_ptr, out_len) -> isize`
///
/// Copies up to `out_len` bytes of the `index`-th table matching the
/// 4-byte signature at `sig_ptr` into `out_ptr`, and returns the table's
/// **full** length — a short buffer yields a truncated copy the caller
/// detects by comparing lengths (the `pci_enumerate` contract). An
/// `out_len` of 0 is a pure size query.
pub fn sys_acpi_table_get(
    sig_user_ptr: usize,
    index: usize,
    out_user_ptr: usize,
    out_len: usize,
) -> isize {
    if let Err(e) = gate() {
        return e;
    }

    let mut sig = [0u8; 4];
    let sig_ok = crate::mm::user_mem::UserSliceRo::new(sig_user_ptr as u64, 4)
        .and_then(|s| s.copy_to_kernel(&mut sig));
    if sig_ok.is_err() {
        return NEG_EFAULT;
    }

    let Some(hdr_ptr) = crate::acpi::table_by_index(&sig, index) else {
        return NEG_ENOENT;
    };
    let hdr_virt = hdr_ptr as usize;
    // SAFETY: table_by_index validated the header; length is its declared
    // byte count including the 36-byte header.
    let length = unsafe {
        let len_ptr = core::ptr::addr_of!((*hdr_ptr).length);
        core::ptr::read_unaligned(len_ptr)
    } as usize;
    if length < 36 {
        return NEG_ENOENT;
    }

    let to_copy = length.min(out_len);
    if to_copy > 0 {
        // SAFETY: the whole table is identity-mapped firmware memory;
        // `table_by_index` checksum-validated `length` bytes at hdr_virt.
        let bytes = unsafe { core::slice::from_raw_parts(hdr_virt as *const u8, to_copy) };
        let copy = crate::mm::user_mem::UserSliceWo::new(out_user_ptr as u64, to_copy)
            .and_then(|s| s.copy_from_kernel(bytes));
        if copy.is_err() {
            return NEG_EFAULT;
        }
    }
    length as isize
}

/// `sys_acpi_sci_subscribe(notification_arg) -> isize`
///
/// Routes the SCI and returns a fresh `Capability::Notification` handle
/// the demux ISR signals (`ACPI_SCI_BIT_PM1`/`ACPI_SCI_BIT_GPE`). Only
/// the fresh-allocation sentinel is accepted, and only one subscriber may
/// exist — the SCI is a platform singleton, not a per-device line.
pub fn sys_acpi_sci_subscribe(notification_arg: u32) -> isize {
    if let Err(e) = gate() {
        return e;
    }
    if notification_arg != NOTIFICATION_SENTINEL_NEW {
        return NEG_EINVAL;
    }
    let Some(fadt) = crate::acpi::fadt_info() else {
        return NEG_ENODEV;
    };
    if fadt.sci_int == 0 {
        return NEG_ENODEV;
    }
    let task_id = match scheduler::current_task_id() {
        Some(t) => t,
        None => return NEG_ESRCH,
    };

    let Some(notif) = crate::ipc::notification::try_create() else {
        return NEG_ENOMEM;
    };
    if !crate::acpi::sci::set_subscriber(notif) {
        crate::ipc::notification::free(notif);
        return NEG_EBUSY;
    }
    let handle = match scheduler::insert_cap(task_id, Capability::Notification(notif)) {
        Ok(h) => h,
        Err(_) => {
            // The subscriber slot stays set (harmless — the ISR signals a
            // freed notification id at worst once); the notification slot
            // is what must not leak.
            crate::ipc::notification::free(notif);
            return NEG_ENOMEM;
        }
    };

    // Route the GSI last, after the ISR's notification target is
    // published — the first SCI can fire the instant the redirection
    // entry unmasks.
    crate::arch::x86_64::apic::route_sci(
        fadt.sci_int,
        crate::arch::x86_64::interrupts::InterruptIndex::Sci as u8,
    );
    handle as isize
}

/// `sys_acpi_pm_read(reg_sel, byte_index) -> isize` — the register value,
/// zero-extended.
pub fn sys_acpi_pm_read(reg_sel: u64, byte_index: u64) -> isize {
    if let Err(e) = gate() {
        return e;
    }
    let Some((port, wide)) = crate::acpi::sci::pm_reg_port(reg_sel, byte_index) else {
        return NEG_EINVAL;
    };
    // SAFETY: the port was resolved from the validated FADT by role; PM1
    // registers are 16-bit, GPE/SMI_CMD 8-bit, per ACPI §4.8.
    unsafe {
        if wide {
            let v: u16 = Port::new(port).read();
            v as isize
        } else {
            let v: u8 = Port::new(port).read();
            v as isize
        }
    }
}

/// `sys_acpi_pm_write(reg_sel, byte_index, value) -> isize` — 0 on success.
pub fn sys_acpi_pm_write(reg_sel: u64, byte_index: u64, value: u64) -> isize {
    if let Err(e) = gate() {
        return e;
    }
    let Some((port, wide)) = crate::acpi::sci::pm_reg_port(reg_sel, byte_index) else {
        return NEG_EINVAL;
    };
    // The ACPI-enable handshake is the one legitimate SMI_CMD write; log
    // it so bring-up is visible in dmesg on hardware.
    if reg_sel == ACPI_PM_REG_SMI_CMD {
        log::info!(
            "[acpi] SMI_CMD {:#x} <- {:#x} (ACPI enable handshake)",
            port,
            value
        );
    }
    // SAFETY: role-resolved FADT port, width per ACPI §4.8.
    unsafe {
        if wide {
            Port::<u16>::new(port).write(value as u16);
        } else {
            Port::<u8>::new(port).write(value as u8);
        }
    }
    0
}
