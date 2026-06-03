//! Interrupt arm + handle path — Phase 82 Track C.5 (polling-primary).
//!
//! AHCI completion is reliably observable by polling `PxCI` (QEMU auto-clears
//! the slot bit on non-NCQ completion), so the data path in [`crate::cmd`] does
//! not depend on IRQ delivery — the IRQ is only a wakeup. This module holds the
//! arm/clear plumbing for the bare-metal/VFIO IRQ path, written to the AHCI
//! 1.3.1 interrupt-clear order: **clear `PxIS` first, then W1C the matching
//! port bit in the HBA-global `IS`** (reversing it latches the global pending
//! bit and a level-triggered/INTx line never deasserts), with `GHC.IE` armed
//! **last** (after every `PxIE` mask is set and all stale W1C status cleared).

use driver_runtime::Mmio;
use kernel_core::storage::ahci::{
    GHC_IE, HBA_GHC, HBA_IS, PORT_INT_MASK, PX_IE, PX_IS, host_is_clear, host_is_port_fired,
    is_decode, port_base, pxis_clear,
};

use crate::init::AhciAbar;

/// Arm completion + error interrupts: set each driven port's `PxIE` mask, clear
/// any stale `PxIS`, then enable `GHC.IE` **last**.
pub fn arm_interrupts(mmio: &Mmio<AhciAbar>, ports: &[u8]) {
    for &p in ports {
        let base = port_base(p as usize);
        // Clear any stale W1C status before arming.
        let is = mmio.read_reg::<u32>(base + PX_IS);
        mmio.write_reg::<u32>(base + PX_IS, pxis_clear(is));
        mmio.write_reg::<u32>(base + PX_IE, PORT_INT_MASK);
    }
    // Clear stale global IS bits for the driven ports, then enable GHC.IE last.
    let global_is = mmio.read_reg::<u32>(HBA_IS);
    mmio.write_reg::<u32>(HBA_IS, global_is);
    let ghc = mmio.read_reg::<u32>(HBA_GHC);
    mmio.write_reg::<u32>(HBA_GHC, ghc | GHC_IE);
}

/// Handle a completion interrupt: for each fired port, clear `PxIS` (W1C)
/// **then** W1C the port's bit in the HBA-global `IS`. Returns the bitmap of
/// ports that fired (so the caller can re-poll their `PxCI`).
pub fn handle_irq(mmio: &Mmio<AhciAbar>, ports: &[u8]) -> u32 {
    let global_is = is_decode(mmio.read_reg::<u32>(HBA_IS));
    let mut fired = 0u32;
    for &p in ports {
        if host_is_port_fired(global_is, p) {
            let base = port_base(p as usize);
            // 1. Clear the port's interrupt status (W1C).
            let pxis = mmio.read_reg::<u32>(base + PX_IS);
            mmio.write_reg::<u32>(base + PX_IS, pxis_clear(pxis));
            // 2. THEN W1C the matching bit in the global IS so the line
            //    deasserts (reversing this order wedges a level-triggered line).
            mmio.write_reg::<u32>(HBA_IS, host_is_clear(p));
            fired |= 1u32 << p;
        }
    }
    fired
}
