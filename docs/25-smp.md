# Phase 25 — Symmetric Multiprocessing (SMP)

## Overview

Phase 25 adds multi-core support to m3OS. All Application Processors (APs)
discovered in the ACPI MADT are brought online via the INIT-SIPI IPI sequence.
Each core has its own GDT, TSS, kernel stack, double-fault stack, LAPIC timer,
and scheduler loop. Inter-processor interrupts (IPIs) provide reschedule and
TLB shootdown mechanisms.

## AP Startup Sequence

Each AP transitions through four stages to reach 64-bit long mode and Rust code:

```
INIT IPI → SIPI → 16-bit real mode → 32-bit protected mode → 64-bit long mode → Rust ap_entry()
```

### Stage 1: 16-bit Real Mode (trampoline at physical 0x8000)

The trampoline page is allocated at physical address 0x8000 (below 1 MiB, as
required by the SIPI vector encoding). The BSP copies hand-assembled machine
code to this page along with data fields (GDT, GDTR, PML4 address, stack
pointer, entry point, per-core data pointer).

The 16-bit code:
1. Disables interrupts (`cli`)
2. Sets DS=0 for absolute addressing
3. Loads a temporary GDT via `lgdt`
4. Sets CR0.PE to enter protected mode
5. Far-jumps to the 32-bit code segment

### Stage 2: 32-bit Protected Mode

1. Loads flat data segments (base=0, limit=4GB)
2. Enables PAE (CR4.PAE)
3. Loads the kernel PML4 into CR3
4. Sets IA32_EFER.LME **and IA32_EFER.NXE** (critical — see below)
5. Sets CR0.PG to enable paging (entering compatibility mode)
6. Far-jumps to the 64-bit code segment

### Stage 3: 64-bit Long Mode

1. Loads 64-bit data segments
2. Loads the AP's pre-assigned kernel stack
3. Loads the per-core data pointer into RDI
4. Jumps to the Rust `ap_entry()` function

### Stage 4: Rust AP Entry

1. Loads BSP's CR4 value (for PGE and other feature flags)
2. Loads the AP's pre-allocated GDT and TSS
3. Loads the shared IDT
4. Sets `gs_base` MSR to point to the AP's `PerCoreData`
5. Initializes the AP's LAPIC (enable + timer at 100 Hz)
6. Sets `is_online = true`
7. Spawns a per-core idle task
8. Enters the scheduler loop (`task::run()`)

## Critical: EFER.NXE Requirement

The bootloader's page tables mark data pages with the NX (No Execute) bit
(bit 63 of page table entries). Per the Intel manual, when `IA32_EFER.NXE = 0`,
bit 63 of any paging-structure entry is **reserved**. Setting a reserved bit
causes a page fault (#PF) on any access to the page.

The AP trampoline MUST set `EFER.NXE` (bit 11) along with `EFER.LME` (bit 8)
before enabling paging. Without NXE, APs cannot access any kernel data pages
(including kernel statics, phys_offset MMIO mappings, and heap memory that
happens to be on NX-marked pages).

## Per-Core Data Layout

Each core has a `PerCoreData` struct accessed via the `IA32_GS_BASE` MSR:

| Field | Purpose |
|---|---|
| `self_ptr` | Self-pointer at offset 0 for O(1) access |
| `core_id` | Logical core index (0 = BSP) |
| `apic_id` | LAPIC ID from the MADT |
| `is_online` | AtomicBool — set when AP finishes init |
| `tss_ptr` | Pointer to this core's TSS (for RSP0 updates) |
| `gdt_ptr` | Pointer to this core's GDT |
| `kernel_stack_top` | Top of this core's syscall/kernel stack |
| `scheduler_rsp` | Scheduler loop RSP (for yield/block) |
| `reschedule` | Per-core reschedule flag (replaces global RESCHEDULE) |
| `current_task_idx` | Index of the currently running task |
| `lapic_virt_base` | LAPIC virtual address (phys_offset + LAPIC phys) |
| `lapic_ticks_per_ms` | BSP-calibrated LAPIC timer rate |

GDTs, TSSs, and stacks for APs are heap-allocated and leaked by the BSP
before sending SIPIs. This avoids heap access from the AP before it is
fully initialized.

## TLB Shootdown

When a page mapping is removed, `tlb_shootdown(addr)` invalidates the page
on all cores:

1. `invlpg` on the local core
2. Send TLB shootdown IPI (vector 0xFD) to all other cores
3. Spin-wait for `pending_acks` to reach 0

Single-core fast path: if only 1 core is online, skip the IPI.

Currently, `munmap` is a stub (no-op), so the TLB shootdown hook is not
yet wired. CoW fault resolution happens in interrupt context on a single
core and uses local `invlpg` only.

## Spinlock Audit

All global locks audited for SMP safety:

| Lock | Assessment |
|---|---|
| `SCHEDULER` | Safe — all cores acquire/release correctly |
| `FRAME_ALLOCATOR` | Safe — single Mutex, no lock held across switch |
| `ENDPOINTS` | Safe — per-endpoint lock not needed at current scale |
| `PROCESS_TABLE` | Safe — Mutex protects all access |
| `STDIN_BUFFER` | Safe — low contention, keyboard ISR on BSP only |
| `FRAMEBUFFER` | Safe — Mutex protects all access |

### SMP-Unsafe Statics (deferred)

The syscall entry path has `static mut` variables that are NOT per-core:

- `SYSCALL_STACK_TOP` — kernel stack for ring-3 → ring-0 transitions
- `SYSCALL_USER_RSP`, `SYSCALL_USER_RBX`, ..., `SYSCALL_USER_RFLAGS`
- `SYSCALL_ARG3` — mmap flags
- `FORK_ENTRY_CTX` — fork child register restore
- `CURRENT_PID` — global AtomicU32

These are written by the assembly `syscall_entry` stub on every syscall.
Making them per-core requires changing the assembly to use `gs`-relative
addressing (reading from `PerCoreData` fields). This is deferred.

**Mitigation**: Only the BSP dispatches non-idle tasks. APs run their idle
tasks and handle timer interrupts. This prevents userspace from running on
APs where the syscall statics would be corrupted.

## QEMU Configuration

SMP is enabled with `-smp 4` in the xtask runner. The kernel discovers all
4 cores from the MADT and boots 3 APs.
