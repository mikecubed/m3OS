# Phase 03 — Interrupts: Task List

**Status:** Complete
**Source Ref:** phase-03
**Depends on:** Phase 1 ✅
**Goal:** Set up the GDT/TSS, build the IDT with exception and hardware interrupt handlers, initialize the PIC, and wire timer and keyboard IRQs so the kernel can respond to hardware events.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | GDT, TSS, interrupt stacks | Phase 1 | ✅ Done |
| B | IDT + exception handlers | A | ✅ Done |
| C | PIC + hardware IRQs | B | ✅ Done |
| D | Validation + docs | C | ✅ Done |

---

## Track A — GDT, TSS, Interrupt Stacks

### A.1 — Set up the GDT, TSS, and interrupt stack entries

**File:** `kernel/src/arch/x86_64/gdt.rs`
**Symbols:** `GDT`, `TSS`, `Selectors`
**Why it matters:** The GDT defines segment descriptors for kernel and user code/data, and the TSS provides the interrupt stack table (IST) entries needed for safe double-fault handling.

**Acceptance:**
- [x] GDT has kernel code, kernel data, user data, user code, and TSS segments
- [x] TSS has at least one IST entry for the double-fault handler
- [x] `syscall_stack_top()` provides the kernel syscall stack address

---

## Track B — IDT + Exception Handlers

### B.1 — Build the IDT and install exception handlers

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbols:** `IDT`, `init`, `breakpoint_handler`, `page_fault_handler`, `general_protection_fault_handler`, `double_fault_handler`
**Why it matters:** Without an IDT, any CPU exception causes a triple fault and immediate reset rather than a debuggable error message.

**Acceptance:**
- [x] Breakpoint, page fault, general protection fault, and double fault handlers are installed
- [x] Double fault handler uses a dedicated IST stack
- [x] Exception handlers emit readable diagnostic output
- [x] `init()` loads the IDT

---

## Track C — PIC + Hardware IRQs

### C.1 — Initialize and remap the PIC

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Why it matters:** The PIC must be remapped so hardware IRQs use a vector range that does not collide with CPU exceptions (vectors 0-31).

**Acceptance:**
- [x] PIC is initialized with a known vector offset
- [x] Hardware IRQs are delivered to the correct IDT entries

### C.2 — Implement timer and keyboard interrupt handlers

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbols:** `timer_handler`, `keyboard_handler`
**Why it matters:** The timer IRQ drives preemptive scheduling (Phase 4) and the keyboard IRQ provides input — both must be minimal, non-allocating, and send EOI promptly.

**Acceptance:**
- [x] Timer interrupt fires consistently and records observable progress
- [x] Keyboard interrupt reads scancodes and places them in a buffer
- [x] All interrupt handlers are minimal, non-allocating, and explicit about EOI

---

## Track D — Validation + Docs

### D.1 — Validate interrupt behavior

**Why it matters:** Confirms that the interrupt path is functional before the scheduler depends on it.

**Acceptance:**
- [x] Breakpoint trap produces readable diagnostic output
- [x] Timer interrupts fire consistently enough to support scheduling
- [x] Keyboard input reaches the log or buffer without blocking in the IRQ path
- [x] Fault handlers emit enough context to debug failures

### D.2 — Document the interrupt architecture

**Why it matters:** The interrupt path, vector layout, and ISR discipline are critical knowledge for anyone modifying kernel code.

**Acceptance:**
- [x] Interrupt path, vector layout, and why IRQ handlers must stay small are documented
- [x] Purpose of the TSS and interrupt stacks is documented at a high level
- [x] A note explains how mature kernels use APIC-style interrupt routing and deferred work models

---

## Documentation Notes

- Adds the `arch/x86_64/gdt.rs` and `arch/x86_64/interrupts.rs` modules.
- Phase 3 depends only on Phase 1 (serial output), not Phase 2 (memory), because interrupt setup does not require heap allocation.
