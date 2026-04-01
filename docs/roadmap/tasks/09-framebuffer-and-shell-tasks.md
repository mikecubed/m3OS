# Phase 9 — Framebuffer and Shell: Task List

**Status:** Complete
**Source Ref:** phase-9
**Depends on:** Phase 7 ✅, Phase 8 ✅
**Goal:** Add framebuffer text rendering and a minimal interactive shell with built-in commands that exercise the console, keyboard, and storage services.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Framebuffer text output | — | ✅ Done |
| B | Console integration and line input | A | ✅ Done |
| C | Shell and built-in commands | B | ✅ Done |
| D | Validation and documentation | A, B, C | ✅ Done |

---

## Track A — Framebuffer Text Output

### A.1 — Parse framebuffer information for text-mode rendering

**File:** `kernel/src/fb/mod.rs`
**Symbol:** `init`
**Why it matters:** The framebuffer info from the bootloader must be parsed before any pixels can be drawn.

**Acceptance:**
- [x] Framebuffer base address, dimensions, and pixel format are parsed from boot info
- [x] Initialization succeeds and the framebuffer is ready for rendering

---

### A.2 — Add fixed-font text rendering primitives

**File:** `kernel/src/fb/mod.rs`
**Symbol:** `render_char_at`, `write_str`
**Why it matters:** A fixed-width font renderer is the minimum needed for a terminal-style display.

**Acceptance:**
- [x] Characters can be rendered at arbitrary grid positions
- [x] Text rendering supports basic terminal operations (newline, scrolling)

---

## Track B — Console Integration and Line Input

### B.1 — Extend console path for serial and framebuffer output

**File:** `kernel/src/fb/mod.rs`
**Symbol:** `write_str` (module-level)
**Why it matters:** Dual output ensures the OS is usable both headless (serial) and with a display (framebuffer).

**Acceptance:**
- [x] Console output reaches both serial and framebuffer simultaneously

---

### B.2 — Implement line input with basic editing

**File:** `userspace/shell/src/main.rs`
**Symbol:** `read_line`
**Why it matters:** Interactive use requires at minimum line-buffered input with backspace support.

**Acceptance:**
- [x] Line input supports basic editing (backspace, enter)
- [x] Input flows through userspace keyboard services into the shell

---

## Track C — Shell and Built-in Commands

### C.1 — Build a shell with built-in commands

**File:** `userspace/shell/src/main.rs`
**Why it matters:** The shell is the primary user interface for interacting with the OS.

**Acceptance:**
- [x] Shell supports built-in commands: help, echo, ls, cat (at minimum)
- [x] File-oriented commands route through the documented VFS service interfaces

---

### C.2 — Route file commands through service interfaces

**Component:** Shell + VFS integration
**Why it matters:** Using IPC for file access validates the service architecture end-to-end.

**Acceptance:**
- [x] Shell file commands (ls, cat) use the VFS IPC path, not direct kernel calls

---

## Track D — Validation and Documentation

### D.1 — Verify text appears on screen and remains readable

**Why it matters:** Visual output must scroll correctly and not corrupt as output grows.

**Acceptance:**
- [x] Text renders correctly on the framebuffer and scrolls when the screen fills

---

### D.2 — Verify keyboard input flows into the shell

**Why it matters:** End-to-end validation of the keyboard service through to shell line input.

**Acceptance:**
- [x] Keyboard input reaches the shell through userspace services

---

### D.3 — Verify built-in commands exercise the storage stack

**Why it matters:** Confirms the shell, VFS, and filesystem backend work together.

**Acceptance:**
- [x] Built-in commands (ls, cat) produce correct output from the filesystem

---

### D.4 — Document framebuffer ownership and terminal management

**Why it matters:** Future phases adding richer terminals or graphics need to understand the current design.

**Acceptance:**
- [x] Framebuffer ownership, text rendering, and terminal-state management are documented

---

### D.5 — Document shell command model and service dependencies

**Why it matters:** Clarifies which services the shell depends on and how commands are dispatched.

**Acceptance:**
- [x] Shell command model and service dependencies are documented

---

### D.6 — Note on mature terminals, process launching, and graphics

**Why it matters:** Sets expectations for features a production OS would add beyond this toy shell.

**Acceptance:**
- [x] Short note explains how real systems add richer terminals, process launching, and graphics stacks

---

## Documentation Notes

- Phase 9 brought together the console, keyboard, and storage services from Phases 7-8 into a usable interactive shell.
- Framebuffer rendering was added alongside serial output, giving the OS both headless and GUI modes.
- The shell validates the IPC service architecture end-to-end.
