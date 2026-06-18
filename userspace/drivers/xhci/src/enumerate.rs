//! Phase 78b Track A-glue — real `UsbHostOps` impl over the Controller's
//! DMA rings.
//!
//! [`XhciHostOps`] wraps a `&mut Controller` and an `&IrqNotification` and
//! implements every method of [`kernel_core::usb::enumerate::UsbHostOps`]
//! using the synchronous `issue_command_and_wait` + `control_transfer`
//! helpers. The input-context DMA layout is copied from
//! `InputContextSnapshot` into the controller's pre-allocated Input Context
//! DMA buffer by `write_input_context`, using the offset helpers from
//! `kernel_core::usb::xhci::context`.
//!
//! # EP0 ring IOVA fix-up
//!
//! The enumeration state machine constructs `InputContextSnapshot` fields
//! (`ep0_dequeue_ptr`) from `ctx.ep0_ring_iova`. Because `ep0_ring_iova` is
//! set to 0 in the initial `EnumContext` (the EP0 ring is not allocated until
//! `enable_slot` returns the slot ID and calls `alloc_slot_context`), the
//! snapshot's `ep0_dequeue_ptr` will be 1 (just DCS bit). We fix this up in
//! `address_device` and `evaluate_context` by patching the snapshot with the
//! real IOVA from the now-allocated `slot_ctx.ep0_ring_iova` before writing
//! the input context into DMA.

use alloc::vec::Vec;

use driver_runtime::IrqNotification;
use kernel_core::usb::enumerate::{InputContextSnapshot, UsbHostOps};
use kernel_core::usb::xhci::context::ep_tr_dequeue_ptr;
use kernel_core::usb::xhci::trb::{self, COMPLETION_SUCCESS};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::write_str;

use crate::controller::Controller;

/// Production `UsbHostOps` backed by real DMA rings on `controller`.
pub struct XhciHostOps<'c> {
    pub controller: &'c mut Controller,
    pub irq: &'c IrqNotification,
}

impl<'c> XhciHostOps<'c> {
    pub fn new(controller: &'c mut Controller, irq: &'c IrqNotification) -> Self {
        Self { controller, irq }
    }

    /// Fix up an `InputContextSnapshot` so that the EP0 dequeue pointer
    /// reflects the real allocated ring IOVA rather than the placeholder (0).
    /// The state machine builds `ep0_dequeue_ptr = ep_tr_dequeue_ptr(ctx.ep0_ring_iova)`;
    /// when `ctx.ep0_ring_iova` is 0 (seeded before alloc_slot_context), the
    /// snapshot has `ep0_dequeue_ptr = 1`. We patch it here.
    fn patched_snapshot(&self, slot_id: u8, snap: &InputContextSnapshot) -> InputContextSnapshot {
        let real_iova = self.controller.ep0_ring_iova(slot_id);
        let mut patched = snap.clone();
        patched.ep0_dequeue_ptr = ep_tr_dequeue_ptr(real_iova);
        patched
    }
}

impl UsbHostOps for XhciHostOps<'_> {
    fn enable_slot(&mut self) -> Result<u8, u8> {
        let cycle = self.controller.producer_cycle();
        let cmd = trb::Trb::enable_slot(0, cycle);
        let ev = self.controller.issue_command_and_wait(self.irq, cmd);
        if ev.completion_code == COMPLETION_SUCCESS && ev.slot_id != 0 {
            // Allocate per-slot context (Output Device Context + EP0 ring +
            // Input Context) now that we have the assigned Slot ID.
            self.controller
                .alloc_slot_context(ev.slot_id)
                .map_err(|_| 0xFDu8)?;
            Ok(ev.slot_id)
        } else {
            Err(ev.completion_code)
        }
    }

    fn address_device(&mut self, slot_id: u8, ctx: &InputContextSnapshot, bsr: bool) -> u8 {
        // Patch the EP0 dequeue pointer to reflect the real allocated ring.
        let patched = self.patched_snapshot(slot_id, ctx);
        let input_ctx_iova = self.controller.write_input_context(slot_id, &patched);
        let cycle = self.controller.producer_cycle();
        let cmd = trb::Trb::address_device(input_ctx_iova, slot_id, bsr, cycle);
        let ev = self.controller.issue_command_and_wait(self.irq, cmd);
        ev.completion_code
    }

    fn evaluate_context(&mut self, slot_id: u8, ctx: &InputContextSnapshot) -> u8 {
        // Patch the EP0 dequeue pointer to reflect the real allocated ring.
        let patched = self.patched_snapshot(slot_id, ctx);
        let input_ctx_iova = self.controller.write_input_context(slot_id, &patched);
        let cycle = self.controller.producer_cycle();
        let cmd = trb::Trb::evaluate_context(input_ctx_iova, slot_id, cycle);
        let ev = self.controller.issue_command_and_wait(self.irq, cmd);
        ev.completion_code
    }

    fn get_device_descriptor(&mut self, slot_id: u8, len: u16) -> Option<Vec<u8>> {
        let setup = trb::SetupPacket::get_device_descriptor(len);
        let bytes = self
            .controller
            .control_transfer(self.irq, slot_id, setup, len, true, None)?;
        // Cache the full 18-byte device descriptor for a later GetDescriptors
        // IPC (Phase 92 H.1). The 8-byte MaxPacketSize-probe read is skipped.
        if len >= 18 {
            self.controller.cache_device_descriptor(slot_id, &bytes);
        }
        Some(bytes)
    }

    fn get_config_short(&mut self, slot_id: u8, len: u16) -> Option<Vec<u8>> {
        let setup = trb::SetupPacket::get_config_descriptor(0, len);
        self.controller
            .control_transfer(self.irq, slot_id, setup, len, true, None)
    }

    fn get_config_full(&mut self, slot_id: u8, len: u16) -> Option<Vec<u8>> {
        let setup = trb::SetupPacket::get_config_descriptor(0, len);
        let bytes = self
            .controller
            .control_transfer(self.irq, slot_id, setup, len, true, None)?;
        // Cache the full configuration blob (wTotalLength bytes) for a later
        // GetDescriptors IPC (Phase 92 H.1) — class drivers read the interface /
        // endpoint / functional descriptors a full config carries.
        self.controller.cache_config_descriptor(slot_id, &bytes);
        Some(bytes)
    }

    fn set_configuration(&mut self, slot_id: u8, value: u8) -> u8 {
        let setup = trb::SetupPacket::set_configuration(value);
        match self
            .controller
            .control_transfer(self.irq, slot_id, setup, 0, false, None)
        {
            Some(_) => {
                write_str(STDOUT_FILENO, "[xhci] SET_CONFIGURATION OK\n");
                COMPLETION_SUCCESS
            }
            None => {
                write_str(STDOUT_FILENO, "[xhci] SET_CONFIGURATION failed\n");
                0xFF
            }
        }
    }

    fn configure_endpoint(&mut self, slot_id: u8, ctx: &InputContextSnapshot) -> u8 {
        // Patch EP0 dequeue pointer and allocate real transfer rings for each
        // interface endpoint in the snapshot. The state machine provides
        // placeholder IOVAs of 0; we replace them with real DMA ring addresses.
        // A1 (EP0) is no longer included in add_flags from kernel-core
        // (build_configure_endpoint_ctx fix), so no workaround needed here.
        let mut patched = self.patched_snapshot(slot_id, ctx);
        for ep_snap in &mut patched.endpoint_contexts {
            // MPS lives at bits 31:16 of ep_dword1 (same layout the controller
            // writes into the endpoint context).
            let mps = (ep_snap.ep_dword1 >> 16) as u16;
            match self
                .controller
                .alloc_interrupt_ep_ring(slot_id, ep_snap.dci, mps)
            {
                Ok(iova_dcs) => {
                    ep_snap.ep_dequeue_ptr = iova_dcs;
                }
                Err(_) => {
                    write_str(
                        STDOUT_FILENO,
                        "[xhci] configure_endpoint: ep ring alloc failed\n",
                    );
                    return 0xFF;
                }
            }
        }
        let input_ctx_iova = self.controller.write_input_context(slot_id, &patched);
        let cycle = self.controller.producer_cycle();
        let cmd = trb::Trb::configure_endpoint(input_ctx_iova, slot_id, cycle);
        let ev = self.controller.issue_command_and_wait(self.irq, cmd);
        ev.completion_code
    }
}
