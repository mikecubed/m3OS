//! USB Hub class support — descriptor parsing, port topology tree, and
//! hub-class control-transfer helpers (USB 2.0 §11, USB 3.2 §10).
//!
//! # Hub Descriptor (USB 2.0 §11.23.2.1, `bDescriptorType` = 0x29)
//!
//! A hub exposes its capabilities through the Hub Descriptor, which the host
//! retrieves with `GET_DESCRIPTOR(Hub)` (class-specific, not standard). The
//! descriptor is at least 9 bytes and carries:
//!
//! * `bNbrPorts` — number of downstream ports.
//! * `wHubCharacteristics` — logical/individual power switching, compound
//!   device flag, over-current mode, TT think time (for HS hubs), and port
//!   indicators support.
//! * `bPwrOn2PwrGood` — time in 2 ms units from power-on until power is
//!   stable. The host must wait this long before accessing a port.
//! * `bHubContrCurrent` — maximum current in mA drawn by the hub controller.
//! * `DeviceRemovable` bitmap — one bit per port (bit 0 unused, bit N → port
//!   N), indicating whether the device attached to that port is permanently
//!   attached to the hub.
//!
//! # PortId — USB topology tree
//!
//! USB devices sit in a tree rooted at the host-controller root hub. Each
//! node in the tree is identified by the chain of port numbers from the root
//! hub port down to the device. xHCI §8.9 and USB 3.2 §8.9 define the
//! **route string** used to address a device through intermediate hubs: five
//! 4-bit tier fields packed into the low 20 bits of a u32. **Tier 1 is the
//! downstream port of the first hub attached to the root port** (not the root
//! port itself). The root-hub port number is carried separately in the Slot
//! Context Root Hub Port Number field and is NOT encoded in the route string.
//! A device directly attached to a root port has route string **0**. Values
//! 0x1–0xF are valid for each tier (a tier value of 0 terminates the route
//! string). Use [`PortTopology::root_hub_port`] to retrieve the root-hub port
//! for the Slot Context.
//!
//! This module stores the topology as a flat arena (a `Vec` of `PortNode`
//! entries with integer parent indices) — cleaner than recursive `Box<…>`
//! in `no_std` and cheaper to walk for route-string computation.
//!
//! # Hub-enumeration helpers
//!
//! Pure-logic helpers that build the USB control-transfer [`SetupPacket`]
//! for hub-class requests:
//!
//! * [`set_port_feature`] — encodes a `SET_FEATURE(feature, port)` request
//!   targeting the hub class, "other" recipient (the port). Used to assert
//!   `PORT_POWER` (bring a port out of power-off) and `PORT_RESET` (reset a
//!   device on a port so it reaches the Enabled state).

extern crate alloc;

use alloc::vec::Vec;

use crate::usb::xhci::trb::SetupPacket;

// ---------------------------------------------------------------------------
// Hub descriptor type
// ---------------------------------------------------------------------------

/// USB Hub Class Descriptor type code (USB 2.0 §11.23.2.1).
pub const DESC_TYPE_HUB: u8 = 0x29;

/// Minimum wire size of a Hub Descriptor in bytes.
///
/// The descriptor is variable-length (the `DeviceRemovable` and
/// `PortPwrCtrlMask` bitmaps at the end depend on `bNbrPorts`), but the
/// first 9 bytes are always present.
pub const HUB_DESCRIPTOR_MIN_LEN: usize = 9;

/// Maximum number of downstream ports a USB 2.0 hub may have (USB 2.0 §11.14).
pub const HUB_MAX_PORTS: u8 = 127;

/// Parsed USB Hub Class Descriptor (USB 2.0 §11.23.2.1 Table 11-13).
///
/// Fields are stored as host integers; the wire format is little-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubDescriptor {
    /// `bLength` — descriptor length in bytes (minimum 9).
    pub b_length: u8,
    /// `bDescriptorType` — must be [`DESC_TYPE_HUB`] (0x29).
    pub b_descriptor_type: u8,
    /// `bNbrPorts` — number of downstream-facing ports.
    pub b_nbr_ports: u8,
    /// `wHubCharacteristics` — power-mode, compound-device, over-current,
    /// TT think-time, and port-indicators flags (USB 2.0 §11.23.2.1
    /// Table 11-13 bits).
    pub w_hub_characteristics: u16,
    /// `bPwrOn2PwrGood` — port power-on to power-good delay in 2 ms units.
    pub b_pwr_on2_pwr_good: u8,
    /// `bHubContrCurrent` — maximum current (mA) drawn by the hub controller
    /// electronics (excluding ports).
    pub b_hub_contr_current: u8,
    /// `DeviceRemovable` bitmap — bit N (1-based) = 1 means the device on
    /// port N is permanently attached (not removable by the user). Stored as
    /// up to two bytes covering ports 1–15 (USB 2.0) or as a longer field
    /// for SuperSpeed hubs. We capture the raw bytes verbatim.
    pub device_removable: [u8; 2],
}

impl HubDescriptor {
    /// Parse a Hub Descriptor from a byte slice.
    ///
    /// Returns `None` if `bytes` is shorter than [`HUB_DESCRIPTOR_MIN_LEN`]
    /// bytes or if `bDescriptorType` ≠ [`DESC_TYPE_HUB`].
    ///
    /// # Wire layout (USB 2.0 §11.23.2.1 Table 11-13)
    ///
    /// | Offset | Field |
    /// |--------|-------|
    /// | 0 | `bLength` |
    /// | 1 | `bDescriptorType` (0x29) |
    /// | 2 | `bNbrPorts` |
    /// | 3–4 | `wHubCharacteristics` (little-endian) |
    /// | 5 | `bPwrOn2PwrGood` |
    /// | 6 | `bHubContrCurrent` |
    /// | 7 | `DeviceRemovable[0]` (ports 1–7, bit 0 unused) |
    /// | 8 | `DeviceRemovable[1]` (ports 8–15 / PortPwrCtrlMask byte) |
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HUB_DESCRIPTOR_MIN_LEN {
            return None;
        }
        if bytes[1] != DESC_TYPE_HUB {
            return None;
        }
        Some(HubDescriptor {
            b_length: bytes[0],
            b_descriptor_type: bytes[1],
            b_nbr_ports: bytes[2],
            w_hub_characteristics: u16::from_le_bytes([bytes[3], bytes[4]]),
            b_pwr_on2_pwr_good: bytes[5],
            b_hub_contr_current: bytes[6],
            device_removable: [bytes[7], bytes[8]],
        })
    }

    /// Returns `true` if port `port` (1-based) is marked as
    /// non-removable in the `DeviceRemovable` bitmap.
    ///
    /// Port numbers outside the range 1–15 always return `false` (the
    /// two-byte capture only covers those ports).
    pub fn is_non_removable(&self, port: u8) -> bool {
        if port == 0 || port > 15 {
            return false;
        }
        // USB 2.0 §11.23.2.1 Table 11-13: DeviceRemovable is indexed by the
        // port number directly — bit N corresponds to port N, and bit 0 of
        // byte 0 is RESERVED (there is no port 0). So port 1 → byte 0 bit 1,
        // port 7 → byte 0 bit 7, port 8 → byte 1 bit 0, port 15 → byte 1 bit 7.
        let byte_idx = (port / 8) as usize;
        let bit = port % 8;
        if byte_idx < self.device_removable.len() {
            (self.device_removable[byte_idx] >> bit) & 1 != 0
        } else {
            false
        }
    }

    /// Power-switching mode (bits 1:0 of `wHubCharacteristics`).
    ///
    /// * 0b00 = ganged (all ports share a single power switch)
    /// * 0b01 = individual (each port has its own power switch)
    /// * 0b1x = no power switching (power always on)
    pub fn power_switching_mode(&self) -> u8 {
        (self.w_hub_characteristics & 0x03) as u8
    }

    /// Returns `true` if the hub is a compound device (bit 2 of
    /// `wHubCharacteristics`).
    pub fn is_compound_device(&self) -> bool {
        self.w_hub_characteristics & (1 << 2) != 0
    }

    /// Over-current protection mode (bits 4:3 of `wHubCharacteristics`).
    ///
    /// * 0b00 = global
    /// * 0b01 = individual
    /// * 0b1x = no over-current protection
    pub fn over_current_mode(&self) -> u8 {
        ((self.w_hub_characteristics >> 3) & 0x03) as u8
    }
}

// ---------------------------------------------------------------------------
// Hub-class feature selectors (USB 2.0 §11.24.2.7 Table 11-17)
// ---------------------------------------------------------------------------

/// `PORT_CONNECTION` feature selector (hub class, USB 2.0 §11.24.2.7).
pub const PORT_CONNECTION: u16 = 0;
/// `PORT_ENABLE` feature selector.
pub const PORT_ENABLE: u16 = 1;
/// `PORT_SUSPEND` feature selector.
pub const PORT_SUSPEND: u16 = 2;
/// `PORT_OVER_CURRENT` feature selector.
pub const PORT_OVER_CURRENT: u16 = 3;
/// `PORT_RESET` feature selector — triggers a USB reset on the port.
pub const PORT_RESET: u16 = 4;
/// `PORT_POWER` feature selector — enables power to the port.
pub const PORT_POWER: u16 = 8;

// ---------------------------------------------------------------------------
// bmRequestType for hub-class port requests (USB 2.0 §11.24.2)
// ---------------------------------------------------------------------------

/// `bmRequestType` for a hub-class SET_FEATURE / CLEAR_FEATURE targeting a
/// **port** (recipient = Other): Host-to-Device (bit 7 = 0), Class type
/// (bits 6:5 = 01), Other recipient (bits 4:0 = 00011) → 0x23.
pub const BM_REQUEST_TYPE_CLASS_OTHER_H2D: u8 = 0x23;

/// `bRequest` for SET_FEATURE (USB 2.0 §9.4 Table 9-4).
pub const B_REQUEST_SET_FEATURE: u8 = 0x03;

/// `bRequest` for CLEAR_FEATURE.
pub const B_REQUEST_CLEAR_FEATURE: u8 = 0x01;

// ---------------------------------------------------------------------------
// Hub-class control-transfer encoders
// ---------------------------------------------------------------------------

/// Encode a hub-class `SET_FEATURE(feature, port)` [`SetupPacket`].
///
/// This is the request the host sends to bring a port out of power-off
/// (`PORT_POWER`) or to reset a device (`PORT_RESET`).
///
/// Per USB 2.0 §11.24.2.7:
/// * `bmRequestType` = 0x23 (class, other recipient, host-to-device)
/// * `bRequest` = 3 (SET_FEATURE)
/// * `wValue` = `feature` selector
/// * `wIndex` = `port` (1-based port number)
/// * `wLength` = 0 (no data stage)
pub const fn set_port_feature(feature: u16, port: u8) -> SetupPacket {
    SetupPacket {
        bm_request_type: BM_REQUEST_TYPE_CLASS_OTHER_H2D,
        b_request: B_REQUEST_SET_FEATURE,
        w_value: feature,
        w_index: port as u16,
        w_length: 0,
    }
}

/// Encode a hub-class `CLEAR_FEATURE(feature, port)` [`SetupPacket`].
///
/// Used to clear change bits (e.g. `C_PORT_CONNECTION`, `C_PORT_RESET`) after
/// handling a port-status-change event.
pub const fn clear_port_feature(feature: u16, port: u8) -> SetupPacket {
    SetupPacket {
        bm_request_type: BM_REQUEST_TYPE_CLASS_OTHER_H2D,
        b_request: B_REQUEST_CLEAR_FEATURE,
        w_value: feature,
        w_index: port as u16,
        w_length: 0,
    }
}

// ---------------------------------------------------------------------------
// Hub-class GET_HUB_DESCRIPTOR request encoder
// ---------------------------------------------------------------------------

/// `bmRequestType` for a hub-class GET_DESCRIPTOR targeting the **device**:
/// Device-to-Host (bit 7 = 1), Class type (bits 6:5 = 01), Device recipient
/// (bits 4:0 = 00000) → 0xA0.
pub const BM_REQUEST_TYPE_CLASS_DEVICE_D2H: u8 = 0xA0;

/// `bmRequestType` for a hub-class GET_STATUS targeting a **port**
/// (recipient = Other): Device-to-Host (bit 7 = 1), Class type
/// (bits 6:5 = 01), Other recipient (bits 4:0 = 00011) → 0xA3.
pub const BM_REQUEST_TYPE_CLASS_OTHER_D2H: u8 = 0xA3;

/// `bRequest` for GET_DESCRIPTOR (USB 2.0 §9.4.3).
pub const B_REQUEST_GET_DESCRIPTOR: u8 = 0x06;

/// `bRequest` for GET_STATUS (USB 2.0 §9.4 Table 9-4).
pub const B_REQUEST_GET_STATUS: u8 = 0x00;

/// Encode a `GET_DESCRIPTOR(Hub)` request to fetch the hub's class descriptor.
///
/// `length` is the number of bytes to request; use [`HUB_DESCRIPTOR_MIN_LEN`]
/// as the minimum (or the exact hub-descriptor length once `bLength` is known).
pub const fn get_hub_descriptor(length: u16) -> SetupPacket {
    SetupPacket {
        bm_request_type: BM_REQUEST_TYPE_CLASS_DEVICE_D2H,
        b_request: B_REQUEST_GET_DESCRIPTOR,
        w_value: (DESC_TYPE_HUB as u16) << 8,
        w_index: 0,
        w_length: length,
    }
}

// ---------------------------------------------------------------------------
// Hub-class GET_PORT_STATUS request encoder (USB 2.0 §11.24.2.7)
// ---------------------------------------------------------------------------

/// Encode a hub-class `GET_STATUS(port)` [`SetupPacket`] (USB 2.0 §11.24.2.7).
///
/// The host sends this to read a port's 4-byte status word
/// `[wPortStatus(2 LE), wPortChange(2 LE)]`.
///
/// Per USB 2.0 §11.24.2.7:
/// * `bmRequestType` = 0xA3 (class, other recipient, device-to-host)
/// * `bRequest` = 0 (GET_STATUS)
/// * `wValue` = 0
/// * `wIndex` = `port` (1-based port number)
/// * `wLength` = 4 (the 4-byte port status response)
pub const fn get_port_status(port: u8) -> SetupPacket {
    SetupPacket {
        bm_request_type: BM_REQUEST_TYPE_CLASS_OTHER_D2H,
        b_request: B_REQUEST_GET_STATUS,
        w_value: 0,
        w_index: port as u16,
        w_length: 4,
    }
}

// ---------------------------------------------------------------------------
// Port-status bitmap decode helpers (USB 2.0 §11.24.2.7 Tables 11-21/11-22)
// ---------------------------------------------------------------------------
//
// The 4-byte GET_PORT_STATUS response is:
//   bytes 0..2  wPortStatus  — current port state
//   bytes 2..4  wPortChange  — change bits (set by hardware, cleared by
//                              CLEAR_FEATURE(C_PORT_*))
//
// wPortStatus bit positions (Table 11-21):
//   bit 0  PORT_CONNECTION (CCS)
//   bit 1  PORT_ENABLE     (PE)
//   bit 4  PORT_RESET
//
// wPortChange bit positions (Table 11-22):
//   bit 0  C_PORT_CONNECTION
//   bit 4  C_PORT_RESET

/// Bit position of PORT_CONNECTION (CCS) in `wPortStatus`.
pub const PORT_STATUS_CCS_BIT: u8 = 0;
/// Bit position of PORT_ENABLE (PE) in `wPortStatus`.
pub const PORT_STATUS_PE_BIT: u8 = 1;
/// Bit position of PORT_RESET in `wPortStatus`.
pub const PORT_STATUS_RESET_BIT: u8 = 4;
/// Bit position of C_PORT_CONNECTION in `wPortChange`.
pub const PORT_CHANGE_CONNECTION_BIT: u8 = 0;
/// Bit position of C_PORT_RESET in `wPortChange`.
pub const PORT_CHANGE_RESET_BIT: u8 = 4;

/// Returns the little-endian `u16` encoded in bytes `[lo, hi]` of a slice,
/// or `0` if the slice is too short. Purely internal helper.
#[inline]
fn le16_at(status4: &[u8], offset: usize) -> u16 {
    if status4.len() < offset + 2 {
        return 0;
    }
    u16::from_le_bytes([status4[offset], status4[offset + 1]])
}

/// Returns `true` if the PORT_CONNECTION (CCS) bit is set in `wPortStatus`.
///
/// A device is currently connected to the port when this is `true`.
///
/// Defensively returns `false` if `status4` is shorter than 4 bytes.
pub fn port_status_connected(status4: &[u8]) -> bool {
    le16_at(status4, 0) & (1 << PORT_STATUS_CCS_BIT) != 0
}

/// Returns `true` if the PORT_ENABLE (PE) bit is set in `wPortStatus`.
///
/// The port is enabled (data transfers are possible) when this is `true`.
///
/// Defensively returns `false` if `status4` is shorter than 4 bytes.
pub fn port_status_enabled(status4: &[u8]) -> bool {
    le16_at(status4, 0) & (1 << PORT_STATUS_PE_BIT) != 0
}

/// Returns `true` if the PORT_RESET bit is set in `wPortStatus`.
///
/// A USB reset is currently in progress on this port when this is `true`.
///
/// Defensively returns `false` if `status4` is shorter than 4 bytes.
pub fn port_status_resetting(status4: &[u8]) -> bool {
    le16_at(status4, 0) & (1 << PORT_STATUS_RESET_BIT) != 0
}

/// Returns `true` if the C_PORT_CONNECTION bit is set in `wPortChange`.
///
/// The connection status has changed since the host last cleared this bit.
///
/// Defensively returns `false` if `status4` is shorter than 4 bytes.
pub fn port_change_connection(status4: &[u8]) -> bool {
    le16_at(status4, 2) & (1 << PORT_CHANGE_CONNECTION_BIT) != 0
}

/// Returns `true` if the C_PORT_RESET bit is set in `wPortChange`.
///
/// A reset sequence has completed since the host last cleared this bit.
///
/// Defensively returns `false` if `status4` is shorter than 4 bytes.
pub fn port_change_reset(status4: &[u8]) -> bool {
    le16_at(status4, 2) & (1 << PORT_CHANGE_RESET_BIT) != 0
}

/// Bit position of "Low-Speed Device Attached" in `wPortStatus` (USB 2.0
/// §11.24.2.7.1, Table 11-21).
pub const PORT_STATUS_LOW_SPEED_BIT: u8 = 9;
/// Bit position of "High-Speed Device Attached" in `wPortStatus`.
pub const PORT_STATUS_HIGH_SPEED_BIT: u8 = 10;

/// Speed code for a device attached downstream of a hub, matching the xHCI
/// server's `EnumerateChild` wire encoding and the kernel's `PortSpeed`:
/// `0` = Low, `1` = Full, `2` = High. A USB 2.0 hub never reports SuperSpeed
/// downstream, so this never returns the SuperSpeed code (`3`).
pub const DOWNSTREAM_SPEED_LOW: u8 = 0;
/// Full-speed device-attached code (the default when neither LS nor HS is set).
pub const DOWNSTREAM_SPEED_FULL: u8 = 1;
/// High-speed device-attached code.
pub const DOWNSTREAM_SPEED_HIGH: u8 = 2;

/// Decode the downstream device speed from a hub's 4-byte `GET_PORT_STATUS`
/// reply (USB 2.0 §11.24.2.7.1). The Low/High-Speed-Device-Attached bits are
/// mutually exclusive; absent both, the device is Full speed.
///
/// Returns one of [`DOWNSTREAM_SPEED_LOW`] / [`DOWNSTREAM_SPEED_FULL`] /
/// [`DOWNSTREAM_SPEED_HIGH`]. Defensively returns Full speed if `status4` is
/// shorter than 2 bytes.
pub fn port_status_speed_code(status4: &[u8]) -> u8 {
    let w = le16_at(status4, 0);
    if w & (1 << PORT_STATUS_LOW_SPEED_BIT) != 0 {
        DOWNSTREAM_SPEED_LOW
    } else if w & (1 << PORT_STATUS_HIGH_SPEED_BIT) != 0 {
        DOWNSTREAM_SPEED_HIGH
    } else {
        DOWNSTREAM_SPEED_FULL
    }
}

// ---------------------------------------------------------------------------
// PortId — USB topology tree (flat arena)
// ---------------------------------------------------------------------------

/// Maximum hub depth (tiers) encoded in the xHCI route string (xHCI §8.9 /
/// USB 3.2 §8.9).
///
/// The route string has five 4-bit tier slots. Tier 1 is the downstream port
/// of the first hub attached to the root port (not the root port itself). A
/// route string can therefore represent up to 5 hubs between the root and the
/// device, corresponding to USB's 7-tier device limit minus the root hub and
/// the root-hub port tier. Devices whose path requires more than 5 route-string
/// nibbles cannot be addressed by xHCI and are rejected by
/// [`PortTopology::add_child_port`].
pub const MAX_HUB_DEPTH: usize = 5;

/// One node in the USB port topology tree.
///
/// Stored by index in a [`PortTopology`] arena. The root of each root-hub
/// port chain has `parent_idx = None`.
#[derive(Debug, Clone)]
pub struct PortNode {
    /// Port number at this tier (1-based, 1–15 for xHCI route string slots).
    pub port: u8,
    /// Arena index of the parent node, or `None` if this is a root-hub port.
    pub parent_idx: Option<usize>,
    /// Arena indices of child port nodes (hub downstream ports leading to
    /// further hubs or leaf devices).
    pub children: Vec<usize>,
}

/// Flat arena of [`PortNode`] entries representing the USB topology tree.
///
/// Adding, walking, and computing route strings are all O(depth), where
/// depth ≤ [`MAX_HUB_DEPTH`].
#[derive(Debug, Default)]
pub struct PortTopology {
    nodes: Vec<PortNode>,
}

impl PortTopology {
    /// Create a new, empty topology.
    pub fn new() -> Self {
        PortTopology { nodes: Vec::new() }
    }

    /// Add a root-hub port (tier-1 node) and return its arena index.
    pub fn add_root_port(&mut self, port: u8) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(PortNode {
            port,
            parent_idx: None,
            children: Vec::new(),
        });
        idx
    }

    /// Add a child port under the node at `parent_idx` and return the new
    /// node's arena index.
    ///
    /// Returns `None` if `parent_idx` is out of bounds or if attaching a
    /// child would exceed [`MAX_HUB_DEPTH`].
    pub fn add_child_port(&mut self, parent_idx: usize, port: u8) -> Option<usize> {
        // Validate parent exists.
        if parent_idx >= self.nodes.len() {
            return None;
        }
        // Count depth by walking parents.
        if self.depth_of(parent_idx) >= MAX_HUB_DEPTH {
            return None;
        }
        let child_idx = self.nodes.len();
        self.nodes.push(PortNode {
            port,
            parent_idx: Some(parent_idx),
            children: Vec::new(),
        });
        self.nodes[parent_idx].children.push(child_idx);
        Some(child_idx)
    }

    /// Return the depth (number of tiers) of the node at `idx`, where a
    /// root-hub port has depth 1.
    pub fn depth_of(&self, idx: usize) -> usize {
        let mut depth = 1usize;
        let mut current = idx;
        while let Some(parent) = self.nodes[current].parent_idx {
            depth += 1;
            current = parent;
        }
        depth
    }

    /// Compute the xHCI route string (USB 3.2 §8.9 / xHCI §8.9) for the
    /// node at `idx`.
    ///
    /// The route string is a 20-bit value packed into the low 20 bits of a
    /// `u32`. Tier 1 occupies bits 3:0, tier 2 bits 7:4, …, tier 5 bits
    /// 19:16. Each tier value is the downstream port number of a **hub** on
    /// the path from the root hub to the device, clamped to 4 bits (0xF for
    /// ports ≥ 16). The **root-hub port number is NOT encoded** here — it goes
    /// in the Slot Context Root Hub Port Number field (see
    /// [`root_hub_port`](PortTopology::root_hub_port)). A device directly
    /// attached to a root port has route string **0**.
    ///
    /// Returns `0` if the topology is empty or `idx` is out of bounds.
    pub fn route_string(&self, idx: usize) -> u32 {
        if idx >= self.nodes.len() {
            return 0;
        }
        // Collect port numbers from `idx` walking toward root. Stop before
        // the root-hub-port node (the node whose parent_idx is None).
        // The root-hub port is NOT part of the route string.
        let mut chain: [u8; MAX_HUB_DEPTH] = [0; MAX_HUB_DEPTH];
        let mut depth = 0usize;
        let mut current = idx;
        while let Some(parent) = self.nodes[current].parent_idx {
            // `current` is a non-root node; include its port.
            if depth < MAX_HUB_DEPTH {
                chain[depth] = self.nodes[current].port;
                depth += 1;
            }
            current = parent;
            // When we reach the root (parent_idx == None), the while-let exits.
        }
        // `chain[0]` is the deepest port (highest tier); `chain[depth-1]` is
        // the port of the first hub on the root port (tier 1, bits 3:0).
        // Reverse to fill tier 1 first.
        let mut route: u32 = 0;
        for tier in 0..depth {
            let port = chain[depth - 1 - tier];
            let nibble = if port > 0xF { 0xF } else { port as u32 };
            route |= nibble << (tier * 4);
        }
        route
    }

    /// Return the root-hub port number for the node at `idx`.
    ///
    /// Walks the parent chain to the root-hub-port node (the node whose
    /// `parent_idx` is `None`) and returns its `port` field. This is the value
    /// that belongs in the xHCI Slot Context Root Hub Port Number field.
    ///
    /// Returns `None` if `idx` is out of bounds.
    pub fn root_hub_port(&self, idx: usize) -> Option<u8> {
        if idx >= self.nodes.len() {
            return None;
        }
        let mut current = idx;
        loop {
            match self.nodes[current].parent_idx {
                Some(parent) => current = parent,
                None => return Some(self.nodes[current].port),
            }
        }
    }

    /// Walk from `idx` to the root, returning the sequence of arena indices
    /// (inclusive of `idx`, ending at the root-hub-port node).
    pub fn path_to_root(&self, idx: usize) -> Vec<usize> {
        let mut path = Vec::new();
        if idx >= self.nodes.len() {
            return path;
        }
        let mut current = idx;
        loop {
            path.push(current);
            match self.nodes[current].parent_idx {
                Some(parent) => current = parent,
                None => break,
            }
        }
        path
    }

    /// Return a shared reference to the node at `idx`, or `None` if out of
    /// bounds.
    pub fn get(&self, idx: usize) -> Option<&PortNode> {
        self.nodes.get(idx)
    }

    /// Return the total number of nodes in the arena.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Return `true` if the arena is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Hub-enumeration helpers
// ---------------------------------------------------------------------------

/// Iterate the downstream port numbers of a parsed hub (1 ..= `bNbrPorts`).
///
/// The caller uses this to issue `GET_PORT_STATUS` for each port and, once a
/// device is detected, `SET_FEATURE(PORT_RESET)` to bring it up.
pub fn enumerate_hub_ports(desc: &HubDescriptor) -> impl Iterator<Item = u8> {
    1u8..=desc.b_nbr_ports
}

// ---------------------------------------------------------------------------
// Interface class classifier (used from the usbhub daemon to recognise hubs)
// ---------------------------------------------------------------------------

/// Returns `true` if `b_interface_class` indicates a USB Hub interface.
///
/// The usbhub daemon calls this to decide whether an [`AttachNotice`] (once
/// the IPC channel lands in 78c) describes a hub it should enumerate.
pub fn is_hub_interface(b_interface_class: u8) -> bool {
    b_interface_class == crate::usb::descriptor::CLASS_HUB
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Real hub descriptor blob
    //
    // Captured from a 4-port generic USB 2.0 hub (single-TT, self-powered,
    // individual power switching). Wire layout per USB 2.0 §11.23.2.1
    // Table 11-13.
    // -----------------------------------------------------------------------
    //
    // Byte-by-byte breakdown:
    //   0x09 bLength = 9
    //   0x29 bDescriptorType = Hub (0x29)
    //   0x04 bNbrPorts = 4
    //   0x09 0x00  wHubCharacteristics = 0x0009
    //              bits 1:0 = 01 (individual power switching)
    //              bit  2   = 0  (not a compound device)
    //              bits 4:3 = 01 (individual over-current protection)
    //   0x32 bPwrOn2PwrGood = 50 (100 ms)
    //   0x32 bHubContrCurrent = 50 mA
    //   0x00 DeviceRemovable[0]: ports 1-8, all removable
    //   0xFF PortPwrCtrlMask[0]: all ports individually controlled
    //
    const HUB4_DESCRIPTOR_BLOB: &[u8] = &[
        0x09, // bLength
        0x29, // bDescriptorType = Hub
        0x04, // bNbrPorts = 4
        0x09, 0x00, // wHubCharacteristics = 0x0009
        0x32, // bPwrOn2PwrGood = 50 (100 ms)
        0x32, // bHubContrCurrent = 50 mA
        0x00, // DeviceRemovable[0]: all removable
        0xFF, // PortPwrCtrlMask[0]
    ];

    // -----------------------------------------------------------------------
    // Hub descriptor parsing tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_hub_descriptor_b_nbr_ports() {
        let desc =
            HubDescriptor::parse(HUB4_DESCRIPTOR_BLOB).expect("4-port hub descriptor must parse");
        assert_eq!(desc.b_nbr_ports, 4);
    }

    #[test]
    fn parse_hub_descriptor_type() {
        let desc = HubDescriptor::parse(HUB4_DESCRIPTOR_BLOB).unwrap();
        assert_eq!(desc.b_descriptor_type, DESC_TYPE_HUB);
        assert_eq!(desc.b_descriptor_type, 0x29);
    }

    #[test]
    fn parse_hub_descriptor_characteristics() {
        let desc = HubDescriptor::parse(HUB4_DESCRIPTOR_BLOB).unwrap();
        assert_eq!(desc.w_hub_characteristics, 0x0009);
        // Individual power switching (bits 1:0 = 01).
        assert_eq!(desc.power_switching_mode(), 0x01);
        // Not a compound device (bit 2 = 0).
        assert!(!desc.is_compound_device());
        // Individual over-current (bits 4:3 = 01).
        assert_eq!(desc.over_current_mode(), 0x01);
    }

    #[test]
    fn parse_hub_descriptor_pwr_on2_pwr_good() {
        let desc = HubDescriptor::parse(HUB4_DESCRIPTOR_BLOB).unwrap();
        assert_eq!(desc.b_pwr_on2_pwr_good, 50);
    }

    #[test]
    fn parse_hub_descriptor_device_removable_all_removable() {
        let desc = HubDescriptor::parse(HUB4_DESCRIPTOR_BLOB).unwrap();
        // All ports removable (DeviceRemovable[0] = 0x00).
        for port in 1u8..=4 {
            assert!(
                !desc.is_non_removable(port),
                "port {port} should be removable"
            );
        }
    }

    #[test]
    fn parse_hub_descriptor_returns_none_for_short_slice() {
        let short = &HUB4_DESCRIPTOR_BLOB[..7];
        assert!(HubDescriptor::parse(short).is_none());
    }

    #[test]
    fn parse_hub_descriptor_returns_none_for_wrong_type() {
        let mut bad = HUB4_DESCRIPTOR_BLOB.to_vec();
        bad[1] = 0x02; // bDescriptorType = Configuration, not Hub
        assert!(HubDescriptor::parse(&bad).is_none());
    }

    #[test]
    fn parse_hub_descriptor_non_removable_ports() {
        // Build a descriptor with ports 1 and 3 marked non-removable.
        // USB 2.0 §11.23.2.1: port N is at bit N (bit 0 reserved), so ports 1
        // and 3 set bits 1 and 3 → 0b0000_1010 = 0x0A.
        let mut blob = HUB4_DESCRIPTOR_BLOB.to_vec();
        blob[7] = 0x0A;
        let desc = HubDescriptor::parse(&blob).unwrap();
        assert!(desc.is_non_removable(1));
        assert!(!desc.is_non_removable(2));
        assert!(desc.is_non_removable(3));
        assert!(!desc.is_non_removable(4));
        // Bit 0 is reserved, never a port; the no-port-0 guard returns false.
        assert!(!desc.is_non_removable(0));
    }

    // -----------------------------------------------------------------------
    // PortTopology / route-string tests
    //
    // Topology under test:
    //
    //   root hub port 1   (root node, idx 0) — root-hub port, NOT in route string
    //     └─ hub A port 2 (tier 1, idx 1)
    //          └─ hub B port 3 (tier 2, idx 2)
    //               └─ device port 4 (tier 3, idx 3)
    //
    // Route strings (xHCI §8.9 — root-hub port goes in Slot Context, NOT here):
    //   idx 0 (root port 1):   0x0000_0000  (directly on root → route string = 0)
    //   idx 1 (hub A port 2):  0x0000_0002  (tier1=2)
    //   idx 2 (hub B port 3):  0x0000_0032  (tier1=2, tier2=3)
    //   idx 3 (device port 4): 0x0000_0432  (tier1=2, tier2=3, tier3=4)
    // -----------------------------------------------------------------------

    fn build_four_tier_topology() -> (PortTopology, [usize; 4]) {
        let mut topo = PortTopology::new();
        let root = topo.add_root_port(1); // tier 1: port 1
        let hub_a = topo.add_child_port(root, 2).unwrap(); // tier 2: port 2
        let hub_b = topo.add_child_port(hub_a, 3).unwrap(); // tier 3: port 3
        let device = topo.add_child_port(hub_b, 4).unwrap(); // tier 4: port 4
        (topo, [root, hub_a, hub_b, device])
    }

    #[test]
    fn route_string_root_port() {
        let (topo, idxs) = build_four_tier_topology();
        // Root-hub port: route string = 0 (root port is NOT in the route string;
        // it goes in the Slot Context Root Hub Port Number field per xHCI §8.9).
        assert_eq!(topo.route_string(idxs[0]), 0x0000_0000);
    }

    #[test]
    fn route_string_hub_a() {
        let (topo, idxs) = build_four_tier_topology();
        // Hub A is at tier 1 (first hub on the root port), port 2 → 0x02.
        assert_eq!(topo.route_string(idxs[1]), 0x0000_0002);
    }

    #[test]
    fn route_string_hub_b() {
        let (topo, idxs) = build_four_tier_topology();
        // Tier 1 = hub A port 2, tier 2 = hub B port 3 → 0x32.
        assert_eq!(topo.route_string(idxs[2]), 0x0000_0032);
    }

    #[test]
    fn route_string_device() {
        let (topo, idxs) = build_four_tier_topology();
        // Tier 1 = hub A port 2, tier 2 = hub B port 3, tier 3 = device port 4
        // → 0x432.
        assert_eq!(topo.route_string(idxs[3]), 0x0000_0432);
    }

    #[test]
    fn root_hub_port_of_each_node() {
        let (topo, idxs) = build_four_tier_topology();
        // All nodes in this chain share root-hub port 1.
        for &idx in &idxs {
            assert_eq!(
                topo.root_hub_port(idx),
                Some(1),
                "root_hub_port must return 1 for idx {idx}"
            );
        }
    }

    #[test]
    fn root_hub_port_out_of_bounds() {
        let topo = PortTopology::new();
        assert_eq!(topo.root_hub_port(0), None);
    }

    #[test]
    fn route_string_nested_topology_walk_to_root() {
        let (topo, idxs) = build_four_tier_topology();
        // Walk from the deepest node to root.
        let path = topo.path_to_root(idxs[3]);
        assert_eq!(path.len(), 4);
        // First element is `idxs[3]`, last is root (idxs[0]).
        assert_eq!(path[0], idxs[3]);
        assert_eq!(path[3], idxs[0]);
    }

    #[test]
    fn depth_of_each_tier() {
        let (topo, idxs) = build_four_tier_topology();
        assert_eq!(topo.depth_of(idxs[0]), 1); // root hub port
        assert_eq!(topo.depth_of(idxs[1]), 2); // hub A
        assert_eq!(topo.depth_of(idxs[2]), 3); // hub B
        assert_eq!(topo.depth_of(idxs[3]), 4); // device
    }

    #[test]
    fn add_child_port_rejects_excessive_depth() {
        let mut topo = PortTopology::new();
        let mut idx = topo.add_root_port(1);
        // Add nodes up to MAX_HUB_DEPTH.
        for port in 2..=(MAX_HUB_DEPTH as u8) {
            idx = topo
                .add_child_port(idx, port)
                .expect("should succeed within depth limit");
        }
        // One more level beyond MAX_HUB_DEPTH should fail.
        assert!(
            topo.add_child_port(idx, 6).is_none(),
            "must reject depth > MAX_HUB_DEPTH"
        );
    }

    #[test]
    fn topology_is_empty_initially() {
        let topo = PortTopology::new();
        assert!(topo.is_empty());
        assert_eq!(topo.len(), 0);
    }

    #[test]
    fn route_string_clamped_to_four_bits_for_large_port() {
        // A child port number ≥ 16 should be stored as 0xF (clamped) per
        // USB 3.2 §8.9. The root-hub port is not in the route string, so the
        // clamp must be tested on a non-root (child) node.
        let mut topo = PortTopology::new();
        let root = topo.add_root_port(1); // root port 1 — NOT in route string
        let child = topo.add_child_port(root, 20).unwrap(); // port 20 → clamp to 0xF
        // Route string for child: tier 1 = 0xF (20 clamped).
        assert_eq!(topo.route_string(child), 0x0F);
        // Root itself still has route string 0.
        assert_eq!(topo.route_string(root), 0x00);
    }

    // -----------------------------------------------------------------------
    // set_port_feature encoding tests
    // -----------------------------------------------------------------------

    #[test]
    fn set_port_feature_port_power_encoding() {
        let pkt = set_port_feature(PORT_POWER, 1);
        // bmRequestType: class | other | host-to-device = 0x23.
        assert_eq!(pkt.bm_request_type, 0x23);
        // bRequest: SET_FEATURE = 3.
        assert_eq!(pkt.b_request, 0x03);
        // wValue: PORT_POWER = 8.
        assert_eq!(pkt.w_value, PORT_POWER);
        assert_eq!(pkt.w_value, 8);
        // wIndex: port number (1).
        assert_eq!(pkt.w_index, 1);
        // wLength: 0 (no data stage).
        assert_eq!(pkt.w_length, 0);
    }

    #[test]
    fn set_port_feature_port_reset_encoding() {
        let pkt = set_port_feature(PORT_RESET, 3);
        assert_eq!(pkt.bm_request_type, BM_REQUEST_TYPE_CLASS_OTHER_H2D);
        assert_eq!(pkt.b_request, B_REQUEST_SET_FEATURE);
        assert_eq!(pkt.w_value, PORT_RESET);
        assert_eq!(pkt.w_value, 4);
        assert_eq!(pkt.w_index, 3);
        assert_eq!(pkt.w_length, 0);
    }

    #[test]
    fn set_port_feature_all_ports() {
        // Verify each of the 4-port hub's ports produces the correct wIndex.
        for port in 1u8..=4 {
            let pkt = set_port_feature(PORT_POWER, port);
            assert_eq!(pkt.w_index, port as u16);
        }
    }

    // -----------------------------------------------------------------------
    // enumerate_hub_ports helper test
    // -----------------------------------------------------------------------

    #[test]
    fn enumerate_hub_ports_yields_all_ports() {
        let desc = HubDescriptor::parse(HUB4_DESCRIPTOR_BLOB).unwrap();
        let ports: Vec<u8> = enumerate_hub_ports(&desc).collect();
        assert_eq!(ports, &[1, 2, 3, 4]);
    }

    // -----------------------------------------------------------------------
    // is_hub_interface classifier test
    // -----------------------------------------------------------------------

    #[test]
    fn is_hub_interface_recognises_hub_class() {
        use crate::usb::descriptor::{CLASS_HID, CLASS_HUB};
        assert!(is_hub_interface(CLASS_HUB));
        assert!(!is_hub_interface(CLASS_HID));
        assert!(!is_hub_interface(0x00));
    }

    // -----------------------------------------------------------------------
    // get_port_status encoding tests (A.3)
    // -----------------------------------------------------------------------

    #[test]
    fn get_port_status_port1_encoding() {
        let pkt = get_port_status(1);
        // bmRequestType: class | other | device-to-host = 0xA3.
        assert_eq!(pkt.bm_request_type, 0xA3);
        assert_eq!(pkt.bm_request_type, BM_REQUEST_TYPE_CLASS_OTHER_D2H);
        // bRequest: GET_STATUS = 0x00.
        assert_eq!(pkt.b_request, 0x00);
        assert_eq!(pkt.b_request, B_REQUEST_GET_STATUS);
        // wValue: 0 (unused for GET_STATUS).
        assert_eq!(pkt.w_value, 0);
        // wIndex: port number (1-based).
        assert_eq!(pkt.w_index, 1);
        // wLength: 4 (the 4-byte wPortStatus + wPortChange response).
        assert_eq!(pkt.w_length, 4);
    }

    #[test]
    fn get_port_status_port3_encoding() {
        let pkt = get_port_status(3);
        assert_eq!(pkt.bm_request_type, BM_REQUEST_TYPE_CLASS_OTHER_D2H);
        assert_eq!(pkt.b_request, B_REQUEST_GET_STATUS);
        assert_eq!(pkt.w_value, 0);
        assert_eq!(pkt.w_index, 3);
        assert_eq!(pkt.w_length, 4);
    }

    #[test]
    fn get_port_status_all_ports_windex() {
        // Verify each of the 4-port hub's ports produces the correct wIndex.
        for port in 1u8..=4 {
            let pkt = get_port_status(port);
            assert_eq!(pkt.w_index, port as u16, "port {port}: wIndex mismatch");
            assert_eq!(pkt.w_length, 4, "port {port}: wLength must be 4");
        }
    }

    // Verify the exact 8 SETUP bytes via as_u64 for port 2:
    // A3 00 00 00 02 00 04 00
    // byte 0: bmRequestType = 0xA3
    // byte 1: bRequest      = 0x00
    // bytes 2-3: wValue     = 0x0000 LE
    // bytes 4-5: wIndex     = 0x0002 LE (port 2)
    // bytes 6-7: wLength    = 0x0004 LE
    #[test]
    fn get_port_status_port2_as_u64_bytes() {
        let pkt = get_port_status(2);
        let raw = pkt.as_u64();
        let bytes = raw.to_le_bytes();
        assert_eq!(bytes[0], 0xA3, "byte 0: bmRequestType");
        assert_eq!(bytes[1], 0x00, "byte 1: bRequest GET_STATUS");
        assert_eq!(bytes[2], 0x00, "byte 2: wValue low");
        assert_eq!(bytes[3], 0x00, "byte 3: wValue high");
        assert_eq!(bytes[4], 0x02, "byte 4: wIndex low (port 2)");
        assert_eq!(bytes[5], 0x00, "byte 5: wIndex high");
        assert_eq!(bytes[6], 0x04, "byte 6: wLength low (4)");
        assert_eq!(bytes[7], 0x00, "byte 7: wLength high");
    }

    // -----------------------------------------------------------------------
    // Port-status bitmap decode helper tests (A.3)
    // -----------------------------------------------------------------------

    // [0x01, 0x00, 0x00, 0x00]: wPortStatus=0x0001 (CCS set), wPortChange=0.
    // → connected, not enabled, not resetting, no change bits.
    #[test]
    fn port_status_connected_only() {
        let s: &[u8] = &[0x01, 0x00, 0x00, 0x00];
        assert!(port_status_connected(s), "CCS must be set");
        assert!(!port_status_enabled(s), "PE must be clear");
        assert!(!port_status_resetting(s), "RESET must be clear");
        assert!(!port_change_connection(s), "C_CONNECTION must be clear");
        assert!(!port_change_reset(s), "C_RESET must be clear");
    }

    // [0x03, 0x00, 0x00, 0x00]: wPortStatus=0x0003 (CCS+PE), wPortChange=0.
    // → connected, enabled, not resetting.
    #[test]
    fn port_status_connected_and_enabled() {
        let s: &[u8] = &[0x03, 0x00, 0x00, 0x00];
        assert!(port_status_connected(s));
        assert!(port_status_enabled(s));
        assert!(!port_status_resetting(s));
        assert!(!port_change_connection(s));
        assert!(!port_change_reset(s));
    }

    // [0x13, 0x00, 0x01, 0x00]:
    //   wPortStatus = 0x0013 = bits 0,1,4 = CCS + PE + RESET
    //   wPortChange = 0x0001 = bit 0 = C_CONNECTION
    #[test]
    fn port_status_connected_enabled_resetting_c_connection() {
        let s: &[u8] = &[0x13, 0x00, 0x01, 0x00];
        assert!(port_status_connected(s), "CCS");
        assert!(port_status_enabled(s), "PE");
        assert!(port_status_resetting(s), "RESET (bit 4)");
        assert!(
            port_change_connection(s),
            "C_CONNECTION (bit 0 of wPortChange)"
        );
        assert!(!port_change_reset(s), "C_RESET must be clear");
    }

    // [0x00, 0x00, 0x10, 0x00]:
    //   wPortStatus = 0x0000 (nothing)
    //   wPortChange = 0x0010 = bit 4 = C_RESET
    #[test]
    fn port_change_reset_set_only() {
        let s: &[u8] = &[0x00, 0x00, 0x10, 0x00];
        assert!(!port_status_connected(s));
        assert!(!port_status_enabled(s));
        assert!(!port_status_resetting(s));
        assert!(!port_change_connection(s));
        assert!(port_change_reset(s), "C_RESET (bit 4 of wPortChange)");
    }

    // All-zero status: everything false.
    #[test]
    fn port_status_all_zero() {
        let s: &[u8] = &[0x00, 0x00, 0x00, 0x00];
        assert!(!port_status_connected(s));
        assert!(!port_status_enabled(s));
        assert!(!port_status_resetting(s));
        assert!(!port_change_connection(s));
        assert!(!port_change_reset(s));
    }

    // Short-slice (2 bytes): all helpers must return false without panicking.
    #[test]
    fn port_status_short_slice_no_panic() {
        let s: &[u8] = &[0xFF, 0xFF]; // only 2 bytes instead of 4
        // wPortStatus: both bytes present → can read LE u16 = 0xFFFF
        assert!(port_status_connected(s)); // CCS bit set in 0xFFFF
        assert!(port_status_enabled(s)); // PE bit set
        assert!(port_status_resetting(s)); // RESET bit set
        // wPortChange: at offset 2, need 2 bytes but only 0 remain → 0 → false
        assert!(
            !port_change_connection(s),
            "must return false for short slice"
        );
        assert!(!port_change_reset(s), "must return false for short slice");
    }

    // Empty slice: all helpers must return false without panicking.
    #[test]
    fn port_status_empty_slice_no_panic() {
        let s: &[u8] = &[];
        assert!(!port_status_connected(s));
        assert!(!port_status_enabled(s));
        assert!(!port_status_resetting(s));
        assert!(!port_change_connection(s));
        assert!(!port_change_reset(s));
    }

    #[test]
    fn port_status_speed_code_decodes_each_speed() {
        // Neither LS nor HS bit → Full speed.
        assert_eq!(
            port_status_speed_code(&[0x01, 0x00, 0x00, 0x00]),
            DOWNSTREAM_SPEED_FULL
        );
        // Bit 9 (Low-Speed Device Attached) set.
        assert_eq!(
            port_status_speed_code(&[0x00, 0x02, 0x00, 0x00]),
            DOWNSTREAM_SPEED_LOW
        );
        // Bit 10 (High-Speed Device Attached) set.
        assert_eq!(
            port_status_speed_code(&[0x00, 0x04, 0x00, 0x00]),
            DOWNSTREAM_SPEED_HIGH
        );
        // Low takes precedence if both somehow set (LS is checked first).
        assert_eq!(
            port_status_speed_code(&[0x00, 0x06, 0x00, 0x00]),
            DOWNSTREAM_SPEED_LOW
        );
        // Short slice → defensively Full.
        assert_eq!(port_status_speed_code(&[0x00]), DOWNSTREAM_SPEED_FULL);
    }
}
