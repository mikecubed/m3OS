# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**m3OS** (technical name: `m3os`) is a bootable OS in Rust: microkernel architecture, x86_64, UEFI boot. Kernel v0.76.2 with functional userspace (init, shell, coreutils, networking, SMP, storage, signals, editor, multi-user, PTY, telnet/SSH servers, crypto, musl cross-compilation, ports system, service manager, IPC, audio, graphical session, terminal emulator), real-hardware storage via NVMe and networking via Intel e1000 (classic 82540EM) on top of the VirtIO baseline, the Phase 55a IOMMU substrate (ACPI DMAR/IVRS parsing, per-device VT-d and AMD-Vi translation domains, IOMMU-routed `DmaBuffer<T>` with an identity-fallback for non-IOMMU platforms), ring-3 driver hosting (capability-gated device-host syscalls, supervised userspace NVMe and e1000 drivers with `RemoteBlockDevice`/`RemoteNic` kernel facades — Phase 55b), Phase 55c ring-3 driver correctness closure (bound-notification event multiplexing, IOMMU BAR identity coverage, userspace EAGAIN visibility during driver restart), the Phase 56 ring-3 display server with focus-aware input dispatch, layer-shell-equivalent surface roles, and a control socket (`display_server` owns the framebuffer via `sys_fb_acquire`; `kbd_server` / `mouse_server` publish typed `KeyEvent` / `PointerEvent` to a focus-aware dispatcher; `Toplevel` / `Layer` / `Cursor` surface roles with anchor / exclusive-zone / keyboard-interactivity semantics; `m3ctl`-style control socket on a separate AF_UNIX path), and Phase 57 audio + local-session (`audio_server` ring-3 supervised driver claiming Intel 82801AA AC'97 `0x8086:0x2415` over the Phase 55b device-host primitives with single-client PCM-out via the `audio_client` library; `session_manager` orchestrating the fixed boot sequence `display_server` → `kbd_server` → `mouse_server` → `audio_server` → `term` with a typed `text-fallback` recovery contract; `term` graphical terminal emulator composing PTY + ANSI parser + Phase 56 client surfaces + audio bell), Phase 63 audio stack implementation (real PCM emission via privileged `SYS_DEVICE_PIO_{READ,WRITE}` syscalls + `Pio<T>` driver_runtime wrapper + `Ac97PioBus` adapter + DMA-allocated BDL/PCM ring; `audio-smoke` gate asserts `frames_consumed > 0` via `AudioControlCommand::GetStats` plus a non-silent recorded WAV; `bell-smoke` verifies the BEL → `Bell::ring` → audible output path via a new `bell-test` binary), Phase 63a DOOM audio wiring (DOOM SFX + Tier 2a square/triangle synth music play through `audio_server` via the new `audio_mixer` + `audio_client_ffi` crates + `m3os_sound.c` / `m3os_music.c` / `m3os_dmx.c` platform layer; `doom-audio-smoke` gate boots DOOM through `fb-takeover`, verifies non-zero `frames_consumed` across two consecutive runs, and re-arms the BEL post-DOOM), Phase 64 session manager lifecycle (real `session_manager` lifecycle: per-service `ServiceTable` tracking PID + per-child `ServiceState`, two-phase `stop_service` driving SIGTERM → 5s grace → SIGKILL with a non-blocking `kill(pid, 0)` liveness-probe reaper — `session_manager` is not the parent of its supervised children so `sys_waitpid` is not available to it; the stop motion is a pure-logic state machine in `lifecycle.rs` driven to completion by a synchronous `nanosleep` poll loop in `runtime.rs` (a deferred-reply event-loop hoist is a documented follow-up), `restart_service` enforcing the `MAX_RETRIES_PER_STEP` / `MAX_RESTART_COUNT` budgets with a `DISPLAY_CRITICAL_SERVICES` escalation to text-fallback, a new typed `SessionStateDetailed` verb + `ServiceStates` reply variant carrying per-service `(name, ServiceState, restart_count, step_failures)` quads sourced from the table (the legacy `m3ctl session-state` CLI continues to issue the session-wide `ControlVerb::SessionState`; a CLI flag for the detailed reply is deferred), `recover.rs` actually dropping display-server children in reverse start order), and Phase 67 IOMMU substrate completion (AMD-Vi fault ISR + typed `AmdViFaultEvent` decoder in `kernel-core/src/iommu/amd.rs` driving structured event-log dispatch with bounded drain + overflow counter; VT-d queued-invalidation engine with `flush_domain` / `flush_iotlb` / `flush_context` over a 4 KiB ring at `IQA` and an IWAIT-polled status word — `IommuError::FlushTimeout` surfaces queue hangs; runtime VT-d scalable-mode capability check + per-domain 4-or-5-level page tables driven by `ECAP.SMTS` and `CR4.LA57`; AMD-Vi multi-BDF domain grouping via a union-find pass over the IVRS alias entries the kernel-core parser already decoded — grouped BDFs share a single `DomainId` via `BdfDomainAssignment`; real `SupervisedSpawn` + `CapHandle::inject_foreign_dma` harness replacing the four `todo!()` isolation-test scaffolds from Phase 55c plus a new `driver_restart_resets_domain` test), and Phase 68 Display Server Closeout (`flush_subscriber_ring` and the four `publish_*` helpers in `kernel_core::display::subscription` close the Phase 56 subscription-push gap; two new `ControlEvent` kinds — `LayerEvent` and `CursorEvent` — extend the subscribable kind space; `DamageTracker` (`kernel_core::display::damage`) plus a cursor-only fast path in `compose.rs` blits strictly fewer pixels than the framebuffer resolution on cursor-only frames; `KeyEvent` gains `modifier_side: ModifierSide` (`Left` / `Right` / `Either`) with the global `PROTOCOL_VERSION` bumped from `1` to `2` and `KeyEvent::encode_v1` / `decode_v1` + `downgrade_for_v1_client` forming the version-1 compatibility shim; `kernel/initrd/etc/services.d/mouse_server.conf` declares `depends=kbd_server` and `kernel_core::init::supervisor::start_services_ordered` enforces dependency-ordered start with a typed `on-restart=` directive (`LogAndContinue` / `TextFallback` / `Panic`) re-exported by `userspace/init/src/manifest.rs` + `supervisor.rs`), and Phase 69 Terminal Contract Foundations (`xtask/terminfo/m3os-term.ti` published at `/usr/share/terminfo/m/m3os-term` and `TERM=m3os-term` set by `init`, `login`, `shell`; `ConsoleCmd::DecPrivateMode { codes: [u16; MAX_PARAMS], count, set }` for all `CSI ? <n> h/l` modes (multi-code form so a single CSI such as the terminfo `XM` capability `\E[?1006;1000h` toggles SGR encoding and Normal-tracking in one batched apply; consumers iterate `codes[..count]`, single-code synthesizers use `ConsoleCmd::dec_private_single`) and `ConsoleCmd::CursorShape { shape }` for DECSCUSR `CSI <n> SP q`; `Screen` carries dual primary/alternate cell grids with a `SavedCursor` snapshot for `?1049` / `?47`, plus a `bracketed_paste_enabled` bit for `?2004` and a typed `CursorShape`; `SgrParams::ops()` yields typed `SgrOp` values covering 256-color (`38 ; 5 ; n`) and truecolor (`38 ; 2 ; r ; g ; b`) SGR, with an `XTERM_256_PALETTE`-backed `color_to_bgra` resolver; new `term::mouse::MouseReporter` encodes pointer events as X10 / button-event / SGR mouse reports for `?9` / `?1000` / `?1006`; new `ServerMessage::SurfaceResized { width, height }` ↔ `Screen::resize` ↔ `ioctl(TIOCSWINSZ)` chain drives SIGWINCH through the kernel PTY layer; new `userspace/tui-smoke` binary + `cargo xtask tui-smoke` gate validates the terminal contract end-to-end), and Phase 69a Termios Raw Mode and Line Discipline (kernel-core `Termios` widened with the full POSIX flag set + `cooked_default` / `raw_default`; LineDiscipline state for VMIN/VTIME / IXON output suspension / IEXTEN VLNEXT-pending; PTY master_write inline IXON/IXOFF/VLNEXT/VDISCARD plumbing; PTY slave_read four-quadrant VMIN/VTIME timer threaded through `block_on_pty_slave_read` via `WaitQueue` deadline; PTY master TCGETS/TCSETS now returns `-ENOTTY`; kernel TTY0 write path honours `OPOST`+`ONLCR`; userspace `syscall-lib` gains `cfmakeraw`, `tcsetattr_when(when)` with TCSANOW/TCSADRAIN/TCSAFLUSH and the full IXON/IXOFF/IUTF8/ECHOK/ECHONL/Vxxxx constant set; new `userspace/tcsmoke` binary with subcommands `round-trip` / `icanon-off` / `echo-off` / `vmin-vtime` / `isig` / `opost-off`; new `cargo xtask termios-smoke` gate wired into the pre-push hook behind `M3OS_TERMIOS_REGRESSION=1`), and Phase 69b Terminal UTF-8 Wire Decoding and Bitmap Glyph Expansion (new `kernel-core::utf8::Utf8Decoder` + `DecoderOutput` strict W3C/WHATWG UTF-8 state machine routed through `Screen::feed` before the Phase 22b parser; `ConsoleCmd::PutChar` payload widened from `char` to `u32` so any Unicode scalar flows through the parser; new `kernel-core::session::glyph_tables` carrying `GLYPH_TABLE_LATIN1` (U+0080..=U+00FF) and `GLYPH_TABLE_BOX_DRAWING` (U+2500..=U+257F) + a unified `resolve_glyph` accessor + `FALLBACK_DOT_GLYPH` centred-dot for uncovered codepoints + `BLANK_GLYPH` for control characters; `BasicBitmapFont::glyph` dispatches through `resolve_glyph` and a new `glyph_or_fallback` accessor exposes the visible-placeholder path; `kernel-core::session::width_of` reserves wide cells for canonical CJK / halfwidth-fullwidth ranges and `Cell::wide_continuation` keeps the cell grid honest in `Screen::put_char` (wide glyph + last-column wrap + leader / trail overwrite invalidation); `EditBuffer::erase_one_codepoint(iutf8)` gives the Phase 69a `IUTF8` termios bit its first behavioural effect — VERASE removes one whole codepoint per press when set, one byte when cleared; new `tui-smoke utf8` subcommand wired into the existing `cargo xtask tui-smoke` gate validates the byte-stream → cell-state → glyph-bitmap chain end-to-end), and Phase 69c TTF Font Loader and Nerd Font Asset Embedding (new `kernel-core::font` module with a vendored `ttf-parser`-backed `Font` façade, a 1-bit scanline rasterizer with non-zero-winding fill + pixel-centre coverage, and a bounded LRU `Atlas` keyed by codepoint with default capacity 1024; `cargo xtask fetch-fonts` downloads + SHA-256-verifies JetBrainsMono Nerd Font Mono Regular into `xtask/assets/fonts/term.ttf` (gitignored; only the checksum is committed); `populate_ext2_files` stages the asset at `/usr/share/fonts/m3os/term.ttf`; `term::Renderer` carries a `GlyphSource` enum that upgrades from `Static` to `Atlas` at boot via `build_atlas`, logging `term: atlas loaded N glyphs` on success and `term: font load failed; using static fallback` otherwise; `FramebufferOwner::put_glyph` now takes a `&GlyphView` borrow that both the static `Glyph` and atlas `RasterBitmap` flatten to; new `tui-smoke fonts` subcommand with five leaves (`startup`/`branch-icon`/`emoji`/`adversarial`/`missing-font`) wired into `cargo xtask tui-smoke`), and Phase 69d ncurses Port and First Quality TUI Apps (new `cargo xtask port build <name>` host-side driver fetching upstream tarballs with SHA-256 verification and cross-compiling ncurses 6.5 — narrow + wide variants — libevent 2.1.12-stable, less 668, htop 3.4.0, and tmux 3.5a against the m3OS musl toolchain into `target/port-stage/<name>/{usr/local,usr/share}`; `populate_phase_69d_ports` mirrors every staged tree onto the ext2 partition so the on-target file system lays out as `/usr/local/bin/{less,htop,tmux}` plus `/usr/share/terminfo/m/m3os-term` compiled via `tic -x`; new `cargo xtask tui-app-smoke` gate boots m3OS, logs into sh0, drives `less /etc/passwd` through alt-screen + first-line + quit, plus full curses launches of `htop` (header + `q` quit) and `tmux` (full session lifecycle: new-session / has-session / split-window / resize-pane / kill-session — see Phase 69d follow-up below); xtask gains `ensure_yacc()` byacc auto-bootstrap for the tmux build and `linux_uapi_arch_include()` Debian/Arch UAPI directory probe for htop's `<asm/types.h>` include path; `SMOKE_EXIT_TUI_APP_SMOKE_FAILED=69` distinguishes per-app failure modes for CI), and Phase 69d follow-up (new `kernel-core::net::msghdr` pure-logic codec for `struct msghdr` / `struct iovec` / `struct cmsghdr` with full SCM_RIGHTS encode/decode; new `kernel::flock` per-fd advisory-lock side-table plus a `UnixSocket`-keyed registry for cross-fd flock visibility — `sys_flock(73)` LOCK_SH/LOCK_EX/LOCK_NB/LOCK_UN; `sys_prctl(157)` PR_SET_NAME / PR_GET_NAME backed by a new `Process.comm` field surfaced through `/proc/<pid>/comm`; new `UnixSocket::anc_queue` of `InflightFd { backend, cloexec, deliver_at_stream_pos }` driving `sys_sendmsg(46)` / `sys_recvmsg(47)` with scatter-gather iov + SCM_RIGHTS fd refcount-aware passing under an atomic `unix_stream_write_with_anc` lock — close + process-exit + socket-free paths all release inflight refcounts; `sys_pipe_with_flags2` honours `O_NONBLOCK` on `pipe2(2)`; `sys_bind_unix` routes its tmpfs path through the canonical `tmpfs_relative_path` so `mkdir /tmp/tmux-0` and the bind agree on the relative path; `UIO_MAXIOV` raised from 32 to 1024; `sys_poll` re-registers its task on every loop iteration so `WaitQueue::wake_all`'s consume-on-wake semantics no longer silently lose subsequent wakes; new `userspace/winsize-bang` forks a 5-second background timer and issues `TIOCSWINSZ` on inherited stdin so the `tui-app-smoke` htop branch can synthesize SIGWINCH and assert a `winsize-bang:fired cols=60 rows=20` ioctl round-trip sentinel (a true cell-grid reflow assertion at the new dimensions is deferred to the headless framebuffer probe); new `userspace/sendmsg-test` regression validates the end-to-end `socketpair → sendmsg(fd) → recvmsg → recovered-fd-reads-same-bytes` chain; full tmux session lifecycle — `new-session -d -s smoke cat`, `has-session -t smoke`, `split-window -h`, `resize-pane -R 5`, `kill-session -t smoke` — passes end-to-end inside `cargo xtask tui-app-smoke` (48 steps in ~45s); kernel bumped to 0.69.5), and Phase 70 DOOM In-GUI Surface (fb-takeover Tier 3) — `userspace/doom/dg_m3os.c` rewritten to use the Phase 56 surface-buffer protocol via a new `userspace/lib/display_client_ffi` C-ABI bridge (mirrors `audio_client_ffi`); `DG_Init` opens the `display_server` socket, sends `Hello` + `CreateSurface` + `SetSurfaceRole(Toplevel)` + `AttachSharedBuffer`, `DG_DrawFrame` memcpys into the SHM region + sends `DamageSurface` + `CommitSurface`, `DG_GetKey` drains `ServerMessage::Key(KeyEvent)` via the new `dc_poll_event` so the focus-aware dispatcher in `display_server::input` controls whether DOOM sees keypresses; `cargo xtask doom-audio-smoke` retargeted to invoke `doom -warp 1 1` directly with no `fb-takeover` prefix; new `cargo xtask doom-concurrent-smoke` gate runs two DOOMs concurrently under a single `display_server` and verifies both complete the autoquit lifecycle (gated behind `M3OS_DOOM_CONCURRENT_REGRESSION=1` in `.githooks/pre-push`); `SYS_FB_YIELD` (`0x101C`) + `SYS_FB_REACQUIRE` (`0x101D`) dispatch arms emit `log::warn!("...deprecated (Phase 70)...")` per-call; `userspace/fb-takeover/Cargo.toml` carries `[package.metadata] deprecated = true` and the binary prints a stderr deprecation warning before exec'ing its child; kernel bumped to 0.70.0), and Phase 71 GUI Login Manager — new `userspace/greeter/` crate (display_server client + BMP/PNG decoder + scale-to-fit blitter + bitmap-font UI + 3-failure/5s backoff state machine + `/etc/passwd` + `/etc/shadow` auth via `syscall_lib::sha256::verify_password`); `DECLARED_SESSION_STEP_NAMES` extended from 5 to 6 to insert `greeter` between `audio_server` and `term`; greeter binary runs as root, authenticates via the GUI form, then `setgid` + `setuid` + `execve(/bin/term)` in-process so term inherits the authenticated UID/GID (no fork+pipe in session_manager required); init grows a `graphical_only_enabled()` predicate gated by `/etc/m3os-graphical-only` so existing smoke tests keep their serial autologin and graphical-only deployments opt in by writing the marker; init's `KNOWN_CONFIGS` adds `/etc/services.d/greeter.conf`; xtask stages `/etc/greeter.conf` + `/etc/services.d/greeter.conf` + `/etc/services.d/term.conf` in every boot mode and init's `skip_for_greeter_filter` selects the active manifest at boot — marker absent ⇒ load `term.conf`, skip `greeter.conf` (default / smoke / regression); marker present ⇒ load `greeter.conf`, skip `term.conf` (greeter execs `/bin/term` after authenticating); kernel bumped to 0.71.0), and Phase 73 Compositor Polish — five new compositor clients live under `userspace/` (`wallpaper` Background-layer, `bar` Top-layer with workspace indicators + clock + mute hint, `launcher` floating Toplevel triggered by `SUPER+SPACE` with fuzzy filter over `/usr/bin` + `/usr/local/bin` + `/bin`, `notifyd` AF_UNIX listener on `/run/notifyd.sock` with `notify-send` CLI, `lockscreen` full-output Layer with `KeyboardInteractivity::Exclusive` invoked by `m3ctl lock`) all built on a shared `userspace/lib/desktop_client` boilerplate crate (handshake, SHM, bitmap text rendering via `kernel_core::session::font`); `userspace/display_server/src/animation.rs` introduces `AnimationEngine` + `Curve` (Linear/EaseOut/Spring) + per-frame `tick(delta) → DamageRegion` for window-open/close/workspace-switch/window-move; `userspace/display_server/src/decoration.rs` adds `RoundedCornerMask` (pre-computed alpha ramp) + `DropShadow` (pre-computed falloff buffer); `[decorations]` and `[wallpaper]` sections added to `/etc/compositor.conf`; `KeybindAction::LaunchLauncher` bound to `SUPER+SPACE` in the default chord set; init's `KNOWN_CONFIGS` extended with `wallpaper.conf` + `bar.conf` + `notifyd.conf` and xtask stages all three on every disk image so the daemons come up on every supervised boot; kernel bumped to 0.73.0), and Phase 74 IPC Capability Grants and Bulk Transfers — `kernel-core::ipc::message::Message` gains `cap_slots: [CapHandle; 2]` and `n_caps: u8` (existing pre-Phase-74 callers default to `n_caps=0` and observe identical behaviour); new `ipc_transfer_caps` helper threads through the existing `transfer_cap` rendezvous path with all-or-nothing atomicity (validate-first, per-slot rollback); new IPC syscalls `SYS_IPC_CALL_WITH_CAPS` (`0x1117`) and `SYS_IPC_RECV_WITH_CAPS` (`0x1118`) read/write the 56-byte cap-bearing wire format; new `SYS_IPC_CALL_TIMEOUT` (`0x1119`) and `SYS_IPC_RECV_TIMEOUT` (`0x111A`) drive the Phase 57a `block_current_until` deadline path with race-free endpoint-queue cleanup on deadline expiry; new `kernel/src/ipc/page_grant.rs` introduces the `PageGrant` kernel object with a monotonic grant epoch and the `SYS_PAGE_GRANT_SEND` (`0x1020`) / `SYS_PAGE_GRANT_RECV` (`0x1021`) ABI surface (the page-table unmap + TLB shootdown + IOMMU `iommu_remap_grant` path is a scoped Track B follow-up); `userspace/syscall-lib` ships `ipc_call_with_caps`, `ipc_recv_with_caps`, `ipc_call_timeout`, `ipc_recv_timeout`, `page_grant_send`, `page_grant_recv`, and the `notif_bind` alias for the Phase 55c-resident `sys_notif_bind`; Phase 6 / 50 / 55c "Deferred Until Later" sections updated with explicit Phase 74 closure references; `EXPECTED_TASK_PREEMPT_FRAME_OFFSET` bumped from 448 to 464 to track the 16-byte `Message` growth (arch assembly takes `*const PreemptFrame` so no asm offsets shift); kernel bumped to 0.74.0), and Phase 75 W^X Enforcement — `kernel/src/mm/elf.rs::map_load_segment` rejects `PT_LOAD` segments with both `PF_W` and `PF_X` set (returns `ElfError::MappingFailed("PT_LOAD with PF_W|PF_X — W^X violation")`, surfaced to `execve(2)` as the new `NEG_ENOEXEC` (`-8`)); `load_elf_into` / `map_load_segment` gain a `binary_name: &str` parameter so both the rejection `log::warn!` and the new per-segment `elf: mapped pid=… p_vaddr=… p_flags=… pte_flags=…` `log::info!` carry caller-provided binary identity; `sys_mprotect` (`kernel/src/arch/x86_64/syscall/mod.rs`) returns `NEG_EINVAL` immediately when `prot & (PROT_WRITE | PROT_EXEC) == (PROT_WRITE | PROT_EXEC)` — before any page-alignment check, VMA walk, or PTE mutation; legacy `setup_user_memory` helper in `kernel/src/mm/user_space.rs` (the `WRITABLE | USER_ACCESSIBLE` no-`NO_EXECUTE` dead-code shape carrying two `// W^X enforcement is deferred to Phase 6+` markers) is removed; `sys_linux_brk` and `sys_linux_mmap` (anonymous demand-fault path in `kernel/src/arch/x86_64/interrupts.rs::demand_map_user_page_locked`) and `sys_mmap_file_backed` already apply `NO_EXECUTE` for `PROT_EXEC`-clear mappings — verified by audit; new `userspace/wx-violation` regression binary asserts `mprotect(PROT_WRITE | PROT_EXEC)` returns `EINVAL` and the JIT pattern (`mprotect(PROT_READ | PROT_EXEC)`) succeeds, wired into the `smoke-runner` gate so every `cargo xtask smoke-test` exercises the invariant; JIT pattern documented in `docs/appendix/architecture-and-syscalls.md`; Phase 11 and Phase 36 "Deferred Until Later" entries updated to point at Phase 75; new `docs/75-wx-enforcement.md` learner doc; kernel bumped to 0.75.0), and Phase 76 Dynamic Linker scaffolding — `kernel/src/mm/elf.rs::load_elf_into_with_interp` honours `PT_INTERP`: reads the interpreter path from the segment content, loads the interpreter ELF via a caller-provided `InterpReader` closure, maps its `PT_LOAD` segments at `interp_load_bias = max(INTERP_LOAD_BASE_HINT (0x4000_0000), main_top_aligned + 64 KiB)`, and applies the existing `R_X86_64_RELATIVE` path to its own PIE rebase; `LoadedElf.aux_extras: Option<AuxExtras>` + the new pure-logic `kernel-core::elf::auxv` module thread `AT_BASE` (interpreter load bias) and `AT_ENTRY` (main binary entry) through `setup_abi_stack_with_envp` so the dynamic linker can locate both itself and the program it is about to bring up — host-tested with byte-exact a_type ordering pins for both static (6-entry pre-Phase-76 shape) and dynamic (8-entry shape with `AT_BASE`/`AT_ENTRY`) layouts; new `userspace/ld-musl-x86_64.so.1/` no_std PIE crate with `_start` (= `_dlstart`) inline-asm entry that walks the SysV-ABI stack for `AT_ENTRY` and `jmp`s — Phase 76's transfer-only stub is deliberately the minimum that proves the kernel → ld.so → main handoff; new `xtask::build_ldso` stages the binary to `target/generated-libs/ld-musl-x86_64.so.1`, `populate_ext2_files` creates `/lib` and writes the linker there, and the kernel ramdisk embeds the same binary at `/lib/ld-musl-x86_64.so.1` so the kernel's `PT_INTERP` reader resolves the path before ext2 mounts; new `userspace/dynlink_smoke/dynlink-smoke.c` is a musl-built dynamic ELF with `PT_INTERP = /lib/ld-musl-x86_64.so.1` and zero `DT_NEEDED` entries (`-nostdlib -nostartfiles -fPIC -Wl,-pie -Wl,-dynamic-linker=…`) that prints `DYNLINK_SMOKE:PASS` via inline-asm `syscall`; the smoke-runner gate execs `/bin/dynlink_smoke` and wires `SMOKE:dynlink-smoke:PASS` / `:SKIP` into the standard `cargo xtask smoke-test` step list. 76c (`dlopen`/`dlsym`/`dlclose` + `dlopen_test`) and 76d (PLT lazy resolve + `DT_GNU_HASH` + symbol versioning) are tracked as separate roadmap phases; kernel bumped to 0.76.0), and Phase 76b Dynamic Linker bring-up — `userspace/ld-musl-x86_64.so.1/` split into a host-testable `ldso_core` library (`src/{reloc,dynlink,elf64}.rs`) carrying `apply_relative` / `apply_glob_dat` / `apply_abs64` pure-logic relocation primitives, the `DynamicSection::parse` PT_DYNAMIC indexer, SysV `elf_hash` + `lookup_in_hash_table` (bucket+chain walker with hops bound), and `topo_sort` (heapless::Vec iterative DFS with cycle detection) — 23 host tests pin byte-exact semantics; new `no_std`+`no_main` runtime in `src/main.rs` with naked-asm `_start` → `dl_relocate_self` (walks own PT_DYNAMIC for DT_RELA and applies R_X86_64_RELATIVE before any GOT-routed read) → `dl_entry` driver that parses auxv (AT_BASE / AT_PHDR / AT_PHNUM / AT_ENTRY), computes main load bias from PT_PHDR vs AT_PHDR, iteratively loads every transitive DT_NEEDED from `/usr/lib/` with SONAME-keyed dedup, runs `topo_sort` over the dep graph (cycle → `exit(80)` ELIBBAD), walks DT_RELA + DT_JMPREL per type (R_X86_64_RELATIVE / R_X86_64_GLOB_DAT / R_X86_64_JUMP_SLOT / R_X86_64_64) resolving names via the loaded-DSO chain, runs DT_INIT + DT_INIT_ARRAY constructors deepest-first, then returns AT_ENTRY for the asm caller to `jmp` into main; `load_dso` works around the m3OS kernel's `MAP_FIXED`-ignoring anonymous mmap by issuing one mmap for the whole image (kernel-chosen base becomes load_bias), copying each PT_LOAD into `load_bias + p_vaddr`, then `mprotect`ing PF_X segments to PROT_R|PROT_X (W^X-clean); new `xtask::build_shared_lib(name, srcs, output)` invokes musl-gcc with `-shared -fPIC -nostdlib -Wl,--hash-style=sysv -Wl,-soname,<name>.so`, `populate_ext2_files` enumerates every `.so` under `target/generated-libs/` and mkdir+writes each at `/usr/lib/<basename>`, kernel ramdisk gains `USR_ENTRIES` / `USR_LIB_ENTRIES` for early-boot resolution; demo binaries `userspace/lib/libhello/hello.{h,c}` (`hello_str()` returning `"HELLO_FROM_SHARED_LIB:OK"`, DT_SONAME=libhello.so, DT_HASH present, DT_GNU_HASH absent) + `userspace/dynlink_hello/dynlink_hello.c` (links `-lhello`, 1 R_X86_64_JUMP_SLOT for `hello_str`, writes sentinel via inline-asm syscall, PT_INTERP=/lib/ld-musl-x86_64.so.1); new `dynlink-hello-smoke` gate runs `/bin/dynlink_hello` twice consecutively (refcount path) and asserts the sentinel; F1.4 negative gates — `userspace/dynlink_missing/dynlink_missing.c` with `DT_NEEDED = libdoesnotexist.so` → linker hits ENOENT → `exit(2)`; `userspace/dynlink_cycle/dynlink_cycle.c` + three-step cycle-lib build (`libcyca_stub.so` → `libcycb.so` → final `libcyca.so` closing the back-edge) → topo_sort detects the libcyca ↔ libcycb cycle → `exit(80)` (ELIBBAD); new `dynlink-missing-smoke` and `dynlink-cycle-smoke` gates assert `WEXITSTATUS == 2` and `== 80` via a new `run_command_expect_exit` smoke-runner helper; kernel bumped to 0.76.1), and Phase 76c libdl runtime — POSIX `dlopen` / `dlsym` / `dlclose` / `dlerror` ship in `userspace/ld-musl-x86_64.so.1/src/dl.rs` against a process-global `DlState` (slot-indexed `[LoadedDso; MAX_SLOTS]`, parallel SONAME / refcount / global-scope / `dep_lists` arrays, plus a new host-tested `ldso_core::handle::HandleTable` slab carrying `(dso_id, generation)` records so forged or freed handles are detected); `dl_entry` self-injects the linker into the bring-up DSO scope at slot 1 — BEFORE walking `DT_NEEDED` — so SysV first-found-wins symbol resolution lands `dlopen` etc. on the linker's real implementations rather than on any stub `libdl.so` the consumer linked; new `build.rs` emits `--hash-style=sysv` + `--export-dynamic` + `-soname=ld-musl-x86_64.so.1` only for the `x86_64-unknown-none` target so the linker's `DT_HASH` carries the libdl symbols AND a `DT_SONAME` GNU ld can scan at link time; `DT_FINI` / `DT_FINI_ARRAY` parsed into `DynamicSection` (parse arms + 4 new dynlink host tests) and a new pub(crate) `run_destructors_for(&LoadedDso)` walks the array in reverse then calls `DT_FINI` via a register-loaded `extern "C" fn()` (NOT a GOT slot — the DSO's GOT is about to be unmapped); `dlclose`'s last-close path captures the `LoadedDso` by value, evicts its slot from `DL_STATE`, runs destructors, then issues a single `munmap(load_bias, image_len)` via the host-tested `ldso_core::dynlink::unmap_dso` pure-logic wrapper (`LoadedDso` moved out of the binary into the library so the host harness can drive it); `dlopen` honours `RTLD_NOW` / `RTLD_LAZY` (`RTLD_LAZY` is treated as `RTLD_NOW` in 76c; PLT lazy resolve ships in 76d) and `RTLD_GLOBAL` / `RTLD_LOCAL`; repeat opens of the same SONAME refcount-increment with `saturating_add` clamped at `REFCOUNT_PERMANENT - 1`, where `REFCOUNT_PERMANENT == u32::MAX` is the sentinel locked on slot 0 (main), slot 1 (linker), and every bring-up `DT_NEEDED` so `dlclose` can never unmap them; `dlerror()` reads-and-clears a `static` slot (process-global until TLS lands); new `userspace/lib/libdl/libdl.c` link-time stub library + `userspace/lib/libhello_fini/hello_fini.c` destructor demo + `userspace/dlopen_test/dlopen_test.c` (exercises positive open / sym / call / close + refcount + the four negative paths) ship via `xtask::build_libdl` / `build_libhello_fini` / `build_dlopen_test`; new `dlopen-test-smoke` gate asserts the strict serial order `DLOPEN_TEST:FINI_PENDING → LIBHELLO_FINI:RAN → DLOPEN_TEST:PASS` (the destructor sentinel goes to fd 1 because m3OS's `dup2` does not share the file description between fd 1 and fd 2); 37 host tests pass in `ldso_core` (9 new for `HandleTable`, 4 for `unmap_dso`, 1 for DT_FINI* parse); kernel bumped to 0.76.2. See `docs/appendix/codebase-map.md` for full workspace and source layout.

## Build & Run

Uses the `xtask` pattern — always build through `cargo xtask`, never `cargo build` directly.

```bash
cargo xtask run          # build + launch in QEMU (headless, serial output)
cargo xtask run --fresh  # same, but recreate data disk first
cargo xtask run-gui      # build + launch in QEMU (GUI with framebuffer)
cargo xtask run-gui --fresh  # same, but recreate data disk first
cargo xtask image        # build bootable disk image (UEFI raw + VHDX)
cargo xtask image --sign # build + sign EFI binary for Secure Boot
cargo xtask check        # clippy (-D warnings) + rustfmt + host tests for kernel-core, passwd, driver_runtime, audio_client, audio_server, surface_buffer, crypto-lib, term, audio_mixer, audio_client_ffi, session_manager
cargo xtask fmt --fix    # auto-format all workspace source
cargo xtask test         # run all kernel tests in QEMU via ISA debug exit
cargo xtask test --test <name>  # run a single QEMU test binary
cargo xtask test --timeout 120  # custom timeout (default 60s)
cargo xtask test --display      # show QEMU window for debugging
cargo xtask sign         # sign EFI binary with Secure Boot keys
cargo xtask clean        # delete disk.img so next run recreates it
cargo test -p kernel-core       # run kernel-core host-side unit tests directly
```

After adding new service configs to the ext2 data disk, run `cargo xtask clean` to force disk recreation.

Tests cannot use `cargo test` on the kernel — it is `no_std` and tests run inside QEMU via the xtask harness. Pure-logic code lives in `kernel-core` and is testable on the host via `cargo test -p kernel-core`.

## Git Workflow

All work must happen on a feature branch with a pull request to `main`. Never commit directly to `main`.

```bash
git checkout -b feat/my-feature       # 1. create feature branch
# ... make changes ...
git add <files> && git commit         # 2. commit
git push -u origin feat/my-feature    # 3. push
gh pr create --base main              # 4. open PR to main
# 5. user merges PR after review
```

Branch naming: `feat/`, `fix/`, `refactor/`, `docs/` prefixes as appropriate.

## First-Time Setup

After cloning, install the git hooks so quality gates run before commits and pushes:

```bash
./setup.sh
```

This sets `core.hooksPath` to `.githooks/`. The pre-commit hook runs
`cargo xtask check`; the pre-push hook runs `cargo xtask check`,
`cargo xtask smoke-test`, and `cargo xtask regression`, plus
`cargo xtask ssh-e1000-banner-check` when `M3OS_E1000_REGRESSION=1`
is set, `cargo xtask doom-audio-smoke` when
`M3OS_DOOM_AUDIO_REGRESSION=1` is set, `cargo xtask termios-smoke` when
`M3OS_TERMIOS_REGRESSION=1` is set, `cargo xtask tui-app-smoke`
when `M3OS_TUI_APP_REGRESSION=1` is set,
`cargo xtask doom-concurrent-smoke` when
`M3OS_DOOM_CONCURRENT_REGRESSION=1` is set, and
`cargo xtask tiling-smoke` when `M3OS_TILING_REGRESSION=1` is set.

## Architecture

Microkernel: ring 0 kernel handles memory management, scheduling, IPC, interrupt routing, and device drivers. Userspace processes run in ring 3 and communicate through IPC and syscalls.

```
Ring 0 (kernel/):                Ring 3 (userspace/):
  - Frame allocator                - init (PID 1 daemon)
  - Page table manager             - sh0 (built-in shell)
  - Scheduler (SMP-aware)          - coreutils (cat, ls, grep, etc.)
  - IPC engine + capabilities      - ping (ICMP network tool)
  - IDT / APIC / interrupt router  - edit (text editor)
                                   - login, su, passwd, adduser
                                   - id, whoami
                                   - ion shell (external)
  - Syscall gate
  - VFS + FAT32 + tmpfs
  - Network stack (IPv4/TCP/UDP)
  - Unix domain sockets (AF_UNIX)
  - VirtIO drivers (blk, net)
  - ACPI / PCI enumeration
  - Framebuffer console
  - TTY + signal handling
  - SMP (multi-core boot + IPI)
```

See `docs/appendix/codebase-map.md` for workspace crates, ports tree, and source layouts.

### Adding a New Userspace Binary

Adding a new userspace binary requires changes in **four** places. Missing any one of these causes the binary to either not be built, not be embedded in the kernel image, or not be found at runtime.

1. **Workspace member** — add the crate to `Cargo.toml` `members` list
2. **xtask build pipeline** — add to the `bins` array in `xtask/src/main.rs` (`build_userspace` function, ~line 141). Set `needs_alloc = true` if the crate depends on `alloc` (e.g., uses `kernel-core` or `Vec`/`Box`/`String`). If `needs_alloc` is true, the binary must define a `#[global_allocator]` (use `syscall_lib::heap::BrkAllocator`) and enable the `alloc` feature on `syscall-lib`.
3. **Ramdisk embedding** — add an `include_bytes!` static and a `BIN_ENTRIES` tuple in `kernel/src/fs/ramdisk.rs`. Generated binaries are staged by `xtask` under `target/generated-initrd/`; checked-in static initrd assets remain under `kernel/initrd/`. Without the ramdisk entry, `execve` returns ENOENT.
4. **Service config (if daemon)** — add a `.conf` file to the ext2 data disk builder in `xtask/src/main.rs` (`populate_ext2_files` function) AND to the `KNOWN_CONFIGS` fallback list in `userspace/init/src/main.rs`. Run `cargo xtask clean` to recreate the disk.

### Adding a New Cross-Compiled Port (ncurses-style)

Ports live under `ports/<category>/<name>/Portfile` and are built host-side by `cargo xtask port build <name>`, which dispatches to a `build_<name>` function in `xtask/src/port_build.rs`. **Every new `build_*` function MUST route through the shared musl-toolchain plumbing or it will fail on toolchains that ship without empty static-compat archives** (Arch `musl-cross-tools`, raiden, hand-built `musl-cross-make`, anything that omits `libdl.a` / `libpthread.a` / `librt.a`). The "C compiler cannot create executables" configure error during the link probe is the symptom.

Required wiring in every port `build_*` function:

1. **Resolve the toolchain via `musl_toolchain()`** — which calls the shared `crate::find_musl_cc()` probe. Never invoke `x86_64-linux-musl-gcc` as a literal string.
2. **Compose LDFLAGS with `musl_extra_ldflags_joined()`**:
   ```rust
   let extra_ld = musl_extra_ldflags_joined();
   let ldflags = if extra_ld.is_empty() {
       "-static -L<stage>/lib".to_string()
   } else {
       format!("-static -L<stage>/lib {extra_ld}")
   };
   ```
   The `extra_ld` value is `-L<workspace>/target/musl-stub-libs/` when xtask auto-generated the empty archives. Without that `-L`, the configure script's `-static -ldl -lpthread -lrt` link probe fails and the build aborts with exit 77.
3. **Pass `--host=x86_64-linux-musl`** to `./configure` so autotools picks the correct cross triple.
4. **Use the `(cc, ar, ranlib)` tuple from `musl_toolchain()`** for `CC` / `AR` / `RANLIB` — the tuple's `ar`/`ranlib` already fall back to host `ar`/`ranlib` when the cross variants are absent (static archives are ELF-target-agnostic so this is safe).

To register a new port: add the name to `PORTS` in `xtask/src/main.rs:10792`, add it to `match name` dispatch in `xtask/src/port_build.rs:port_build` (~line 366), implement `build_<name>` following the pattern above, and add the resulting binary path to `tui_app_smoke_steps` if the port participates in the gate.

## Critical Conventions

### Target flags — do not remove

In `.cargo/config.toml` / target spec:

- `"disable-redzone": true` — hardware interrupts use the stack; removing this causes silent stack corruption
- `"-mmx,-sse"` — disables SIMD to avoid FPU state save/restore on context switches
- `"panic-strategy": "abort"` — no unwinding; panics halt the machine

### `no_std` everywhere in kernel and userspace

All crates under `kernel/` and `userspace/` are `#![no_std]`. Only use `alloc` types (`Vec`, `Box`, `Arc`) after heap initialization. `kernel-core` supports both `no_std` (kernel) and `std` (host tests) via feature flags.

### `unsafe` only at hardware boundaries

Acceptable only for: hardware register/port I/O, page table/GDT/IDT setup, `enter_userspace()`/`switch_context()` asm stubs, global allocator initialization, APIC/ACPI MMIO access, VirtIO ring manipulation. Always wrap in a safe abstraction immediately.

All crates use Rust **edition 2024** — the body of an `unsafe fn` is *not* implicitly unsafe. You must wrap unsafe operations in explicit `unsafe {}` blocks inside unsafe functions.

### IPC model — read the doc before touching `kernel/src/ipc/`

Synchronous rendezvous + async notification objects (seL4-style):

- Server-to-server: sync `call`/`reply_recv`
- IRQ/vsync: `Notification` objects (word-sized bitfield, safe to signal from interrupt handlers)
- Bulk data: page capability grants, never IPC payloads
- Userspace servers must never share writable memory

### Interrupt handlers

Do the minimum: read scancode / ack interrupt / push to ring buffer / send EOI. No allocation, no blocking, no IPC from within an interrupt handler.

### Capabilities

Integer index into the current process's `CapabilityTable`. Kernel validates every handle on every syscall. Transfer via `sys_cap_grant` — never forge or copy raw capability values.

### Syscall ABI

| Register | Role |
|---|---|
| `rax` | Syscall number (in) / return value (out) |
| `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` | Arguments 1–6 |

`rcx` and `r11` are clobbered by `syscall` — never use them for arguments.

### Context switch

`switch_context(current, next)` saves/restores only callee-saved registers (`rbx`, `rbp`, `r12`–`r15`, `rsp`, `rip`). Do not change without auditing every call site.

### SMP conventions

- BSP (bootstrap processor) completes full kernel init before waking APs
- APs initialize their own GDT, IDT, APIC, and enter the scheduler idle loop
- Use IPI for TLB shootdown on page table updates affecting multiple cores
- Per-CPU data accessed via APIC ID — avoid global mutable state without proper locking

### QEMU test exit convention

```rust
// Write to I/O port 0xf4 (isa-debug-exit device)
// QEMU exit codes: 0x21 = success, 0x23 = failure
const QEMU_EXIT_SUCCESS: u32 = 0x10;
const QEMU_EXIT_FAILURE: u32 = 0x11;
```

### Userspace-first rule

New high-level policy defaults to userspace. Before adding policy-heavy code to ring 0, check the architecture review checklist in `docs/appendix/architecture-and-syscalls.md`.

### `BootInfo` is read-only after init

Parse memory regions, framebuffer, RSDP during `kernel_main` init and store in typed kernel structures. Do not hold long-lived references to `BootInfo`.

## Key Crates

| Crate | Purpose |
|---|---|
| `bootloader_api` | Kernel entry point macro, `BootInfo` |
| `x86_64` | `PageTable`, `IDT`, `GDT`, `PhysAddr`/`VirtAddr`, port I/O |
| `uart_16550` | Serial port driver — primary debug output |
| `pic8259` | 8259 PIC init and EOI |
| `spin` | `Mutex`/`RwLock` for `no_std` |
| `log` | Logging facade; backend writes to serial |
| `kernel-core` | Shared pure-logic library, host-testable |

## Documentation in `docs/`

Before making significant changes to a subsystem, read the corresponding phase doc. Full index in `docs/appendix/codebase-map.md`. Roadmaps and task lists live in `docs/roadmap/`.

### Documentation templates — all docs must conform

All roadmap docs must follow the templates in `docs/appendix/doc-templates.md`. When creating or updating docs, use the matching template:

| Doc type | Template section | Required fields |
|---|---|---|
| Phase design doc | `docs/roadmap/NN-slug.md` | Status, Source Ref, Depends on, Builds on, Primary Components, Milestone Goal, Why This Phase Exists, Learning Goals, Feature Scope, Important Components and How They Work, How This Builds on Earlier Phases, Implementation Outline, Acceptance Criteria, Companion Task List, How Real OS Implementations Differ, Deferred Until Later |
| Phase task doc | `docs/roadmap/tasks/NN-slug-tasks.md` | Status, Source Ref, Depends on, Goal, Track Layout table, per-track sections with tasks containing File/Symbol/Why it matters/Acceptance, Documentation Notes |
| Roadmap README row | `docs/roadmap/README.md` | Phase, Theme, Primary Outcome, Status, Source Ref, Milestone link, Tasks link |

Rules:

- Never create a task doc without all template sections populated.
- Never create a design doc missing Status, Source Ref, Depends on, or Builds on.
- Task acceptance items must be concrete and measurable — no vague "works correctly".
- Each task must have File, Symbol, and Why it matters fields.
- Update the roadmap README row when creating or completing a phase.
