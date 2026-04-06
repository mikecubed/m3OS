//! Local APIC and I/O APIC initialization.
//!
//! Migrates interrupt routing from the legacy 8259 PIC to the APIC system.
//! After `init()` returns, the LAPIC timer drives vector 32 (replacing the
//! PIT) and the I/O APIC routes keyboard (IRQ 1) to vector 33.

use spin::Once;
use x86_64::instructions::port::Port;

use super::interrupts::USING_APIC;

// ===========================================================================
// Local APIC
// ===========================================================================

/// LAPIC register offsets (memory-mapped, 32-bit aligned).
const LAPIC_ID: usize = 0x020;
const LAPIC_VERSION: usize = 0x030;
const LAPIC_TPR: usize = 0x080;
const LAPIC_EOI: usize = 0x0B0;
const LAPIC_SPURIOUS: usize = 0x0F0;
#[allow(dead_code)]
const LAPIC_ICR_LOW: usize = 0x300;
#[allow(dead_code)]
const LAPIC_ICR_HIGH: usize = 0x310;
const LAPIC_LVT_TIMER: usize = 0x320;
const LAPIC_TIMER_INIT_COUNT: usize = 0x380;
const LAPIC_TIMER_CURRENT_COUNT: usize = 0x390;
const LAPIC_TIMER_DIVIDE_CONFIG: usize = 0x3E0;

/// Convert the LAPIC physical address to a virtual address using the
/// bootloader's identity-mapped physical memory offset.
fn lapic_base() -> usize {
    let phys = crate::acpi::local_apic_address() as u64;
    (crate::mm::phys_offset() + phys) as usize
}

/// Read a 32-bit LAPIC register.
///
/// # Safety
/// `offset` must be a valid LAPIC register offset. The caller must ensure the
/// LAPIC MMIO region is mapped.
unsafe fn lapic_read(offset: usize) -> u32 {
    unsafe {
        // SAFETY: LAPIC MMIO is identity-mapped by the bootloader. The register
        // is naturally aligned and 32 bits wide — volatile access is correct.
        core::ptr::read_volatile((lapic_base() + offset) as *const u32)
    }
}

/// Write a 32-bit LAPIC register.
///
/// # Safety
/// `offset` must be a valid LAPIC register offset. The caller must ensure the
/// LAPIC MMIO region is mapped and the write is semantically safe.
unsafe fn lapic_write(offset: usize, value: u32) {
    unsafe {
        // SAFETY: same as `lapic_read`.
        core::ptr::write_volatile((lapic_base() + offset) as *mut u32, value);
    }
}

/// Signal end-of-interrupt to the Local APIC.
///
/// Must be called at the end of every APIC-routed interrupt handler (except
/// spurious interrupts, which must **not** send EOI).
pub fn lapic_eoi() {
    // SAFETY: writing 0 to the EOI register is always safe when called from
    // an interrupt handler after the interrupt has been serviced.
    unsafe {
        lapic_write(LAPIC_EOI, 0);
    }
}

/// Enable the Local APIC, set the spurious-interrupt vector, and zero TPR.
fn lapic_init() {
    unsafe {
        // Enable the LAPIC: set bit 8 (software enable) and use vector 0xFF
        // for spurious interrupts.
        let spur = lapic_read(LAPIC_SPURIOUS);
        lapic_write(LAPIC_SPURIOUS, spur | 0x1FF);

        // Accept all interrupt priorities.
        lapic_write(LAPIC_TPR, 0);

        let id = lapic_read(LAPIC_ID) >> 24;
        let ver = lapic_read(LAPIC_VERSION);
        log::info!("[apic] LAPIC enabled: id={}, version={:#x}", id, ver & 0xFF);
    }
}

// ===========================================================================
// I/O APIC
// ===========================================================================

#[allow(dead_code)]
const IOAPIC_REGSEL: usize = 0x00;
const IOAPIC_WIN: usize = 0x10;

/// Convert the I/O APIC physical address to a virtual address.
fn ioapic_base() -> usize {
    let phys = crate::acpi::io_apic_address().expect("no I/O APIC found in MADT") as u64;
    (crate::mm::phys_offset() + phys) as usize
}

/// Read an I/O APIC register via the indirect register-select / data-window
/// mechanism.
///
/// # Safety
/// `reg` must be a valid I/O APIC register index. The I/O APIC MMIO region
/// must be mapped.
unsafe fn ioapic_read(reg: u32) -> u32 {
    unsafe {
        let base = ioapic_base();
        // SAFETY: I/O APIC MMIO is identity-mapped. Writing to REGSEL selects
        // the register; reading from WIN returns its value.
        core::ptr::write_volatile(base as *mut u32, reg);
        core::ptr::read_volatile((base + IOAPIC_WIN) as *const u32)
    }
}

/// Write an I/O APIC register.
///
/// # Safety
/// Same requirements as `ioapic_read`, plus the value must be semantically
/// valid for the target register.
unsafe fn ioapic_write(reg: u32, value: u32) {
    unsafe {
        let base = ioapic_base();
        // SAFETY: see `ioapic_read`.
        core::ptr::write_volatile(base as *mut u32, reg);
        core::ptr::write_volatile((base + IOAPIC_WIN) as *mut u32, value);
    }
}

/// Write a 64-bit redirection table entry for the given GSI (pin).
///
/// # Safety
/// `gsi` must be within the I/O APIC's redirection entry count.
unsafe fn ioapic_write_redir(gsi: u32, low: u32, high: u32) {
    unsafe {
        let reg_low = 0x10 + 2 * gsi;
        let reg_high = 0x11 + 2 * gsi;
        // SAFETY: caller guarantees `gsi` is valid.
        ioapic_write(reg_low, low);
        ioapic_write(reg_high, high);
    }
}

/// Decode MADT IRQ override flags into (active_low, level_triggered) bools.
///
/// ISA bus default: active-high, edge-triggered.
fn decode_override_flags(flags: u16) -> (bool, bool) {
    let polarity = flags & 0x03;
    let trigger = (flags >> 2) & 0x03;

    // Polarity: 00 = bus default (active-high for ISA), 01 = active-high,
    //           11 = active-low
    let active_low = polarity == 0x03;

    // Trigger: 00 = bus default (edge for ISA), 01 = edge, 11 = level
    let level_triggered = trigger == 0x03;

    (active_low, level_triggered)
}

/// Build the low 32 bits of a redirection table entry.
fn redir_entry_low(vector: u8, active_low: bool, level_triggered: bool, masked: bool) -> u32 {
    let mut low = vector as u32; // bits 7:0 — vector
    // delivery mode 000 (fixed) — bits 10:8 already zero
    // dest mode 0 (physical) — bit 11 already zero
    if active_low {
        low |= 1 << 13; // polarity: active-low
    }
    if level_triggered {
        low |= 1 << 15; // trigger: level
    }
    if masked {
        low |= 1 << 16; // mask bit
    }
    low
}

/// Convert a GSI to an I/O APIC pin index and validate it fits in the
/// redirection table. Returns `None` if the GSI is below `gsi_base` or
/// the resulting pin exceeds `max_redir`.
fn gsi_to_pin(gsi: u32, gsi_base: u32, max_redir: u32) -> Option<u32> {
    let pin = gsi.checked_sub(gsi_base)?;
    if pin <= max_redir { Some(pin) } else { None }
}

/// Program the I/O APIC redirection table.
///
/// Routes keyboard (IRQ 1) to vector 33 and configures COM1 (IRQ 4) to use
/// vector 36 on the BSP, but keeps the COM1 entry masked until a serial IRQ
/// handler is installed. All other entries are masked.
fn ioapic_init() {
    unsafe {
        // Read the maximum redirection entry count from version register (reg 1).
        let ver = ioapic_read(1);
        let max_redir = (ver >> 16) & 0xFF;
        log::info!(
            "[apic] I/O APIC version={:#x}, max redirection entries={}",
            ver & 0xFF,
            max_redir + 1
        );
        IOAPIC_MAX_REDIR.call_once(|| max_redir);

        // BSP LAPIC ID in the high byte of the destination field.
        let bsp_lapic_id = lapic_read(LAPIC_ID) & 0xFF00_0000; // already in bits 24-31

        // --- Mask all entries first ---
        for pin in 0..=max_redir {
            let low = redir_entry_low(0, false, false, true); // masked
            ioapic_write_redir(pin, low, 0);
        }

        let gsi_base = crate::acpi::ioapic_gsi_base();

        // --- Keyboard: ISA IRQ 1 → vector 33 ---
        {
            let (gsi, active_low, level_triggered) = if let Some(ovr) = crate::acpi::irq_override(1)
            {
                let (al, lt) = decode_override_flags(ovr.flags);
                (ovr.global_system_interrupt, al, lt)
            } else {
                (gsi_base + 1, false, false) // ISA default: active-high, edge
            };
            if let Some(pin) = gsi_to_pin(gsi, gsi_base, max_redir) {
                let low = redir_entry_low(33, active_low, level_triggered, false);
                ioapic_write_redir(pin, low, bsp_lapic_id);
                log::info!(
                    "[apic] I/O APIC: IRQ 1 → GSI {} (pin {}) → vector 33 (active_low={}, level={})",
                    gsi,
                    pin,
                    active_low,
                    level_triggered
                );
            } else {
                log::warn!(
                    "[apic] I/O APIC: IRQ 1 GSI {} not routable (base={}, max_pin={}); skipped",
                    gsi,
                    gsi_base,
                    max_redir
                );
            }
        }

        // --- COM1: ISA IRQ 4 → vector 36 ---
        // Kept masked: no IDT handler for vector 36 is installed yet.
        // Unmasking without a handler would triple-fault on UART interrupts.
        // A future serial IRQ handler can unmask this entry when ready.
        {
            let (gsi, active_low, level_triggered) = if let Some(ovr) = crate::acpi::irq_override(4)
            {
                let (al, lt) = decode_override_flags(ovr.flags);
                (ovr.global_system_interrupt, al, lt)
            } else {
                (gsi_base + 4, false, false)
            };
            if let Some(pin) = gsi_to_pin(gsi, gsi_base, max_redir) {
                let low = redir_entry_low(36, active_low, level_triggered, true); // masked
                ioapic_write_redir(pin, low, bsp_lapic_id);
                log::info!(
                    "[apic] I/O APIC: IRQ 4 → GSI {} (pin {}) → vector 36 (masked, no handler yet)",
                    gsi,
                    pin,
                );
            } else {
                log::warn!(
                    "[apic] I/O APIC: IRQ 4 GSI {} not routable (base={}, max_pin={}); skipped",
                    gsi,
                    gsi_base,
                    max_redir
                );
            }
        }

        // --- Timer: ISA IRQ 0 ---
        // The PIT timer (IRQ 0) may have an override (QEMU maps it to GSI 2).
        // We route it to vector 32 so the existing timer handler works, but the
        // LAPIC timer will supersede it once calibration is done. After the LAPIC
        // timer starts, we mask this entry.
        {
            let (gsi, active_low, level_triggered) = if let Some(ovr) = crate::acpi::irq_override(0)
            {
                let (al, lt) = decode_override_flags(ovr.flags);
                (ovr.global_system_interrupt, al, lt)
            } else {
                (gsi_base, false, false)
            };
            if let Some(pin) = gsi_to_pin(gsi, gsi_base, max_redir) {
                let low = redir_entry_low(32, active_low, level_triggered, false);
                ioapic_write_redir(pin, low, bsp_lapic_id);
                log::info!(
                    "[apic] I/O APIC: IRQ 0 → GSI {} (pin {}) → vector 32 (for PIT calibration)",
                    gsi,
                    pin,
                );
            } else {
                log::warn!(
                    "[apic] I/O APIC: IRQ 0 GSI {} not routable (base={}, max_pin={}); skipped",
                    gsi,
                    gsi_base,
                    max_redir
                );
            }
        }
    }
}

// ===========================================================================
// LAPIC timer calibration (via PIT channel 2)
// ===========================================================================

static IOAPIC_MAX_REDIR: Once<u32> = Once::new();
static LAPIC_TICKS_PER_MS: Once<u32> = Once::new();

/// TSC value captured at the moment the LAPIC timer calibration completed.
static BOOT_TSC: Once<u64> = Once::new();

/// Invariant TSC ticks per millisecond, calibrated against PIT channel 2.
static TSC_PER_MS: Once<u64> = Once::new();

/// Return the TSC value at kernel boot (end of LAPIC calibration).
#[inline]
pub fn boot_tsc() -> u64 {
    *BOOT_TSC.get().unwrap_or(&0)
}

/// Return invariant TSC ticks per millisecond.
#[inline]
pub fn tsc_per_ms() -> u64 {
    *TSC_PER_MS.get().unwrap_or(&0)
}

/// Return the BSP-calibrated LAPIC timer ticks per millisecond.
///
/// Used by APs to configure their LAPIC timers without re-calibrating.
pub fn lapic_ticks_per_ms() -> u32 {
    *LAPIC_TICKS_PER_MS
        .get()
        .expect("LAPIC timer not calibrated")
}

/// Calibrate the LAPIC timer by using PIT channel 2 as a ~10 ms reference.
///
/// Returns the number of LAPIC timer ticks per millisecond (with divide-by-16).
fn calibrate_lapic_timer() -> u32 {
    // PIT oscillator frequency: 1,193,182 Hz.
    // For ~10 ms: count = 1_193_182 / 100 = 11_932.
    const PIT_10MS_COUNT: u16 = 11_932;

    unsafe {
        let mut pit_cmd: Port<u8> = Port::new(0x43);
        let mut pit_ch2: Port<u8> = Port::new(0x42);
        let mut pit_gate: Port<u8> = Port::new(0x61);

        // Enable PIT channel 2 gate (bit 0), disable speaker (clear bit 1).
        // SAFETY: port 0x61 controls NMI / speaker / PIT gate — standard x86 I/O.
        let gate = pit_gate.read();
        pit_gate.write((gate & 0xFC) | 0x01);

        // Mode 0 (one-shot), lobyte/hibyte access, channel 2.
        // SAFETY: PIT command register — standard x86 I/O.
        pit_cmd.write(0xB0);

        // Load the 10 ms countdown value.
        pit_ch2.write((PIT_10MS_COUNT & 0xFF) as u8);
        pit_ch2.write((PIT_10MS_COUNT >> 8) as u8);

        // Set LAPIC timer: divide-by-16, maximum initial count.
        lapic_write(LAPIC_TIMER_DIVIDE_CONFIG, 0x03); // divide config = 16
        lapic_write(LAPIC_TIMER_INIT_COUNT, 0xFFFF_FFFF);

        // Re-trigger PIT channel 2 by toggling the gate.
        let gate = pit_gate.read();
        pit_gate.write(gate & 0xFE); // gate low — resets the counter

        // Snapshot TSC just before the countdown starts.
        let tsc_start = core::arch::x86_64::_rdtsc();

        pit_gate.write(gate | 0x01); // gate high — starts countdown

        // Spin until PIT channel 2 output goes high (bit 5 of port 0x61).
        while pit_gate.read() & 0x20 == 0 {
            core::hint::spin_loop();
        }

        // Snapshot TSC immediately after the 10ms window ends.
        let tsc_end = core::arch::x86_64::_rdtsc();

        // Read how many LAPIC timer ticks elapsed.
        let remaining = lapic_read(LAPIC_TIMER_CURRENT_COUNT);
        let elapsed = 0xFFFF_FFFFu32.wrapping_sub(remaining);

        // Stop the LAPIC timer.
        lapic_write(LAPIC_TIMER_INIT_COUNT, 0);

        // elapsed ticks in ~10 ms with divide-by-16 → ticks_per_ms = elapsed / 10.
        let ticks_per_ms = elapsed / 10;

        // TSC ticks in ~10 ms → tsc_per_ms = delta / 10.
        let tsc_delta = tsc_end.wrapping_sub(tsc_start);
        let tsc_per_ms_val = tsc_delta / 10;

        log::info!(
            "[apic] LAPIC timer calibration: {} ticks in ~10ms, {} ticks/ms (div16)",
            elapsed,
            ticks_per_ms
        );
        log::info!(
            "[apic] TSC calibration: {} ticks in ~10ms, {} ticks/ms",
            tsc_delta,
            tsc_per_ms_val
        );

        // Store TSC calibration results.  Record the TSC *after* the window so
        // that boot_tsc() represents a known-good reference point.
        TSC_PER_MS.call_once(|| tsc_per_ms_val);
        BOOT_TSC.call_once(|| tsc_end);

        ticks_per_ms
    }
}

/// Configure the LAPIC timer in periodic mode.
///
/// `period_ms` — timer period in milliseconds (e.g. 10 for 100 Hz).
/// Returns `true` if the timer was successfully started.
fn start_lapic_timer(period_ms: u32) -> bool {
    let tpm = *LAPIC_TICKS_PER_MS
        .get()
        .expect("LAPIC timer not calibrated");

    if tpm == 0 {
        log::error!("[apic] LAPIC timer calibration returned 0 ticks/ms; timer not started");
        return false;
    }

    // Use u64 to avoid overflow on fast systems.
    let init_count_64 = tpm as u64 * period_ms as u64;
    let init_count = if init_count_64 > u32::MAX as u64 {
        log::warn!(
            "[apic] LAPIC timer initial count {} exceeds u32::MAX; clamping",
            init_count_64
        );
        u32::MAX
    } else {
        init_count_64 as u32
    };

    unsafe {
        // LVT Timer: vector 32, periodic mode (bit 17 set).
        lapic_write(LAPIC_LVT_TIMER, 32 | (1 << 17));
        lapic_write(LAPIC_TIMER_DIVIDE_CONFIG, 0x03); // divide-by-16
        lapic_write(LAPIC_TIMER_INIT_COUNT, init_count);
    }
    log::info!(
        "[apic] LAPIC timer: periodic, {}ms, {} ticks/ms",
        period_ms,
        tpm
    );
    true
}

// ===========================================================================
// Legacy PIC disable
// ===========================================================================

/// Mask all lines on the legacy 8259 PIC, effectively disabling it.
///
/// The PIC must already have been initialized (by `init_pics()`) so that it
/// is remapped to vectors 0x20-0x2F and does not interfere with CPU exceptions.
pub fn disable_legacy_pic() {
    unsafe {
        // SAFETY: ports 0x21 and 0xA1 are the PIC data registers — standard x86 I/O.
        let mut pic1_data: Port<u8> = Port::new(0x21);
        let mut pic2_data: Port<u8> = Port::new(0xA1);
        pic1_data.write(0xFF);
        pic2_data.write(0xFF);
    }
    log::info!("[apic] legacy 8259 PIC disabled (all lines masked)");
}

// ===========================================================================
// Public helpers for routing additional IRQs (P16-T011)
// ===========================================================================

/// Route a PCI interrupt line through the I/O APIC to the given IDT vector.
///
/// PCI interrupt lines are GSIs in the I/O APIC. This function programs the
/// redirection entry for `irq_line` to deliver `vector` to the BSP.
///
/// Level-triggered, active-low is the standard for PCI interrupts.
pub fn route_pci_irq(irq_line: u8, vector: u8) {
    let max_redir = match IOAPIC_MAX_REDIR.get() {
        Some(&m) => m,
        None => {
            log::warn!("[apic] route_pci_irq: I/O APIC not initialized");
            return;
        }
    };

    let gsi_base = crate::acpi::ioapic_gsi_base();
    let gsi = gsi_base + irq_line as u32;

    if let Some(pin) = gsi_to_pin(gsi, gsi_base, max_redir) {
        let bsp_id = unsafe { lapic_read(LAPIC_ID) & 0xFF00_0000 };
        // PCI interrupts: level-triggered, active-low.
        let low = redir_entry_low(vector, true, true, false);
        unsafe {
            ioapic_write_redir(pin, low, bsp_id);
        }
        log::info!(
            "[apic] I/O APIC: PCI IRQ {} → GSI {} (pin {}) → vector {} (level, active-low)",
            irq_line,
            gsi,
            pin,
            vector
        );
    } else {
        log::warn!(
            "[apic] I/O APIC: PCI IRQ {} GSI {} not routable (base={}, max_pin={})",
            irq_line,
            gsi,
            gsi_base,
            max_redir
        );
    }
}

// ===========================================================================
// Orchestration
// ===========================================================================

/// Switch from PIC to APIC interrupt routing.
///
/// Must be called after `enable_interrupts()` (which initialises the PIC and
/// enables IRQs). After this function returns:
///
/// * The Local APIC is enabled (spurious vector 0xFF).
/// * The I/O APIC routes keyboard (IRQ 1 → vec 33) and COM1 (IRQ 4 → vec 36).
/// * The LAPIC timer fires vector 32 at ~100 Hz (10 ms period).
/// * The legacy 8259 PIC is fully masked.
pub fn init() {
    // Perform the entire APIC handoff with interrupts disabled to avoid
    // acknowledging IRQs on the wrong controller during the transition.
    x86_64::instructions::interrupts::without_interrupts(|| {
        // 1. Enable Local APIC.
        lapic_init();

        // 2. Program I/O APIC redirection entries.
        ioapic_init();

        // 3. Calibrate LAPIC timer against PIT channel 2.
        let tpm = calibrate_lapic_timer();
        LAPIC_TICKS_PER_MS.call_once(|| tpm);

        // 4. Start LAPIC timer (1 ms periodic → 1000 Hz).
        if !start_lapic_timer(1) {
            log::error!("[apic] LAPIC timer failed to start; staying on PIC");
            return;
        }

        // 5. Mask the PIT's I/O APIC entry now that the LAPIC timer is running.
        //    Preserve the override's polarity/trigger flags in case the entry
        //    is ever unmasked or inspected later.
        unsafe {
            let gsi_base = crate::acpi::ioapic_gsi_base();
            let max_redir = *IOAPIC_MAX_REDIR.get().unwrap();
            let (gsi, active_low, level_triggered) = if let Some(ovr) = crate::acpi::irq_override(0)
            {
                let (al, lt) = decode_override_flags(ovr.flags);
                (ovr.global_system_interrupt, al, lt)
            } else {
                (gsi_base, false, false)
            };
            if let Some(pin) = gsi_to_pin(gsi, gsi_base, max_redir) {
                let low = redir_entry_low(32, active_low, level_triggered, true); // masked
                let bsp_id = lapic_read(LAPIC_ID) & 0xFF00_0000;
                ioapic_write_redir(pin, low, bsp_id);
            }
        }

        // 6. Disable legacy PIC entirely before switching handlers to APIC EOIs.
        disable_legacy_pic();

        // 7. Switch interrupt handlers to APIC EOI path now that the PIC is off.
        // Relaxed is sufficient: the entire transition runs with interrupts
        // disabled, so no handler can observe the flag until after this
        // closure returns and interrupts are re-enabled.
        USING_APIC.store(true, core::sync::atomic::Ordering::Relaxed);

        log::info!("[apic] APIC interrupt routing active");
    });
}
