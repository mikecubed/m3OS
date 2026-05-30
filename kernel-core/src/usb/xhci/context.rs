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
}
