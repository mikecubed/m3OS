//! xHCI Slot / Endpoint / Input context layout math (xHCI 1.2b §6.2).
//!
//! The controller describes every USB device with a **Device Context**: an
//! array of 32 context entries — a Slot Context at index 0 (Device Context
//! Index 0) followed by up to 31 Endpoint Contexts at DCIs 1..=31. To *modify*
//! a device the driver builds an **Input Context**: an Input Control Context
//! (which Add/Drop flags select the entries being changed) followed by its own
//! copy of the 32-entry Device Context.
//!
//! Each context entry is either 32 or 64 bytes, selected by `HCCPARAMS1.CSZ`
//! (see [`crate::usb::xhci::regs::Hccparams1::csz_64`]). All the offset helpers
//! here take the entry size as a parameter so the same code serves both
//! layouts. The 64-byte layout simply doubles every stride; the field bit
//! positions within an entry are identical.
//!
//! No MMIO, no DMA: this module computes byte offsets and encodes a handful of
//! Slot Context fields.

// ---------------------------------------------------------------------------
// Context entry sizing
// ---------------------------------------------------------------------------

/// Entry size in bytes for a 32-byte (CSZ=0) context.
pub const CONTEXT_ENTRY_SIZE_32: usize = 32;
/// Entry size in bytes for a 64-byte (CSZ=1) context.
pub const CONTEXT_ENTRY_SIZE_64: usize = 64;

/// Number of context entries in a Device Context: the Slot Context plus 31
/// Endpoint Contexts (xHCI §6.2.1).
pub const DEVICE_CONTEXT_ENTRIES: usize = 32;

/// Size in bytes of a single context entry, selected by `CSZ`.
/// `csz_64 == true` → 64 bytes (the `HCCPARAMS1.CSZ=1` layout), else 32 bytes.
pub const fn context_entry_size(csz_64: bool) -> usize {
    if csz_64 {
        CONTEXT_ENTRY_SIZE_64
    } else {
        CONTEXT_ENTRY_SIZE_32
    }
}

// ---------------------------------------------------------------------------
// Device Context offsets (xHCI §6.2.1)
// ---------------------------------------------------------------------------

/// Byte offset of the context entry at Device Context Index `dci` within a
/// Device Context, given the entry size in bytes.
///
/// DCI 0 is the Slot Context; DCIs 1..=31 are Endpoint Contexts. The offset is
/// simply `dci * entry_size`.
pub const fn device_context_entry_offset(dci: u8, entry_size: usize) -> usize {
    (dci as usize) * entry_size
}

// ---------------------------------------------------------------------------
// Input Context offsets (xHCI §6.2.5)
// ---------------------------------------------------------------------------

/// Byte offset of the Input Control Context within an Input Context. It is
/// always first (xHCI §6.2.5.1).
pub const fn input_control_offset() -> usize {
    0
}

/// Byte offset of the Slot Context within an Input Context, given the entry
/// size. The Slot Context follows the (one-entry-sized) Input Control Context,
/// so its offset is `entry_size`.
pub const fn input_slot_offset(entry_size: usize) -> usize {
    entry_size
}

/// Byte offset of the Endpoint Context with Device Context Index `dci` within
/// an Input Context, given the entry size.
///
/// The Input Context is: Input Control Context (1 entry) + Device Context (32
/// entries, indexed by DCI). The Endpoint Context for `dci` therefore lives at
/// `(1 + dci) * entry_size` — the leading `1` accounts for the Input Control
/// Context. (For `dci == 1`, the EP0 context, this is `2 * entry_size`.)
pub const fn input_endpoint_offset(dci: u8, entry_size: usize) -> usize {
    (1 + dci as usize) * entry_size
}

// ---------------------------------------------------------------------------
// Input Control Context Add/Drop flags (xHCI §6.2.5.1)
// ---------------------------------------------------------------------------

/// Add-flag bit A0 — selects the Slot Context (DCI 0).
pub const ADD_FLAG_SLOT: u32 = 1 << 0;
/// Add-flag bit A1 — selects the EP0 (default control endpoint) Context, DCI 1.
pub const ADD_FLAG_EP0: u32 = 1 << 1;

/// Build the Add Flags dword (`A0`..`A31`) for an Input Control Context from a
/// list of Device Context Indices. Bit `n` (`An`) is set for each DCI `n`
/// present in `dcis`. To include the Slot Context, pass DCI 0.
///
/// DCIs greater than 31 are ignored (no such context entry exists).
pub fn add_flags(dcis: &[u8]) -> u32 {
    let mut flags = 0u32;
    for &dci in dcis {
        if (dci as usize) < DEVICE_CONTEXT_ENTRIES {
            flags |= 1 << dci;
        }
    }
    flags
}

// ---------------------------------------------------------------------------
// Slot Context field encoders (xHCI §6.2.2)
// ---------------------------------------------------------------------------

/// Mask of the Route String field within Slot Context dword 0 (bits 19:0).
pub const SLOT_ROUTE_STRING_MASK: u32 = 0x000F_FFFF;
/// Shift of the Speed field within Slot Context dword 0 (bits 23:20).
pub const SLOT_SPEED_SHIFT: u32 = 20;
/// Mask of the Speed field after shifting (4 bits).
pub const SLOT_SPEED_MASK: u32 = 0xF;
/// Shift of the Context Entries field within Slot Context dword 0 (bits 31:27).
pub const SLOT_CONTEXT_ENTRIES_SHIFT: u32 = 27;
/// Mask of the Context Entries field after shifting (5 bits).
pub const SLOT_CONTEXT_ENTRIES_MASK: u32 = 0x1F;
/// Shift of the Root Hub Port Number field within Slot Context dword 1 (bits
/// 23:16).
pub const SLOT_ROOT_HUB_PORT_SHIFT: u32 = 16;
/// Mask of the Root Hub Port Number field after shifting (8 bits).
pub const SLOT_ROOT_HUB_PORT_MASK: u32 = 0xFF;

/// Encode **Slot Context dword 0** from its three relevant bring-up fields
/// (xHCI §6.2.2.1):
///
/// * `route_string` (bits 19:0) — the USB3 route string to the device.
/// * `speed` (bits 23:20) — the Protocol Speed ID of the device.
/// * `context_entries` (bits 31:27) — index of the last valid endpoint context
///   (i.e. the highest DCI in use).
///
/// Reserved bits 26:24 are left zero.
pub const fn slot_context_dword0(route_string: u32, speed: u8, context_entries: u8) -> u32 {
    (route_string & SLOT_ROUTE_STRING_MASK)
        | (((speed as u32) & SLOT_SPEED_MASK) << SLOT_SPEED_SHIFT)
        | (((context_entries as u32) & SLOT_CONTEXT_ENTRIES_MASK) << SLOT_CONTEXT_ENTRIES_SHIFT)
}

/// Decode the Route String from Slot Context dword 0 (bits 19:0).
pub const fn slot_route_string(dword0: u32) -> u32 {
    dword0 & SLOT_ROUTE_STRING_MASK
}

/// Decode the Speed field from Slot Context dword 0 (bits 23:20).
pub const fn slot_speed(dword0: u32) -> u8 {
    ((dword0 >> SLOT_SPEED_SHIFT) & SLOT_SPEED_MASK) as u8
}

/// Decode the Context Entries field from Slot Context dword 0 (bits 31:27).
pub const fn slot_context_entries(dword0: u32) -> u8 {
    ((dword0 >> SLOT_CONTEXT_ENTRIES_SHIFT) & SLOT_CONTEXT_ENTRIES_MASK) as u8
}

/// Encode **Slot Context dword 1**'s Root Hub Port Number (bits 23:16,
/// xHCI §6.2.2.1) — the 1-based root-hub port the device is attached to.
/// Other dword-1 fields (Max Exit Latency, Number of Ports) are left zero.
pub const fn slot_context_dword1(root_hub_port_number: u8) -> u32 {
    ((root_hub_port_number as u32) & SLOT_ROOT_HUB_PORT_MASK) << SLOT_ROOT_HUB_PORT_SHIFT
}

/// Decode the Root Hub Port Number from Slot Context dword 1 (bits 23:16).
pub const fn slot_root_hub_port(dword1: u32) -> u8 {
    ((dword1 >> SLOT_ROOT_HUB_PORT_SHIFT) & SLOT_ROOT_HUB_PORT_MASK) as u8
}

// ---------------------------------------------------------------------------
// Endpoint Context field encoders (xHCI §6.2.3)
// ---------------------------------------------------------------------------

// --- Endpoint Type (EP Type field, bits 5:3 of dword 1) ---

/// EP Type value for a Bidirectional Control endpoint (xHCI §6.2.3 Table 6-9).
/// Used for EP0, the default control endpoint.
pub const EP_TYPE_CONTROL: u8 = 4;
/// EP Type value for a Bulk OUT endpoint.
pub const EP_TYPE_BULK_OUT: u8 = 2;
/// EP Type value for a Bulk IN endpoint.
pub const EP_TYPE_BULK_IN: u8 = 6;
/// EP Type value for an Interrupt OUT endpoint.
pub const EP_TYPE_INTERRUPT_OUT: u8 = 3;
/// EP Type value for an Interrupt IN endpoint.
pub const EP_TYPE_INTERRUPT_IN: u8 = 7;
/// EP Type value for an Isochronous OUT endpoint (xHCI §6.2.3 Table 6-9) — the
/// USB-audio (UAC) PCM-out direction.
pub const EP_TYPE_ISOCH_OUT: u8 = 1;
/// EP Type value for an Isochronous IN endpoint — the USB-video (UVC) frame-in
/// direction.
pub const EP_TYPE_ISOCH_IN: u8 = 5;

/// Shift of the Endpoint Type field in Endpoint Context dword 1 (bits 5:3).
pub const EP_TYPE_SHIFT: u32 = 3;
/// Mask of the Endpoint Type field after shifting (3 bits).
pub const EP_TYPE_MASK: u32 = 0x7;
/// Shift of the Error Count (CErr) field in Endpoint Context dword 1 (bits
/// 2:1). For non-isoch endpoints this is set to 3 (maximum retries).
pub const EP_CERR_SHIFT: u32 = 1;
/// Mask of the CErr field after shifting (2 bits).
pub const EP_CERR_MASK: u32 = 0x3;
/// Shift of the Max Burst Size field in Endpoint Context dword 1 (bits 15:8).
/// Zero for full-speed endpoints; for HS/SS isochronous endpoints it carries
/// `bMaxBurst` (the additional transactions per service interval).
pub const EP_MAX_BURST_SHIFT: u32 = 8;
/// Mask of the Max Burst Size field after shifting (8 bits).
pub const EP_MAX_BURST_MASK: u32 = 0xFF;
/// Shift of Max Packet Size in Endpoint Context dword 1 (bits 31:16).
pub const EP_MAX_PACKET_SIZE_SHIFT: u32 = 16;
/// Mask of Max Packet Size after shifting (16 bits).
pub const EP_MAX_PACKET_SIZE_MASK: u32 = 0xFFFF;

/// Shift of the Interval field in Endpoint Context dword 0 (bits 23:16).
/// For interrupt and isochronous endpoints: the service interval = 2^Interval
/// microframes (HS) or frames (FS/LS, after adjustment).
pub const EP_INTERVAL_SHIFT: u32 = 16;
/// Mask of the Interval field after shifting (8 bits).
pub const EP_INTERVAL_MASK: u32 = 0xFF;

/// DCS — Dequeue Cycle State: bit 0 of the TR Dequeue Pointer in Endpoint
/// Context dwords 2–3. Must be set to 1 on the initial context so the
/// controller starts polling the ring with the matching cycle bit.
pub const EP_DCS_BIT: u64 = 1;

/// Error Count (CErr = 3) for non-isochronous endpoints (xHCI §6.2.3).
///
/// Non-zero CErr causes the controller to retry failed transactions; 3 is the
/// maximum and the standard choice for control/bulk/interrupt endpoints.
pub const EP_CERR_3: u8 = 3;

/// Error Count (CErr = 0) for isochronous endpoints (xHCI §6.2.3). Isoch has no
/// retry — a failed/missed transaction's data is dropped, not resent — so the
/// CErr field **must** be 0 for an isochronous endpoint context.
pub const EP_CERR_0: u8 = 0;

/// Encode **Endpoint Context dword 0** for an interrupt endpoint (xHCI §6.2.3).
///
/// Sets the Interval field (bits 23:16); all other bits in dword 0 are left
/// zero (Max ESIT Payload and Max Burst Size are not required for FS/HS
/// interrupt endpoints in basic enumeration).
///
/// `interval` is the raw xHCI Interval value (not the bInterval from the
/// endpoint descriptor — the caller must convert: for HS interrupt, use
/// `bInterval - 1` clamped to 0..=15; for FS/LS, use log2(bInterval) where
/// bInterval is 1..=255 ms).
pub const fn ep_context_dword0_interval(interval: u8) -> u32 {
    ((interval as u32) & EP_INTERVAL_MASK) << EP_INTERVAL_SHIFT
}

/// Encode **Endpoint Context dword 1** from its three fields (xHCI §6.2.3):
///
/// * `ep_type` (bits 5:3) — the endpoint type (see `EP_TYPE_*` constants).
/// * `cerr`    (bits 2:1) — error count, use [`EP_CERR_3`] for non-isoch.
/// * `max_packet_size` (bits 31:16) — the endpoint's `wMaxPacketSize`.
///
/// Bits 15:8 (Max Burst Size) and bits 7:6 (reserved) are left zero. Equivalent
/// to [`ep_context_dword1_burst`] with `max_burst = 0` — the correct choice for
/// control/bulk/interrupt and full-speed isochronous endpoints.
pub const fn ep_context_dword1(ep_type: u8, cerr: u8, max_packet_size: u16) -> u32 {
    ep_context_dword1_burst(ep_type, cerr, 0, max_packet_size)
}

/// Encode **Endpoint Context dword 1** including the **Max Burst Size** field
/// (bits 15:8, xHCI §6.2.3). For a high-speed/SuperSpeed isochronous endpoint
/// `max_burst` is `bMaxBurst` (the additional opportunities per service
/// interval); full-speed endpoints pass 0.
pub const fn ep_context_dword1_burst(
    ep_type: u8,
    cerr: u8,
    max_burst: u8,
    max_packet_size: u16,
) -> u32 {
    (((ep_type as u32) & EP_TYPE_MASK) << EP_TYPE_SHIFT)
        | (((cerr as u32) & EP_CERR_MASK) << EP_CERR_SHIFT)
        | (((max_burst as u32) & EP_MAX_BURST_MASK) << EP_MAX_BURST_SHIFT)
        | (((max_packet_size as u32) & EP_MAX_PACKET_SIZE_MASK) << EP_MAX_PACKET_SIZE_SHIFT)
}

/// Decode the Max Burst Size from Endpoint Context dword 1 (bits 15:8).
pub const fn ep_max_burst(dword1: u32) -> u8 {
    ((dword1 >> EP_MAX_BURST_SHIFT) & EP_MAX_BURST_MASK) as u8
}

/// Encode the **TR Dequeue Pointer** value stored in Endpoint Context dwords
/// 2–3 (xHCI §6.2.3.2).
///
/// `ring_iova` is the device-visible address of the first TRB on the
/// endpoint's transfer ring, which **must** be 16-byte aligned (low 4 bits
/// zero). This function ORs in the Dequeue Cycle State bit (`DCS = 1`), which
/// tells the controller the initial producer cycle state so it starts polling
/// the ring correctly.
pub const fn ep_tr_dequeue_ptr(ring_iova: u64) -> u64 {
    ring_iova | EP_DCS_BIT
}

/// Decode the Endpoint Type from Endpoint Context dword 1 (bits 5:3).
pub const fn ep_type(dword1: u32) -> u8 {
    ((dword1 >> EP_TYPE_SHIFT) & EP_TYPE_MASK) as u8
}

/// Decode the Error Count (CErr) from Endpoint Context dword 1 (bits 2:1).
pub const fn ep_cerr(dword1: u32) -> u8 {
    ((dword1 >> EP_CERR_SHIFT) & EP_CERR_MASK) as u8
}

/// Decode Max Packet Size from Endpoint Context dword 1 (bits 31:16).
pub const fn ep_max_packet_size(dword1: u32) -> u16 {
    ((dword1 >> EP_MAX_PACKET_SIZE_SHIFT) & EP_MAX_PACKET_SIZE_MASK) as u16
}

/// Decode the Interval from Endpoint Context dword 0 (bits 23:16).
pub const fn ep_interval(dword0: u32) -> u8 {
    ((dword0 >> EP_INTERVAL_SHIFT) & EP_INTERVAL_MASK) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_size_by_csz() {
        assert_eq!(context_entry_size(false), 32);
        assert_eq!(context_entry_size(true), 64);
    }

    #[test]
    fn device_context_offsets() {
        // 32-byte layout.
        assert_eq!(device_context_entry_offset(0, 32), 0); // Slot Context
        assert_eq!(device_context_entry_offset(1, 32), 32); // EP0
        assert_eq!(device_context_entry_offset(3, 32), 96);
        assert_eq!(device_context_entry_offset(31, 32), 992);
        // 64-byte layout.
        assert_eq!(device_context_entry_offset(0, 64), 0);
        assert_eq!(device_context_entry_offset(1, 64), 64);
        assert_eq!(device_context_entry_offset(3, 64), 192);
    }

    #[test]
    fn input_context_offsets_32() {
        assert_eq!(input_control_offset(), 0);
        assert_eq!(input_slot_offset(32), 32);
        // EP0 == DCI 1.
        assert_eq!(input_endpoint_offset(1, 32), 64);
        // DCI 2.
        assert_eq!(input_endpoint_offset(2, 32), 96);
    }

    #[test]
    fn input_context_offsets_64() {
        assert_eq!(input_control_offset(), 0);
        assert_eq!(input_slot_offset(64), 64);
        // EP0 == DCI 1 -> (1 + 1) * 64 = 128.
        assert_eq!(input_endpoint_offset(1, 64), 128);
        assert_eq!(input_endpoint_offset(2, 64), 192);
    }

    #[test]
    fn add_flags_builder() {
        // Slot (A0) + EP0 (A1).
        assert_eq!(add_flags(&[0, 1]), 0b11);
        assert_eq!(ADD_FLAG_SLOT | ADD_FLAG_EP0, 0b11);
        // Slot + DCI 3 -> bit 0 and bit 3.
        assert_eq!(add_flags(&[0, 3]), 0b1001);
        // Out-of-range DCI is ignored.
        assert_eq!(add_flags(&[0, 32, 100]), ADD_FLAG_SLOT);
        // DCI 31 sets the top bit.
        assert_eq!(add_flags(&[31]), 1 << 31);
    }

    #[test]
    fn slot_context_dword0_roundtrip() {
        // route_string=0xABCDE, speed=4 (SuperSpeed PSI), context_entries=1.
        let d0 = slot_context_dword0(0xABCDE, 4, 1);
        assert_eq!(slot_route_string(d0), 0xABCDE);
        assert_eq!(slot_speed(d0), 4);
        assert_eq!(slot_context_entries(d0), 1);

        // Field clamping: oversized inputs do not bleed into neighbours.
        let d0b = slot_context_dword0(0xFFF_FFFF, 0xF, 0x1F);
        assert_eq!(slot_route_string(d0b), 0x000F_FFFF);
        assert_eq!(slot_speed(d0b), 0xF);
        assert_eq!(slot_context_entries(d0b), 0x1F);
    }

    #[test]
    fn slot_context_dword1_roundtrip() {
        let d1 = slot_context_dword1(7);
        assert_eq!(slot_root_hub_port(d1), 7);
        // Field is at bits 23:16.
        assert_eq!(d1, 7 << 16);
    }

    // -----------------------------------------------------------------------
    // Endpoint Context encoder tests (xHCI §6.2.3)
    // -----------------------------------------------------------------------

    #[test]
    fn ep_context_dword1_control_ep0_full_speed() {
        // EP0 full/low speed: EP Type = Control (4), CErr = 3, MPS = 8.
        let d1 = ep_context_dword1(EP_TYPE_CONTROL, EP_CERR_3, 8);
        assert_eq!(ep_type(d1), EP_TYPE_CONTROL);
        assert_eq!(ep_cerr(d1), 3);
        assert_eq!(ep_max_packet_size(d1), 8);
        // Type at bits 5:3 → Control = 4 → 4 << 3 = 0x20.
        // CErr at bits 2:1 → 3 → 3 << 1 = 0x06.
        // MPS at bits 31:16 → 8 → 8 << 16.
        assert_eq!(d1 & 0b11_1110, 0x20 | 0x06);
        assert_eq!(d1 >> 16, 8);
    }

    #[test]
    fn ep_context_dword1_control_ep0_high_speed() {
        // EP0 high speed: MPS = 64.
        let d1 = ep_context_dword1(EP_TYPE_CONTROL, EP_CERR_3, 64);
        assert_eq!(ep_type(d1), EP_TYPE_CONTROL);
        assert_eq!(ep_cerr(d1), 3);
        assert_eq!(ep_max_packet_size(d1), 64);
    }

    #[test]
    fn ep_context_dword1_control_ep0_super_speed() {
        // EP0 superspeed: MPS = 512.
        let d1 = ep_context_dword1(EP_TYPE_CONTROL, EP_CERR_3, 512);
        assert_eq!(ep_type(d1), EP_TYPE_CONTROL);
        assert_eq!(ep_max_packet_size(d1), 512);
    }

    #[test]
    fn ep_context_dword1_interrupt_in() {
        // Interrupt IN endpoint (e.g. HID keyboard), MPS = 8, CErr = 3.
        let d1 = ep_context_dword1(EP_TYPE_INTERRUPT_IN, EP_CERR_3, 8);
        assert_eq!(ep_type(d1), EP_TYPE_INTERRUPT_IN);
        assert_eq!(ep_cerr(d1), 3);
        assert_eq!(ep_max_packet_size(d1), 8);
    }

    #[test]
    fn ep_context_dword1_isoch_out_zero_cerr() {
        // Isochronous OUT endpoint (UAC PCM-out), MPS = 192, CErr MUST be 0.
        let d1 = ep_context_dword1(EP_TYPE_ISOCH_OUT, EP_CERR_0, 192);
        assert_eq!(ep_type(d1), EP_TYPE_ISOCH_OUT);
        assert_eq!(ep_cerr(d1), 0, "isochronous endpoints must have CErr = 0");
        assert_eq!(ep_max_packet_size(d1), 192);
        // Full-speed isoch carries no burst.
        assert_eq!(ep_max_burst(d1), 0);
    }

    #[test]
    fn ep_context_dword1_burst_roundtrip() {
        // High-speed isoch IN (UVC) with bMaxBurst = 2 and MPS = 1024.
        let d1 = ep_context_dword1_burst(EP_TYPE_ISOCH_IN, EP_CERR_0, 2, 1024);
        assert_eq!(ep_type(d1), EP_TYPE_ISOCH_IN);
        assert_eq!(ep_cerr(d1), 0);
        assert_eq!(ep_max_burst(d1), 2, "Max Burst Size occupies bits 15:8");
        assert_eq!(ep_max_packet_size(d1), 1024);
        // The burst field must not bleed into MPS or EP-type neighbours.
        assert_eq!((d1 >> EP_MAX_BURST_SHIFT) & EP_MAX_BURST_MASK, 2);
    }

    #[test]
    fn ep_context_dword0_interval_roundtrip() {
        // Interval = 10 (for a HS interrupt endpoint with bInterval = 11).
        let d0 = ep_context_dword0_interval(10);
        assert_eq!(ep_interval(d0), 10);
        // Field sits at bits 23:16.
        assert_eq!(d0, 10u32 << 16);

        // Interval = 0 yields zero dword.
        assert_eq!(ep_context_dword0_interval(0), 0);
        // Interval = 255 maximum (8-bit field).
        let d0b = ep_context_dword0_interval(255);
        assert_eq!(ep_interval(d0b), 255);
    }

    #[test]
    fn ep_tr_dequeue_ptr_sets_dcs() {
        // A 16-byte-aligned ring address: low 4 bits are zero, DCS is ORed in.
        let ring = 0x0010_0000u64;
        let ptr = ep_tr_dequeue_ptr(ring);
        assert_eq!(ptr & !1u64, ring); // address bits preserved
        assert_eq!(ptr & 1, 1); // DCS = 1
    }

    #[test]
    fn ep_type_constants_match_spec() {
        // xHCI §6.2.3 Table 6-9 values.
        assert_eq!(EP_TYPE_CONTROL, 4);
        assert_eq!(EP_TYPE_BULK_OUT, 2);
        assert_eq!(EP_TYPE_BULK_IN, 6);
        assert_eq!(EP_TYPE_INTERRUPT_OUT, 3);
        assert_eq!(EP_TYPE_INTERRUPT_IN, 7);
        assert_eq!(EP_TYPE_ISOCH_OUT, 1);
        assert_eq!(EP_TYPE_ISOCH_IN, 5);
        // EP0 control endpoint type encodes as 4.
        let d1 = ep_context_dword1(EP_TYPE_CONTROL, 0, 0);
        assert_eq!((d1 >> EP_TYPE_SHIFT) & EP_TYPE_MASK, 4);
    }
}
