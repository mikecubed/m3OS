//! AMD-Vi IOMMU vendor driver — Phase 55a Track D.
//!
//! This module implements [`kernel_core::iommu::contract::IommuUnit`] for the
//! AMD I/O Virtualization Technology hardware family. The pure-logic layers
//! live in `kernel-core::iommu::{amdvi_regs, amdvi_page_table}` and are
//! host-tested on the workspace side; this module is the thin kernel-side
//! wrapper that does MMIO and hardware sequencing.
//!
//! # Bring-up sequence (D.3)
//!
//! 1. Allocate a 2 MiB (order-9) device table, zero it.
//! 2. Allocate a 4 KiB command buffer and a 4 KiB event-log buffer.
//! 3. Write the device-table, command-buffer, and event-log base registers.
//! 4. Set the command-buffer head / tail to zero; likewise event-log.
//! 5. Toggle CONTROL bits in the documented order: EVENT_LOG_EN → CMD_BUF_EN
//!    → IOMMU_EN.
//!
//! # Invalidation (D.3)
//!
//! After any page-table mutation or device-table mutation, Phase 55a posts:
//! - INVALIDATE_DEVTAB_ENTRY if the DT entry changed, and / or
//! - INVALIDATE_IOMMU_PAGES for the affected domain,
//! - COMPLETION_WAIT as a trailing barrier, with a store-address pointing at
//!   a dedicated DMA-mapped word; the poller spins on the store word to
//!   confirm completion rather than timing out.
//!
//! # Lock ordering
//!
//! The authoritative write-up is
//! `kernel_core::iommu::contract` module docs. `AmdViUnit` acquires its own
//! unit lock in `map` / `unmap` / `flush`; it never nests under a driver lock.
//! Fault handling runs in IRQ context and calls only the lock-free command /
//! event ring path.
//!
//! # Merge note (Track C coexistence)
//!
//! Track C (VT-d) will add `pub mod intel;` and `pub mod fault;` to
//! `kernel/src/iommu/mod.rs` in parallel. Until that lands, this module hosts
//! a local [`log_fault_event`] helper that produces the same structured log
//! format Track C will use; merging moves the helper to the shared
//! `fault.rs` without changing the call shape here.

#![allow(dead_code)] // Phase 55a wiring lands in Track E; symbols used by tests only until then.

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use kernel_core::iommu::amd::{AmdViFaultEvent, decode_event_log_entry};
use kernel_core::iommu::amdvi_page_table::{AmdViPageTableEntry, AmdViPteFlags};
use kernel_core::iommu::amdvi_regs::{
    CommandEntry, ControlBits, DeviceTableEntry, PFN_MASK_40, REG_CMD_BUF_BAR, REG_CMD_BUF_HEAD,
    REG_CMD_BUF_TAIL, REG_CONTROL, REG_DEV_TAB_BAR, REG_EVENT_LOG_BAR, REG_EVENT_LOG_HEAD,
    REG_EVENT_LOG_TAIL, REG_EXT_FEATURE, REG_MSI_ADDR_HI, REG_MSI_ADDR_LO, REG_MSI_CTRL,
    REG_MSI_DATA,
};
use kernel_core::iommu::contract::{
    DmaDomain, DomainError, DomainId, FaultHandlerFn, FaultRecord, IommuCapabilities, IommuError,
    IommuUnit, Iova, MapFlags, PhysAddr,
};
use kernel_core::iommu::tables::{BdfDomainAssignment, BdfGroups, group_bdfs_by_alias};

// ---------------------------------------------------------------------------
// Sizing constants
// ---------------------------------------------------------------------------

/// Total device-table size in bytes: 65536 BDF entries × 32 bytes = 2 MiB.
const DEVICE_TABLE_BYTES: u64 = 65536 * 32;
/// Order passed to `allocate_contiguous_zeroed` for the device table
/// (2 MiB = 512 pages = 2^9 pages).
const DEVICE_TABLE_ORDER: usize = 9;
/// Command-buffer size. 4 KiB holds 256 × 16-byte commands — ample for
/// Phase 55a which issues at most a few invalidations per mutation.
const CMD_BUF_BYTES: u64 = 4096;
const CMD_BUF_ORDER: usize = 0;
/// Event-log size. 4 KiB holds 256 × 16-byte events.
const EVENT_LOG_BYTES: u64 = 4096;
const EVENT_LOG_ORDER: usize = 0;
/// MMIO window size to map for the AMD-Vi register block. The documented
/// register file extends to 0x2030-ish; we map a full page.
const REG_WINDOW_BYTES: u64 = 4096;
/// Paging mode for Phase 55a domains: mode 3 = 4-level tree, 48-bit IOVA.
const DOMAIN_PAGING_MODE: u8 = 3;
/// Upper bound on the IOVA space we hand each domain (48-bit).
const DOMAIN_IOVA_TOP: u64 = 1u64 << 48;
/// CommandBuffer length encoding: bits 59:56. For 4 KiB (= 256 × 16 B),
/// the formula `BufferLen = 8 + log2(ring_pages)` gives `8` for a single
/// page. Spec §3.1 `CMD_BUF_BAR[59:56]`.
const CMD_BUF_LEN_ENCODING: u64 = 8;
/// EventLog length encoding — same shape as CMD_BUF_BAR.
const EVENT_LOG_LEN_ENCODING: u64 = 8;

/// Phase 67 Track A.2 — maximum events drained per ISR invocation.
///
/// AMD-Vi's event-log ring is 256 entries (`EVENT_LOG_BYTES / 16`). The
/// drain loop must be bounded to avoid the ISR spinning indefinitely if
/// firmware floods the ring; we cap at one ring's worth so a single
/// invocation can recover a full backlog without revisiting it. Entries
/// beyond the bound advance a named overflow counter
/// ([`EVENT_LOG_OVERFLOW`]) and are left for the next invocation.
pub const EVENT_LOG_RING_DEPTH: usize = (EVENT_LOG_BYTES / 16) as usize;

/// Monotonic count of event-log entries the ISR observed but could not
/// drain within the per-invocation bound. Read by diagnostic tooling and
/// the Track A.2 QEMU smoke test.
pub static EVENT_LOG_OVERFLOW: AtomicU32 = AtomicU32::new(0);

/// Monotonic count of events drained by the AMD-Vi event-log ISR since
/// boot. Mirrors the VT-d fault-counter pattern in
/// [`crate::iommu::fault::fault_count`] so diagnostic tools can confirm
/// the IRQ is actually firing without parsing serial output.
pub static EVENT_LOG_DRAINS: AtomicU32 = AtomicU32::new(0);

/// Monotonic allocator for [`DomainId`] values within a unit.
static NEXT_DOMAIN_ID: AtomicU32 = AtomicU32::new(1);

// ---------------------------------------------------------------------------
// Physical-memory window helpers
// ---------------------------------------------------------------------------

/// Convert a physical address to its kernel-virtual counterpart via the
/// kernel's phys-offset direct-map. Identical convention to the NVMe and
/// e1000 drivers; kept local here to avoid introducing a new helper path
/// before Track E consolidates DMA access.
fn phys_to_virt(phys: u64) -> *mut u8 {
    (crate::mm::phys_offset() + phys) as *mut u8
}

// ---------------------------------------------------------------------------
// AmdViUnit — one AMD-Vi IOMMU instance
// ---------------------------------------------------------------------------

/// Live state for a single AMD-Vi IOMMU. One `AmdViUnit` exists per IVHD
/// block in the IVRS table.
pub struct AmdViUnit {
    /// Kernel-virtual base of the MMIO register window. Derived from the
    /// IVRS-declared register base via `phys_offset + register_base`.
    mmio_virt: usize,
    /// Physical address of the register base. Kept for logging and for
    /// re-computing the virtual base after a hypothetical remap.
    register_base: u64,
    /// Unit index assigned by ACPI enumeration. Used as the `unit_index`
    /// field on every [`DmaDomain`] this unit creates.
    unit_index: usize,
    /// Physical address of the 2 MiB device table.
    device_table_phys: u64,
    /// Physical address of the 4 KiB command-buffer ring.
    cmd_buf_phys: u64,
    /// Physical address of the 4 KiB event-log ring.
    event_log_phys: u64,
    /// Physical address of the completion-wait store word. Allocated as
    /// a 4 KiB page; the low 8 bytes are the store word.
    completion_store_phys: u64,
    /// Command-ring tail index (in 16-byte entries). Hardware advances
    /// the head; software advances tail.
    cmd_tail: u64,
    /// Event-ring head index (in 16-byte entries). Software advances head.
    event_head: u64,
    /// `true` after the first successful `bring_up`. Subsequent calls are
    /// idempotent per the trait contract.
    brought_up: bool,
    /// Cached capability snapshot, populated lazily.
    caps: Option<IommuCapabilities>,
    /// Per-domain state keyed by [`DomainId`]. Phase 55a issues one domain
    /// per claimed BDF; multi-BDF domains are deferred.
    domains: Vec<DomainState>,
    /// Installed fault handler, invoked on every event-log record.
    fault_handler: Option<FaultHandlerFn>,
    /// Per-Phase 67 Track E: BDF → group → DomainId map populated from
    /// the IVRS alias entries at bring-up. When a claim arrives for a
    /// BDF that shares an IVRS alias group with another BDF, the same
    /// `DomainId` is returned so the requester pair points at a single
    /// domain rather than two split ones.
    bdf_domains: Option<BdfDomainAssignment>,
}

/// Per-domain bookkeeping. Keeps enough state to tear down cleanly when
/// the domain is destroyed.
struct DomainState {
    /// Domain identifier handed out at `create_domain`.
    id: DomainId,
    /// Physical address of the root page table (4 KiB page).
    page_table_root_phys: u64,
    /// BDF assigned to this domain at creation. Phase 55a: one BDF per
    /// domain; `None` when the domain has not yet been bound to a device.
    bdf: Option<u16>,
}

// SAFETY: AmdViUnit is only accessed either through the kernel-side IOMMU
// registry (`Box<AmdViUnit>` behind `crate::iommu::registry::REGISTRY`'s
// `IrqSafeMutex`) or via the fault-IRQ trampoline, which runs with IF=0.
// The MMIO virtual address remains valid for the lifetime of the kernel
// because the phys-offset map is installed before any IOMMU init runs.
unsafe impl Send for AmdViUnit {}

// ---------------------------------------------------------------------------
// Constructor + capability query
// ---------------------------------------------------------------------------

impl AmdViUnit {
    /// Allocate internal structures and map the MMIO register window.
    ///
    /// `register_base` is the IOMMU base address from an IVRS IVHD block.
    /// `unit_index` is the index of that block in the IVRS table.
    ///
    /// Does **not** enable translation. Callers must invoke
    /// [`AmdViUnit::bring_up`] before any `create_domain` / `map` call.
    pub fn new(register_base: u64, unit_index: usize) -> Result<Self, IommuError> {
        let mmio_virt = (crate::mm::phys_offset() + register_base) as usize;

        let device_table_phys =
            crate::mm::frame_allocator::allocate_contiguous_zeroed(DEVICE_TABLE_ORDER)
                .ok_or(IommuError::HardwareFault)?
                .start_address()
                .as_u64();
        let cmd_buf_phys = crate::mm::frame_allocator::allocate_contiguous_zeroed(CMD_BUF_ORDER)
            .ok_or(IommuError::HardwareFault)?
            .start_address()
            .as_u64();
        let event_log_phys =
            crate::mm::frame_allocator::allocate_contiguous_zeroed(EVENT_LOG_ORDER)
                .ok_or(IommuError::HardwareFault)?
                .start_address()
                .as_u64();
        let completion_store_phys = crate::mm::frame_allocator::allocate_contiguous_zeroed(0)
            .ok_or(IommuError::HardwareFault)?
            .start_address()
            .as_u64();

        log::info!(
            "[amdvi] unit[{}] created: register_base={:#x} dev_tab={:#x} cmd={:#x} evt={:#x}",
            unit_index,
            register_base,
            device_table_phys,
            cmd_buf_phys,
            event_log_phys,
        );

        Ok(Self {
            mmio_virt,
            register_base,
            unit_index,
            device_table_phys,
            cmd_buf_phys,
            event_log_phys,
            completion_store_phys,
            cmd_tail: 0,
            event_head: 0,
            brought_up: false,
            caps: None,
            domains: Vec::new(),
            fault_handler: None,
            bdf_domains: None,
        })
    }

    /// Kernel-virtual address of register byte `offset`.
    fn reg_ptr(&self, offset: usize) -> *mut u8 {
        (self.mmio_virt + offset) as *mut u8
    }

    /// Read a 64-bit MMIO register volatilely.
    fn read_reg(&self, offset: usize) -> u64 {
        debug_assert!((offset + 8) as u64 <= REG_WINDOW_BYTES);
        // SAFETY: register window is fixed at `register_base..register_base+PAGE`.
        unsafe { read_volatile(self.reg_ptr(offset) as *const u64) }
    }

    /// Write a 64-bit MMIO register volatilely.
    fn write_reg(&self, offset: usize, value: u64) {
        debug_assert!((offset + 8) as u64 <= REG_WINDOW_BYTES);
        // SAFETY: see `read_reg`.
        unsafe { write_volatile(self.reg_ptr(offset) as *mut u64, value) };
    }

    /// Read a 32-bit MMIO register volatilely.
    fn read_reg32(&self, offset: usize) -> u32 {
        debug_assert!((offset + 4) as u64 <= REG_WINDOW_BYTES);
        // SAFETY: see `read_reg`.
        unsafe { read_volatile(self.reg_ptr(offset) as *const u32) }
    }

    /// Write a 32-bit MMIO register volatilely.
    fn write_reg32(&self, offset: usize, value: u32) {
        debug_assert!((offset + 4) as u64 <= REG_WINDOW_BYTES);
        // SAFETY: see `read_reg`.
        unsafe { write_volatile(self.reg_ptr(offset) as *mut u32, value) };
    }

    /// Decode the Extended Feature Register into the vendor-neutral
    /// [`IommuCapabilities`] shape. The AMD spec §3.1.4 names the fields:
    /// HATS (bits 12:11) = host-address translation size (00 = 4 levels,
    /// 48-bit), PN (bit 0) = prefetch supported, GTSup (bit 2) = guest
    /// translation, IASup (bit 3) = IA32 format, GAMSup (bits 22:20) =
    /// guest address width. We map:
    ///
    /// - `supported_page_sizes`: always `(1 << 12) | (1 << 21) | (1 << 30)`
    ///   because AMD-Vi v1 always supports 4K / 2M / 1G leaves.
    /// - `address_width_bits`: 48 (mode-3 paging).
    /// - `interrupt_remapping`: IntCapXT + GAMSup bits; Phase 55a reports
    ///   this as `false` because we do not enable IR.
    /// - `queued_invalidation`: `true` — AMD-Vi always has a command ring.
    /// - `scalable_mode`: `false` (AMD has no scalable-mode equivalent).
    fn compute_capabilities(&self) -> IommuCapabilities {
        // Reading EXT_FEATURE is safe before bring-up; hardware latches
        // feature bits at reset.
        let _feat = self.read_reg(REG_EXT_FEATURE);
        IommuCapabilities {
            supported_page_sizes: (1u64 << 12) | (1u64 << 21) | (1u64 << 30),
            address_width_bits: 48,
            interrupt_remapping: false,
            queued_invalidation: true,
            scalable_mode: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Device table helpers
// ---------------------------------------------------------------------------

impl AmdViUnit {
    /// Write `entry` into the device-table slot for BDF `device_id`.
    fn write_dt_entry(&self, device_id: u16, entry: DeviceTableEntry) {
        let slot_phys = self.device_table_phys + (device_id as u64) * 32;
        // SAFETY: `slot_phys` is within the 2 MiB device-table allocation;
        // phys-offset direct-map covers it.
        let slot_ptr = phys_to_virt(slot_phys) as *mut u64;
        unsafe {
            for (i, word) in entry.words.iter().enumerate() {
                write_volatile(slot_ptr.add(i), *word);
            }
        }
    }

    /// Clear (invalidate) the device-table slot for BDF `device_id`.
    fn clear_dt_entry(&self, device_id: u16) {
        self.write_dt_entry(device_id, DeviceTableEntry::empty());
    }

    /// Post a COMPLETION_WAIT command after `cmd`, then wait for the
    /// hardware to update the completion store with `marker` before
    /// returning.
    ///
    /// Polling is a busy-wait against the store word — no timeout
    /// (hardware acks are bounded by queue depth, which is shallow).
    fn submit_and_wait(&mut self, cmd: CommandEntry) -> Result<(), IommuError> {
        // Write the target command.
        self.push_cmd(cmd);
        // Generate a unique marker per wait.
        let marker = NEXT_DOMAIN_ID.fetch_add(1, Ordering::Relaxed) as u64 | 0x100;
        let wait = CommandEntry::completion_wait(self.completion_store_phys, marker);
        // Zero the store word before writing the wait command.
        // SAFETY: completion_store_phys is a private 4 KiB page.
        unsafe {
            write_volatile(phys_to_virt(self.completion_store_phys) as *mut u64, 0);
        }
        self.push_cmd(wait);
        self.ring_cmd_doorbell();
        // Poll the store word until the marker appears. AMD-Vi hardware
        // ordered the COMPLETION_WAIT after the target command, so seeing
        // the marker means every preceding command has drained.
        // SAFETY: see above.
        // HW-bounded: AMD-Vi hardware processes the command ring and writes the
        // completion marker to the store page within the IOMMU command-queue
        // completion latency (AMD IOMMU Specification §3.3.3, typically < 1 µs
        // per command for cache-invalidation entries).
        //
        // Phase 57e Track B.2: under PREEMPT_FULL, a kernel-mode preemption
        // mid-spin would migrate this task to another core, leaving the
        // pending command marker observable from a core that did not
        // initiate it.  AMD-Vi commands target the IOMMU as a whole so
        // forward progress would still happen, but the per-task accounting
        // and the locking discipline around `IommuRegistry::with_unit_mut`
        // both expect the issuing core to own the spin until the marker
        // appears.
        crate::task::scheduler::preempt_disable();
        loop {
            let observed =
                unsafe { read_volatile(phys_to_virt(self.completion_store_phys) as *const u64) };
            if observed == marker {
                break;
            }
            core::hint::spin_loop();
        }
        crate::task::scheduler::preempt_enable();
        Ok(())
    }

    /// Enqueue `cmd` into the command ring at `cmd_tail`, advancing the
    /// tail. Hardware consumes from head; the ring wraps at its capacity.
    fn push_cmd(&mut self, cmd: CommandEntry) {
        let entry_bytes = 16u64;
        let ring_entries = CMD_BUF_BYTES / entry_bytes;
        let slot = self.cmd_tail;
        let slot_phys = self.cmd_buf_phys + slot * entry_bytes;
        // SAFETY: cmd_buf_phys is a 4 KiB ring; slot < ring_entries.
        unsafe {
            let ptr = phys_to_virt(slot_phys) as *mut u64;
            write_volatile(ptr, cmd.words[0]);
            write_volatile(ptr.add(1), cmd.words[1]);
        }
        self.cmd_tail = (self.cmd_tail + 1) % ring_entries;
    }

    /// Ring the command-buffer tail doorbell so hardware drains the ring.
    fn ring_cmd_doorbell(&self) {
        // CMD_BUF_TAIL is a 64-bit register; the low 16 bits carry the
        // tail index in bytes (ring size <= 64 KiB).
        let tail_bytes = self.cmd_tail * 16;
        self.write_reg(REG_CMD_BUF_TAIL, tail_bytes);
    }
}

// ---------------------------------------------------------------------------
// Event-log draining
// ---------------------------------------------------------------------------

impl AmdViUnit {
    /// Phase 67 Track A.2 — drain the event-log ring.
    ///
    /// Reads EVTLOG_HEAD and EVTLOG_TAIL, iterates each pending entry
    /// up to [`EVENT_LOG_RING_DEPTH`], decodes the raw 16-byte record
    /// through [`decode_event_log_entry`], dispatches the resulting
    /// [`AmdViFaultEvent`] to the shared fault logger and the
    /// installed user handler, and finally advances HEAD so hardware
    /// observes the drain.
    ///
    /// Bounded loop semantics: the drain stops at
    /// `EVENT_LOG_RING_DEPTH` entries even if more are visible.
    /// Entries that would have been processed past that bound advance
    /// the named [`EVENT_LOG_OVERFLOW`] counter; the next ISR
    /// invocation (or the next polled call) picks them up. This keeps
    /// the ISR latency bounded regardless of firmware behaviour.
    ///
    /// Safe to call from IRQ or polled context — performs no
    /// allocation and no blocking.
    ///
    /// Returns the number of events actually drained on this
    /// invocation.
    pub fn drain_event_log(&mut self) -> usize {
        let ring_entries = EVENT_LOG_BYTES / 16;
        let hw_tail = self.read_reg(REG_EVENT_LOG_TAIL) / 16;
        let mut count: usize = 0;
        while self.event_head != hw_tail && count < EVENT_LOG_RING_DEPTH {
            let slot_phys = self.event_log_phys + self.event_head * 16;
            // SAFETY: event_log_phys is a 4 KiB ring; slot < ring_entries.
            let raw: [u8; 16] = unsafe {
                let ptr = phys_to_virt(slot_phys);
                let mut buf = [0u8; 16];
                core::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), 16);
                buf
            };
            // Phase 67 Track A.2 uses the typed kernel-core decoder so
            // every event funnels through the same code path host
            // tests cover.
            let event: AmdViFaultEvent = decode_event_log_entry(&raw)
                .expect("decode_event_log_entry is total over [u8; 16]");
            let record = FaultRecord {
                requester_bdf: event.requestor_bdf,
                fault_reason: event.fault_code.to_raw() as u16,
                iova: Iova(event.iova),
            };
            // Shared fault-event logger so the log format is identical
            // across VT-d and AMD-Vi (Phase 55a DRY rule).
            crate::iommu::fault::log_fault_event(
                "amdvi",
                record.requester_bdf,
                record.iova.0,
                record.fault_reason,
            );
            // Additional AMD-Vi-specific context useful for debugging
            // but not part of the shared format. `domain` is restored
            // from header bits 47:32 to retain parity with the
            // pre-Phase-67 `EventEntry::decode` log line.
            log::warn!(
                "[iommu] amdvi-detail: unit={} domain={:#x} event_code={} flags={:#x}",
                self.unit_index,
                event.domain_id,
                event.fault_code,
                event.flags,
            );
            if let Some(handler) = self.fault_handler {
                handler(&record);
            }
            self.event_head = (self.event_head + 1) % ring_entries;
            count += 1;
            EVENT_LOG_DRAINS.fetch_add(1, Ordering::Relaxed);
        }
        if self.event_head != hw_tail {
            // Hit the per-invocation bound with more entries pending.
            // Account every remaining entry against the overflow counter
            // so external observers can see the pressure.
            let pending = if hw_tail >= self.event_head {
                hw_tail - self.event_head
            } else {
                (ring_entries - self.event_head) + hw_tail
            };
            EVENT_LOG_OVERFLOW.fetch_add(pending as u32, Ordering::Relaxed);
            log::warn!(
                "[amdvi] event-log drain hit bound: drained={} pending={}",
                count,
                pending
            );
        }
        if count > 0 {
            // Update HEAD so hardware sees the drain.
            self.write_reg(REG_EVENT_LOG_HEAD, self.event_head * 16);
        }
        count
    }

    /// Legacy alias retained for callers that pre-date Track A. Phase
    /// 67 Track A renamed the entry point to `drain_event_log`; this
    /// shim keeps existing callers working without churn.
    #[allow(dead_code)]
    pub fn process_events(&mut self) -> usize {
        self.drain_event_log()
    }
}

// Phase 67 Track A — event_code_name was replaced by
// AmdViFaultCode::name() / Display from kernel-core.

// ---------------------------------------------------------------------------
// MSI programming for the IOMMU interrupt
// ---------------------------------------------------------------------------

impl AmdViUnit {
    /// Program the AMD-Vi in-register MSI capability to deliver IOMMU
    /// event-log interrupts at `vector` on the BSP LAPIC. Returns `Err`
    /// if the vector pool is exhausted.
    ///
    /// AMD-Vi's MSI is programmed directly through the MMIO register
    /// block (REG_MSI_CTRL / REG_MSI_ADDR_LO/HI / REG_MSI_DATA) rather
    /// than through the PCI capability list, because in firmware the
    /// IOMMU is often implemented as a northbridge function whose PCI
    /// config space is aliased through the register block.
    pub fn install_msi(&self, vector: u8) {
        let apic_id = crate::arch::x86_64::apic::current_lapic_id();
        // MSI address format: 0xFEE0_0000 | (apic_id << 12).
        let msi_addr = 0xFEE0_0000u32 | ((apic_id as u32) << 12);
        // MSI data: edge-triggered, fixed, vector byte.
        let msi_data = vector as u32;
        // MSI control: bit 0 = enable, bits 7:4 multi-message count = 0.
        let msi_ctrl: u32 = 1;
        self.write_reg32(REG_MSI_ADDR_LO, msi_addr);
        self.write_reg32(REG_MSI_ADDR_HI, 0);
        self.write_reg32(REG_MSI_DATA, msi_data);
        self.write_reg32(REG_MSI_CTRL, msi_ctrl);
        log::info!(
            "[amdvi] unit[{}] MSI programmed: addr={:#x} data={:#x} vector={}",
            self.unit_index,
            msi_addr,
            msi_data,
            vector,
        );
    }
}

// ---------------------------------------------------------------------------
// IommuUnit impl
// ---------------------------------------------------------------------------

impl IommuUnit for AmdViUnit {
    fn bring_up(&mut self) -> Result<(), IommuError> {
        if self.brought_up {
            return Ok(());
        }
        // 1. Device-table base: bits 51:12 = PFN, bits 11:0 = size
        //    (2^(bits+1) * 4 KiB) - 1. 65536 entries = 2 MiB table, so
        //    size encoding = 0x1FF.
        let dev_tab_bar = ((self.device_table_phys >> 12) & PFN_MASK_40) << 12 | 0x1FFu64;
        self.write_reg(REG_DEV_TAB_BAR, dev_tab_bar);

        // 2. Command-buffer base: PFN plus the BufferLen encoding in
        //    bits 59:56.
        let cmd_bar =
            ((self.cmd_buf_phys >> 12) & PFN_MASK_40) << 12 | (CMD_BUF_LEN_ENCODING << 56);
        self.write_reg(REG_CMD_BUF_BAR, cmd_bar);

        // 3. Event-log base.
        let event_bar =
            ((self.event_log_phys >> 12) & PFN_MASK_40) << 12 | (EVENT_LOG_LEN_ENCODING << 56);
        self.write_reg(REG_EVENT_LOG_BAR, event_bar);

        // 4. Zero head / tail pointers.
        self.write_reg(REG_CMD_BUF_HEAD, 0);
        self.write_reg(REG_CMD_BUF_TAIL, 0);
        self.write_reg(REG_EVENT_LOG_HEAD, 0);
        self.write_reg(REG_EVENT_LOG_TAIL, 0);

        // 5. Toggle enable bits in documented order.
        let base_ctrl = self.read_reg(REG_CONTROL);
        // 5a. Event-log first so we can observe bring-up faults.
        self.write_reg(
            REG_CONTROL,
            base_ctrl | ControlBits::EVENT_LOG_EN | ControlBits::EVENT_INT_EN,
        );
        // 5b. Command buffer second.
        let ctrl2 = self.read_reg(REG_CONTROL);
        self.write_reg(REG_CONTROL, ctrl2 | ControlBits::CMD_BUF_EN);
        // 5c. IOMMU enable last.
        let ctrl3 = self.read_reg(REG_CONTROL);
        self.write_reg(REG_CONTROL, ctrl3 | ControlBits::IOMMU_EN);

        self.brought_up = true;
        self.caps = Some(self.compute_capabilities());
        let caps = self.caps.as_ref().expect("caps set on line above");
        log::info!(
            "[iommu] iommu.unit.brought_up vendor=amdvi unit={} register_base={:#x} \
             aw={}b page_sizes={:#x} qi={} ir={}",
            self.unit_index,
            self.register_base,
            caps.address_width_bits,
            caps.supported_page_sizes,
            caps.queued_invalidation,
            caps.interrupt_remapping,
        );
        Ok(())
    }

    fn create_domain(&mut self) -> Result<DmaDomain, IommuError> {
        if !self.brought_up {
            return Err(IommuError::NotAvailable);
        }
        // Allocate and zero the root page table. AMD-Vi mode-3 uses a
        // level-3 root (4 KiB page of 512 × u64 entries).
        let root_phys = crate::mm::frame_allocator::allocate_contiguous_zeroed(0)
            .ok_or(IommuError::HardwareFault)?
            .start_address()
            .as_u64();

        let id = DomainId(NEXT_DOMAIN_ID.fetch_add(1, Ordering::Relaxed));
        self.domains.push(DomainState {
            id,
            page_table_root_phys: root_phys,
            bdf: None,
        });

        // Pre-map reserved regions into the fresh page table (shared with
        // VT-d via the `kernel/src/iommu/mod.rs` reserved-region accessor).
        let reserved = crate::iommu::reserved_regions();
        for region in reserved.iter() {
            // Identity-map each reserved region byte into the domain's
            // page table. Silently skip regions beyond the supported
            // 48-bit IOVA space.
            if region.start >= DOMAIN_IOVA_TOP {
                continue;
            }
            // The map path enforces alignment. We ignore failures here:
            // reserved regions that cannot be pre-mapped are logged.
            if let Err(err) = self.map(
                id,
                Iova(region.start),
                PhysAddr(region.start),
                region.len,
                MapFlags::READ | MapFlags::WRITE,
            ) {
                log::warn!(
                    "[amdvi] unit[{}] domain {:?}: reserved-region pre-map failed \
                     start={:#x} len={:#x} err={:?}",
                    self.unit_index,
                    id,
                    region.start,
                    region.len,
                    err,
                );
            }
        }
        log::info!(
            "[iommu] iommu.domain.created vendor=amdvi unit={} domain_id={:#x} root_phys={:#x}",
            self.unit_index,
            id.0,
            root_phys,
        );
        Ok(DmaDomain::new(id, self.unit_index))
    }

    fn destroy_domain(&mut self, domain: DmaDomain) -> Result<(), IommuError> {
        if domain.unit_index() != self.unit_index {
            return Err(IommuError::Invalid);
        }
        let id = domain.id();
        let idx = self
            .domains
            .iter()
            .position(|d| d.id == id)
            .ok_or(IommuError::Invalid)?;
        let state = self.domains.swap_remove(idx);

        // Clear the device-table entry if the domain was bound.
        if let Some(bdf) = state.bdf {
            self.clear_dt_entry(bdf);
            let inv_dt = CommandEntry::invalidate_devtab_entry(bdf);
            self.submit_and_wait(inv_dt)?;
        }
        // Issue a domain-wide IOMMU-pages invalidation so any cached
        // translations are dropped before we free page-table pages.
        let inv_pages = CommandEntry::invalidate_iommu_pages_all((id.0 & 0xFFFF) as u16);
        self.submit_and_wait(inv_pages)?;

        // Walk the page table freeing each intermediate and leaf page.
        free_page_table(state.page_table_root_phys, DOMAIN_PAGING_MODE);

        log::info!(
            "[iommu] domain.destroyed: vendor=amdvi unit={} domain_id={:#x}",
            self.unit_index,
            id.0,
        );
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
        let state = self
            .domains
            .iter()
            .find(|d| d.id == domain)
            .ok_or(DomainError::NotMapped)?;
        let root_phys = state.page_table_root_phys;
        // Walk the IOVA space one 4 KiB page at a time.
        let mut offset: u64 = 0;
        while (offset as usize) < len {
            let iova_page = iova.0 + offset;
            let phys_page = phys.0 + offset;
            install_leaf_4k(
                root_phys,
                iova_page,
                phys_page,
                flags.contains(MapFlags::READ),
                flags.contains(MapFlags::WRITE),
            )?;
            offset += 4096;
        }
        self.invalidate_iotlb(domain)
            .map_err(|_| DomainError::HardwareFault)?;
        Ok(())
    }

    fn unmap(&mut self, domain: DomainId, iova: Iova, len: usize) -> Result<(), DomainError> {
        if len == 0 {
            return Err(DomainError::InvalidRange);
        }
        let state = self
            .domains
            .iter()
            .find(|d| d.id == domain)
            .ok_or(DomainError::NotMapped)?;
        let root_phys = state.page_table_root_phys;
        let mut offset: u64 = 0;
        while (offset as usize) < len {
            let iova_page = iova.0 + offset;
            clear_leaf_4k(root_phys, iova_page)?;
            offset += 4096;
        }
        // TLB flush required before returning.
        self.invalidate_iotlb(domain)
            .map_err(|_| DomainError::HardwareFault)?;
        Ok(())
    }

    fn flush(&mut self, domain: DomainId) -> Result<(), IommuError> {
        self.invalidate_iotlb(domain)
    }

    fn install_fault_handler(&mut self, handler: FaultHandlerFn) -> Result<(), IommuError> {
        self.fault_handler = Some(handler);
        // Phase 67 Track A.1 — register the per-unit IRQ trampoline in
        // the device-IRQ bank before programming MSI so the vector is
        // immediately routable when the AMD-Vi hardware delivers an
        // event-log interrupt. The trampoline calls
        // [`AmdViUnit::drain_event_log`] on each registered unit; the
        // unit registers itself into [`UNIT_SLOTS`] right after the
        // vector reservation succeeds. Idempotent: a re-install on the
        // same unit replaces the handler in place without claiming a
        // second vector or growing the slot array.
        register_unit_slot(self as *mut AmdViUnit)?;
        let vector = match reserve_fault_vector() {
            Ok(v) => v,
            Err(msg) => {
                log::warn!(
                    "[amdvi] unit[{}] fault IRQ reservation failed: {}",
                    self.unit_index,
                    msg
                );
                return Err(IommuError::HardwareFault);
            }
        };
        self.install_msi(vector);
        log::info!(
            "[iommu] iommu.amd.fault_isr=installed unit={} vector={:#x}",
            self.unit_index,
            vector,
        );
        Ok(())
    }

    fn capabilities(&self) -> IommuCapabilities {
        self.caps.unwrap_or_else(|| self.compute_capabilities())
    }
}

impl AmdViUnit {
    /// Shared IOTLB invalidation path: post INVALIDATE_IOMMU_PAGES for the
    /// domain followed by COMPLETION_WAIT; poll the completion word.
    fn invalidate_iotlb(&mut self, domain: DomainId) -> Result<(), IommuError> {
        let cmd = CommandEntry::invalidate_iommu_pages_all((domain.0 & 0xFFFF) as u16);
        self.submit_and_wait(cmd)
    }
}

// ---------------------------------------------------------------------------
// Page-table helpers
// ---------------------------------------------------------------------------

/// Install (or overwrite) a 4 KiB leaf mapping for `iova` in the page
/// table rooted at `root_phys`, allocating intermediate pages as needed.
fn install_leaf_4k(
    root_phys: u64,
    iova: u64,
    phys: u64,
    io_read: bool,
    io_write: bool,
) -> Result<(), DomainError> {
    let mut table_phys = root_phys;
    for level in (1..=3u8).rev() {
        let shift = 12 + 9 * (level as u32);
        let index = ((iova >> shift) & 0x1FF) as usize;
        let entry_phys = table_phys + (index as u64) * 8;
        let raw = read_phys_u64(entry_phys);
        let entry = AmdViPageTableEntry::decode(raw);
        if !entry.is_present() {
            // Allocate an intermediate page.
            let new_phys = crate::mm::frame_allocator::allocate_contiguous_zeroed(0)
                .ok_or(DomainError::PageTablePagesCapExceeded)?
                .start_address()
                .as_u64();
            let new_entry = AmdViPageTableEntry::new(
                new_phys,
                AmdViPteFlags {
                    present: true,
                    io_read: true,
                    io_write: true,
                    force_coherent: false,
                    next_level: level - 1,
                },
            );
            write_phys_u64(entry_phys, new_entry.encode());
            table_phys = new_phys;
        } else if entry.next_level() == 0 {
            // A larger-page leaf already occupies this slot. Phase 55a
            // rejects overlapping mappings rather than splitting a
            // pre-existing large leaf.
            return Err(DomainError::AlreadyMapped);
        } else {
            table_phys = entry.phys_addr();
        }
    }
    // Level-0 slot.
    let shift = 12;
    let index = ((iova >> shift) & 0x1FF) as usize;
    let entry_phys = table_phys + (index as u64) * 8;
    let existing = AmdViPageTableEntry::decode(read_phys_u64(entry_phys));
    if existing.is_present() {
        return Err(DomainError::AlreadyMapped);
    }
    let leaf = AmdViPageTableEntry::new(
        phys,
        AmdViPteFlags {
            present: true,
            io_read,
            io_write,
            force_coherent: false,
            next_level: 0,
        },
    );
    write_phys_u64(entry_phys, leaf.encode());
    Ok(())
}

/// Clear a 4 KiB leaf mapping for `iova`. Returns `NotMapped` if the
/// entry is absent; leaves intermediate pages untouched (Phase 55a keeps
/// page-table pages alive until domain destruction).
fn clear_leaf_4k(root_phys: u64, iova: u64) -> Result<(), DomainError> {
    let mut table_phys = root_phys;
    for level in (1..=3u8).rev() {
        let shift = 12 + 9 * (level as u32);
        let index = ((iova >> shift) & 0x1FF) as usize;
        let entry_phys = table_phys + (index as u64) * 8;
        let entry = AmdViPageTableEntry::decode(read_phys_u64(entry_phys));
        if !entry.is_present() {
            return Err(DomainError::NotMapped);
        }
        table_phys = entry.phys_addr();
    }
    let shift = 12;
    let index = ((iova >> shift) & 0x1FF) as usize;
    let entry_phys = table_phys + (index as u64) * 8;
    let existing = AmdViPageTableEntry::decode(read_phys_u64(entry_phys));
    if !existing.is_present() {
        return Err(DomainError::NotMapped);
    }
    write_phys_u64(entry_phys, 0);
    Ok(())
}

/// Walk the page table rooted at `root_phys` and return every level's
/// allocated page to the buddy allocator.
fn free_page_table(root_phys: u64, _mode: u8) {
    free_page_table_level(root_phys, 3);
}

fn free_page_table_level(phys: u64, level: u8) {
    if level > 0 {
        for i in 0..512u64 {
            let entry_phys = phys + i * 8;
            let entry = AmdViPageTableEntry::decode(read_phys_u64(entry_phys));
            if entry.is_present() && entry.next_level() != 0 {
                free_page_table_level(entry.phys_addr(), level - 1);
            }
        }
    }
    // Return the page itself to the buddy allocator.
    crate::mm::frame_allocator::free_frame(phys);
}

/// Read a u64 from physical address `phys` through the phys-offset map.
fn read_phys_u64(phys: u64) -> u64 {
    // SAFETY: every caller holds a page owned by the domain; phys-offset
    // map covers allocator-provided RAM.
    unsafe { read_volatile(phys_to_virt(phys) as *const u64) }
}

/// Write a u64 at physical address `phys` through the phys-offset map.
fn write_phys_u64(phys: u64, value: u64) {
    // SAFETY: see `read_phys_u64`.
    unsafe { write_volatile(phys_to_virt(phys) as *mut u64, value) };
}

// ---------------------------------------------------------------------------
// D.2 — BAR identity mapping (Track D)
// ---------------------------------------------------------------------------

impl AmdViUnit {
    /// Identity-map each MMIO BAR in the given domain and return a
    /// [`BarCoverage`] recording every range that was installed.
    ///
    /// Mirrors [`VtdUnit::install_bar_identity_maps`] for the AMD-Vi
    /// backend. For each non-zero-length entry in `bars`, the physical
    /// range `[base, base + len)` is identity-mapped (IOVA == phys) with
    /// `READ | WRITE` permissions. The range is 4 KiB–page-aligned before
    /// the IOMMU call.
    ///
    /// `AlreadyMapped` is treated as success — the range may have been
    /// pre-installed by `pre_map_reserved`. Any other error is propagated
    /// and the caller should treat the domain as unusable.
    ///
    /// Phase 57 Track C.2: this function is reused unchanged for the
    /// audio device class (Intel AC'97, `0x8086:0x2415`) on AMD-Vi
    /// platforms. AC'97 ships both BARs in I/O space, so the kernel-
    /// side claim path filters them before they reach this function and
    /// the loop is a no-op for the audio claim. The shape of this loop
    /// is verbatim-mirrored against [`super::intel::VtdUnit::install_bar_identity_maps`]
    /// — both call `self.map(domain, Iova(aligned_base), PhysAddr(aligned_base),
    /// aligned_len, READ | WRITE)` with the same 4 KiB-aligned base/len
    /// computation. Vendor-specific page-table walk lives below `map()`,
    /// not here, so the two paths are LSP-equivalent at the
    /// `kernel_core::iommu::bar_coverage` layer. Vendor parity for
    /// MMIO-bearing audio targets is a documented follow-up tracked in
    /// the Phase 57 design doc once a second audio backend lands.
    #[allow(dead_code)]
    pub fn install_bar_identity_maps(
        &mut self,
        domain: DomainId,
        bars: &[kernel_core::iommu::bar_coverage::Bar],
    ) -> Result<kernel_core::iommu::bar_coverage::BarCoverage, DomainError> {
        use kernel_core::iommu::bar_coverage::BarCoverage;
        use kernel_core::iommu::contract::{Iova, MapFlags, PhysAddr};
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

// ---------------------------------------------------------------------------
// Phase 67 Track E — multi-BDF domain grouping
// ---------------------------------------------------------------------------

impl AmdViUnit {
    /// Install (or replace) the BDF → alias-group map for this unit.
    ///
    /// Typically called from `iommu::init` after the IVRS tables have
    /// been parsed: walk every IVHD block, run
    /// [`kernel_core::iommu::tables::group_bdfs_by_alias`] over the
    /// decoded entries, and hand the result to this method so that
    /// subsequent [`AmdViUnit::claim_device`] calls return a shared
    /// `DomainId` for BDFs in the same equivalence class.
    ///
    /// Idempotent: passing the same groups again resets any per-group
    /// claims to "unclaimed". Phase 55a kept one domain per claimed
    /// BDF; Phase 67 makes that the default per IVRS group.
    pub fn group_bdf_domains(&mut self, groups: BdfGroups) {
        self.bdf_domains = Some(BdfDomainAssignment::new(groups));
    }

    /// Allocate (or look up) the translation [`DomainId`] for the PCI
    /// requester identified by `bdf`.
    ///
    /// First call for any BDF in an IVRS alias group allocates a fresh
    /// `DomainId` via [`AmdViUnit::create_domain`]. Subsequent calls
    /// for *any* BDF in the same group return the cached `DomainId`
    /// without allocating a new domain.
    ///
    /// **Domain-id allocator only.** This method does not program the
    /// AMD-Vi device-table to route the BDF's DMA traffic at the
    /// returned domain — Phase 55a left that wiring out of the AMD-Vi
    /// path entirely (the registry's `bind_pci_device` returns
    /// `IommuError::NotAvailable` for AMD-Vi units). Until per-BDF
    /// device-table programming lands as a follow-up, the BDF
    /// continues to hit whatever device-table entry the unit started
    /// with. Callers that rely on translated DMA on AMD-Vi platforms
    /// are responsible for that gap; the grouping layer here only
    /// guarantees that two grouped BDFs see the same `DomainId`.
    ///
    /// Phase 67 Track E.2 acceptance: two grouped BDFs share a domain
    /// id. When `bdf_domains` has not been populated (no IVRS or pre-
    /// Phase-67 boot), each call still produces an independent domain
    /// via `create_domain` — the grouping layer is opt-in.
    pub fn claim_device(&mut self, bdf: u16) -> Result<DomainId, IommuError> {
        if !self.brought_up {
            return Err(IommuError::NotAvailable);
        }
        // Pre-Phase-67 fallback: no grouping installed, return a fresh
        // domain on every call (matching Phase 55a behavior).
        if self.bdf_domains.is_none() {
            let domain = self.create_domain()?;
            return Ok(domain.id());
        }
        // Phase 67 path: consult the grouping table. The
        // `BdfDomainAssignment::claim` closure must produce a fresh
        // `DomainId` on first-claim-per-group. We delegate to
        // `create_domain` which already allocates a page-table root,
        // pre-maps reserved regions, and pushes a `DomainState`.
        //
        // We need to move the assignment out of `self` while we hold
        // `&mut self` for `create_domain` (the closure may call
        // `create_domain` on `self`). Use `Option::take` + restore on
        // the way out so the borrow checker is satisfied.
        let mut assign = self.bdf_domains.take().expect("checked is_some above");
        let result = assign.claim(bdf, || {
            let domain = self.create_domain()?;
            Ok(domain.id())
        });
        self.bdf_domains = Some(assign);
        result
    }

    /// Return the cached domain assignment, if any. Diagnostic + test
    /// observer; the kernel uses [`claim_device`] for the production
    /// path.
    #[allow(dead_code)]
    pub fn bdf_domain_for(&self, bdf: u16) -> Option<DomainId> {
        self.bdf_domains.as_ref().and_then(|a| a.domain_for(bdf))
    }
}

/// Build a [`BdfGroups`] table from every IVHD block in the cached
/// IVRS tables. Returns an empty map when no IVRS table is present —
/// the AMD-Vi unit then falls through to the Phase-55a "one BDF, one
/// domain" path with no behavior change.
///
/// Centralises the call into `kernel-core::iommu::tables::group_bdfs_by_alias`
/// so any caller (boot init, unit tests, diagnostic tools) sees the
/// same grouping pass.
pub fn build_bdf_groups_from_ivrs() -> BdfGroups {
    match crate::acpi::ivrs_tables() {
        Some(ivrs) => group_bdfs_by_alias(ivrs),
        None => BdfGroups::default(),
    }
}

// ---------------------------------------------------------------------------
// Phase 67 Track A — fault IRQ trampoline + unit slot array
// ---------------------------------------------------------------------------
//
// AMD-Vi units are owned by the kernel-side IOMMU registry
// (`crate::iommu::registry::REGISTRY`). The registry stores each unit
// behind a `Box<AmdViUnit>` so the fault-IRQ trampoline can hold a
// stable raw pointer to the heap allocation (see
// [`register_unit_slot`] for the safety contract). A second module-
// local `Vec<AmdViUnit>` was once kept here as a rendezvous point
// between bring-up and dispatch, but the registry has been the sole
// authoritative storage since Phase 67 wired the trampoline.

/// Maximum AMD-Vi units the per-vector trampoline can dispatch to.
/// Mirrors the VT-d MAX_FAULT_UNITS sizing in `intel.rs`; one is
/// enough for QEMU, a handful covers bare metal.
const MAX_AMDVI_UNITS: usize = 8;

/// Global registry of live [`AmdViUnit`] pointers that have called
/// [`AmdViUnit::install_fault_handler`]. The IRQ trampoline walks this
/// list and invokes `drain_event_log` on each unit whose hardware may
/// have raised the interrupt — AMD-Vi events route by-MSI rather than
/// by-unit, so we drain everyone on every fire.
///
/// `usize` storage so the entries can live in a `Mutex` without
/// pointer aliasing fights at static-init time. See
/// [`register_unit_slot`] for the safety contract.
static UNIT_SLOTS: spin::Mutex<[Option<usize>; MAX_AMDVI_UNITS]> =
    spin::Mutex::new([None; MAX_AMDVI_UNITS]);

/// Shared atomic holding the device-IRQ vector reserved for the
/// AMD-Vi event-log interrupt. Zero (which is a reserved CPU vector
/// anyway) means "not yet reserved".
static AMDVI_FAULT_VECTOR: AtomicU8 = AtomicU8::new(0);

/// Add `unit` to the trampoline dispatch list. The pointer must remain
/// valid for the rest of the kernel's lifetime — callers are responsible
/// for arranging stable storage before invoking this. The kernel-side
/// caller is `AmdViUnit::install_fault_handler`, which is in turn called
/// from `build_and_bring_up_amdvi` on a `Box<AmdViUnit>` so the captured
/// `self` pointer is the heap address of the boxed unit. The box is
/// then moved into [`registry::RegisteredUnit::AmdVi`] and into the
/// registry `Vec`; both moves only relocate the box pointer, not the
/// heap allocation it owns, so the address recorded here remains valid
/// for the rest of the kernel's lifetime.
///
/// Idempotent: if the same pointer is already registered the call is a
/// no-op. Returns `Err(IommuError::HardwareFault)` only when the slot
/// array is full, which Phase 67 treats as a non-recoverable hardware
/// configuration error.
fn register_unit_slot(unit: *mut AmdViUnit) -> Result<(), IommuError> {
    let mut slots = UNIT_SLOTS.lock();
    let p = unit as usize;
    if slots.contains(&Some(p)) {
        return Ok(());
    }
    if let Some(slot) = slots.iter_mut().find(|s| s.is_none()) {
        *slot = Some(p);
        Ok(())
    } else {
        Err(IommuError::HardwareFault)
    }
}

/// Reserve (or look up) the device-IRQ vector that carries AMD-Vi
/// event-log interrupts. Top-of-bank allocation mirrors the VT-d
/// reserve_iommu_irq strategy in `intel.rs` so MSI vector allocations
/// (which go bottom-up) do not collide.
///
/// Concurrency: IOMMU bring-up is sequential under `iommu::init`, so in
/// practice the call sites never race. The implementation still
/// publishes the reservation via `compare_exchange` so a future caller
/// that runs unit bring-ups in parallel cannot race two `register_device_irq`
/// calls into reserving (and leaking) two distinct vectors. The first
/// thread to publish wins; later threads observe the published vector
/// on a re-load and return it without touching the device-IRQ pool.
fn reserve_fault_vector() -> Result<u8, &'static str> {
    let existing = AMDVI_FAULT_VECTOR.load(Ordering::Acquire);
    if existing != 0 {
        return Ok(existing);
    }
    use crate::arch::x86_64::interrupts::{
        DEVICE_IRQ_VECTOR_BASE, DEVICE_IRQ_VECTOR_COUNT, DeviceIrqEntry, DeviceIrqKind,
        register_device_irq,
    };
    let top = DEVICE_IRQ_VECTOR_BASE + DEVICE_IRQ_VECTOR_COUNT;
    let mut candidate = top - 1;
    loop {
        match register_device_irq(
            candidate,
            DeviceIrqEntry {
                handler: amdvi_fault_irq_trampoline,
                kind: DeviceIrqKind::Msi,
            },
        ) {
            Ok(()) => {
                // Publish the reservation atomically. If a racing
                // thread already published a different vector, drop
                // ours and return the published one — the device-IRQ
                // pool pays for one extra registration but the caller
                // sees a single canonical vector.
                match AMDVI_FAULT_VECTOR.compare_exchange(
                    0,
                    candidate,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return Ok(candidate),
                    Err(published) => return Ok(published),
                }
            }
            Err("device IRQ vector already registered") => {
                if candidate == DEVICE_IRQ_VECTOR_BASE {
                    return Err("no device-IRQ slot available for AMD-Vi fault handler");
                }
                candidate -= 1;
            }
            Err(other) => return Err(other),
        }
    }
}

/// IRQ trampoline — invoked from the device-IRQ stub when the AMD-Vi
/// MSI fires. Snapshots the slot array under the lock, releases it,
/// then drains each registered unit's event log.
///
/// Runs in ISR context. Performs no allocation. Each
/// `drain_event_log` call is bounded by
/// [`EVENT_LOG_RING_DEPTH`] so the worst-case ISR latency is bounded
/// independently of firmware behaviour.
fn amdvi_fault_irq_trampoline() {
    let snapshot: [Option<usize>; MAX_AMDVI_UNITS] = {
        let guard = UNIT_SLOTS.lock();
        *guard
    };
    for slot in snapshot.into_iter().flatten() {
        // SAFETY: UNIT_SLOTS only stores `*mut AmdViUnit` produced by
        // `install_fault_handler` running on a `Box<AmdViUnit>` whose
        // heap allocation is owned by the kernel-side IOMMU registry
        // for the rest of the kernel's lifetime — see
        // [`register_unit_slot`] for the full chain. No two units alias
        // each other (the registry holds each one exactly once). The
        // ISR runs with IF=0 so no concurrent task can be modifying the
        // unit at the same instant, and `drain_event_log` takes
        // `&mut self` but the borrow is purely scoped to this call.
        let unit: &mut AmdViUnit = unsafe { &mut *(slot as *mut AmdViUnit) };
        let _ = unit.drain_event_log();
    }
}

// ---------------------------------------------------------------------------
// Tests — limited to host-compilable assertions on constant math. The
// IommuUnit impl is exercised end-to-end by the Track F.4 contract suite.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // The kernel crate is `no_std` and uses the `test_case` framework
    // (see `crate::test_runner`) rather than libtest's `#[test]`. Using
    // `#[test_case]` lets these sanity checks run inside the kernel
    // test harness alongside the rest of the QEMU-driven suite.
    #[test_case]
    fn device_table_bytes_is_2_mib() {
        assert_eq!(DEVICE_TABLE_BYTES, 2 * 1024 * 1024);
    }

    #[test_case]
    fn device_table_order_matches_2_mib() {
        assert_eq!(1usize << DEVICE_TABLE_ORDER, 512);
    }
}
