// Pure-logic MMIO bounds-check and cache-mode selection — Phase 55b Track B.2.
//
// The kernel-side syscall handler (`kernel/src/syscall/device_host.rs`) resolves
// a BAR's physical size and prefetchable flag from PCI config space, then asks
// this module to:
//
//   * validate that the BAR is non-zero and fits a sane upper bound,
//   * pick the correct [`MmioCacheMode`] (WC for prefetchable, UC otherwise),
//   * build an [`MmioWindowDescriptor`] that captures the full BAR region.
//
// Everything here is pure math + data-shape validation so the host-test suite
// pins every class of bug (wrong cache mode, off-by-one on the size, bogus
// page count) before the kernel-side mapping code ever runs. No allocation, no
// `std`, no `panic!()` — errors are typed through [`MmioBoundsError`].

use super::types::{MmioCacheMode, MmioWindowDescriptor};

/// Conservative per-BAR size cap for user-space mapping.
///
/// 64 MiB is above the largest BAR the drivers we host in Phase 55b request
/// (NVMe uses a 16 KiB BAR for its admin MMIO; e1000 uses 128 KiB for its
/// register file); anything that claims more than this is almost certainly a
/// mis-sized BAR whose mapping would waste a large user-VA window and is
/// rejected up front. The constant is exposed for test assertions and so a
/// future real driver that legitimately needs more can raise it in a single
/// audited place.
pub const MAX_MMIO_BAR_BYTES: u64 = 64 * 1024 * 1024;

/// Errors surfaced by [`validate_mmio_bar_size`] and [`build_mmio_window`].
///
/// Each variant maps 1-to-1 onto a distinct negative errno at the syscall
/// boundary, so the host-side tests can assert the exact error surface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MmioBoundsError {
    /// `bar_index >= 6` — outside the type-0 PCI header's BAR table.
    BarIndexOutOfRange,
    /// The BAR's actual size came back as zero — device reports it
    /// unimplemented and there is nothing to map.
    ZeroSizedBar,
    /// The BAR is larger than [`MAX_MMIO_BAR_BYTES`] — refused to map a
    /// suspiciously oversized window.
    BarTooLarge,
    /// The BAR's physical base is zero — device treats it as unimplemented.
    ZeroPhysBase,
    /// The BAR's physical base is not 4 KiB-page-aligned — mapping would
    /// require a sub-page offset which the user-VA allocator does not support.
    UnalignedPhysBase,
}

/// Validate that `bar_index` is in range and that `bar_size` is a sane
/// non-zero length that fits within [`MAX_MMIO_BAR_BYTES`].
///
/// The task-doc acceptance item "an out-of-range index returns `-EINVAL`" and
/// "a zero-size BAR returns `-ENODEV`" pin the exact errors returned here. The
/// kernel-side dispatcher converts the variants as follows:
///
/// | `MmioBoundsError`           | errno             |
/// |-----------------------------|-------------------|
/// | `BarIndexOutOfRange`        | `-EINVAL`         |
/// | `ZeroSizedBar`              | `-ENODEV`         |
/// | `BarTooLarge`               | `-EINVAL`         |
/// | `ZeroPhysBase`              | `-ENODEV`         |
/// | `UnalignedPhysBase`         | `-EINVAL`         |
pub fn validate_mmio_bar_size(bar_index: u8, bar_size: u64) -> Result<(), MmioBoundsError> {
    if bar_index >= 6 {
        return Err(MmioBoundsError::BarIndexOutOfRange);
    }
    if bar_size == 0 {
        return Err(MmioBoundsError::ZeroSizedBar);
    }
    if bar_size > MAX_MMIO_BAR_BYTES {
        return Err(MmioBoundsError::BarTooLarge);
    }
    Ok(())
}

/// Select the right [`MmioCacheMode`] for a BAR.
///
/// Write-combining is appropriate for prefetchable BARs (typically framebuffer
/// / ring-buffer style regions where coalesced writes are safe). Everything
/// else maps as uncacheable so MMIO register writes reach the device on every
/// store. The single-line helper is factored out so the syscall path and the
/// unit tests agree on exactly one rule.
pub const fn cache_mode_for_bar(prefetchable: bool) -> MmioCacheMode {
    if prefetchable {
        MmioCacheMode::WriteCombining
    } else {
        MmioCacheMode::Uncacheable
    }
}

/// Build an [`MmioWindowDescriptor`] for a validated BAR.
///
/// The caller has already resolved `phys_base`, `bar_size`, and `prefetchable`
/// from config space. This helper performs the final bounds-check on the
/// physical base, picks the cache mode, and assembles the descriptor so the
/// kernel-side mapper has one pure-data structure to work against.
pub fn build_mmio_window(
    bar_index: u8,
    phys_base: u64,
    bar_size: u64,
    prefetchable: bool,
) -> Result<MmioWindowDescriptor, MmioBoundsError> {
    validate_mmio_bar_size(bar_index, bar_size)?;
    if phys_base == 0 {
        return Err(MmioBoundsError::ZeroPhysBase);
    }
    // 4 KiB page alignment — x86_64 PTEs only describe 4 KiB leaves at this
    // granularity and user-VA mapping operates page-by-page.
    if phys_base & 0xFFF != 0 {
        return Err(MmioBoundsError::UnalignedPhysBase);
    }
    Ok(MmioWindowDescriptor {
        phys_base,
        len: bar_size as usize,
        bar_index,
        prefetchable,
        cache_mode: cache_mode_for_bar(prefetchable),
    })
}

/// Round `bytes` up to the next multiple of 4096 and return the page count.
///
/// Returns `None` if the rounding would overflow a `u64` — indicates the BAR
/// is absurdly close to the address-space top and the caller should treat it
/// as unmappable.
pub fn bar_page_count(bytes: u64) -> Option<u64> {
    bytes.checked_add(0xFFF).map(|v| v >> 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- validate_mmio_bar_size -----------------------------------------

    #[test]
    fn valid_bar_index_and_size_passes() {
        assert_eq!(validate_mmio_bar_size(0, 0x1000), Ok(()));
        assert_eq!(validate_mmio_bar_size(5, 0x10000), Ok(()));
    }

    #[test]
    fn bar_index_six_is_out_of_range() {
        assert_eq!(
            validate_mmio_bar_size(6, 0x1000),
            Err(MmioBoundsError::BarIndexOutOfRange),
        );
        assert_eq!(
            validate_mmio_bar_size(255, 0x1000),
            Err(MmioBoundsError::BarIndexOutOfRange),
        );
    }

    #[test]
    fn zero_bar_size_is_rejected() {
        assert_eq!(
            validate_mmio_bar_size(0, 0),
            Err(MmioBoundsError::ZeroSizedBar),
        );
    }

    #[test]
    fn bar_size_exceeding_cap_is_rejected() {
        assert_eq!(
            validate_mmio_bar_size(0, MAX_MMIO_BAR_BYTES + 1),
            Err(MmioBoundsError::BarTooLarge),
        );
        // Exactly at the cap is allowed — cap is inclusive.
        assert_eq!(validate_mmio_bar_size(0, MAX_MMIO_BAR_BYTES), Ok(()));
    }

    // ---- cache_mode_for_bar ---------------------------------------------

    #[test]
    fn prefetchable_bar_selects_write_combining() {
        assert_eq!(cache_mode_for_bar(true), MmioCacheMode::WriteCombining);
    }

    #[test]
    fn non_prefetchable_bar_selects_uncacheable() {
        assert_eq!(cache_mode_for_bar(false), MmioCacheMode::Uncacheable);
    }

    // ---- build_mmio_window ----------------------------------------------

    #[test]
    fn build_window_for_valid_bar_returns_descriptor() {
        let desc =
            build_mmio_window(0, 0xfebf_0000, 0x1_0000, false).expect("valid non-prefetchable BAR");
        assert_eq!(desc.phys_base, 0xfebf_0000);
        assert_eq!(desc.len, 0x1_0000);
        assert_eq!(desc.bar_index, 0);
        assert!(!desc.prefetchable);
        assert_eq!(desc.cache_mode, MmioCacheMode::Uncacheable);
    }

    #[test]
    fn build_window_for_prefetchable_bar_sets_wc() {
        let desc =
            build_mmio_window(2, 0x1_0000_0000, 0x400_0000, true).expect("valid prefetchable BAR");
        assert!(desc.prefetchable);
        assert_eq!(desc.cache_mode, MmioCacheMode::WriteCombining);
    }

    #[test]
    fn build_window_rejects_zero_phys_base() {
        assert_eq!(
            build_mmio_window(0, 0, 0x1000, false),
            Err(MmioBoundsError::ZeroPhysBase),
        );
    }

    #[test]
    fn build_window_rejects_unaligned_phys_base() {
        assert_eq!(
            build_mmio_window(0, 0xfebf_0800, 0x1000, false),
            Err(MmioBoundsError::UnalignedPhysBase),
        );
    }

    #[test]
    fn build_window_rejects_out_of_range_bar_index() {
        assert_eq!(
            build_mmio_window(6, 0xfebf_0000, 0x1000, false),
            Err(MmioBoundsError::BarIndexOutOfRange),
        );
    }

    #[test]
    fn build_window_rejects_zero_size() {
        assert_eq!(
            build_mmio_window(0, 0xfebf_0000, 0, false),
            Err(MmioBoundsError::ZeroSizedBar),
        );
    }

    // ---- bar_page_count -------------------------------------------------

    #[test]
    fn page_count_rounds_up() {
        assert_eq!(bar_page_count(0), Some(0));
        assert_eq!(bar_page_count(1), Some(1));
        assert_eq!(bar_page_count(4095), Some(1));
        assert_eq!(bar_page_count(4096), Some(1));
        assert_eq!(bar_page_count(4097), Some(2));
        assert_eq!(bar_page_count(0x10_0000), Some(0x100));
    }

    #[test]
    fn page_count_detects_overflow_near_top_of_address_space() {
        // A BAR that claims to start 0xFFF below u64::MAX would overflow the
        // round-up arithmetic — ensure the helper signals that instead of
        // wrapping.
        assert_eq!(bar_page_count(u64::MAX), None);
    }

    // ---- Property-style: round-trip with MmioWindowDescriptor ------------

    #[test]
    fn build_window_output_round_trips_over_stable_byte_encoding() {
        let desc = build_mmio_window(3, 0xfebf_0000, 0x2000, false).expect("valid BAR");
        let bytes = desc.to_bytes();
        let back = MmioWindowDescriptor::from_bytes(bytes).expect("stable round-trip");
        assert_eq!(desc, back);
    }
}
