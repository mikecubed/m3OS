//! Phase 84 Track A.1 — host-testable model of the KPTI page-table pair.
//!
//! KPTI splits each process's single PML4 into a **pair**:
//! * the **kernel** PML4 — the full map (unchanged from today's
//!   `new_process_page_table`), used while ring 0 runs;
//! * the **user** PML4 — the user-mapping slots ([`USER_PML4_SLOTS`]: the
//!   image/brk/mmap slot `PML4[0]` and the stack slot `PML4[255]`, shared
//!   verbatim with the kernel half) plus a **minimal entry set** (the
//!   syscall/IRQ trampoline text, the IDT, the GDT/TSS, the per-CPU entry
//!   stack + per-core data), and *nothing else* of the kernel. The user-mode
//!   CR3 points here, so the CPU cannot even speculatively reach kernel
//!   `.text` / heap / the physical direct map → Meltdown is defeated.
//!
//! The actual page-table construction is in `kernel/src/mm` (it touches live
//! hardware page tables). This module pins the *policy* — which top-level PML4
//! slots the user half may carry, and the invariant a self-test asserts — as
//! pure, host-tested logic so the rule is reviewable without QEMU. The kernel
//! self-test walks the real user PML4 and checks the same invariant.
//!
//! **Important subtlety the model encodes:** the minimal entry set is mapped at
//! **4 KiB-page granularity through fresh lower-level tables**, never by
//! cloning a whole kernel `PML4[i]` slot — cloning `PML4[256]` (the direct map)
//! would re-expose all of physical memory and silently defeat KPTI. So a
//! kernel top-level slot is legal in the user half **only** for the
//! user-mapping slots ([`USER_PML4_SLOTS`]) and for slots that hold
//! *exclusively* entry-set pages via private sub-tables.

/// Role of a kernel virtual range, for the user-half admission decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelRange {
    /// Lower-half user pages (`PML4[0]`). Always present in the user half.
    UserLowerHalf,
    /// Kernel `.text` / `.rodata` / `.data`. Must be **absent** from the user
    /// half (its handlers run only after the CR3 switch to the kernel half).
    KernelImage,
    /// Kernel heap. Must be absent.
    KernelHeap,
    /// The physical-memory direct map (the Meltdown prize). Must be absent.
    DirectMap,
    /// A minimal-entry-set page (trampoline text / IDT / GDT / TSS / per-CPU
    /// entry stack + data). Mapped into the user half at page granularity.
    EntrySet,
}

impl KernelRange {
    /// Whether a range of this role may be reachable from the **user** PML4.
    #[inline]
    pub fn allowed_in_user_half(self) -> bool {
        matches!(self, KernelRange::UserLowerHalf | KernelRange::EntrySet)
    }
}

/// PML4 slot of the kernel image: the PIE base is relocated by a fixed 1 TiB
/// (`0x100_0000_0000` → bits 39..47 = 2), making it the lower canonical
/// half's **single kernel occupant**. Everything else the kernel owns (heap
/// `PML4[256]`, kernel stacks `PML4[257]`, the physical direct map) lives in
/// the upper half.
pub const KERNEL_IMAGE_PML4_SLOT: usize = 2;

/// The top-level slots that carry **user** mappings in the m3OS VA layout —
/// the slots the user half shares verbatim with the kernel half so user
/// mappings created after the pair is built (mmap/brk growth, demand-paged
/// stack) stay in sync automatically:
///
/// - `PML4[0]` — the ELF image + brk (from `USER_VADDR_MIN` `0x20_0000`) and
///   the anonymous-mmap region (`ANON_MMAP_BASE` `0x20_0000_0000` + ASLR,
///   still far below the 512 GiB slot boundary).
/// - `PML4[255]` — the user stack (`ELF_STACK_TOP` `0x7FFF_FF00_0000` minus
///   the ASLR jitter, plus its demand-page window).
///
/// A user region moving to (or a new one appearing in) any other slot MUST be
/// added here, or processes touching it fault-loop on the user CR3 (its
/// mapping exists only in the kernel half). The sub-tree of each shared slot
/// must already exist in the kernel half when the pair is built — true today:
/// the ELF loader maps image + stack before any `AddressSpace` is created.
pub const USER_PML4_SLOTS: [usize; 2] = [0, 255];

/// Whether `PML4[idx]` is one of the user-mapping slots ([`USER_PML4_SLOTS`]).
#[inline]
pub fn is_user_pml4_slot(idx: usize) -> bool {
    USER_PML4_SLOTS.contains(&idx)
}

/// Decide whether a whole top-level `PML4[idx]` slot may be **cloned verbatim**
/// into the user half. Only the user-mapping slots ([`USER_PML4_SLOTS`])
/// qualify — sharing them is the point of the pair (one set of user page
/// tables, two views). Every kernel slot must instead be rebuilt at page
/// granularity (see module docs), so this returns `false` for the kernel
/// image slot and the whole upper half. Guards against the classic
/// "clone the direct-map slot" KPTI no-op.
#[inline]
pub fn may_clone_slot_into_user_half(idx: usize) -> bool {
    is_user_pml4_slot(idx)
}

/// The KPTI walk invariant for the **user** PML4, expressed over a flat list of
/// `(role, present_in_user_half)` observations a walker produces. Returns `Ok`
/// iff every kernel-secret range (`KernelImage`/`KernelHeap`/`DirectMap`) is
/// absent and the user lower half + entry set are present. The kernel self-test
/// feeds real walk results here; host tests feed synthetic ones.
pub fn check_user_half_invariant<'a>(
    observed: impl IntoIterator<Item = &'a (KernelRange, bool)>,
) -> Result<(), KptiInvariantError> {
    let mut saw_user = false;
    let mut saw_entry_set = false;
    for (role, present) in observed {
        match (*role, *present) {
            // A secret range present in the user half is a hard violation.
            (KernelRange::KernelImage, true) => return Err(KptiInvariantError::KernelImagePresent),
            (KernelRange::KernelHeap, true) => return Err(KptiInvariantError::KernelHeapPresent),
            (KernelRange::DirectMap, true) => return Err(KptiInvariantError::DirectMapPresent),
            (KernelRange::UserLowerHalf, true) => saw_user = true,
            (KernelRange::EntrySet, true) => saw_entry_set = true,
            _ => {}
        }
    }
    if !saw_user {
        return Err(KptiInvariantError::UserHalfMissing);
    }
    if !saw_entry_set {
        return Err(KptiInvariantError::EntrySetMissing);
    }
    Ok(())
}

/// Reason a user-PML4 walk failed the KPTI invariant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KptiInvariantError {
    /// Kernel `.text`/`.rodata`/`.data` reachable from the user CR3.
    KernelImagePresent,
    /// Kernel heap reachable from the user CR3.
    KernelHeapPresent,
    /// The physical direct map reachable from the user CR3 (full Meltdown).
    DirectMapPresent,
    /// The user lower half is not mapped (process cannot run).
    UserHalfMissing,
    /// The minimal entry set is not mapped (entry would fault before the CR3
    /// switch can reach the kernel half → triple fault).
    EntrySetMissing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_user_slots_may_be_cloned() {
        for idx in USER_PML4_SLOTS {
            assert!(
                may_clone_slot_into_user_half(idx),
                "PML4[{idx}] carries user mappings and must be shared with the user half — \
                 omitting it fault-loops every process that touches the region on the user CR3"
            );
        }
        for idx in 0..512 {
            if USER_PML4_SLOTS.contains(&idx) {
                continue;
            }
            assert!(
                !may_clone_slot_into_user_half(idx),
                "PML4[{idx}] (a kernel slot) must NOT be cloned verbatim into the user half — \
                 cloning e.g. the direct-map slot silently defeats KPTI"
            );
        }
        // The lower half's single kernel occupant and the upper half's first
        // slot, called out explicitly (the two most dangerous accidents).
        assert!(!may_clone_slot_into_user_half(KERNEL_IMAGE_PML4_SLOT));
        assert!(!may_clone_slot_into_user_half(256));
    }

    #[test]
    fn user_slots_are_lower_half_and_exclude_the_kernel_image() {
        for idx in USER_PML4_SLOTS {
            assert!(idx < 256, "user mappings live in the lower canonical half");
            assert_ne!(
                idx, KERNEL_IMAGE_PML4_SLOT,
                "the kernel image slot can never carry user mappings"
            );
        }
    }

    #[test]
    fn allowed_in_user_half_roles() {
        assert!(KernelRange::UserLowerHalf.allowed_in_user_half());
        assert!(KernelRange::EntrySet.allowed_in_user_half());
        assert!(!KernelRange::KernelImage.allowed_in_user_half());
        assert!(!KernelRange::KernelHeap.allowed_in_user_half());
        assert!(!KernelRange::DirectMap.allowed_in_user_half());
    }

    #[test]
    fn invariant_passes_for_correct_user_pml4() {
        // A correct user PML4: user half + entry set present, no kernel secrets.
        let obs = [
            (KernelRange::UserLowerHalf, true),
            (KernelRange::EntrySet, true),
            (KernelRange::KernelImage, false),
            (KernelRange::KernelHeap, false),
            (KernelRange::DirectMap, false),
        ];
        assert_eq!(check_user_half_invariant(obs.iter()), Ok(()));
    }

    #[test]
    fn invariant_catches_direct_map_leak() {
        // The Redox trap: the kernel half (here, the direct map) is still
        // present in the user PML4 — KPTI is a silent no-op.
        let obs = [
            (KernelRange::UserLowerHalf, true),
            (KernelRange::EntrySet, true),
            (KernelRange::DirectMap, true),
        ];
        assert_eq!(
            check_user_half_invariant(obs.iter()),
            Err(KptiInvariantError::DirectMapPresent)
        );
    }

    #[test]
    fn invariant_catches_kernel_image_and_heap_leaks() {
        assert_eq!(
            check_user_half_invariant(
                [
                    (KernelRange::UserLowerHalf, true),
                    (KernelRange::EntrySet, true),
                    (KernelRange::KernelImage, true),
                ]
                .iter()
            ),
            Err(KptiInvariantError::KernelImagePresent)
        );
        assert_eq!(
            check_user_half_invariant(
                [
                    (KernelRange::UserLowerHalf, true),
                    (KernelRange::EntrySet, true),
                    (KernelRange::KernelHeap, true),
                ]
                .iter()
            ),
            Err(KptiInvariantError::KernelHeapPresent)
        );
    }

    #[test]
    fn invariant_catches_missing_entry_set_or_user_half() {
        // No entry set → the first instruction after SYSCALL/IRQ faults before
        // the CR3 switch can reach the kernel half (triple fault).
        assert_eq!(
            check_user_half_invariant([(KernelRange::UserLowerHalf, true)].iter()),
            Err(KptiInvariantError::EntrySetMissing)
        );
        // No user half → the process cannot execute at all.
        assert_eq!(
            check_user_half_invariant([(KernelRange::EntrySet, true)].iter()),
            Err(KptiInvariantError::UserHalfMissing)
        );
    }
}
