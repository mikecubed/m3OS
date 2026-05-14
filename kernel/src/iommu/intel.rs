//! Intel VT-d IOMMU vendor driver — Phase 55a Track C.
//!
//! Implements [`kernel_core::iommu::contract::IommuUnit`] over an Intel VT-d
//! remapping unit. One [`VtdUnit`] instance wraps one DRHD register window
//! (4 KiB, typically at 0xFED9_0000 on QEMU q35). The unit owns:
//!
//! * The register MMIO view (kernel-virtual pointer via the phys-offset
//!   window — VT-d register pages are already identity-mapped at boot).
//! * The 4 KiB root table (256 entries × 16 bytes).
//! * Lazily-allocated context tables (one per populated bus).
//! * A small pool of live [`VtdDomainState`] records — Phase 55a wires one
//!   device per domain, so the pool is expected to stay at single digits.
//!
//! # Lock ordering (Track C contribution)
//!
//! Mirrors the authoritative order documented in
//! [`kernel_core::iommu::contract`]:
//!
//! ```text
//! domain lock  →  unit lock  →  buddy-allocator lock
//! ```
//!
//! [`VtdUnit`] itself lives behind a `Mutex` at the call-site level; the
//! `&mut self` receiver on every trait method is the lock boundary. When a
//! `map` / `unmap` call allocates a page-table page, the buddy allocator
//! lock is taken *below* the unit's `&mut self`. Domain-side locks (if any
//! future driver adds them) are always acquired *before* reaching into
//! `VtdUnit`.
//!
//! # Scalable mode
//!
//! Phase 55a deliberately leaves scalable-mode translation disabled. The
//! unit reports `scalable_mode = false` in its [`IommuCapabilities`]
//! regardless of the actual `ECAP.SMTS` bit and drives the hardware in
//! legacy second-level mode.
//!
//! # Interrupt remapping
//!
//! Also deliberately disabled for Phase 55a: the unit never writes `IRTA`
//! and never sets `GCMD.IRE`. Reported in capabilities but not engaged.

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU8, Ordering};

use kernel_core::iommu::contract::{
    DmaDomain, DomainError, DomainId, FaultHandlerFn, FaultRecord, IommuCapabilities, IommuError,
    IommuUnit, Iova, MapFlags, PhysAddr,
};
use kernel_core::iommu::iova::IovaAllocator;
use kernel_core::iommu::vtd_page_table::{
    PAGE_SIZE, VtdPageTableEntry, VtdPteFlags, level_index_n,
};
use kernel_core::iommu::vtd_regs::{
    CCMD_CIRG_GLOBAL, CCMD_ICC, GCMD_QIE_BIT, GCMD_SRTP, GCMD_TE, GSTS_QIES_BIT, GSTS_RTPS,
    GSTS_TES, IOTLB_IIRG_GLOBAL, IOTLB_IVT, IOTLB_REG, QiDescriptor, VtdCap, VtdEcap, VtdRegs,
    VtdVersion, encode_iqa, encode_rtaddr_legacy,
};

use crate::arch::x86_64::apic;
use crate::arch::x86_64::interrupts::{
    DEVICE_IRQ_VECTOR_BASE, DEVICE_IRQ_VECTOR_COUNT, DeviceIrqEntry, DeviceIrqKind,
    register_device_irq,
};
use crate::mm::frame_allocator;
use crate::mm::phys_offset;

use super::fault as iommu_fault;

/// Phase 67 Track D.2 — `true` when the CPU's `CR4.LA57` bit is set.
///
/// When LA57 is enabled the host paging hierarchy uses 5 levels (the
/// extra PML5 sits above the legacy PML4). VT-d scalable-mode IOMMU
/// page tables must match — running the IOMMU in 4-level mode against
/// a 5-level CPU produces an address-width mismatch faulting on every
/// translation. Phase 67 reads the bit live so a kernel that enables
/// LA57 after `iommu::init` (none today, but a future power-state
/// change might) still routes through the right page-table depth on
/// the next `create_domain`.
fn cr4_la57_enabled() -> bool {
    use x86_64::registers::control::{Cr4, Cr4Flags};
    Cr4::read().contains(Cr4Flags::L5_PAGING)
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Address-width used for legacy-mode 4-level SL tables. VT-d 3.3 §9.2.
const VTD_ADDRESS_WIDTH_BITS: u8 = 48;

/// Cap on page-table pages any single domain may consume. Prevents a
/// runaway caller from exhausting kernel frames.
const DOMAIN_PT_PAGE_CAP: u32 = 4096; // 16 MiB of tables per domain — generous.

/// Busy-loop bound on a GSTS ack poll. One spin = one `pause`; at a
/// typical clock this bounds us to a few milliseconds of wall time even
/// on a wedged unit.
const GSTS_POLL_LIMIT: u32 = 2_000_000;

/// Maximum fault records drained per IRQ invocation.
const FAULT_DRAIN_CAP: usize = 32;

// ---------------------------------------------------------------------------
// Root / context table layouts (documented inline — we access the backing
// page by byte offset rather than via a strong `#[repr(C)]` type, so there
// is no standalone type to keep in sync with the spec).
//
// * Root table — 256 × 16-byte entries at `self.root_phys`. Low 64 bits of
//   each entry hold `present | reserved | context-table-ptr` (pfn-aligned).
// * Context table — 256 × 16-byte entries. Low 64 bits hold
//   `present | fault-disable | translation-type(2) | reserved(8) |
//   SL-PT-ptr`; high 64 bits hold `AW(3) | reserved | domain-id(16)`.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Per-domain state
// ---------------------------------------------------------------------------

/// Book-keeping for one live domain. Stored in `VtdUnit::domains`.
#[allow(dead_code)]
struct VtdDomainState {
    /// The integer handle handed back to callers.
    id: DomainId,
    /// The 16-bit domain-id field embedded in the context-table entry.
    vtd_domain_id: u16,
    /// Physical address of the root page-table page (the top-level
    /// page; either PML4 for 4-level or PML5 for 5-level scalable
    /// mode). The legacy field name is kept for source-compat with
    /// pre-Phase-67 callers — semantically this is "the table root".
    pml4_phys: u64,
    /// IOVA allocator for this domain. Window is [0, 2^48).
    iova: IovaAllocator,
    /// Number of page-table pages (excluding the PML4) held live.
    pt_pages: u32,
    /// PCI BDF this domain is bound to. Phase 55a: one BDF per domain.
    /// Stored as `bus << 8 | device << 3 | function`.
    bound_bdf: u16,
    /// Phase 67 Track D.2 — number of page-table levels in this
    /// domain's SL hierarchy: 4 for legacy 4-level (`scalable_mode ==
    /// false` or `CR4.LA57 == 0`) or 5 for scalable-mode + LA57. Set
    /// at `create_domain` time from [`VtdUnit::init_page_tables`].
    levels: u8,
}

// ---------------------------------------------------------------------------
// VtdUnit
// ---------------------------------------------------------------------------

/// One Intel VT-d remapping unit. Construct via [`VtdUnit::new`] from the
/// register base learned through the ACPI DMAR table.
pub struct VtdUnit {
    /// Zero-based index into the unit descriptor vector. Kept so
    /// `create_domain` can stamp the returned `DmaDomain` with the right
    /// owning-unit reference.
    unit_index: usize,
    /// Register window (kernel-virtual byte pointer).
    regs_virt: *mut u8,
    /// Cached capability snapshots. Populated in `new`.
    cap: VtdCap,
    ecap: VtdEcap,
    /// Advertised capability profile — derived from cap/ecap.
    capabilities: IommuCapabilities,
    /// Address of the root table in kernel memory. Allocated lazily on
    /// first `bring_up` from the buddy allocator.
    root_phys: Option<u64>,
    /// Map from bus number to owning context-table phys address.
    context_tables: [Option<u64>; 256],
    /// Live domain state vector. Indexed lookup by `DomainId`.
    domains: Vec<VtdDomainState>,
    /// Next DomainId to allocate.
    next_domain_id: u32,
    /// Next VT-d domain-id field value (16-bit).
    next_vtd_domain_id: u16,
    /// `true` after `bring_up` succeeded.
    up: bool,
    /// Phase 67 Track C.1 — physical address of the 4 KiB invalidation
    /// queue ring. `None` until `bring_up` allocates it. When `Some`,
    /// the unit submits queued-invalidation descriptors instead of the
    /// register-path CCMD/IOTLB sequence.
    iq_ring_phys: Option<u64>,
    /// Software-managed producer cursor (in descriptors). Advanced
    /// after every queued descriptor write.
    iq_tail: u64,
    /// Phase 67 Track C.2 — physical address of the invalidation-wait
    /// status word. Aligned so the IWAIT descriptor's status-address
    /// field can encode it directly.
    iq_status_phys: Option<u64>,
    /// Monotonically advancing marker stamped into each IWAIT
    /// descriptor. The poller watches the status word for this marker
    /// to know the queue has drained past the wait descriptor.
    iq_status_marker: u32,
}

// SAFETY: VtdUnit is single-owner; the trait takes `&mut self` so the
// Rust aliasing rules prevent concurrent access to the MMIO pointer.
// Callers put VtdUnit behind a higher-level Mutex as part of the lock
// ordering contract.
unsafe impl Send for VtdUnit {}

impl VtdUnit {
    /// Construct a unit around the DMAR-published `register_base`
    /// physical address. Reads CAP/ECAP/VER so the returned capability
    /// snapshot is ready before `bring_up`.
    #[allow(dead_code)]
    pub fn new(unit_index: usize, register_base: PhysAddr) -> Self {
        let virt = (phys_offset() + register_base.0) as *mut u8;
        // SAFETY: VT-d register window is 4 KiB and falls into the
        // kernel's phys-offset identity map. Reads of CAP/ECAP/VER are
        // safe on a well-formed DRHD entry; a malformed one surfaces as
        // zero bits in CAP and the caller gets a degenerate capability
        // profile rather than undefined behaviour.
        let cap = VtdCap(unsafe { read_volatile(virt.add(VtdRegs::CAP) as *const u64) });
        let ecap = VtdEcap(unsafe { read_volatile(virt.add(VtdRegs::ECAP) as *const u64) });
        let capabilities = IommuCapabilities {
            supported_page_sizes: cap.supported_page_sizes_mask(),
            address_width_bits: cap.address_width_bits(),
            interrupt_remapping: ecap.interrupt_remapping(),
            queued_invalidation: ecap.queued_invalidation(),
            // Phase 67 Track D.1 — surface the runtime ECAP.SMTS bit
            // instead of the previously hardcoded `false`. Callers
            // querying `IommuCapabilities::scalable_mode` now observe
            // whether the underlying hardware really advertises
            // scalable-mode translation support.
            scalable_mode: ecap.scalable_mode(),
        };
        Self {
            unit_index,
            regs_virt: virt,
            cap,
            ecap,
            capabilities,
            root_phys: None,
            context_tables: [None; 256],
            domains: Vec::new(),
            next_domain_id: 1,
            next_vtd_domain_id: 1,
            up: false,
            iq_ring_phys: None,
            iq_tail: 0,
            iq_status_phys: None,
            iq_status_marker: 0,
        }
    }

    /// Decoded version register.
    #[allow(dead_code)]
    pub fn version(&self) -> VtdVersion {
        let raw = self.read_u32(VtdRegs::VER);
        VtdVersion::from_raw(raw)
    }

    /// Raw capability register snapshot.
    #[allow(dead_code)]
    pub fn cap(&self) -> VtdCap {
        self.cap
    }

    /// Raw extended-capability register snapshot.
    #[allow(dead_code)]
    pub fn ecap(&self) -> VtdEcap {
        self.ecap
    }

    /// Phase 67 Track D.1 — runtime scalable-mode capability check.
    ///
    /// Reads the live `ECAP.SMTS` bit through the cached snapshot
    /// captured at `new`. Callers that previously consulted the
    /// hardcoded `false` constant should switch to this accessor so
    /// scalable mode actually engages on capable hardware.
    pub fn supports_scalable_mode(&self) -> bool {
        self.ecap.scalable_mode()
    }

    /// Phase 67 Track D.2 — bring up page tables for a fresh domain.
    /// Returns the chosen level count (4 or 5) given the unit's
    /// scalable-mode capability and the CPU's `CR4.LA57` state.
    ///
    /// A scalable-mode-capable unit on a CPU that has LA57 enabled
    /// uses 5-level tables (`PML5 → PML4 → PDPT → PD → PT`); every
    /// other configuration stays on the legacy 4-level tree
    /// (`PML4 → PDPT → PD → PT`) Phase 55a shipped.
    pub fn init_page_tables(&self) -> u8 {
        if self.supports_scalable_mode() && cr4_la57_enabled() {
            5
        } else {
            4
        }
    }

    // ---- MMIO primitives ------------------------------------------------

    #[inline]
    fn read_u32(&self, offset: usize) -> u32 {
        // SAFETY: offset is a compile-time constant into the 4 KiB
        // register window established in `new`.
        unsafe { read_volatile(self.regs_virt.add(offset) as *const u32) }
    }

    #[inline]
    fn write_u32(&self, offset: usize, value: u32) {
        // SAFETY: same as read_u32.
        unsafe { write_volatile(self.regs_virt.add(offset) as *mut u32, value) }
    }

    #[inline]
    fn read_u64(&self, offset: usize) -> u64 {
        // SAFETY: same as read_u32.
        unsafe { read_volatile(self.regs_virt.add(offset) as *const u64) }
    }

    #[inline]
    fn write_u64(&self, offset: usize, value: u64) {
        // SAFETY: same as read_u32.
        unsafe { write_volatile(self.regs_virt.add(offset) as *mut u64, value) }
    }

    /// Poll GSTS until `bit` becomes set or the bound expires.
    fn wait_gsts_bit(&self, bit: u32) -> Result<(), IommuError> {
        // Phase 57e Track B.2: under PREEMPT_FULL, a kernel-mode preemption
        // mid-spin would migrate this task to another core, leaving the GSTS
        // observation pending on a different MMIO mapping in the worst case.
        // VT-d configuration runs once at unit bring-up but the bit-poll
        // pattern is reused for runtime invalidations, so the wrapper applies
        // unconditionally.
        crate::task::scheduler::preempt_disable();
        let result = (|| {
            for _ in 0..GSTS_POLL_LIMIT {
                if (self.read_u32(VtdRegs::GSTS) & bit) != 0 {
                    return Ok(());
                }
                // HW-bounded: VT-d hardware updates GSTS within ~1 µs of receiving the
                // write (Intel VT-d Architecture Specification §11.4.4, 'Global Status
                // Register').  No IRQ-on-completion mechanism exists for GSTS updates.
                core::hint::spin_loop();
            }
            Err(IommuError::HardwareFault)
        })();
        crate::task::scheduler::preempt_enable();
        result
    }

    // ---- Table-page allocation -----------------------------------------

    /// Allocate and zero a 4 KiB table page. Returns the phys address.
    fn alloc_table_page() -> Option<u64> {
        let frame = frame_allocator::allocate_contiguous_zeroed(0)?;
        Some(frame.start_address().as_u64())
    }

    /// Write a `u64` at a byte offset inside a table page identified by
    /// its phys address. Used for entries in the root, context, and SL
    /// page tables.
    fn write_table_entry(table_phys: u64, offset_bytes: usize, value: u64) {
        let addr = phys_offset() + table_phys + offset_bytes as u64;
        // SAFETY: table_phys is a kernel-owned 4 KiB frame; offset_bytes
        // is bounded by the table size (256 × 16 or 512 × 8), both
        // within a page.
        unsafe { write_volatile(addr as *mut u64, value) }
    }

    /// Read a `u64` at a byte offset inside a table page.
    fn read_table_entry(table_phys: u64, offset_bytes: usize) -> u64 {
        let addr = phys_offset() + table_phys + offset_bytes as u64;
        // SAFETY: same as write_table_entry.
        unsafe { read_volatile(addr as *const u64) }
    }

    /// Zero a full 4 KiB page at `phys`.
    #[allow(dead_code)]
    fn zero_page(phys: u64) {
        let virt = (phys_offset() + phys) as *mut u8;
        // SAFETY: caller freshly allocated the page; we own it.
        unsafe { core::ptr::write_bytes(virt, 0, PAGE_SIZE as usize) }
    }

    // ---- Root / context-table installation -----------------------------

    /// Ensure a root table exists. Idempotent. Returns the root phys.
    fn ensure_root_table(&mut self) -> Result<u64, IommuError> {
        if let Some(p) = self.root_phys {
            return Ok(p);
        }
        let phys = Self::alloc_table_page().ok_or(IommuError::HardwareFault)?;
        // Frame allocator's zeroed variant handles the scrub.
        self.root_phys = Some(phys);
        Ok(phys)
    }

    /// Ensure a context table exists for `bus`. Returns the context
    /// phys. Installs the root-table entry on first allocation.
    fn ensure_context_table(&mut self, bus: u8) -> Result<u64, IommuError> {
        if let Some(p) = self.context_tables[bus as usize] {
            return Ok(p);
        }
        let ctx_phys = Self::alloc_table_page().ok_or(IommuError::HardwareFault)?;
        self.context_tables[bus as usize] = Some(ctx_phys);

        // Install the root-table entry: present | ctx_phys (pfn-aligned).
        let root_phys = self.ensure_root_table()?;
        let entry_offset = (bus as usize) * 16;
        // Low dword: present bit 0, context-table-ptr in [63:12].
        let low = (ctx_phys & !0xFFFu64) | 0x1;
        Self::write_table_entry(root_phys, entry_offset, low);
        // High dword reserved — leave zero.
        Ok(ctx_phys)
    }

    /// Install a context-table entry binding device `bdf_low = (device
    /// << 3) | function` on `bus` to the second-level root `sl_root_phys`
    /// with the given 16-bit `vtd_domain_id`.
    fn install_context_entry(
        &mut self,
        bus: u8,
        dev_fn: u8,
        sl_root_phys: u64,
        vtd_domain_id: u16,
    ) -> Result<(), IommuError> {
        let ctx_phys = self.ensure_context_table(bus)?;
        let entry_offset = (dev_fn as usize) * 16;

        // Low 64 bits:
        //   bit 0    : present
        //   bit 1    : fault-processing disable (0 → faults enabled)
        //   bits 3:2 : translation type (00 = legacy multi-level / second-level)
        //   bits 11:4: reserved
        //   bits 63:12 : second-level page-table pointer (pfn-aligned)
        let low = sl_root_phys & !0xFFFu64;
        // High 64 bits:
        //   bits 2:0   : address-width (48-bit = value 2, per §9.3)
        //   bits 15:8  : reserved
        //   bits 23:8  : domain-id
        // VT-d §9.3 encoding: AW field value → width.
        //   0 -> 30b, 1 -> 39b, 2 -> 48b, 3 -> 57b, 4 -> 64b.
        let aw_code: u64 = match VTD_ADDRESS_WIDTH_BITS {
            48 => 2,
            39 => 1,
            _ => 2,
        };
        let high = aw_code | ((vtd_domain_id as u64) << 8);

        Self::write_table_entry(ctx_phys, entry_offset + 8, high);
        Self::write_table_entry(ctx_phys, entry_offset, low);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Self::write_table_entry(ctx_phys, entry_offset, low | 0x1);
        Ok(())
    }

    // ---- Invalidation --------------------------------------------------

    /// Issue a global context-cache invalidation via the register path.
    fn invalidate_context_cache_global(&self) {
        self.write_u64(VtdRegs::CCMD, CCMD_ICC | CCMD_CIRG_GLOBAL);
        // Wait for ICC bit (63) to clear.
        // Phase 57e Track B.2: see `wait_gsts_bit` for the migration-safety
        // rationale.  Invalidations may run from any task context (e.g. when
        // a userspace driver re-binds a device), so the wrapper is required.
        crate::task::scheduler::preempt_disable();
        for _ in 0..GSTS_POLL_LIMIT {
            if (self.read_u64(VtdRegs::CCMD) & CCMD_ICC) == 0 {
                crate::task::scheduler::preempt_enable();
                return;
            }
            // HW-bounded: VT-d hardware clears ICC (bit 63 of CCMD_REG) after
            // context-cache invalidation completes (Intel VT-d spec §11.4.4).
            core::hint::spin_loop();
        }
        crate::task::scheduler::preempt_enable();
    }

    /// Issue a global IOTLB invalidation via the register path.
    fn invalidate_iotlb_global(&self) {
        // Compute IOTLB block offset from ECAP.IRO. Block layout inside
        // the unit: IVA at iotlb_base + 0x00, IOTLB_REG at
        // iotlb_base + 0x08.
        let iotlb_base = (self.ecap.iro_16byte_units() as usize) * 16;
        if iotlb_base == 0 {
            // Unit without IRO — nothing we can flush through the
            // register path; rely on global re-enable semantics.
            return;
        }
        let cmd_offset = iotlb_base + IOTLB_REG;
        let value = IOTLB_IVT | IOTLB_IIRG_GLOBAL;
        self.write_u64(cmd_offset, value);
        // Phase 57e Track B.2: same rationale as `invalidate_context_cache_global`.
        crate::task::scheduler::preempt_disable();
        for _ in 0..GSTS_POLL_LIMIT {
            if (self.read_u64(cmd_offset) & IOTLB_IVT) == 0 {
                crate::task::scheduler::preempt_enable();
                return;
            }
            // HW-bounded: VT-d hardware clears IVT (bit 63 of IOTLB_REG) after IOTLB
            // invalidation completes (Intel VT-d spec §11.4.4).
            core::hint::spin_loop();
        }
        crate::task::scheduler::preempt_enable();
    }

    // ---- Phase 67 Track C — queued invalidation engine ----------------

    /// Bound on a poll for IQH (head register) to reach IQT (tail).
    /// Same shape as `GSTS_POLL_LIMIT`; sized to bound wall time even
    /// on a wedged unit.
    const IQ_POLL_LIMIT: u32 = 2_000_000;

    /// Number of 16-byte descriptors in the QS=0 ring. VT-d 3.3
    /// §10.4.21: QS=0 maps to 256 entries (4 KiB total).
    const IQ_DESC_COUNT: u64 = 256;

    /// Allocate the invalidation-queue ring + status word and program
    /// the IQA / IQH / IQT registers so subsequent flush calls can
    /// submit descriptors. Idempotent: returns immediately if the
    /// queue was already set up.
    ///
    /// Phase 67 Track C.1 acceptance:
    /// - Allocates a 4 KiB ring page via the frame allocator.
    /// - Writes IQA with QS=0 (256 descriptors).
    /// - Verifies the IQA write by reading it back.
    fn bring_up_invalidation_queue(&mut self) -> Result<(), IommuError> {
        if self.iq_ring_phys.is_some() {
            return Ok(());
        }
        // Caller must have set up the root table before enabling QI —
        // the queue handler refuses to write descriptors until the
        // unit reports translation is configured. Phase 67 lifts the
        // queue right after `ensure_root_table` so the order is
        // satisfied at the call site.
        let ring_phys = Self::alloc_table_page().ok_or(IommuError::HardwareFault)?;
        let status_phys = Self::alloc_table_page().ok_or(IommuError::HardwareFault)?;

        // Zero IQH / IQT before pointing IQA at the new ring so
        // hardware does not race against stale head/tail values.
        self.write_u64(VtdRegs::IQH, 0);
        self.write_u64(VtdRegs::IQT, 0);
        let iqa = encode_iqa(ring_phys, 0);
        self.write_u64(VtdRegs::IQA, iqa);
        // Read-back verification per acceptance.
        let observed = self.read_u64(VtdRegs::IQA);
        if (observed & !0xFFFu64) != (ring_phys & !0xFFFu64) {
            log::warn!(
                "[vtd] unit[{}] IQA read-back mismatch: wrote={:#x} read={:#x}",
                self.unit_index,
                iqa,
                observed,
            );
            return Err(IommuError::HardwareFault);
        }

        // Enable queued-invalidation in GCMD; wait for GSTS.QIES.
        self.write_u32(VtdRegs::GCMD, GCMD_QIE_BIT);
        self.wait_gsts_bit(GSTS_QIES_BIT)?;

        self.iq_ring_phys = Some(ring_phys);
        self.iq_status_phys = Some(status_phys);
        self.iq_tail = 0;
        self.iq_status_marker = 0;
        log::info!(
            "[iommu] vtd unit[{}] queued invalidation enabled: ring={:#x} status={:#x}",
            self.unit_index,
            ring_phys,
            status_phys,
        );
        Ok(())
    }

    /// Submit `desc` to the queue ring at the current tail and advance
    /// IQT. Returns the descriptor slot the entry was written at so
    /// callers can correlate completions if needed.
    ///
    /// Pre-condition: [`bring_up_invalidation_queue`] succeeded —
    /// callers that hit this with `iq_ring_phys == None` get a
    /// `NotAvailable` error rather than a panic.
    ///
    /// Pre-condition (queue capacity): callers must drive the queue to
    /// completion (via [`Self::submit_iwait_and_poll`]) before
    /// submitting more than `IQ_DESC_COUNT - 1` descriptors. This
    /// helper does *not* check that the slot at `iq_tail` has been
    /// observed by hardware; on overflow it would write a fresh
    /// descriptor over a still-pending one and the wrap-around would
    /// leave `IQT == IQH`, which the IOMMU treats as "queue empty".
    /// Phase 67's only caller (`flush_*` → submit two descriptors +
    /// `submit_iwait_and_poll`) honours this trivially under the
    /// `&mut self` borrow boundary; future bulk invalidators must
    /// either call back into the IWAIT poll between batches or learn
    /// to read `IQH` and reserve a sentinel slot.
    fn submit_qi_descriptor(&mut self, desc: QiDescriptor) -> Result<u64, IommuError> {
        let ring = self.iq_ring_phys.ok_or(IommuError::NotAvailable)?;
        let slot = self.iq_tail;
        let slot_phys = ring + slot * 16;
        // SAFETY: ring is a 4 KiB frame; slot < IQ_DESC_COUNT.
        let virt = (phys_offset() + slot_phys) as *mut u64;
        unsafe {
            write_volatile(virt, desc.low);
            write_volatile(virt.add(1), desc.high);
        }
        // Advance tail with wrap-around.
        self.iq_tail = (self.iq_tail + 1) % Self::IQ_DESC_COUNT;
        self.write_u64(VtdRegs::IQT, self.iq_tail * 16);
        Ok(slot)
    }

    /// Submit an IWAIT descriptor and poll the status word until the
    /// fresh marker shows up. Bounded by [`IQ_POLL_LIMIT`]; on timeout
    /// returns [`IommuError::FlushTimeout`].
    ///
    /// Each call advances the per-unit marker so concurrent flushes
    /// would each see their own value (Phase 67 has at most one
    /// in-flight flush per unit because `&mut self` is the lock
    /// boundary, but the marker advance is cheap insurance).
    ///
    /// Marker `0` is reserved as a sentinel: the routine zeroes the
    /// status word before submitting and polls for an observed match
    /// against the fresh marker. If the marker were allowed to be
    /// `0`, the poll would observe a match against the freshly-zeroed
    /// status word *before* hardware wrote the IWAIT completion and
    /// return prematurely. We therefore skip `0` on wrap-around.
    fn submit_iwait_and_poll(&mut self) -> Result<(), IommuError> {
        let status_phys = self.iq_status_phys.ok_or(IommuError::NotAvailable)?;
        self.iq_status_marker = self.iq_status_marker.wrapping_add(1);
        if self.iq_status_marker == 0 {
            self.iq_status_marker = 1;
        }
        let marker = self.iq_status_marker;
        // Zero the status word before submitting so we observe the new
        // marker arriving.
        let status_virt = (phys_offset() + status_phys) as *mut u32;
        // SAFETY: status_phys was allocated via the frame allocator
        // and is owned solely by this unit.
        unsafe {
            write_volatile(status_virt, 0);
        }
        let desc = QiDescriptor::iwait(status_phys, marker);
        self.submit_qi_descriptor(desc)?;
        // Poll the status word for the marker. Same preempt-discipline
        // as the legacy GSTS poll.
        crate::task::scheduler::preempt_disable();
        let result = (|| {
            for _ in 0..Self::IQ_POLL_LIMIT {
                let observed = unsafe { read_volatile(status_virt as *const u32) };
                if observed == marker {
                    return Ok(());
                }
                core::hint::spin_loop();
            }
            Err(IommuError::FlushTimeout)
        })();
        crate::task::scheduler::preempt_enable();
        result
    }

    /// Look up the 16-bit VT-d domain id for a kernel-side
    /// [`DomainId`]. Returns `IommuError::Invalid` for unknown ids.
    fn vtd_did_for(&self, domain: DomainId) -> Result<u16, IommuError> {
        self.domains
            .iter()
            .find(|d| d.id == domain)
            .map(|d| d.vtd_domain_id)
            .ok_or(IommuError::Invalid)
    }

    /// Phase 67 Track C.2 — queued context-cache + IOTLB flush for a
    /// single domain. Submits one CC_INV (domain-granularity), one
    /// IOTLB_INV (domain-granularity), then an IWAIT and polls.
    pub fn flush_domain(&mut self, domain: DomainId) -> Result<(), IommuError> {
        let did = self.vtd_did_for(domain)?;
        if self.iq_ring_phys.is_none() {
            // Queue not up — caller is responsible for register-path
            // fallback. Returning Ok here would silently drop the
            // flush.
            return Err(IommuError::NotAvailable);
        }
        // Domain-granular CIRG = 0b10 / IIRG = 0b10 per VT-d 3.3
        // §10.4.27 (CCMD) and §10.4.31 (IOTLB).
        self.submit_qi_descriptor(QiDescriptor::cc_inv(0b10, did))?;
        self.submit_qi_descriptor(QiDescriptor::iotlb_inv(0b10, did))?;
        self.submit_iwait_and_poll()
    }

    /// Phase 67 Track C.2 — queued IOTLB-only flush for a domain.
    pub fn flush_iotlb(&mut self, domain: DomainId) -> Result<(), IommuError> {
        let did = self.vtd_did_for(domain)?;
        if self.iq_ring_phys.is_none() {
            return Err(IommuError::NotAvailable);
        }
        self.submit_qi_descriptor(QiDescriptor::iotlb_inv(0b10, did))?;
        self.submit_iwait_and_poll()
    }

    /// Phase 67 Track C.2 — queued context-cache flush for a domain.
    pub fn flush_context(&mut self, domain: DomainId) -> Result<(), IommuError> {
        let did = self.vtd_did_for(domain)?;
        if self.iq_ring_phys.is_none() {
            return Err(IommuError::NotAvailable);
        }
        self.submit_qi_descriptor(QiDescriptor::cc_inv(0b10, did))?;
        self.submit_iwait_and_poll()
    }

    // ---- Page-table walk/install ---------------------------------------

    /// Translate [`MapFlags`] into SL-PTE bits.
    fn encode_pte_flags(flags: MapFlags) -> VtdPteFlags {
        let mut pte = VtdPteFlags::NONE;
        if flags.contains(MapFlags::READ) {
            pte = pte | VtdPteFlags::READ;
        }
        if flags.contains(MapFlags::WRITE) {
            pte = pte | VtdPteFlags::WRITE;
        }
        pte
    }

    /// Walk the SL table rooted at `root_phys` down to the leaf for
    /// `iova`, allocating intermediate levels as needed. Returns the
    /// leaf-entry byte offset (`table_phys`, `entry_offset_bytes`) and
    /// a count of newly-allocated intermediate pages (added to the
    /// caller's pt_pages counter).
    ///
    /// Phase 67 Track D.2: `levels` selects between the legacy 4-level
    /// hierarchy (`PML4 → PDPT → PD → PT`) and the scalable-mode
    /// 5-level hierarchy (`PML5 → PML4 → PDPT → PD → PT`). Domains
    /// stamp their chosen depth at create-time and pass it through on
    /// every walk.
    fn walk_and_install_intermediates(
        root_phys: u64,
        iova: u64,
        levels: usize,
    ) -> Result<(u64, usize, u32), IommuError> {
        let mut table_phys = root_phys;
        let mut allocated = 0u32;
        for level in 0..(levels - 1) {
            let idx = level_index_n(iova, level, levels);
            let entry_offset = idx * 8;
            let raw = Self::read_table_entry(table_phys, entry_offset);
            let entry = VtdPageTableEntry::decode(raw);
            let next_phys = if entry.is_present() {
                entry.phys()
            } else {
                let new = Self::alloc_table_page().ok_or(IommuError::HardwareFault)?;
                allocated += 1;
                // Intermediate tables are always R|W to permit descent;
                // device permissions are decided at the leaf.
                let pte = VtdPageTableEntry::new(new, VtdPteFlags::READ | VtdPteFlags::WRITE);
                Self::write_table_entry(table_phys, entry_offset, pte.encode());
                new
            };
            table_phys = next_phys;
        }
        // Leaf table.
        let idx = level_index_n(iova, levels - 1, levels);
        let entry_offset = idx * 8;
        Ok((table_phys, entry_offset, allocated))
    }

    /// Walk the SL table looking up the leaf entry for `iova` *without*
    /// allocating intermediates. Used by unmap. See
    /// [`walk_and_install_intermediates`] for the `levels` contract.
    fn walk_read_only(root_phys: u64, iova: u64, levels: usize) -> Option<(u64, usize)> {
        let mut table_phys = root_phys;
        for level in 0..(levels - 1) {
            let idx = level_index_n(iova, level, levels);
            let entry_offset = idx * 8;
            let raw = Self::read_table_entry(table_phys, entry_offset);
            let entry = VtdPageTableEntry::decode(raw);
            if !entry.is_present() {
                return None;
            }
            table_phys = entry.phys();
        }
        let idx = level_index_n(iova, levels - 1, levels);
        Some((table_phys, idx * 8))
    }

    /// Recursively free every page-table page reachable from `table_phys`
    /// at `level`. Leaf-level (PT) pages free their own frame; inner
    /// levels recurse into every present non-super-page entry first.
    ///
    /// Phase 67 Track D.2: `total_levels` is 4 for the legacy 4-level
    /// tree or 5 for scalable-mode 5-level (LA57) trees; leaf level
    /// equals `total_levels - 1`.
    fn free_subtree(table_phys: u64, level: usize, total_levels: usize) {
        let leaf_level = total_levels - 1;
        if level < leaf_level {
            for i in 0..512 {
                let raw = Self::read_table_entry(table_phys, i * 8);
                let entry = VtdPageTableEntry::decode(raw);
                if entry.is_present() && !entry.is_super_page() {
                    Self::free_subtree(entry.phys(), level + 1, total_levels);
                }
            }
        }
        // Free this table page itself. Order 0 = single page.
        frame_allocator::free_contiguous(table_phys, 0);
    }

    // ---- Fault path ----------------------------------------------------

    /// Read the fault-recording register block and drain any posted
    /// fault records. Called from the IRQ handler.
    ///
    /// Kept as a `&self` helper even though the live IRQ path uses the
    /// static `dispatch_fault_irq` entry point — this function makes
    /// unit-tests (and possibly a future `peek_faults` tool) easier to
    /// write without needing the global slot array.
    #[allow(dead_code)]
    fn drain_fault_records(&self) {
        let cap = self.cap;
        let fro = (cap.fro_16byte_units() as usize) * 16;
        if fro == 0 {
            return;
        }
        let nfr = (cap.nfr() as usize) + 1;
        let drain_limit = FAULT_DRAIN_CAP.min(nfr);

        for i in 0..drain_limit {
            let fr_base = fro + i * 16;
            // High 64 bits: F (bit 63) indicates fault present.
            let high = self.read_u64(fr_base + 8);
            if (high & (1u64 << 63)) == 0 {
                continue;
            }
            let low = self.read_u64(fr_base);
            // Decode: requester (bits 79:64) = high[15:0]; fault reason
            // bits 39:32 of high. IOVA in low register.
            let requester_bdf = (high & 0xFFFF) as u16;
            let fault_reason = ((high >> 32) & 0xFF) as u16;
            let iova = low & !0xFFFu64;

            let rec = FaultRecord {
                requester_bdf,
                fault_reason,
                iova: Iova(iova),
            };
            iommu_fault::dispatch("vtd", &rec);

            // W1C the F bit to acknowledge.
            self.write_u64(fr_base + 8, high | (1u64 << 63));
        }

        // Clear fault overflow bit (bit 0 of FSTS) — W1C.
        let fsts = self.read_u32(VtdRegs::FSTS);
        if fsts != 0 {
            self.write_u32(VtdRegs::FSTS, fsts);
        }
    }

    /// Register-level MMIO base accessor for the fault handler shim.
    /// Used by the global IRQ dispatch to find the unit whose record
    /// ring needs draining.
    fn dispatch_fault_irq() {
        // Snapshot the slot array under the lock; iterate outside the
        // lock so the handler is not holding UNIT_SLOTS across MMIO.
        let snapshots: [Option<*mut u8>; MAX_FAULT_UNITS] = {
            let guard = UNIT_SLOTS.lock();
            let mut out: [Option<*mut u8>; MAX_FAULT_UNITS] = [None; MAX_FAULT_UNITS];
            for (i, slot) in guard.iter().enumerate() {
                out[i] = slot.map(|p| p as *mut u8);
            }
            out
        };
        for s in snapshots.into_iter().flatten() {
            // SAFETY: UNIT_SLOTS only stores register bases from
            // currently-live VtdUnit instances; the pointer is valid as
            // long as the unit is not dropped. Units bound to the IRQ
            // outlive the IRQ handler install point (units live for the
            // process lifetime).
            let cap_raw = unsafe { read_volatile(s.add(VtdRegs::CAP) as *const u64) };
            let cap = VtdCap(cap_raw);
            let fro = (cap.fro_16byte_units() as usize) * 16;
            if fro == 0 {
                continue;
            }
            let nfr = (cap.nfr() as usize) + 1;
            let drain = FAULT_DRAIN_CAP.min(nfr);
            for i in 0..drain {
                let fr_base = fro + i * 16;
                let high = unsafe { read_volatile(s.add(fr_base + 8) as *const u64) };
                if (high & (1u64 << 63)) == 0 {
                    continue;
                }
                let low = unsafe { read_volatile(s.add(fr_base) as *const u64) };
                let requester_bdf = (high & 0xFFFF) as u16;
                let fault_reason = ((high >> 32) & 0xFF) as u16;
                let iova = low & !0xFFFu64;
                let rec = FaultRecord {
                    requester_bdf,
                    fault_reason,
                    iova: Iova(iova),
                };
                iommu_fault::dispatch("vtd", &rec);
                unsafe {
                    write_volatile(s.add(fr_base + 8) as *mut u64, high | (1u64 << 63));
                };
            }
            let fsts = unsafe { read_volatile(s.add(VtdRegs::FSTS) as *const u32) };
            if fsts != 0 {
                unsafe { write_volatile(s.add(VtdRegs::FSTS) as *mut u32, fsts) };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fault-unit registration
// ---------------------------------------------------------------------------
//
// A global slot array lets the shared IRQ entry point reach every VT-d unit
// that installed a handler. `MAX_FAULT_UNITS` is deliberately small; Phase
// 55a expects one unit (QEMU q35's single intel-iommu), but bare metal may
// see a handful.

const MAX_FAULT_UNITS: usize = 8;

/// Phase 57b G.5 — IRQ-shared `spin::Mutex` per the Track A.1 audit
/// (`docs/handoffs/57b-spinlock-callsite-audit.md` row
/// `kernel/src/iommu/intel.rs:597`). [`dispatch_fault_irq`] (called from
/// the VT-d MSI fault ISR via [`vtd_fault_irq_trampoline`]) acquires this
/// lock to snapshot the slot-pointer array, so converting to
/// `IrqSafeMutex` would not work with the ISR's existing pattern (the ISR
/// runs with IF=0 and never raises the per-task preempt counter).
///
/// Task-context callers must explicitly `preempt_disable` +
/// `interrupts::without_interrupts` to satisfy the F.1 preempt-discipline.
/// See [`with_unit_slots`].
static UNIT_SLOTS: spin::Mutex<[Option<usize>; MAX_FAULT_UNITS]> =
    spin::Mutex::new([None; MAX_FAULT_UNITS]);

/// Phase 57b G.5 — task-context wrapper around [`UNIT_SLOTS`].
///
/// Wraps the `preempt_disable` + `interrupts::without_interrupts` +
/// `UNIT_SLOTS.lock()` + `preempt_enable` boilerplate so every
/// task-context callsite shares the same shape.
///
/// **Do not call this from an ISR**: [`dispatch_fault_irq`] takes
/// `UNIT_SLOTS` directly because interrupt handlers already run with IF=0
/// and follow their own discipline (no kernel preemption, no nested
/// `preempt_disable`).
///
/// Lock-ordering: `preempt_disable` is lock-free (Phase 57b D.2), so
/// calling it before `without_interrupts` cannot recurse.
fn with_unit_slots<R>(f: impl FnOnce(&mut [Option<usize>; MAX_FAULT_UNITS]) -> R) -> R {
    crate::task::scheduler::preempt_disable();
    let result = x86_64::instructions::interrupts::without_interrupts(|| {
        let mut slots = UNIT_SLOTS.lock();
        f(&mut slots)
    });
    crate::task::scheduler::preempt_enable();
    result
}

/// Tracks which device-IRQ vector (if any) the VT-d fault handler has
/// claimed. Written once on first `install_fault_handler`.
static FAULT_VECTOR: AtomicU8 = AtomicU8::new(0);

/// Attempt to reserve a device-IRQ vector for the VT-d fault handler.
/// Scans the bank from the top down so MSI allocations (which go
/// bottom-up via `MsiVectorAllocator`) and the IOMMU claim do not
/// collide. Returns the vector on success.
fn reserve_iommu_irq() -> Result<u8, &'static str> {
    let existing = FAULT_VECTOR.load(Ordering::Acquire);
    if existing != 0 {
        return Ok(existing);
    }
    // Walk from the top of the stub bank downwards, claim the first
    // unused slot.
    let entry = DeviceIrqEntry {
        handler: vtd_fault_irq_trampoline,
        kind: DeviceIrqKind::Msi,
    };
    let top = DEVICE_IRQ_VECTOR_BASE + DEVICE_IRQ_VECTOR_COUNT;
    let mut candidate = top - 1;
    loop {
        match register_device_irq(
            candidate,
            DeviceIrqEntry {
                handler: entry.handler,
                kind: entry.kind,
            },
        ) {
            Ok(()) => {
                FAULT_VECTOR.store(candidate, Ordering::Release);
                return Ok(candidate);
            }
            Err("device IRQ vector already registered") => {
                if candidate == DEVICE_IRQ_VECTOR_BASE {
                    return Err("no device-IRQ slot available for VT-d fault handler");
                }
                candidate -= 1;
            }
            Err(other) => return Err(other),
        }
    }
}

/// IRQ trampoline — bounce into `VtdUnit::dispatch_fault_irq` which
/// iterates over the registered VT-d units.
fn vtd_fault_irq_trampoline() {
    VtdUnit::dispatch_fault_irq();
}

// ---------------------------------------------------------------------------
// IommuUnit trait impl
// ---------------------------------------------------------------------------

impl IommuUnit for VtdUnit {
    fn bring_up(&mut self) -> Result<(), IommuError> {
        if self.up {
            return Ok(());
        }

        // 1. Ensure root table exists. Queued-invalidation is deferred —
        //    register-based path is sufficient for Phase 55a.
        let root_phys = self.ensure_root_table()?;

        // 2. Global context-cache + IOTLB invalidation so no stale
        //    state survives bring-up.
        self.invalidate_context_cache_global();
        self.invalidate_iotlb_global();

        // 3. Write RTADDR; pulse SRTP via GCMD; wait for GSTS.RTPS ack.
        self.write_u64(VtdRegs::RTADDR, encode_rtaddr_legacy(root_phys));
        self.write_u32(VtdRegs::GCMD, GCMD_SRTP);
        self.wait_gsts_bit(GSTS_RTPS)?;

        // 4. Enable translation. Must come after the root-table is
        //    committed.
        self.write_u32(VtdRegs::GCMD, GCMD_TE);
        self.wait_gsts_bit(GSTS_TES)?;

        // 5. Phase 67 Track C.1 — bring up queued invalidation if the
        //    hardware advertises it. Failure is non-fatal: log + leave
        //    the ring un-installed so subsequent `flush_*` calls return
        //    `NotAvailable` and the existing register-path stays
        //    operational. This deliberate degradation matches Phase
        //    55a's "IOMMU regressions degrade, do not brick" policy.
        if self.capabilities.queued_invalidation
            && let Err(e) = self.bring_up_invalidation_queue()
        {
            log::warn!(
                "[iommu] vtd unit[{}] queued-invalidation bring-up failed: {} \
                 — register-path flushes still available",
                self.unit_index,
                e
            );
        }

        self.up = true;
        log::info!(
            "[iommu] iommu.unit.brought_up vendor=vtd unit={} register_base={:#x} \
             aw={}b page_sizes={:#x} qi={} ir={}",
            self.unit_index,
            self.regs_virt as u64 - phys_offset(),
            self.capabilities.address_width_bits,
            self.capabilities.supported_page_sizes,
            self.capabilities.queued_invalidation,
            self.capabilities.interrupt_remapping,
        );
        Ok(())
    }

    fn create_domain(&mut self) -> Result<DmaDomain, IommuError> {
        if !self.up {
            return Err(IommuError::NotAvailable);
        }

        // Allocate the SL-page-table PML4.
        let pml4_phys = Self::alloc_table_page().ok_or(IommuError::HardwareFault)?;

        // Assign a new IOVA allocator covering the full address window
        // minus the reserved-region set. Reserved regions are injected
        // by the Track E domain-creation helper; in Phase 55a Track C
        // we start with an empty live set and let the caller call
        // `iova.reserve` separately when Track E wires it in.
        let window_bits = VTD_ADDRESS_WIDTH_BITS as u64;
        // Use a 4 KiB minimum alignment. Start the window at 4 KiB so
        // IOVA 0 is never handed out (many PCI devices treat 0 as a
        // terminator).
        let iova = IovaAllocator::new(PAGE_SIZE, 1u64 << window_bits, PAGE_SIZE as usize);

        let id = DomainId(self.next_domain_id);
        self.next_domain_id = self.next_domain_id.saturating_add(1);
        let vtd_domain_id = self.next_vtd_domain_id;
        self.next_vtd_domain_id = self.next_vtd_domain_id.saturating_add(1);

        // Phase 67 Track D.2 — stamp the chosen page-table depth per
        // domain. `init_page_tables` returns 5 only when both the
        // hardware advertises scalable mode and the CPU has LA57
        // enabled; otherwise it returns the legacy 4.
        let levels = self.init_page_tables();
        self.domains.push(VtdDomainState {
            id,
            vtd_domain_id,
            pml4_phys,
            iova,
            pt_pages: 0,
            bound_bdf: 0,
            levels,
        });

        // Flush the context cache so a subsequent map is observed by
        // the hardware walker on first DMA.
        self.invalidate_context_cache_global();

        log::info!(
            "[iommu] iommu.domain.created vendor=vtd unit={} domain_id={:#x} root_phys={:#x} levels={}",
            self.unit_index,
            id.0,
            pml4_phys,
            levels,
        );

        Ok(DmaDomain::new(id, self.unit_index))
    }

    fn destroy_domain(&mut self, domain: DmaDomain) -> Result<(), IommuError> {
        if domain.unit_index() != self.unit_index {
            return Err(IommuError::Invalid);
        }
        let id = domain.id();
        let pos = self
            .domains
            .iter()
            .position(|d| d.id == id)
            .ok_or(IommuError::Invalid)?;
        let state = self.domains.remove(pos);

        // Clear the context-table entry if this domain was bound.
        if state.bound_bdf != 0 {
            let bus = (state.bound_bdf >> 8) as u8;
            let dev_fn = (state.bound_bdf & 0xFF) as u8;
            if let Some(ctx_phys) = self.context_tables[bus as usize] {
                let entry_offset = (dev_fn as usize) * 16;
                Self::write_table_entry(ctx_phys, entry_offset, 0);
                Self::write_table_entry(ctx_phys, entry_offset + 8, 0);
            }
        }

        // Walk the SL table freeing every page-table page. Phase 67
        // Track D.2: pass the per-domain `levels` so 5-level trees are
        // freed correctly when scalable mode is active.
        Self::free_subtree(state.pml4_phys, 0, state.levels as usize);

        // Global flush so stale translations from the torn-down domain
        // cannot leak through.
        self.invalidate_context_cache_global();
        self.invalidate_iotlb_global();

        domain.release();
        Ok(())
    }

    fn map(
        &mut self,
        domain: DomainId,
        iova: Iova,
        phys: PhysAddr,
        len: usize,
        flags: MapFlags,
    ) -> Result<(), DomainError> {
        if len == 0 {
            return Err(DomainError::InvalidRange);
        }
        if !iova.0.is_multiple_of(PAGE_SIZE)
            || !phys.0.is_multiple_of(PAGE_SIZE)
            || !len.is_multiple_of(PAGE_SIZE as usize)
        {
            return Err(DomainError::InvalidRange);
        }
        let pte_flags = Self::encode_pte_flags(flags);
        // Snapshot the pml4, cap, and level count under the domain
        // lookup, then drop the borrow before walking so we can mutate
        // pt_pages after.
        let (pml4_phys, mut pt_pages, levels) = {
            let state = self
                .domains
                .iter()
                .find(|d| d.id == domain)
                .ok_or(DomainError::InvalidRange)?;
            (state.pml4_phys, state.pt_pages, state.levels as usize)
        };

        let npages = (len / PAGE_SIZE as usize) as u64;
        for i in 0..npages {
            let page_iova = iova.0 + i * PAGE_SIZE;
            let page_phys = phys.0 + i * PAGE_SIZE;

            let (leaf_phys, entry_offset, new_pages) =
                Self::walk_and_install_intermediates(pml4_phys, page_iova, levels)
                    .map_err(|_| DomainError::HardwareFault)?;
            pt_pages = pt_pages.saturating_add(new_pages);
            if pt_pages > DOMAIN_PT_PAGE_CAP {
                // Persist the updated count so destroy_domain frees any
                // partially-installed pages and return the cap error.
                if let Some(state) = self.domains.iter_mut().find(|d| d.id == domain) {
                    state.pt_pages = pt_pages;
                }
                return Err(DomainError::PageTablePagesCapExceeded);
            }

            // Leaf must not already be present — overlapping map.
            let existing = Self::read_table_entry(leaf_phys, entry_offset);
            if VtdPageTableEntry::decode(existing).is_present() {
                // Persist pt_pages so we don't forget the partial work.
                if let Some(state) = self.domains.iter_mut().find(|d| d.id == domain) {
                    state.pt_pages = pt_pages;
                }
                return Err(DomainError::AlreadyMapped);
            }

            let pte = VtdPageTableEntry::new(page_phys, pte_flags);
            Self::write_table_entry(leaf_phys, entry_offset, pte.encode());
        }

        if let Some(state) = self.domains.iter_mut().find(|d| d.id == domain) {
            state.pt_pages = pt_pages;
        }
        // Fresh translations are not guaranteed to become visible until the
        // IOTLB is invalidated; otherwise devices can keep faulting on a
        // just-mapped IOVA range.
        //
        // Phase 67 Track C.2: prefer the queued-invalidation domain
        // flush when the queue is up so only the affected domain pays
        // the invalidation cost. Falls back to the global register
        // path on any QI-side failure.
        self.flush_iotlb_or_global(domain);
        Ok(())
    }

    fn unmap(&mut self, domain: DomainId, iova: Iova, len: usize) -> Result<(), DomainError> {
        if len == 0 {
            return Err(DomainError::InvalidRange);
        }
        if !iova.0.is_multiple_of(PAGE_SIZE) || !len.is_multiple_of(PAGE_SIZE as usize) {
            return Err(DomainError::InvalidRange);
        }

        let (pml4_phys, levels) = {
            let state = self
                .domains
                .iter()
                .find(|d| d.id == domain)
                .ok_or(DomainError::InvalidRange)?;
            (state.pml4_phys, state.levels as usize)
        };

        let npages = (len / PAGE_SIZE as usize) as u64;
        for i in 0..npages {
            let page_iova = iova.0 + i * PAGE_SIZE;
            let Some((leaf_phys, entry_offset)) =
                Self::walk_read_only(pml4_phys, page_iova, levels)
            else {
                return Err(DomainError::NotMapped);
            };
            let raw = Self::read_table_entry(leaf_phys, entry_offset);
            if !VtdPageTableEntry::decode(raw).is_present() {
                return Err(DomainError::NotMapped);
            }
            Self::write_table_entry(leaf_phys, entry_offset, 0);
        }

        // Required by the trait: IOTLB invalidation before returning
        // success.
        //
        // Phase 67 Track C.2: per-domain queued flush; fall back to
        // global on QI-side failure.
        self.flush_iotlb_or_global(domain);
        Ok(())
    }

    fn flush(&mut self, domain: DomainId) -> Result<(), IommuError> {
        // Phase 67 Track C.2 — prefer the queued-invalidation engine
        // when available so the trait-level flush exercises the new
        // per-domain path. On any QI-side failure (timeout, queue not
        // up, domain not registered yet) fall back to the global
        // register-path invalidation that Phase 55a shipped.
        if self.iq_ring_phys.is_some() {
            match self.flush_domain(domain) {
                Ok(()) => return Ok(()),
                Err(IommuError::FlushTimeout) | Err(IommuError::Invalid) => {
                    log::warn!(
                        "[iommu] vtd unit[{}] queued flush_domain failed; falling back to global invalidation",
                        self.unit_index
                    );
                }
                Err(_) => {}
            }
        }
        self.invalidate_iotlb_global();
        Ok(())
    }

    fn install_fault_handler(&mut self, handler: FaultHandlerFn) -> Result<(), IommuError> {
        // 1. Register the user handler in the shared slot.
        iommu_fault::install(handler);

        // 2. Claim a vector from the device-IRQ bank (idempotent across
        //    units).
        let vector = match reserve_iommu_irq() {
            Ok(v) => v,
            Err(msg) => {
                log::warn!("[iommu] vtd fault IRQ reservation failed: {}", msg);
                return Err(IommuError::HardwareFault);
            }
        };

        // 3. Register this unit's register base in the shared slot
        //    array so the global IRQ dispatch can reach it. Phase 57b
        //    G.5 — `with_unit_slots` wraps the `preempt_disable` +
        //    `without_interrupts` boilerplate around the IRQ-shared
        //    `UNIT_SLOTS` lock.
        let slot_result: Result<(), IommuError> = with_unit_slots(|slots| {
            if slots.contains(&Some(self.regs_virt as usize)) {
                return Ok(());
            }
            if let Some(slot) = slots.iter_mut().find(|s| s.is_none()) {
                *slot = Some(self.regs_virt as usize);
                Ok(())
            } else {
                Err(IommuError::HardwareFault)
            }
        });
        if let Err(e) = slot_result {
            log::warn!(
                "[iommu] vtd unit[{}] fault slot array full, fault IRQ not installed",
                self.unit_index
            );
            return Err(e);
        }

        // 4. Program FEDATA / FEADDR to route faults to the LAPIC of
        //    the current core with the chosen vector. Delivery mode =
        //    0 (fixed), no redirection hint.
        let lapic_id = apic::current_lapic_id();
        let fedata = vector as u32;
        // LAPIC address is 0xFEE0_0000 | (lapic_id << 12). Phase 55a
        // delivers in physical mode.
        let feaddr = 0xFEE0_0000u32 | ((lapic_id as u32) << 12);
        self.write_u32(VtdRegs::FEDATA, fedata);
        self.write_u32(VtdRegs::FEADDR, feaddr);
        self.write_u32(VtdRegs::FEUADDR, 0);
        // Clear the FECTL interrupt-mask bit (bit 31) so hardware
        // actually delivers the interrupt.
        self.write_u32(VtdRegs::FECTL, 0);

        log::info!(
            "[iommu] vtd unit[{}] fault handler installed: vector={:#x} lapic={}",
            self.unit_index,
            vector,
            lapic_id
        );
        Ok(())
    }

    fn capabilities(&self) -> IommuCapabilities {
        self.capabilities
    }
}

// ---------------------------------------------------------------------------
// Helpers for bind-to-BDF (Phase 55a entry point — Track E will call this).
// ---------------------------------------------------------------------------

impl VtdUnit {
    /// Bind a PCI device identified by `(bus, device, function)` to the
    /// named domain. Populates the context-table entry and flushes the
    /// context cache.
    ///
    /// Phase 55a expects one BDF per domain (the "one device per
    /// domain" default). Re-binding the same BDF replaces the prior
    /// entry.
    #[allow(dead_code)]
    pub fn bind_device(
        &mut self,
        domain_id: DomainId,
        bus: u8,
        device: u8,
        function: u8,
    ) -> Result<(), IommuError> {
        if !self.up {
            return Err(IommuError::NotAvailable);
        }
        let (pml4_phys, vtd_domain_id) = {
            let state = self
                .domains
                .iter()
                .find(|d| d.id == domain_id)
                .ok_or(IommuError::Invalid)?;
            (state.pml4_phys, state.vtd_domain_id)
        };
        let dev_fn = (device << 3) | (function & 0x7);
        self.install_context_entry(bus, dev_fn, pml4_phys, vtd_domain_id)?;
        // Record binding.
        if let Some(state) = self.domains.iter_mut().find(|d| d.id == domain_id) {
            state.bound_bdf = ((bus as u16) << 8) | (dev_fn as u16);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // Phase 67 Track C.2: prefer queued context-cache + IOTLB
        // flush for the bound domain. The global path stays as the
        // fall-back when the queue is not up or the descriptor flush
        // timed out.
        self.flush_context_or_global(domain_id);
        self.flush_iotlb_or_global(domain_id);
        Ok(())
    }

    /// Phase 67 Track C.2 helper — flush the domain's IOTLB through
    /// queued invalidation when the ring is up, otherwise fall back
    /// to the global register-path invalidation. Swallows
    /// non-recoverable QI errors with a `warn!` so callers see the
    /// best-effort "translations are coherent again" guarantee.
    fn flush_iotlb_or_global(&mut self, domain: DomainId) {
        if self.iq_ring_phys.is_some() {
            match self.flush_iotlb(domain) {
                Ok(()) => return,
                Err(IommuError::FlushTimeout) | Err(IommuError::Invalid) => {
                    log::warn!(
                        "[iommu] vtd unit[{}] queued flush_iotlb fell back to global",
                        self.unit_index
                    );
                }
                Err(_) => {}
            }
        }
        self.invalidate_iotlb_global();
    }

    /// Phase 67 Track C.2 helper — same pattern as
    /// [`flush_iotlb_or_global`] for the context-cache.
    fn flush_context_or_global(&mut self, domain: DomainId) {
        if self.iq_ring_phys.is_some() {
            match self.flush_context(domain) {
                Ok(()) => return,
                Err(IommuError::FlushTimeout) | Err(IommuError::Invalid) => {
                    log::warn!(
                        "[iommu] vtd unit[{}] queued flush_context fell back to global",
                        self.unit_index
                    );
                }
                Err(_) => {}
            }
        }
        self.invalidate_context_cache_global();
    }
}

// ---------------------------------------------------------------------------
// D.1 — BAR identity mapping (Track D)
// ---------------------------------------------------------------------------

impl VtdUnit {
    /// Identity-map each MMIO BAR in the given domain and return a
    /// [`BarCoverage`] recording every range that was installed.
    ///
    /// For each non-zero-length entry in `bars`, the physical range
    /// `[base, base + len)` is identity-mapped (IOVA == phys) with
    /// `READ | WRITE` permissions. The range is 4 KiB–page-aligned before
    /// the IOMMU call: `base` is rounded down and `len` is rounded up to
    /// cover the full BAR window.
    ///
    /// `AlreadyMapped` from the underlying [`IommuUnit::map`] is treated as
    /// success — the range may have been pre-installed by `pre_map_reserved`
    /// and the coverage is still valid. Any other error is propagated and
    /// the caller should treat the domain as unusable.
    ///
    /// Called from `sys_device_claim` (Track D.3) through
    /// `install_and_verify_bar_coverage`; also directly accessible for
    /// integration tests under `cargo xtask device-smoke --device nvme
    /// --iommu`.
    ///
    /// Phase 57 Track C.2: this function is reused unchanged for the
    /// audio device class (Intel AC'97, `0x8086:0x2415`). AC'97 ships
    /// both BARs in I/O space — the kernel-side claim path filters PIO
    /// BARs before they reach this function, so the audio claim sees an
    /// empty `bars` slice and the loop is a no-op. Kernel-core unit
    /// tests pin that contract (see
    /// `kernel-core/tests/phase57_c2_audio_bar_coverage.rs`). A future
    /// MMIO-bearing audio target (e.g. Intel HDA) will hit this loop
    /// with no per-class branching needed; the
    /// `kernel_core::device_host::audio_class::BarLayout` descriptor
    /// records each target's MMIO BAR count alongside its PCI ID.
    #[allow(dead_code)]
    pub fn install_bar_identity_maps(
        &mut self,
        domain: DomainId,
        bars: &[kernel_core::iommu::bar_coverage::Bar],
    ) -> Result<kernel_core::iommu::bar_coverage::BarCoverage, DomainError> {
        use kernel_core::iommu::bar_coverage::BarCoverage;
        let mut coverage = BarCoverage::new();
        for bar in bars {
            if bar.len == 0 {
                continue;
            }
            let aligned_base = bar.base & !0xFFF;
            let end = bar.base.saturating_add(bar.len as u64);
            let aligned_end = (end + 0xFFF) & !0xFFF;
            let aligned_len = (aligned_end - aligned_base) as usize;
            match self.map(
                domain,
                Iova(aligned_base),
                PhysAddr(aligned_base),
                aligned_len,
                MapFlags::READ | MapFlags::WRITE,
            ) {
                Ok(()) | Err(DomainError::AlreadyMapped) => {}
                Err(e) => return Err(e),
            }
            coverage.record_mapped(aligned_base, aligned_len);
        }
        Ok(coverage)
    }
}
