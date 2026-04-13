//! Debug helpers for memory map and frame allocator reporting.
//!
//! # Precondition
//! All functions in this module call `super::memory_map::regions()`, which panics
//! if invoked before `memory_map::init()` has been called. Helpers may be used
//! as soon as `memory_map::init()` has completed (i.e. during or after `mm::init()`).

// These are on-demand diagnostic helpers, not all called during normal boot.
#![allow(dead_code)]

use bootloader_api::info::MemoryRegionKind;

const PAGE_SIZE: u64 = 4096;
use super::frame_allocator::ALLOC_MIN_ADDR;

/// Log each memory region (kind, start, end, size in KB) at `debug` level,
/// followed by an `info`-level summary of usable vs total bytes.
///
/// # Panics
/// Panics if `memory_map::init()` has not been called yet.
pub fn log_memory_map() {
    let regions = super::memory_map::regions();

    let mut total_bytes: u64 = 0;
    let mut usable_bytes: u64 = 0;

    for region in regions {
        let size = region.end.saturating_sub(region.start);
        let size_kb = size / 1024;

        log::debug!(
            "[mm/debug] {:?} start={:#010x} end={:#010x} size={}KB",
            region.kind,
            region.start,
            region.end,
            size_kb,
        );

        total_bytes = total_bytes.saturating_add(size);
        if region.kind == MemoryRegionKind::Usable {
            usable_bytes = usable_bytes.saturating_add(size);
        }
    }

    log::info!(
        "[mm/debug] memory map: usable={} KB, total={} KB ({} regions)",
        usable_bytes / 1024,
        total_bytes / 1024,
        regions.len(),
    );
}

/// Log a summary of usable physical memory derived from the memory map,
/// including how many frames have been consumed by the allocator so far.
///
/// # Panics
/// Panics if `memory_map::init()` has not been called yet.
pub fn log_frame_stats() {
    let regions = super::memory_map::regions();

    let mut total_frames: u64 = 0;

    for region in regions {
        if region.kind != MemoryRegionKind::Usable {
            continue;
        }

        // Align inward and clamp to 1 MiB to match the frame allocator's
        // ALLOC_MIN_ADDR skip — sub-1MiB frames are never handed out.
        let start = align_up(region.start.max(ALLOC_MIN_ADDR), PAGE_SIZE);
        let end = align_down(region.end, PAGE_SIZE);

        if end > start {
            let frames = (end - start) / PAGE_SIZE;
            log::debug!(
                "[mm/debug] usable region {:#010x}..{:#010x} -> {} frames",
                start,
                end,
                frames,
            );
            total_frames = total_frames.saturating_add(frames);
        }
    }

    let free = super::frame_allocator::available_count();
    let total = super::frame_allocator::total_frames();
    let total_mib = (total as u64 * PAGE_SIZE) / (1024 * 1024);

    log::info!(
        "[mm/debug] frame stats: {}/{} frames available ({} MiB total usable)",
        free,
        total,
        total_mib,
    );
}

/// Log non-`Usable` regions whose start address is below 1 MiB.
///
/// These regions cover legacy real-mode structures (IVT, BDA, EBDA, VGA, ROM)
/// that must never be handed out by the frame allocator. Finding them here
/// confirms they exist; the allocator skips them by only touching `Usable` regions.
///
/// # Panics
/// Panics if `memory_map::init()` has not been called yet.
pub fn log_reserved_below_1mib() {
    const ONE_MIB: u64 = 0x0010_0000;

    let regions = super::memory_map::regions();
    let mut found = false;

    for region in regions {
        if region.kind != MemoryRegionKind::Usable && region.start < ONE_MIB {
            log::debug!(
                "[mm/debug] reserved below 1 MiB: {:?} {:#010x}..{:#010x}",
                region.kind,
                region.start,
                region.end,
            );
            found = true;
        }
    }

    if found {
        log::info!(
            "[mm/debug] reserved regions below 1 MiB present (expected — allocator skips non-Usable)"
        );
    } else {
        log::info!("[mm/debug] no reserved regions below 1 MiB found");
    }
}

#[inline]
fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

#[inline]
fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}
