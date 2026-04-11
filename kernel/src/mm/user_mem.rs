//! Safe user-memory access primitives (P12-T005).
//!
//! Kernel syscall handlers must never dereference userspace pointers directly:
//! an unmapped-but-in-range address triggers a ring-0 page fault rather than
//! gracefully returning an error.
//!
//! These functions validate the user virtual address range via page-table
//! translation before copying, returning `Err(())` on any unmapped page.
//! They use `paging::get_mapper()` (which operates on the current CR3) to walk
//! the page tables, so callers must ensure the correct CR3 is active — i.e. the
//! target process's page table must be loaded before calling these functions.

use x86_64::{
    VirtAddr,
    structures::paging::{PageTableFlags, Translate, mapper::TranslateResult},
};

/// Copy `len` bytes from userspace virtual address `src_vaddr` into `dst`.
///
/// Validates each 4 KiB page of the source range using the page tables
/// (must be `PRESENT` and `USER_ACCESSIBLE`). Returns `Err(())` if any page
/// is unmapped, the address range is not in canonical user space, or `len`
/// exceeds the limit defined in `kernel_core::user_range::MAX_COPY_LEN`.
///
/// # Phase 52d B.3 — generation tracking
///
/// Snapshots the address-space generation counter before the copy loop and
/// checks it again afterward.  A mismatch means a mapping-mutating operation
/// (mmap/munmap/mprotect/CoW) raced with this copy.  On SMP this is
/// possible if another thread in the same address space calls munmap while
/// this thread is mid-copy.  The mismatch is logged as a diagnostic — the
/// copy result is still returned because the per-page validation already
/// catches unmapped pages.
///
/// # Safety
///
/// The physical-memory offset (from `crate::mm::phys_offset()`) must be
/// correct and the kernel's page table walk must be coherent with the
/// currently-active CR3. On single-CPU without kernel preemption this holds
/// as long as `mm::init` has run.
fn copy_from_user(dst: &mut [u8], src_vaddr: u64) -> Result<(), ()> {
    let len = dst.len();
    kernel_core::user_range::validate_user_range(src_vaddr, len)?;

    let phys_off = crate::mm::phys_offset();

    // Phase 52d B.3: snapshot generation before copy.
    let gen_before = addr_space_generation();
    let mut local_bumps = 0u64;

    let mut copied = 0usize;
    let mut vaddr = src_vaddr;

    while copied < len {
        let page_offset = (vaddr & 0xFFF) as usize;
        let page_base = vaddr & !0xFFF;
        let avail = (0x1000 - page_offset).min(len - copied);

        let mut need_demand_fault = false;
        {
            let addr_space = crate::process::current_addr_space();
            let _page_table_guard = addr_space.map(|addr_space| addr_space.lock_page_tables());

            let translated = {
                let mapper = unsafe { crate::mm::paging::get_mapper() };
                mapper.translate_addr(VirtAddr::new(page_base))
            };

            if let Some(phys) = translated {
                let mapper = unsafe { crate::mm::paging::get_mapper() };
                if !is_user_accessible(&mapper, VirtAddr::new(page_base)) {
                    return Err(());
                }

                let frame_virt = phys_off + phys.as_u64() + page_offset as u64;
                // SAFETY: frame_virt is a kernel virtual address for a mapped
                // frame, and the address-space lock pins the translation for the
                // duration of this copy chunk.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        frame_virt as *const u8,
                        dst[copied..].as_mut_ptr(),
                        avail,
                    );
                }
            } else {
                need_demand_fault = true;
            }
        }

        if need_demand_fault {
            if !try_demand_fault(page_base) {
                return Err(());
            }
            local_bumps = local_bumps.saturating_add(1);
            continue;
        }

        copied += avail;
        vaddr += avail as u64;
    }

    // Phase 52d B.3: check generation after copy.
    report_generation_divergence(gen_before, local_bumps, "copy_from_user", src_vaddr, len);

    Ok(())
}

/// Copy `src` bytes into userspace virtual address `dst_vaddr`.
///
/// Same validation rules as `copy_from_user`; additionally requires pages to
/// be `WRITABLE`. If a page is a CoW (copy-on-write) page (present, user-
/// accessible, BIT_9 set, but not writable), the CoW fault is resolved
/// in-place before copying.
fn copy_to_user(dst_vaddr: u64, src: &[u8]) -> Result<(), ()> {
    let len = src.len();
    kernel_core::user_range::validate_user_range(dst_vaddr, len)?;

    let phys_off = crate::mm::phys_offset();

    // Phase 52d B.3: snapshot generation before copy.
    let gen_before = addr_space_generation();
    let mut local_bumps = 0u64;

    let mut copied = 0usize;
    let mut vaddr = dst_vaddr;

    while copied < len {
        let page_offset = (vaddr & 0xFFF) as usize;
        let page_base = vaddr & !0xFFF;
        let avail = (0x1000 - page_offset).min(len - copied);

        enum PageWriteAction {
            Copied,
            NeedDemandFault,
            NeedCow,
            Fault,
        }

        let action = {
            let addr_space = crate::process::current_addr_space();
            let _page_table_guard = addr_space.map(|addr_space| addr_space.lock_page_tables());

            let mapper = unsafe { crate::mm::paging::get_mapper() };
            if is_user_writable(&mapper, VirtAddr::new(page_base)) {
                let phys = mapper.translate_addr(VirtAddr::new(page_base)).ok_or(())?;
                let frame_virt = phys_off + phys.as_u64() + page_offset as u64;
                // SAFETY: frame_virt is a kernel virtual address for a mapped
                // writable frame, and the address-space lock pins the
                // translation for the duration of this copy chunk.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src[copied..].as_ptr(),
                        frame_virt as *mut u8,
                        avail,
                    );
                }
                PageWriteAction::Copied
            } else if mapper.translate_addr(VirtAddr::new(page_base)).is_none() {
                PageWriteAction::NeedDemandFault
            } else if is_cow_page(&mapper, VirtAddr::new(page_base)) {
                PageWriteAction::NeedCow
            } else {
                PageWriteAction::Fault
            }
        };

        match action {
            PageWriteAction::Copied => {
                copied += avail;
                vaddr += avail as u64;
            }
            PageWriteAction::NeedDemandFault => {
                if try_demand_fault_writable(page_base) {
                    local_bumps = local_bumps.saturating_add(1);
                    continue;
                }
                return Err(());
            }
            PageWriteAction::NeedCow => {
                if !crate::arch::x86_64::interrupts::resolve_cow_fault(page_base) {
                    log::warn!("[copy_to_user] OOM resolving CoW at {:#x}", page_base);
                    return Err(()); // OOM — callers return EFAULT (no ENOMEM path yet)
                }
                local_bumps = local_bumps.saturating_add(1);
                continue;
            }
            PageWriteAction::Fault => return Err(()),
        }
    }

    // Phase 52d B.3: check generation after copy.
    report_generation_divergence(gen_before, local_bumps, "copy_to_user", dst_vaddr, len);

    Ok(())
}

/// Like [`try_demand_fault`] but also requires the VMA to be writable.
/// Used by `copy_to_user` to avoid allocating frames for read-only VMAs
/// that would immediately fail the writability check.
fn try_demand_fault_writable(page_base: u64) -> bool {
    crate::arch::x86_64::interrupts::demand_map_vma_page_from_kernel(page_base, true)
}

/// Phase 36: demand-fault a user page if it is in a valid VMA but not yet
/// present in the page table. Called from `copy_from_user` when the page
/// table walk finds no mapping.
///
/// Returns `true` if the page was successfully demand-mapped; `false` if the
/// address is not in any VMA or allocation failed.
fn try_demand_fault(page_base: u64) -> bool {
    crate::arch::x86_64::interrupts::demand_map_vma_page_from_kernel(page_base, false)
}

/// Check whether the page at `vaddr` is a CoW page (present, user-accessible,
/// BIT_9 marker set, but not writable).
fn is_cow_page(mapper: &x86_64::structures::paging::OffsetPageTable<'_>, vaddr: VirtAddr) -> bool {
    match mapper.translate(vaddr) {
        TranslateResult::Mapped { flags, .. } => {
            flags.contains(
                PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::BIT_9,
            ) && !flags.contains(PageTableFlags::WRITABLE)
        }
        _ => false,
    }
}

/// Check whether the page at `vaddr` is mapped USER_ACCESSIBLE.
fn is_user_accessible(
    mapper: &x86_64::structures::paging::OffsetPageTable<'_>,
    vaddr: VirtAddr,
) -> bool {
    match mapper.translate(vaddr) {
        TranslateResult::Mapped { flags, .. } => {
            flags.contains(PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE)
        }
        _ => false,
    }
}

/// Check whether the page at `vaddr` is USER_ACCESSIBLE and WRITABLE.
fn is_user_writable(
    mapper: &x86_64::structures::paging::OffsetPageTable<'_>,
    vaddr: VirtAddr,
) -> bool {
    match mapper.translate(vaddr) {
        TranslateResult::Mapped { flags, .. } => flags.contains(
            PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE,
        ),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Phase 52d B.3: address-space generation helpers for user-copy diagnostics
// ---------------------------------------------------------------------------

/// Read the current process's address-space generation counter.
///
/// Returns `None` if the process has no dedicated address space (kernel tasks,
/// early-boot processes) — generation tracking is effectively disabled for
/// those.
fn addr_space_generation() -> Option<u64> {
    let pid = crate::process::current_pid();
    if pid == 0 {
        return None;
    }
    let table = crate::process::PROCESS_TABLE.lock();
    table
        .find(pid)
        .and_then(|p| p.addr_space.as_ref().map(|a| a.generation()))
}

/// Compare the starting generation plus the copy's expected local bumps with
/// the current address-space generation.
///
/// `local_bumps` counts mapping mutations triggered by the copy itself
/// (demand faults or CoW resolution).  A mismatch after subtracting those
/// expected local changes indicates a concurrent or otherwise untracked
/// mapping mutation while the copy was in flight.
fn report_generation_divergence(
    gen_before: Option<u64>,
    local_bumps: u64,
    caller: &str,
    vaddr: u64,
    len: usize,
) {
    let Some(gen_before) = gen_before else {
        return;
    };
    let Some(gen_after) = addr_space_generation() else {
        return;
    };
    let expected_after = gen_before.saturating_add(local_bumps);
    if gen_after != expected_after {
        log::warn!(
            "[user_mem] {}: address-space generation divergence (gen {} -> {}, expected {} after {} local bumps) \
             during copy at {:#x} len={:#x} — concurrent or untracked mapping mutation detected",
            caller,
            gen_before,
            gen_after,
            expected_after,
            local_bumps,
            vaddr,
            len,
        );
    }
}

// ---------------------------------------------------------------------------
// Typed user-buffer wrappers (Phase 52b Track D)
// ---------------------------------------------------------------------------

/// A validated read-only view of user memory. Kernel can copy data FROM
/// this user buffer TO a kernel buffer.
pub struct UserSliceRo {
    vaddr: u64,
    len: usize,
}

/// A validated write-only view of user memory. Kernel can copy data FROM
/// a kernel buffer INTO this user buffer.
pub struct UserSliceWo {
    vaddr: u64,
    len: usize,
}

/// A validated read-write view of user memory.
#[allow(dead_code)]
pub struct UserSliceRw {
    vaddr: u64,
    len: usize,
}

impl UserSliceRo {
    /// Validate and create a read-only user slice.
    pub fn new(vaddr: u64, len: usize) -> Result<Self, ()> {
        validate_user_range(vaddr, len)?;
        Ok(Self { vaddr, len })
    }

    /// Copy data from user memory into a kernel buffer.
    pub fn copy_to_kernel(&self, dst: &mut [u8]) -> Result<(), ()> {
        if dst.len() > self.len {
            return Err(());
        }
        copy_from_user(dst, self.vaddr)
    }

    /// Read a single `Copy` value from user memory.
    ///
    /// # Safety
    ///
    /// `T` must be a type where every bit pattern is valid (e.g. integer
    /// primitives, `#[repr(C)]` structs of such types). Using types like
    /// `bool`, `char`, enums, or `NonZero*` is undefined behavior because
    /// user-supplied bytes may not form a valid value.
    #[allow(dead_code)]
    pub unsafe fn read_val<T: Copy>(&self) -> Result<T, ()> {
        if core::mem::size_of::<T>() > self.len {
            return Err(());
        }
        let mut buf = [0u8; 256]; // Stack buffer — values > 256 bytes are rejected above
        let size = core::mem::size_of::<T>();
        if size > buf.len() {
            return Err(());
        }
        copy_from_user(&mut buf[..size], self.vaddr)?;
        // SAFETY: buf[..size] is initialized and size_of::<T>() == size.
        // Caller guarantees T allows all bit patterns.
        Ok(unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const T) })
    }

    /// Return the user virtual address.
    #[allow(dead_code)]
    pub fn addr(&self) -> u64 {
        self.vaddr
    }
    /// Return the validated length.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }
}

impl UserSliceWo {
    /// Validate and create a write-only user slice.
    pub fn new(vaddr: u64, len: usize) -> Result<Self, ()> {
        validate_user_range(vaddr, len)?;
        Ok(Self { vaddr, len })
    }

    /// Copy data from a kernel buffer into user memory.
    pub fn copy_from_kernel(&self, src: &[u8]) -> Result<(), ()> {
        if src.len() > self.len {
            return Err(());
        }
        copy_to_user(self.vaddr, src)
    }

    /// Write a single `Copy` value to user memory.
    ///
    /// # Safety
    ///
    /// `T` must not contain padding bytes. Padding is uninitialized memory
    /// and copying it to userspace leaks kernel stack/register contents.
    /// Use only with integer primitives or `#[repr(C)]` structs that have
    /// no padding (e.g. all fields naturally aligned with no gaps).
    #[allow(dead_code)]
    pub unsafe fn write_val<T: Copy>(&self, val: &T) -> Result<(), ()> {
        let size = core::mem::size_of::<T>();
        if size > self.len {
            return Err(());
        }
        // SAFETY: Caller guarantees T has no padding bytes.
        let bytes = unsafe { core::slice::from_raw_parts(val as *const T as *const u8, size) };
        copy_to_user(self.vaddr, bytes)
    }

    /// Return the user virtual address.
    #[allow(dead_code)]
    pub fn addr(&self) -> u64 {
        self.vaddr
    }
    /// Return the validated length.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }
}

#[allow(dead_code)]
impl UserSliceRw {
    /// Validate and create a read-write user slice.
    pub fn new(vaddr: u64, len: usize) -> Result<Self, ()> {
        validate_user_range(vaddr, len)?;
        Ok(Self { vaddr, len })
    }

    /// Copy data from user memory into a kernel buffer.
    pub fn copy_to_kernel(&self, dst: &mut [u8]) -> Result<(), ()> {
        if dst.len() > self.len {
            return Err(());
        }
        copy_from_user(dst, self.vaddr)
    }

    /// Copy data from a kernel buffer into user memory.
    pub fn copy_from_kernel(&self, src: &[u8]) -> Result<(), ()> {
        if src.len() > self.len {
            return Err(());
        }
        copy_to_user(self.vaddr, src)
    }

    /// Read a single `Copy` value from user memory.
    ///
    /// # Safety
    ///
    /// `T` must be a type where every bit pattern is valid (e.g. integer
    /// primitives, `#[repr(C)]` structs of such types). Using types like
    /// `bool`, `char`, enums, or `NonZero*` is undefined behavior because
    /// user-supplied bytes may not form a valid value.
    pub unsafe fn read_val<T: Copy>(&self) -> Result<T, ()> {
        if core::mem::size_of::<T>() > self.len {
            return Err(());
        }
        let mut buf = [0u8; 256];
        let size = core::mem::size_of::<T>();
        if size > buf.len() {
            return Err(());
        }
        copy_from_user(&mut buf[..size], self.vaddr)?;
        // SAFETY: buf[..size] is initialized and size_of::<T>() == size.
        // Caller guarantees T allows all bit patterns.
        Ok(unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const T) })
    }

    /// Write a single `Copy` value to user memory.
    ///
    /// # Safety
    ///
    /// `T` must not contain padding bytes. Padding is uninitialized memory
    /// and copying it to userspace leaks kernel stack/register contents.
    /// Use only with integer primitives or `#[repr(C)]` structs that have
    /// no padding (e.g. all fields naturally aligned with no gaps).
    pub unsafe fn write_val<T: Copy>(&self, val: &T) -> Result<(), ()> {
        let size = core::mem::size_of::<T>();
        if size > self.len {
            return Err(());
        }
        // SAFETY: Caller guarantees T has no padding bytes.
        let bytes = unsafe { core::slice::from_raw_parts(val as *const T as *const u8, size) };
        copy_to_user(self.vaddr, bytes)
    }

    /// Return the user virtual address.
    pub fn addr(&self) -> u64 {
        self.vaddr
    }
    /// Return the validated length.
    pub fn len(&self) -> usize {
        self.len
    }
}

/// Validate that a user address range is in canonical user space and within limits.
///
/// Delegates to `kernel_core::user_range::validate_user_range` so the kernel
/// and host tests share a single implementation.
fn validate_user_range(vaddr: u64, len: usize) -> Result<(), ()> {
    kernel_core::user_range::validate_user_range(vaddr, len)
}
