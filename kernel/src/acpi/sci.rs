//! Phase 101 Track D — the kernel half of the SCI path.
//!
//! The System Control Interrupt is a single **level-triggered** line that
//! demuxes every ACPI fixed event (power button, RTC) and General-Purpose
//! Event (lid, EC, battery). The kernel must own the hardware ack: a
//! level SCI whose status bits stay set re-asserts the instant it is
//! EOI'd, so a userspace-only handler would storm. The ISR here does the
//! minimum the contract allows — read the PM1a/GPE0 status+enable
//! registers, **mask** the asserted enable bits (de-asserting the line),
//! EOI, and signal the subscribed `Notification`. All *policy* (which
//! `_Lxx`/`_Exx` method to run, what the event means, clearing status and
//! re-arming the enables) lives in the ring-3 `acpid`, which reaches
//! these registers through `SYS_ACPI_PM_READ`/`WRITE`.
//!
//! Register layout (ACPI 6.5 §4.8): the PM1 event block is two 16-bit
//! halves (status, then enable); the GPE0 block is two byte-array halves
//! (status bytes, then enable bytes). Ports come from the cached
//! [`super::FadtInfo`], never from userspace.

use core::sync::atomic::{AtomicU8, Ordering};

use kernel_core::device_host::syscalls::{
    ACPI_PM_REG_GPE0_EN, ACPI_PM_REG_GPE0_STS, ACPI_PM_REG_PM1A_CNT, ACPI_PM_REG_PM1A_EN,
    ACPI_PM_REG_PM1A_STS, ACPI_PM_REG_SMI_CMD, ACPI_SCI_BIT_GPE, ACPI_SCI_BIT_PM1,
};
use x86_64::instructions::port::Port;

use crate::ipc::notification::NotifId;

/// `NotifId.0` of the subscribed `acpid` notification; `0xFF` = none.
/// Same publish discipline as the device-IRQ shim: the ISR loads with
/// `Acquire`, the subscriber stores with `Release` after routing is set up.
static SCI_NOTIF: AtomicU8 = AtomicU8::new(0xFF);

/// Register the (single) SCI subscriber. Returns `false` if one is
/// already registered.
pub fn set_subscriber(notif: NotifId) -> bool {
    SCI_NOTIF
        .compare_exchange(0xFF, notif.0, Ordering::Release, Ordering::Relaxed)
        .is_ok()
}

/// A PM register resolved from the FADT: I/O port + 16-bit-wide flag.
pub fn pm_reg_port(sel: u64, byte_index: u64) -> Option<(u16, bool)> {
    let f = super::fadt_info()?;
    let gpe_half = (f.gpe0_blk_len / 2) as u64;
    match sel {
        ACPI_PM_REG_PM1A_STS if f.pm1a_evt_blk != 0 => Some((f.pm1a_evt_blk as u16, true)),
        ACPI_PM_REG_PM1A_EN if f.pm1a_evt_blk != 0 => {
            Some(((f.pm1a_evt_blk + (f.pm1_evt_len as u32) / 2) as u16, true))
        }
        ACPI_PM_REG_PM1A_CNT if f.pm1a_cnt_blk != 0 => Some((f.pm1a_cnt_blk as u16, true)),
        ACPI_PM_REG_GPE0_STS if f.gpe0_blk != 0 && byte_index < gpe_half => {
            Some(((f.gpe0_blk as u64 + byte_index) as u16, false))
        }
        ACPI_PM_REG_GPE0_EN if f.gpe0_blk != 0 && byte_index < gpe_half => {
            Some(((f.gpe0_blk as u64 + gpe_half + byte_index) as u16, false))
        }
        ACPI_PM_REG_SMI_CMD if f.smi_cmd != 0 => Some((f.smi_cmd as u16, false)),
        _ => None,
    }
}

/// The SCI ISR body: demux PM1a + GPE0, mask what asserted, signal the
/// subscriber. Interrupt context — port I/O and atomics only (no
/// allocation, no locks, no IPC). Returns whether any event was pending
/// (a `false` is a spurious/shared-line pass-through).
pub fn sci_demux() -> bool {
    let Some(f) = super::fadt_info() else {
        return false;
    };
    let mut any = false;

    // PM1a fixed events: 16-bit status + enable halves.
    if f.pm1a_evt_blk != 0 && f.pm1_evt_len >= 4 {
        let sts_port = f.pm1a_evt_blk as u16;
        let en_port = (f.pm1a_evt_blk + (f.pm1_evt_len as u32) / 2) as u16;
        // SAFETY: FADT-declared PM1a event block ports, 16-bit accesses
        // per ACPI §4.8.1; interrupt context is the intended reader.
        unsafe {
            let sts: u16 = Port::new(sts_port).read();
            let mut en_reg: Port<u16> = Port::new(en_port);
            let en: u16 = en_reg.read();
            let pending = sts & en;
            if pending != 0 {
                // Mask the asserted enables so the level line drops;
                // acpid re-arms them after servicing.
                en_reg.write(en & !pending);
                any = true;
                let notif = SCI_NOTIF.load(Ordering::Acquire);
                if notif != 0xFF {
                    crate::ipc::notification::signal_irq_bit(NotifId(notif), ACPI_SCI_BIT_PM1);
                }
            }
        }
    }

    // GPE0 events: byte-array status + enable halves.
    if f.gpe0_blk != 0 && f.gpe0_blk_len >= 2 {
        let half = (f.gpe0_blk_len / 2) as u32;
        let mut any_gpe = false;
        for i in 0..half {
            // SAFETY: FADT-declared GPE0 block ports, byte accesses per
            // ACPI §4.8.4.1; interrupt context is the intended reader.
            unsafe {
                let sts: u8 = Port::new((f.gpe0_blk + i) as u16).read();
                let mut en_reg: Port<u8> = Port::new((f.gpe0_blk + half + i) as u16);
                let en: u8 = en_reg.read();
                let pending = sts & en;
                if pending != 0 {
                    en_reg.write(en & !pending);
                    any_gpe = true;
                }
            }
        }
        if any_gpe {
            any = true;
            let notif = SCI_NOTIF.load(Ordering::Acquire);
            if notif != 0xFF {
                crate::ipc::notification::signal_irq_bit(NotifId(notif), ACPI_SCI_BIT_GPE);
            }
        }
    }

    any
}

/// Phase 103 F.3 — restore the SCI plumbing after an S3 resume. The
/// wake-side machine reset cleared the IOAPIC redirection entry
/// `route_sci` installed at subscribe time (the resume path's
/// `ioapic_init` re-writes only the standard ISA routes) and zeroed the
/// PM1 enable register (acpid armed `PWRBTN_EN` at boot). Re-arm both
/// when a subscriber exists so the power button keeps working after a
/// suspend/resume cycle. GPE0 enables are NOT restored here — a
/// documented residual until a hardware platform needs post-resume
/// `Notify` events (QEMU's fixed power button is PM1-only).
pub fn reroute_after_resume() {
    if SCI_NOTIF.load(Ordering::Acquire) == 0xFF {
        return;
    }
    let Some(fadt) = super::fadt_info() else {
        return;
    };
    crate::arch::x86_64::apic::route_sci(
        fadt.sci_int,
        crate::arch::x86_64::interrupts::InterruptIndex::Sci as u8,
    );
    if fadt.pm1a_evt_blk != 0 {
        // PWRBTN_EN is bit 8 of the PM1 enable register (ACPI §4.8.4.1).
        let en_port = (fadt.pm1a_evt_blk + (fadt.pm1_evt_len as u32) / 2) as u16;
        // SAFETY: FADT-resolved PM1a enable register, 16-bit per spec.
        unsafe {
            let cur: u16 = Port::<u16>::new(en_port).read();
            Port::<u16>::new(en_port).write(cur | (1 << 8));
        }
    }
    log::info!("[acpi] SCI re-routed + PWRBTN re-armed after S3 resume");
}
