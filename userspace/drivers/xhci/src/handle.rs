//! Controller-multiplexing slot handle codec.
//!
//! The `AttachNotice` protocol carries a single `u8` `slot_id`. When the server
//! services more than one xHCI controller it must encode *which* controller a
//! device belongs to into that one byte so a later request routes back to the
//! right controller. This module is the pure (no_std-and-host-testable) codec
//! for that packing; the server policy that uses it lives in `server.rs` (which
//! is `#[cfg(not(test))]` because it pulls in the driver runtime, so the codec
//! is split out here to keep it unit-testable on the host).

/// Pack a `(controller index, hardware slot id)` pair into the single `u8`
/// `slot_id` field the `AttachNotice` protocol carries. The top two bits index
/// the controller (up to 4) and the low six bits the slot. For controller 0 the
/// handle equals the raw slot id, so the single-controller path (and the QEMU
/// smoke gates) are byte-for-byte unchanged. xHCI assigns the few attached
/// devices small slot ids (1..N), well within six bits.
///
/// **Fails closed**: returns `None` when the pair cannot be encoded losslessly
/// (controller index > 3 or slot id > 63) rather than silently truncating into
/// a colliding handle. A colliding handle would route the device's later
/// control/bulk transfers to a *different* controller/slot — so an unencodable
/// device is dropped (not served) at the call site instead of misrouted.
pub(crate) fn pack_handle(ctrl_idx: usize, slot_id: u8) -> Option<u8> {
    if ctrl_idx > 0b11 || slot_id > 0x3F {
        return None;
    }
    Some(((ctrl_idx as u8) << 6) | (slot_id & 0x3F))
}

/// Inverse of [`pack_handle`]: recover `(controller index, hardware slot id)`
/// from a packed handle. Total over all `u8` — every byte decodes to an
/// in-range `(0..=3, 0..=63)` pair, which is the exact inverse of every value
/// [`pack_handle`] can produce.
pub(crate) fn unpack_handle(handle: u8) -> (usize, u8) {
    ((handle >> 6) as usize, handle & 0x3F)
}

#[cfg(test)]
mod tests {
    use super::{pack_handle, unpack_handle};

    #[test]
    fn pack_handle_single_controller_is_identity() {
        // Controller 0 with a 6-bit slot must encode to the raw slot id, so the
        // single-controller path (and the QEMU smoke gates that read slot ids
        // off the wire) stay byte-for-byte unchanged.
        for slot in 0u8..=0x3F {
            assert_eq!(pack_handle(0, slot), Some(slot));
        }
    }

    #[test]
    fn pack_unpack_round_trips_within_range() {
        for ctrl in 0usize..=0b11 {
            for slot in 0u8..=0x3F {
                let handle = pack_handle(ctrl, slot).expect("in-range pair must encode");
                assert_eq!(unpack_handle(handle), (ctrl, slot));
            }
        }
    }

    #[test]
    fn pack_handle_fails_closed_out_of_range() {
        // A (controller, slot) pair that doesn't fit one byte (>=4 controllers
        // or slot > 63) must return None — the caller drops the device — rather
        // than truncate into a colliding handle that misroutes transfers.
        assert_eq!(pack_handle(4, 1), None); // controller index overflows 2 bits
        assert_eq!(pack_handle(0, 64), None); // slot overflows 6 bits
        assert_eq!(pack_handle(0, 0xFF), None);
        assert_eq!(pack_handle(7, 70), None);
        // Boundary: the largest encodable pair is still Some.
        assert_eq!(pack_handle(3, 63), Some(0b1111_1111));
    }

    #[test]
    fn unpack_is_total_and_left_inverse_of_pack() {
        // Every byte decodes to an in-range pair, and re-packing reproduces it.
        for handle in 0u8..=u8::MAX {
            let (ctrl, slot) = unpack_handle(handle);
            assert!(ctrl <= 0b11 && slot <= 0x3F);
            assert_eq!(pack_handle(ctrl, slot), Some(handle));
        }
    }
}
