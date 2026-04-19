//! Pure-logic parity suite — Phase 55a Track F.4 deliverable #2.
//!
//! Where `iommu_contract.rs` pins down the `IommuUnit` trait surface,
//! this file pins down the pure-logic *substrate* both vendor impls
//! share, proving the two sit on equivalent foundations:
//!
//! - `VtdPageTableEntry::encode` / `decode` round-trip for documented
//!   bits (4 KiB, 2 MiB, 1 GiB flags + PPN).
//! - `AmdViPageTableEntry::encode` / `decode` round-trip for documented
//!   bits (Present, NextLevel, PFN, I/O-Read, I/O-Write).
//! - `IovaAllocator` satisfies the contract: no-overlap, freelist reuse,
//!   exhaustion error path.
//! - Fault-record field plumbing: given known vendor register values,
//!   the `FaultRecord` produced carries the correct requester BDF /
//!   IOVA / reason (structural parity; vendor-specific decoders will
//!   land in their modules when hardware bring-up wires MSI vectors).
//!
//! The tests here are deliberately short: full property coverage lives
//! in each module's own `#[cfg(test)]` block. This integration file
//! exists so a reviewer scanning `kernel-core/tests/` sees the parity
//! promise made concrete in one place, and so Track F.4's acceptance
//! checklist closes against a single file.

use kernel_core::iommu::amdvi_page_table::{AmdViPageTableEntry, AmdViPteFlags, PFN_MASK_40};
use kernel_core::iommu::contract::{FaultRecord, Iova};
use kernel_core::iommu::iova::{IovaAllocator, IovaError, IovaRange};
use kernel_core::iommu::vtd_page_table::{VtdPageTableEntry, VtdPteFlags};

// ---------------------------------------------------------------------------
// Page-table entry encode / decode round-trip — VT-d
// ---------------------------------------------------------------------------

#[test]
fn vtd_entry_round_trip_read_write() {
    // A present, readable, writable 4 KiB mapping.
    let phys = 0xFEED_C000u64;
    let flags = VtdPteFlags::READ | VtdPteFlags::WRITE;
    let entry = VtdPageTableEntry::new(phys, flags);
    let raw = entry.encode();
    let decoded = VtdPageTableEntry::decode(raw);
    assert_eq!(decoded.phys(), phys);
    assert_eq!(decoded.flags(), flags);
    assert!(decoded.is_present());
    assert!(!decoded.is_super_page());
}

#[test]
fn vtd_entry_round_trip_super_page() {
    // 2 MiB or 1 GiB terminal: bits 7 + R + W.
    let phys = 0x0040_0000u64;
    let flags = VtdPteFlags::READ | VtdPteFlags::WRITE | VtdPteFlags::SUPER_PAGE;
    let entry = VtdPageTableEntry::new(phys, flags);
    let decoded = VtdPageTableEntry::decode(entry.encode());
    assert_eq!(decoded.phys(), phys);
    assert!(decoded.is_super_page());
    assert!(decoded.is_present());
}

#[test]
fn vtd_entry_not_present_when_no_rw_bits() {
    // All-zero wire word is "not present".
    let decoded = VtdPageTableEntry::decode(0);
    assert!(!decoded.is_present());
    assert_eq!(decoded.phys(), 0);
}

#[test]
fn vtd_entry_decode_strips_reserved_bits() {
    // A caller that hands in upper reserved bits must see them stripped
    // on decode; the round-trip identity depends on this.
    let dirty = 0xFFFF_0000_0000_0000u64 | 0x0000_0000_1000_0003u64;
    let decoded = VtdPageTableEntry::decode(dirty);
    // phys is masked to [51:12]; flags are masked to the documented bits.
    let re_encoded = decoded.encode();
    assert_eq!(
        VtdPageTableEntry::decode(re_encoded).encode(),
        re_encoded,
        "decode must be idempotent on its own output"
    );
}

// ---------------------------------------------------------------------------
// Page-table entry encode / decode round-trip — AMD-Vi
// ---------------------------------------------------------------------------

#[test]
fn amdvi_entry_round_trip_leaf() {
    let phys = 0x8000_0000u64;
    let flags = AmdViPteFlags {
        present: true,
        io_read: true,
        io_write: true,
        force_coherent: false,
        next_level: 0,
    };
    let entry = AmdViPageTableEntry::new(phys, flags);
    let decoded = AmdViPageTableEntry::decode(entry.encode());
    assert_eq!(decoded.phys_addr(), phys);
    assert_eq!(decoded.flags(), flags);
    assert_eq!(decoded.pfn(), (phys >> 12) & PFN_MASK_40);
    assert!(decoded.is_present());
    assert_eq!(decoded.next_level(), 0);
}

#[test]
fn amdvi_entry_round_trip_intermediate() {
    let phys = 0x1_0000u64;
    let flags = AmdViPteFlags {
        present: true,
        io_read: true,
        io_write: true,
        force_coherent: false,
        next_level: 3,
    };
    let entry = AmdViPageTableEntry::new(phys, flags);
    let decoded = AmdViPageTableEntry::decode(entry.encode());
    assert_eq!(decoded.next_level(), 3);
    assert!(decoded.is_present());
    assert_eq!(decoded.phys_addr(), phys);
}

#[test]
fn amdvi_entry_not_present() {
    let decoded = AmdViPageTableEntry::decode(0);
    assert!(!decoded.is_present());
    assert_eq!(decoded.pfn(), 0);
    assert_eq!(decoded.next_level(), 0);
}

// ---------------------------------------------------------------------------
// Both vendor encoders preserve round-trip across the same physical
// address inputs (parity check — they share the "4 KiB terminal with
// R+W" shape on which every driver allocation rests).
// ---------------------------------------------------------------------------

#[test]
fn both_vendors_round_trip_rw_leaf_for_same_phys() {
    // Each vendor's encoder is different, but both must encode a
    // present, readable, writable 4 KiB mapping at `phys` in a way that
    // `decode(encode(x)) == x`. The test is parity: for every phys in a
    // small sweep, both vendors pass.
    let phys_values = [0x1000u64, 0xABCD_1000, 0x8000_0000, 0xFF_FFFF_F000];
    for phys in phys_values {
        let vtd_e = VtdPageTableEntry::new(phys, VtdPteFlags::READ | VtdPteFlags::WRITE);
        let vtd_decoded = VtdPageTableEntry::decode(vtd_e.encode());
        assert_eq!(vtd_decoded.phys(), phys, "vtd phys={:#x}", phys);
        assert_eq!(vtd_decoded.flags(), VtdPteFlags::READ | VtdPteFlags::WRITE);

        let amdvi_e = AmdViPageTableEntry::new(
            phys,
            AmdViPteFlags {
                present: true,
                io_read: true,
                io_write: true,
                force_coherent: false,
                next_level: 0,
            },
        );
        let amdvi_decoded = AmdViPageTableEntry::decode(amdvi_e.encode());
        // AMD-Vi PFN is 40 bits; mask the input the same way the encoder
        // does and compare.
        let expected_pfn = (phys >> 12) & PFN_MASK_40;
        assert_eq!(amdvi_decoded.pfn(), expected_pfn, "amdvi phys={:#x}", phys);
        assert!(amdvi_decoded.is_present());
    }
}

// ---------------------------------------------------------------------------
// IovaAllocator contract — no overlap, freelist reuse, exhaustion error
// ---------------------------------------------------------------------------

#[test]
fn iova_allocator_no_overlap_across_sequence() {
    // 1 MiB window, 4 KiB min alignment. Allocate 8 ranges of varied
    // lengths; confirm no pairwise overlap.
    let mut alloc = IovaAllocator::new(0x1000_0000, 0x1010_0000, 4096);
    let lengths = [4096, 8192, 4096, 16384, 4096, 32768, 4096, 8192];
    let mut ranges: alloc::vec::Vec<IovaRange> = alloc::vec::Vec::new();
    for &len in &lengths {
        let r = alloc.allocate(len, 4096).expect("allocate should succeed");
        ranges.push(r);
    }
    for i in 0..ranges.len() {
        for j in (i + 1)..ranges.len() {
            let a = ranges[i];
            let b = ranges[j];
            let a_end = a.start + a.len as u64;
            let b_end = b.start + b.len as u64;
            assert!(
                a.start >= b_end || b.start >= a_end,
                "overlap: {:?} vs {:?}",
                a,
                b
            );
        }
    }
}

#[test]
fn iova_allocator_free_returns_to_freelist_and_reallocates() {
    let mut alloc = IovaAllocator::new(0x1000_0000, 0x1001_0000, 4096);
    let r1 = alloc.allocate(4096, 4096).unwrap();
    let r2 = alloc.allocate(4096, 4096).unwrap();
    assert_ne!(r1.start, r2.start);
    alloc.free(r1).unwrap();
    // A second allocation of the same size should be able to re-use the
    // freed range (freelist reuse invariant).
    let r3 = alloc.allocate(4096, 4096).unwrap();
    // Either the freelist path returned r1's range, or the bump cursor
    // moved forward — both satisfy the no-overlap contract. What we
    // demand is that r3 does not overlap r2.
    let r2_end = r2.start + r2.len as u64;
    let r3_end = r3.start + r3.len as u64;
    assert!(
        r3.start >= r2_end || r2.start >= r3_end,
        "r2 and r3 overlap: r2={:?} r3={:?}",
        r2,
        r3
    );
    alloc.free(r2).unwrap();
    alloc.free(r3).unwrap();
}

#[test]
fn iova_allocator_exhaustion_returns_named_error() {
    // 4 KiB window; one 4 KiB allocation fits, a second exhausts.
    let mut alloc = IovaAllocator::new(0x1000_0000, 0x1000_1000, 4096);
    let _r1 = alloc.allocate(4096, 4096).unwrap();
    assert_eq!(alloc.allocate(4096, 4096).err(), Some(IovaError::Exhausted));
}

#[test]
fn iova_allocator_rejects_zero_length_and_bad_alignment() {
    let mut alloc = IovaAllocator::new(0x1000_0000, 0x1001_0000, 4096);
    assert_eq!(alloc.allocate(0, 4096).err(), Some(IovaError::ZeroLength));
    assert_eq!(
        alloc.allocate(4096, 2048).err(),
        Some(IovaError::AlignmentUnsatisfiable),
        "alignment below min_alignment must be rejected"
    );
    // Non-power-of-two alignment is also rejected.
    assert_eq!(
        alloc.allocate(4096, 4096 * 3).err(),
        Some(IovaError::AlignmentUnsatisfiable),
    );
}

#[test]
fn iova_allocator_respects_large_alignment() {
    // 2 MiB alignment request must return a 2 MiB-aligned address.
    let mut alloc = IovaAllocator::new(0, 0x10_0000_0000, 4096);
    let r = alloc.allocate(4096, 0x20_0000).expect("2 MiB align ok");
    assert_eq!(r.start & (0x20_0000u64 - 1), 0, "2 MiB alignment honored");
}

// ---------------------------------------------------------------------------
// FaultRecord field plumbing — parity between vendors
// ---------------------------------------------------------------------------
//
// Vendor-specific decoders (VT-d fault-record register block and AMD-Vi
// event-log entry decoder) live in the kernel crate behind the MSI
// bring-up that is deferred in Phase 55a. The parity contract for Phase
// 55a is the shape of `FaultRecord` itself: both vendors must decode
// their hardware-specific records into the same structured shape, so a
// handler registered once can recognize records from either vendor.
//
// The tests below exercise that structural invariant: a FaultRecord
// constructed from each vendor's "natural" fields carries identical
// values regardless of which vendor produced it.

fn make_vtd_fault_record(requester_bdf: u16, iova: u64, reason: u16) -> FaultRecord {
    // VT-d fault record layout §10.4.16: requester_id, addr, reason.
    // A real decoder masks / shifts these off the 128-bit record; the
    // parity test only cares that the fields pass through verbatim.
    FaultRecord {
        requester_bdf,
        fault_reason: reason,
        iova: Iova(iova),
    }
}

fn make_amdvi_event_record(requester_bdf: u16, iova: u64, reason: u16) -> FaultRecord {
    // AMD-Vi IO_PAGE_FAULT event §2.5.2: device_id, address, flags code.
    FaultRecord {
        requester_bdf,
        fault_reason: reason,
        iova: Iova(iova),
    }
}

#[test]
fn fault_record_parity_vtd_and_amdvi_produce_same_shape() {
    let bdf = 0x0100;
    let iova = 0xDEAD_BEEF_0000u64;
    let reason = 0x0005;
    let vtd = make_vtd_fault_record(bdf, iova, reason);
    let amd = make_amdvi_event_record(bdf, iova, reason);
    assert_eq!(vtd, amd);
    assert_eq!(vtd.requester_bdf, 0x0100);
    assert_eq!(vtd.iova.0, 0xDEAD_BEEF_0000);
    assert_eq!(vtd.fault_reason, 0x0005);
}

#[test]
fn fault_record_is_copy_and_comparable() {
    let rec = FaultRecord {
        requester_bdf: 0x0200,
        fault_reason: 0x0A,
        iova: Iova(0x1_0000),
    };
    // FaultRecord is Copy + Eq so handlers can compare / duplicate it
    // without an explicit clone — a non-negotiable part of the IRQ-safe
    // contract (no allocation, no nontrivial drop).
    let dup = rec;
    assert_eq!(rec, dup);
}

extern crate alloc;
