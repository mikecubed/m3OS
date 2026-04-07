# Phase 47 — DOOM: Task List

**Status:** Complete
**Source Ref:** phase-47
**Depends on:** Phase 9 (Framebuffer) ✅, Phase 12 (POSIX Compat) ✅, Phase 24 (Persistent Storage) ✅
**Goal:** Make DOOM playable inside m3OS. Expose the UEFI framebuffer to userspace via
new syscalls, deliver raw PS/2 scancodes for key-down/key-up input, port the
doomgeneric platform layer as a statically-linked C binary, and place the shareware
`doom1.wad` on the ext2 disk. The game must render to the framebuffer at a playable
frame rate and accept keyboard input for full gameplay.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Kernel framebuffer syscalls (`sys_framebuffer_info`, `sys_framebuffer_mmap`) | — | Complete |
| B | Kernel raw scancode syscall (`sys_read_scancode`) | — | Complete |
| C | Framebuffer console yield/restore (dual-mode console) | A | Complete |
| D | doomgeneric platform layer (C userspace code) | A, B | Complete |
| E | xtask build integration and WAD disk image | D | Complete |
| F | Integration testing and documentation | A–E | Complete |

---

## Track A — Kernel Framebuffer Syscalls

Expose framebuffer metadata and physical memory to userspace so graphical programs
can write pixels directly. The kernel already holds framebuffer info in the
`FbConsole` struct (`fb::CONSOLE`); these syscalls make it available to ring 3.

### A.1 — `sys_framebuffer_info` syscall

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_framebuffer_info`
**Why it matters:** Userspace needs the framebuffer dimensions, pitch, bytes-per-pixel,
and pixel format before it can render anything — this is the discovery mechanism.

**Acceptance:**
- [ ] New syscall number `0x1002` dispatched in the `match number` block at the `syscall_handler` function
- [ ] Writes a packed struct (`FbInfo { width: u32, height: u32, stride: u32, bpp: u32, pixel_format: u32 }`) to a user-supplied buffer pointer
- [ ] Reads framebuffer metadata from `fb::CONSOLE` (fields: `width`, `height`, `stride`, `bytes_per_pixel`, `pixel_format`)
- [ ] Returns 0 on success, `NEG_EINVAL` if no framebuffer is available or buffer pointer is invalid
- [ ] Validates user pointer with the same bounds check used by other syscalls (e.g. `USER_LIMIT`)

### A.2 — `sys_framebuffer_mmap` syscall

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/mm/user_space.rs`

**Symbol:** `sys_framebuffer_mmap`
**Why it matters:** DOOM must write pixels directly to the framebuffer from userspace;
this syscall maps the framebuffer physical pages into the calling process's virtual
address space using the existing `map_user_frames` infrastructure.

**Acceptance:**
- [ ] New syscall number `0x1003` dispatched in the `syscall_handler` match block
- [ ] Computes the framebuffer physical base address from `FbConsole.buf` and the kernel physical offset (`mm::PHYS_OFFSET`)
- [ ] Calls `map_user_frames` (from `kernel/src/mm/user_space.rs`) to map the framebuffer physical frames into the process's page table with `USER_ACCESSIBLE | WRITABLE` flags
- [ ] Returns the userspace virtual address of the mapped framebuffer on success
- [ ] Returns `NEG_EINVAL` if no framebuffer exists or if page table mapping fails, `NEG_EBUSY` if another process already owns the framebuffer
- [ ] Records the mapping in the process's `mappings` vector as a `MemoryMapping` so `munmap` can clean it up

### A.3 — Syscall constants in `syscall-lib`

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `SYS_FRAMEBUFFER_INFO`, `SYS_FRAMEBUFFER_MMAP`
**Why it matters:** Rust userspace programs (and the C platform layer via inline
assembly or direct syscall invocation) need named constants for the new syscall
numbers to avoid magic numbers.

**Acceptance:**
- [ ] `pub const SYS_FRAMEBUFFER_INFO: u64 = 0x1002;` defined after `SYS_MEMINFO`
- [ ] `pub const SYS_FRAMEBUFFER_MMAP: u64 = 0x1003;` defined after `SYS_FRAMEBUFFER_INFO`
- [ ] High-level wrapper `pub fn framebuffer_info(buf: &mut [u8]) -> isize` calls `syscall2`
- [ ] High-level wrapper `pub fn framebuffer_mmap() -> u64` calls `syscall0` and returns virtual address

---

## Track B — Kernel Raw Scancode Syscall

Expose raw PS/2 make/break scancodes to userspace. The existing `read_scancode()`
function in `interrupts.rs` already consumes from the ring buffer — the new syscall
bridges that to userspace.

### B.1 — `sys_read_scancode` syscall

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_read_scancode`
**Why it matters:** DOOM needs key-down and key-up events for movement and shooting;
the cooked terminal input path strips this information, so raw scancodes are required.

**Acceptance:**
- [ ] New syscall number `0x1004` dispatched in the `syscall_handler` match block
- [ ] Calls `crate::arch::x86_64::interrupts::read_scancode()` to pop one scancode from `SCANCODE_BUF`
- [ ] Returns the scancode as a `u64` (0x00–0xFF) on success
- [ ] Returns 0 if no scancode is available (non-blocking semantics)
- [ ] Make codes (key down) and break codes (key up, `0x80 | make`) are both delivered unmodified

### B.2 — Syscall constant in `syscall-lib`

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `SYS_READ_SCANCODE`
**Why it matters:** Provides a named constant so the doomgeneric platform layer can
invoke the scancode syscall without hardcoding a magic number.

**Acceptance:**
- [ ] `pub const SYS_READ_SCANCODE: u64 = 0x1004;` defined after `SYS_FRAMEBUFFER_MMAP`
- [ ] High-level wrapper `pub fn read_scancode() -> u64` calls `syscall0` and returns raw scancode or 0

---

## Track C — Framebuffer Console Yield/Restore

When a graphical program takes over the framebuffer, the text console must stop
writing to it. On program exit, the text console must resume.

### C.1 — Console yield function

**File:** `kernel/src/fb/mod.rs`
**Symbol:** `yield_console`
**Why it matters:** Without this, the kernel text console and DOOM would race on the
same framebuffer memory, causing garbled output.

**Acceptance:**
- [ ] `pub fn yield_console()` sets a flag (e.g. `CONSOLE_YIELDED: AtomicBool`) that suppresses all `write_str` output to the framebuffer
- [ ] While yielded, `write_str` is a no-op (serial output continues via the log backend)
- [ ] Called by `sys_framebuffer_mmap` when a process first maps the framebuffer

### C.2 — Console restore function

**File:** `kernel/src/fb/mod.rs`
**Symbol:** `restore_console`
**Why it matters:** When DOOM exits, the user must get the text console back for the
shell prompt; without restore the framebuffer stays frozen on the last game frame.

**Acceptance:**
- [ ] `pub fn restore_console()` clears `CONSOLE_YIELDED`, repaints the screen (calls `clear` on `FbConsole`)
- [ ] Called during process cleanup when the process that holds the framebuffer mapping exits
- [ ] After restore, `write_str` resumes rendering to the framebuffer
- [ ] Serial and telnet sessions are unaffected by yield/restore

---

## Track D — doomgeneric Platform Layer

Implement the platform abstraction that bridges doomgeneric to m3OS syscalls. This
is ~200 lines of C living alongside the doomgeneric source.

### D.1 — Clone doomgeneric source

**File:** `userspace/doom/doomgeneric/doomgeneric.h`
**Symbol:** `DG_ScreenBuffer`
**Why it matters:** `DG_ScreenBuffer` is the shared pixel buffer between the engine and
the platform layer — it is the central interface point; getting the source in place and
compiling cleanly is the prerequisite for every other Track D task.

**Acceptance:**
- [ ] doomgeneric source cloned into `userspace/doom/doomgeneric/` (or fetched by xtask at build time into `target/doomgeneric-src/`)
- [ ] `doomgeneric.h` declares `DG_ScreenBuffer`, `DG_Init`, `DG_DrawFrame`, `DG_SleepMs`, `DG_GetTicksMs`, `DG_GetKey`, `DOOMGENERIC_RESX`, `DOOMGENERIC_RESY`
- [ ] Engine C files compile cleanly with `musl-gcc -static`

### D.2 — `DG_Init` implementation

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_Init`
**Why it matters:** This is the entry point that sets up framebuffer access and input;
without it the game cannot start.

**Acceptance:**
- [ ] Calls `syscall(0x1002, ...)` to retrieve `FbInfo` (width, height, stride, bpp, pixel_format)
- [ ] Calls `syscall(0x1003)` to map the framebuffer into userspace and stores the returned virtual address
- [ ] Computes scale factor: `scale = min(fb_width / 320, fb_height / 200)` for nearest-neighbor scaling
- [ ] Computes centering offsets: `x_offset = (fb_width - 320 * scale) / 2`, `y_offset = (fb_height - 200 * scale) / 2`
- [ ] Stores framebuffer pointer, dimensions, and scaling parameters in file-scope static variables

### D.3 — `DG_DrawFrame` implementation

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_DrawFrame`
**Why it matters:** This is the hot loop — called every frame to blit DOOM's 320×200
palette-indexed buffer to the native-resolution ARGB framebuffer.

**Acceptance:**
- [ ] Reads DOOM's internal `DG_ScreenBuffer` (320×200 array of `uint32_t` ARGB pixels)
- [ ] Performs nearest-neighbor scaling: each source pixel written as a `scale × scale` block
- [ ] Writes to the mapped framebuffer at the correct offset using the pitch from `FbInfo`
- [ ] Handles both RGB and BGR pixel formats (swap R and B bytes based on `pixel_format`)
- [ ] Frame rate is ≥15 FPS at 3× scale on QEMU

### D.4 — `DG_SleepMs` and `DG_GetTicksMs` implementations

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_SleepMs`, `DG_GetTicksMs`
**Why it matters:** DOOM uses these for frame pacing and game logic timing; incorrect
timing makes the game run too fast or too slow.

**Acceptance:**
- [ ] `DG_SleepMs` calls `nanosleep()` with the appropriate `struct timespec` (ms × 1_000_000 nanoseconds)
- [ ] `DG_GetTicksMs` calls `gettimeofday()` and returns `tv_sec * 1000 + tv_usec / 1000`
- [ ] Monotonically increasing tick count (no wraparound within a gameplay session)

### D.5 — `DG_GetKey` implementation

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_GetKey`
**Why it matters:** Translates raw PS/2 scancodes into DOOM key events so the player
can move, shoot, and navigate menus.

**Acceptance:**
- [ ] Calls `syscall(0x1004)` to read a raw scancode
- [ ] Returns 0 (no key) when syscall returns 0
- [ ] Distinguishes make codes (key down: scancode < 0x80) from break codes (key up: scancode & 0x80)
- [ ] Maps PS/2 set 1 scancodes to DOOM key constants (`KEY_UPARROW`, `KEY_DOWNARROW`, `KEY_LEFTARROW`, `KEY_RIGHTARROW`, `KEY_FIRE`, `KEY_USE`, `KEY_ENTER`, `KEY_ESCAPE`)
- [ ] Arrow keys (0x48/0x50/0x4B/0x4D), Ctrl (0x1D), Space (0x39), Enter (0x1C), Escape (0x01) all mapped correctly

### D.6 — Palette conversion lookup table

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `build_palette_lut`
**Why it matters:** DOOM renders with a 256-color VGA palette but the framebuffer is
32-bit ARGB; the lookup table makes per-pixel conversion O(1) instead of O(3).

**Acceptance:**
- [ ] Reads the PLAYPAL lump from the WAD (768 bytes: 256 × 3 RGB triplets)
- [ ] Builds `uint32_t palette[256]` where each entry is `0xFF000000 | (r << 16) | (g << 8) | b` (ARGB)
- [ ] Palette is rebuilt when doomgeneric signals a palette change (gamma correction)
- [ ] `DG_DrawFrame` uses the palette LUT to convert each `DG_ScreenBuffer` index to ARGB before blitting

---

## Track E — xtask Build Integration and WAD Disk Image

Wire the doomgeneric build into the xtask pipeline and place the shareware WAD on
the ext2 disk image.

### E.1 — Build doomgeneric with musl-gcc

**File:** `xtask/src/main.rs`
**Symbol:** `build_doom`
**Why it matters:** doomgeneric is a multi-file C project (~30 .c files) that needs its
own build function, similar to `build_pdpmake` which collects all `.c` files from a
cloned source directory.

**Acceptance:**
- [ ] New function `build_doom()` clones doomgeneric into `target/doomgeneric-src/` (or uses cached clone)
- [ ] Collects all `.c` files from the doomgeneric source plus `userspace/doom/dg_m3os.c`
- [ ] Compiles with `musl-gcc -static -O2` (or `x86_64-linux-musl-gcc`) passing all source files
- [ ] Output binary is `kernel/initrd/doom`
- [ ] Called from `build_kernel()` alongside `build_pdpmake()`
- [ ] Gracefully creates an empty placeholder if musl-gcc is not available (same pattern as `build_pdpmake`)

### E.2 — Add doom to initrd embedding

**File:** `kernel/src/fs/ramdisk.rs`
**Symbol:** `DOOM_BIN`
**Why it matters:** All initrd binaries are embedded as `static &[u8]` payloads in
`ramdisk.rs` and registered in the ramdisk table — this follows the exact pattern used
by every other binary (e.g. `EDIT_ELF`, `SH0_ELF`) so `init` can `exec` the binary.

**Acceptance:**
- [ ] `static DOOM_BIN: &[u8] = include_bytes!("../../initrd/doom");` added after the last existing ELF static in `ramdisk.rs`
- [ ] Entry added to the ramdisk file table mapping `"/bin/doom"` to `DOOM_BIN`
- [ ] The binary is accessible as `/bin/doom` in the VFS after boot
- [ ] `doom` command is recognized by the shell and can be exec'd

### E.3 — Place `doom1.wad` on ext2 disk image

**File:** `xtask/src/main.rs`
**Symbol:** `populate_doom_files`
**Why it matters:** DOOM opens and reads the WAD file via `open()`/`read()`/`lseek()`;
it must exist on the persistent ext2 filesystem at a known path.

**Acceptance:**
- [ ] New function `populate_doom_files(part_path, output_dir)` uses `debugfs -w` to create `/usr/share/doom/` directory
- [ ] Writes `doom1.wad` to `/usr/share/doom/doom1.wad` on the ext2 partition
- [ ] Sets directory permissions to 0o755 (`sif ... mode 0x41ED`) and file permissions to 0o644 (`sif ... mode 0x81A4`)
- [ ] Called from `create_data_disk()` after `populate_ext2_files()`
- [ ] Documents where to obtain the shareware `doom1.wad` (freely distributable, ~4 MB)

---

## Track F — Integration Testing and Documentation

### F.1 — Manual smoke test with `cargo xtask run-gui`

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_run_gui`
**Why it matters:** DOOM requires a visible QEMU window (`QemuDisplayMode::Gui`) for
framebuffer rendering — headless mode has no framebuffer.

**Acceptance:**
- [ ] `cargo xtask run-gui` boots the OS with QEMU in GUI mode
- [ ] Running `doom` from the shell displays the DOOM title screen
- [ ] Arrow keys navigate menus and move the player
- [ ] Ctrl fires the weapon, Space opens doors, Enter selects menu items
- [ ] Exiting via the quit menu returns to the shell prompt
- [ ] The text console restores correctly after DOOM exits

### F.2 — Smoke-test step for doom binary presence

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_smoke_test`
**Why it matters:** The xtask smoke-test harness boots the OS in headless QEMU and
does substring matching on serial output; adding a step that verifies `/bin/doom`
exists in the ramdisk catches initrd embedding regressions without needing a GUI.

**Acceptance:**
- [ ] New `SmokePlan` step added to `cmd_smoke_test` that runs `ls /bin/doom` and waits for `/bin/doom` in serial output
- [ ] Step runs in headless mode (does not require `run-gui`) and completes within the default 60 s timeout
- [ ] `cargo xtask smoke-test` passes with the new step present

### F.3 — Update Phase 47 design doc and roadmap

**Files:**
- `docs/roadmap/47-doom.md`
- `docs/roadmap/README.md`

**Symbol:** `## Companion Task List` (section heading in `47-doom.md`), Phase 47 table row (in `README.md`)
**Why it matters:** Documentation must link to the task list and reflect implementation
progress so contributors can find work items.

**Acceptance:**
- [ ] `docs/roadmap/47-doom.md` links to `./tasks/47-doom-tasks.md` in the Companion Task List section
- [ ] `docs/roadmap/README.md` Phase 47 row has Tasks column linking to `./tasks/47-doom-tasks.md`
- [ ] Status updated to Complete and linked from the roadmap once the phase lands

---

## Documentation Notes

- Phase 47 adds three new custom syscalls (`0x1002`–`0x1004`) in the `0x1000+` m3OS
  extension range, continuing the numbering after `SYS_MEMINFO` (`0x1001`).
- The framebuffer was previously kernel-only (`fb::CONSOLE` in `kernel/src/fb/mod.rs`);
  these changes expose it to userspace for the first time.
- Raw scancode access bypasses the cooked terminal input path; the existing
  `read_scancode()` function in `kernel/src/arch/x86_64/interrupts.rs` is reused
  rather than duplicated.
- The dual-mode console (yield/restore) is a new concept: previous phases assumed the
  kernel always owns the framebuffer.
- The doomgeneric build follows the `build_pdpmake` pattern: clone upstream source,
  collect `.c` files, compile with `musl-gcc -static`.
- WAD file placement on ext2 follows the `populate_tcc_files` pattern: create
  directories and write files via `debugfs -w`.
