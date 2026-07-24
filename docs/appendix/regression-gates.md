# Regression Gates — Full Descriptions

Each section below corresponds to one named gate in the `AGENTS.md` pre-push
gate table. The lean table in [`AGENTS.md`](../../AGENTS.md) gives the
env var and a one-line purpose; this file gives the full description verbatim.

Gates are ordered identically to the AGENTS.md lean table.

## ssh-e1000-banner-check

**Env var:** `M3OS_E1000_REGRESSION=1`

## doom-audio-smoke

**Env var:** `M3OS_DOOM_AUDIO_REGRESSION=1`

## termios-smoke

**Env var:** `M3OS_TERMIOS_REGRESSION=1`

## tui-app-smoke

**Env var:** `M3OS_TUI_APP_REGRESSION=1`

## doom-concurrent-smoke

**Env var:** `M3OS_DOOM_CONCURRENT_REGRESSION=1`

## tiling-smoke

**Env var:** `M3OS_TILING_REGRESSION=1`

## clipboard-smoke

**Env var:** `M3OS_CLIPBOARD_REGRESSION=1`

## screenshot-smoke

**Env var:** `M3OS_SCREENSHOT_REGRESSION=1`

## imgview-smoke

**Env var:** `M3OS_IMGVIEW_REGRESSION=1`

## htop-render-probe

**Env var:** `M3OS_HTOP_REGRESSION=1`

## term-daily-driver-smoke

**Env var:** `M3OS_TERM_POLISH_REGRESSION=1`

Phase 112. Boots the graphical stack headlessly (QMP + VNC) with an extra
`usb-tablet` on the xHCI bus, then asserts on the **composited framebuffer**
— a serial `Wait` proves a program ran, not that the screen scrolled or a
highlight painted. Four arms:

1. **Scrollback viewport.** `dmesg` fills far past the 25-row live grid, so
   the early lines exist only in the ring. Shift+PageUp must change ≥20
   scanlines versus the live-tail frame (evicted rows are back on screen),
   and a subsequent Return must change ≥20 scanlines versus the scrolled
   frame (snap-to-bottom fired).
2. **Report-protocol wheel.** The same viewport assertion driven by QMP
   `send_wheel`. The `usb-tablet` is load-bearing: QEMU's `usb-mouse` is
   Boot-subclass and the boot decoder discards the wheel byte, so only a
   Report-protocol pointer (subclass 0 → `decode_pointer_report`) delivers
   `wheel_dy` to `term`. The PS/2 path never carries a wheel at all — see
   the Phase 112 deferral on IntelliMouse.
3. **Selection + clipboard.** A QMP left-button drag over a known marker
   must paint a visible highlight, and copy-on-release must land the text in
   the compositor's `ClipboardStore`. The read-back is done by an
   **independent** client (`clip-smoke --paste`, added in Phase 112) rather
   than by `term` itself, so the assertion covers the broker and not just
   `term`'s own memory.
4. **Paste reaches the PTY.** Ctrl+Shift+V into a bare `cat` must visibly
   change the frame *and* leave the terminal on screen (asserted via the
   frame's black-pixel ratio — term paints black, the compositor teal). The
   `cat` sink is load-bearing: `ion` does not enable bracketed-paste mode,
   so `wrap_paste` correctly passes the payload through unframed, and
   pasting a multi-row selection at a shell prompt would execute every line
   in it and take the shell down — which is exactly what this gate caught
   before the sink was added, while a naive "did the frame change?" check
   scored it a pass. The `ESC[200~` / `ESC[201~` framing is therefore not
   observable on this lane (it is conditioned on the application enabling
   `?2004h`) and is covered by the `wrap_paste` host tests in `term::input`.

**Known upstream limitation.** The tablet's absolute coordinates arrive
unscaled: `usb-hid` injects the decoded logical value straight into
`PointerEvent::abs_position` without mapping the report's logical range onto
the framebuffer. QEMU's `usb-tablet` has a 0..0x7FFF logical range, so QMP
abs values pass through 1:1 and this gate deliberately sends **screen-pixel**
coordinates rather than the usual normalized range — sending 0x4000 would
hit-test at (16384, 16384), miss every surface, and silently drop the event.
Fixing this properly needs `ReportField` to carry logical min/max (it does
not today) plus a framebuffer-size query in the driver; that is Phase 92b
work, not Phase 112.

## toolkit-render-probe

**Env var:** `M3OS_M3UI_REGRESSION=1`

## settings-smoke

**Env var:** `M3OS_SETTINGS_REGRESSION=1`

Boots the graphical stack headlessly (QMP + VNC) with the AC'97 device
attached so `audio_server` runs its real io loop, launches the `settings`
Toplevel from the term prompt, and drives the default-focused volume slider
with QMP keyboard `Left` presses. Asserts the full keyboard → widget → IPC →
server path twice (100%→99%→98%): the client ack sentinel
(`SETTINGS:volume=<pct> q15=<q> ack=ok`), the server gain-state sentinel
(`AUDIO_SMOKE:master_gain q15=<q>`), and a ≥12-scanline repaint of the
composited frame (the volume label + slider knob visibly updated). The gain
*application* to PCM is host-tested (kernel-core `audio::gain`,
`audio_server` `gained_pcm`, `audio_mixer`); this gate owns the live path.

## symphonia-smoke

**Env var:** `M3OS_SYMPHONIA_REGRESSION=1`

Builds the `symphonia-play` port (the tree's first local-source port: a
musl-`std` Rust cargo crate under `ports/util/symphonia-play/src` that
reaches the m3OS audio IPC via raw syscalls), boots with the AC'97
WAV-capture backend, decodes and plays the 48 kHz WAV and FLAC fixtures
(`/usr/share/symphonia/`) in separate invocations (proving the
single-client Open/Close cycle re-opens), asserts the per-file
`SYMPHONIA_PLAY:ok` serial sentinels, and finally verifies the captured
WAV is non-silent via `assert_wav_non_silent` — the same audible-output
oracle as `doom-audio-smoke`/`hda-smoke`. A silent capture exits with
the shared `SMOKE_EXIT_WAV_SILENT` code.

### Bundle: M3OS_USB_REGRESSION=1

## xhci-bringup-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

## xhci-enum-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

## usb-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

## usb-report-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

Phase 92b: a `usb-tablet` Report-Protocol pointer decodes against the parsed `ReportField` layout → `HID_REPORT:pointer` (B.2), a `caps_lock` press drives a `SET_REPORT(Output)` LED write the device ACKs → `USB_HID:led` (B.4), and a normal key injected right after that control write still decodes → `USB_HID:key … sym=…62` (H.2 — no interrupt-IN drop across the interleaved control write)

## usb-hotplug-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

Phase 92b C.4: also asserts the usb-hid detach release — `usb-hid: hot-attached`/`usb-hid: released` across each of the 3 attach/detach cycles

## usb-storage-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

Phase 92a H.4: also asserts `USB_STORAGE:shm-dma-ok` — an 8192-byte zero-copy WRITE+READ over an IOMMU-mapped shared-memory region via `SYS_DEVICE_DMA_MAP_SHM` + `SubmitShmTransfer`

## usb-hub-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

Phase 92a: also asserts **tier-2** `XHCI_HUB:child-enumerated` for a full-speed HID device behind the hub

## usb-mount-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

Phase 92a: `mount /dev/usb0 /mnt/usb0` + `ls`/read/overwrite-readback over the secondary-mount routing; **PR #253**: also asserts `USB_MOUNT:remount-ok` — remounting the SAME prefix 6× (2× the usable `MAX_REMOTE_BLOCK` budget) keeps succeeding and the volume still reads back, the regression guard for the `mount_usb` displaced-dev_id unregister — without it the 3rd remount exhausts the `blk::remote` registry and `mount` returns ENODEV

## usb-unmount-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

Phase 92 C.4: mounts `/mnt/usb0`, QMP-`device_del`s the stick, and asserts the resident `usb-storage` daemon's `ipc_recv_msg_timeout` idle reconcile detected the detach and unmounted `/mnt/usb0` — `USB_STORAGE:detached-unmounted`, freeing the kernel `blk::remote` slot via the new `/mnt/usb*` `umount2` path — without wedging the VFS

## usb-storage-dual-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

Phase 92 D.4: two `usb-storage` devices on one xHCI bus; the single daemon enters multi-device mode, registers `usb0.block` + `usb1.block`, and both `/mnt/usb0` and `/mnt/usb1` mount + read their **own distinct content** — independent concurrent sticks served from one event loop, the m3OS single-threaded analog of the Track F multi-controller pattern

## usb-multi-controller-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

Phase 92d: second `qemu-xhci,id=xhci1,addr=0x7` with a `usb-mouse` behind it; asserts `XHCI:controller-1:ready` (the secondary controller's IRQ is subscribed into the primary's bound notification → the single server loop wakes on either controller's interrupt) and that a QMP mouse move on the controller-1 device decodes — proving controller 1 is serviced on its own interrupt with no primary traffic

## usb-eth-smoke

**Env var:** `M3OS_USB_REGRESSION=1`

Phase 92e: CDC-ECM/NCM USB-Ethernet — **skip-with-reason** since QEMU has no CDC-ECM model; CI coverage is the host-tested `kernel_core::usb::cdc` device-match registry + CDC functional-descriptor parse + NTB-16 framing + ECM MAC parse, plus the `usb-net` crate compiling for `x86_64-m3os`. Set `M3OS_USB_ETH_REGRESSION=1` to acknowledge hardware-only validation via VFIO passthrough with a CDC dongle

## tls-smoke

**Env var:** `M3OS_TLS_REGRESSION=1`

`tls-smoke` PASS (not SKIP)

The `tls-smoke`/`dns-smoke` gates assert the musl-built smoke stage actually `PASS`ed rather than `SKIP`ped — a `SKIP` means the musl cross-compiler was absent at build time, which would otherwise let the Track C PT_TLS/pthread and Track D.1 DNS fixes ride **unverified**. Set them on branches touching the kernel clone path, `PT_TLS` loader, futex `CHILD_CLEARTID` wake, or the DNS resolver / `recvmsg` / ephemeral-UDP path (they require a musl cross-gcc).

## dns-smoke

**Env var:** `M3OS_DNS_REGRESSION=1`

`dns-smoke` PASS (not SKIP)

The `tls-smoke`/`dns-smoke` gates assert the musl-built smoke stage actually `PASS`ed rather than `SKIP`ped — a `SKIP` means the musl cross-compiler was absent at build time, which would otherwise let the Track C PT_TLS/pthread and Track D.1 DNS fixes ride **unverified**. Set them on branches touching the kernel clone path, `PT_TLS` loader, futex `CHILD_CLEARTID` wake, or the DNS resolver / `recvmsg` / ephemeral-UDP path (they require a musl cross-gcc).

## multi-nic-smoke

**Env var:** `M3OS_MULTI_NIC_REGRESSION=1`

(e1000 + e1000e + igb arms)

## ure-smoke

**Env var:** `M3OS_URE_REGRESSION=1`

Phase 96: passes the physical Realtek **RTL8156** `0bda:8156` 2.5GbE USB-Ethernet dongle through to an emulated xHCI and asserts the full bring-up chain on real silicon — enumerate → `ure` claim + MAC read (control IN, `URE_STAGE1A:OK`) → chip init via control-OUT (`URE_STAGE1B:OK`, `PLA_CR` `RE\|TE` latched) → `link up <speed>` → `RemoteNic` registration (`URE_STAGE2:NIC-UP` + the kernel `[remote_nic] … registered ring-3 NIC driver … mac=…` bootstrap). The USB analogue of `multi-nic-smoke`'s registration+link arms, against real hardware. **Skip-with-reason** (sysfs-scanned) when no `0bda:8156` is present — CI has no dongle (mirrors `tls-smoke`/`wifi-smoke`). The live **DHCP/ping/HTTP-over-`ure`** arm is **opt-in** (`M3OS_URE_NET=1`) and not CI-deterministic: the passthrough device sits on the host's physical LAN (not a SLIRP backend), so it needs that LAN to provide DHCP + egress and m3OS to route over `ure` rather than the default virtio/SLIRP NIC — drive it manually via `scripts/ure-vfio-validate.md` (mirrors `git-https-smoke`'s `M3OS_GIT_HTTPS_NET`). Needs a readable `/dev/bus/usb` node for the dongle — see the SKIP message for the one-time `chmod`. Run with `--timeout 360`+ (a fresh-disk boot is slow before the `ure` output lands)

## hda-smoke

**Env var:** `M3OS_HDA_REGRESSION=1`

(`-device intel-hda -device hda-duplex`, non-silent WAV)

## usb-audio-smoke

**Env var:** `M3OS_USB_AUDIO_REGRESSION=1`

Phase 92c: `-device qemu-xhci` + `-device usb-audio` wired to a WAV audiodev; the ring-3 `usb-audio` driver binds the UAC AudioStreaming interface surfaced by the xHCI server, `GetDescriptors` + `find_isoch_out_stream` to locate the isoch OUT endpoint, `SET_INTERFACE` the alt-setting, and registers `audio.hw` → `AUDIO:usb-sink`; then `audio-demo` mixes a tone through `audio_server` → the USB sink → the isochronous OUT endpoint (`SubmitIsochOut` → controller `submit_isoch_out`, one SIA Isoch TRB per interval), asserting `AUDIO_DEMO:PASS`, a non-zero `frames_consumed` (the `audio.hw` race-fallback guard), and a **non-silent captured WAV** — the falsifiable proof PCM reached the device over isoch. m3OS's first isochronous-endpoint class driver. Set on branches touching the usb-audio driver, the isoch TRB path (`submit_isoch_out`, `kernel_core::usb::{uac,xhci}` isoch primitives, `SubmitIsochOut`), the xHCI server's audio-interface surfacing, or `audio_server`

## wifi-smoke

**Env var:** `M3OS_WIFI_REGRESSION=1`

(no QEMU mt76 model — skip-with-reason; radio validated via VFIO)

### Bundle: M3OS_AHCI_REGRESSION=1

## ahci-smoke

**Env var:** `M3OS_AHCI_REGRESSION=1`

`-device ich9-ahci` + scratch `ide-hd`; IDENTIFY/write/read-back-compare/flush/IDENTIFY-after-write/induced-TFES-recovery; BOHC/SSS/hot-plug skip-with-reason on QEMU, validated via VFIO

## ahci-root-smoke

**Env var:** `M3OS_AHCI_REGRESSION=1`

Real ext2 data disk routed to AHCI; asserts the root mounts off `ahci.block` end-to-end — virtio root absent → driver MBR/ext2 probe → owner-gate accept → `init: / mounted (ext2 via ring-3 ahci.block)` → login prompt

## ahci-rw-smoke

**Env var:** `M3OS_AHCI_REGRESSION=1`

Phase 87: the ring-3 **write**-path proof the other two lack — boots the AHCI root, logs in, and runs `ext2-coherence-smoke` ON it: a 200 KiB file write + fresh-process byte-verify read-back, so a payload WRITE actually round-trips `blk::remote::write_sectors` → `do_write_ipc` → ring-3 `ahci_driver` `handle_write`; a `data[1]`/recv-buffer regression truncates the write at the driver and fails the gate; skip-with-reason without musl. **Also always-on in CI** (`pr.yml`) since the default smoke/regression suites only exercise in-kernel virtio-blk

## ahci-persist-smoke

**Env var:** `M3OS_AHCI_REGRESSION=1`

Phase 87: the **reboot-persistence** proof — a two-boot gate against the SAME ext2 disk: boot 1 writes a marker to the AHCI root + idles past one periodic write-back flush (deferred metadata drains + `BLK_FLUSH` issued), QEMU is torn down, then boot 2 re-mounts ext2 fresh and re-reads the marker; also asserts boot 1 logged no `[blk] remote block flush failed`. Validates durable on-disk write + remount consistency + the `BLK_FLUSH` IPC path. Caveat: QEMU's writeback host cache means a process restart can't isolate host-power-loss device-flush durability. echo/cat only — no musl. Also always-on in CI

## mitigations-status-smoke

**Env var:** `M3OS_MITIGATIONS_REGRESSION=1`

Phase 84 (extended by Phase 110 A.4 + A.5 + B.3): boots + asserts the boot-policy log **including live KPTI enforcement** — the `[sec] mitigations=…` line must carry `kpti(policy=true active=true) pcid(active=false supported=false) cet(active=false supported=false) global_kernel_ptes=0` (QEMU TCG reports `rdcl_no=false`, so the default `auto` build activates KPTI on every boot now that `KPTI_WIRED` is true; `auto` ≡ `full` for KPTI on this lane, which is how the Phase 110 "assert `active=true` under `full`" acceptance is satisfied without an env rebuild) — and the `m3ctl mitigations status` reporter output: the Meltdown line must read `Meltdown: Mitigation: PTI` (the reporter overrides Meltdown with the *actual* `kpti_active`, so this asserts enforcement, not policy), plus the compiled-in retpoline line, UNADDRESSED enumeration, the **Phase 110 A.5** `KPTI PCID: fallback (full TLB flush; no PCID/INVPCID)` posture line, and the **Phase 110 B.3** `CET: not-supported` posture line. QEMU TCG advertises neither PCID/INVPCID nor CET, so `pcid_active = false` (full-flush fallback, byte-identical to A.4) and `cet_active = false` (the whole shadow-stack substrate is inert); the PCID-**active** boot + ≤30 % perf bound and the CET-**active** shadow-stack + `#CP`-reject arms are bare-metal-only (`next-dell-session.md`). This gate is the CI proof of B.3's no-op path (the CET boot line + reporter line); the CET substrate's inert-path non-regression is covered by `smoke-test`/`termios-smoke`/`cargo xtask test`. The `off` / `RDCL_NO` deactivation arms are host-tested in `kernel_core::spectre`; the CET wire round-trip + formatter states in `kernel_core::spectre` + `m3ctl`.

## kpti-selftest-smoke

**Env var:** `M3OS_KPTI_REGRESSION=1`

Phase 110 Track A.1: boots the **stock** kernel (no special feature) and asserts the boot-time KPTI user-half self-test passes (`KPTI_SELFTEST:PASS`, and no `FAIL`/`SKIP`). The self-test (`kernel/src/mm/kpti.rs`, run unconditionally in `mitigations::init_bsp`) builds a real **user PML4** — a synthetic user page at `USER_VADDR_MIN` plus the minimal **entry set** mapped through *fresh private sub-tables* at their existing kernel VAs (never cloning a kernel `PML4[i]` slot, which would re-expose the direct map and silently defeat KPTI): the `PerCoreData` page(s) — load-bearing because m3OS is `swapgs`-free, so `GS_BASE` = `PerCoreData` in **both** rings and the KPTI entry asm reads `gs:[…]` before the CR3 switch — the page-aligned `.text.kpti_entry` section (as of A.2: both SYSCALL stubs — `syscall_entry` and the CR3-switching `syscall_entry_kpti` — plus the shared body and KPTI-aware sysret tail; the self-test also asserts both stub symbols lie inside the section, since a stub drifting out of the mapped range would instruction-fetch-fault on the user CR3 once live), the **GDT / IDT / TSS** (as of A.3a, mapped at their live linear addresses from `sgdt`/`sidt` + `gdt::tss_extent()` — the CPU reads all three through the *active* user paging when delivering a ring-3 → ring-0 interrupt, so a missing one is delivery-time `#DF` → triple fault), and a fresh entry stack. It then walks the built table back and feeds every present leaf's `(role, present)` to the host-tested `kernel_core::kpti::check_user_half_invariant`, proving on QEMU — which cannot exercise Meltdown itself — that **no** kernel image / heap / kstack / direct-map leaf is reachable from the user CR3, **and** round-trip-translates every entry-set page in the built user PML4 (`translate_in`) to prove everything the live trampoline touches is actually reachable (not merely "nothing extra leaks"). The throwaway pair is freed leak-free (a recording allocator tracks exactly the private page-table frames created; real kernel pages the entry set points at are never freed). A.1 landed the **builder + validation**; A.2 the **syscall CR3 trampoline** (LSTAR-selected on `kpti_active`, so production boots still install the non-KPTI stub); A.3a the **full interrupt-delivery entry set + reachability round-trip**; A.3b the rest of the substrate — the naked IRQ/exception entry stubs (`.text.kpti_irq_entry`, incl. `#PF`/`#GP` error-code and NMI/`#DF` paranoid save-restore variants + IST-top mapping), all four ring0→ring3 exit trampolines (`.text.kpti_exit`: preempt / fork / execve — which now zeroes initial GPRs — / sigreturn, each owning the CR3 flip before its `iretq`), `PageIsolated` page isolation of GDT/IDT/TSS/PerCoreData/BSP entry stacks (asserted page-aligned: `reason=entry-struct-alignment`), and the per-process pair on `AddressSpace` (`build_kpti_user_half` at spawn/fork/execve gated on `kpti_active` and Drop-time `free_user_half`). **A.4 activated the pair** (`KPTI_WIRED = true`): the per-core CR3 pair is published at every dispatch locus (`smp::publish_kpti_cr3_pair` — scheduler dispatch, fork trampoline, execve retarget, `restore_kernel_cr3` republish) and the BSP re-installs LSTAR to `syscall_entry_kpti` after the policy decision, so on this default-`auto` TCG lane (`rdcl_no=false`) **every boot of this gate — and of every other gate — runs ring 3 on the user half live**. A.4 also grew the shared user slots to `kernel_core::kpti::USER_PML4_SLOTS = [0, 255]` — `PML4[255]` is the user stack (`ELF_STACK_TOP`), and sharing only `PML4[0]` silently #PF-looped the first ring-3 stack access — with matching self-test additions: a synthetic stack-slot leaf, the `user-slot-unreachable` round-trip, and the `user-slot-overlap` guard (kernel-image/direct-map slots must never collide with the user-slot set, or the walk's classifier would mask a leak). **Post-A.5 hardening (18–21/n) — the dedicated per-CPU trampoline stack**: TSS.RSP0 targets each core's `kpti_tramp_top` (an 8 KiB address-stable per-core stack whose user-mapped top page joins the entry set) while KPTI is active; the entry stubs copy ring-3 frames to the task kstack after the CR3 switch (to the exact pre-hardening address, `gs:[SYSCALL_STACK_TOP]-160/168`), the exits build their `iretq` frames on the trampoline stack, and **no kernel-stack page is mapped into any user half** — the self-test's `kstack-reachable` negative round-trip asserts this, closing the A.3b ~4 KiB kstack self-leak, and the `CLONE_VM` per-thread kstack map (`map_kstack_top_into_user_half`) is deleted. **Hardening (22/n) — the `#PF`-time top-level-slot sync**: the self-test's `slot-sync` arm stales the throwaway user half's `PML4[255]`, proves the VA goes unreachable, that `sync_slot_raw` (gated by the host-tested `kernel_core::kpti::may_sync_slot_on_fault`, admitting only the declared user slots) repairs it, and that a second sync is a no-op — the boot proof of the live `page_fault_body` recovery that closes the empty-user-slot silent-`#PF`-loop wedge. The self-test still builds a **throwaway** pair (never loaded), keeping the isolation proof independent of the live tables. Set on branches touching `kernel/src/mm/kpti.rs`, the syscall/IRQ entry asm, the exit trampolines, `AddressSpace`, the entry set, the dispatch publish sites, or `kernel_core::kpti`.

## meltdown-poc-smoke + rop-cet-poc-smoke + nested-sig-cet-poc-smoke

**Env var:** `M3OS_SEC_POC_REGRESSION=1`

Phase 110 Dell-validation PoCs — the userspace binaries that *functionally* prove the two Track A/B defenses on real silicon (`/bin/meltdown-poc`, A.6; `/bin/rop-cet-poc`, B.3) plus the nested-signal CET follow-up (`/bin/nested-sig-cet-poc`, B.3). These CI gates are **run-to-completion checks only**: QEMU TCG models no caches, no speculation, and no CET, so the actual leak-reject, `#CP`-kill, and nested-`#CP` security assertions are **bare-metal-only** and live in the [2026-07-09 Dell validation runbook](../handoffs/2026-07-09-dell-validation-session.md) (Blocks 2a/2b/4b). What they assert under QEMU:

- **`meltdown-poc-smoke`** boots, logs in, runs `meltdown-poc`, and asserts it loads (including its ~1 MiB flush+reload channel BSS) and runs the channel primitives + the known-user-byte calibration control + the **mispredicted-branch speculative kernel read** without faulting or panicking — pass = the first kernel-leak line `MELTDOWN_POC:leak off=0` (reached only after `MELTDOWN_POC:start` + the full control arm + one complete speculative round), fail = `meltdown-poc: PANIC`. The speculative illegal read never architecturally retires, so it cannot `#PF` here regardless of KPTI posture — which is exactly why the same binary is safe to run on the KPTI-on default image *and* the `mitigations=off` image at the bench. The leak-vs-no-leak result itself is meaningless under TCG (no cache timing) and is the HW arm.
- **`rop-cet-poc-smoke`** runs `rop-cet-poc`, whose naked `vulnerable()` does a precise return-address overwrite (`mov [rsp],rdi; ret`). With no CET modeled, the overwrite succeeds and transfers to `pwned()` — pass = `ROP_CET_POC:before` then `ROP_CET_POC:PWNED` (itself proof the overwrite *mechanism* works end to end), fail = `ROP_CET_POC:after-NOT-OVERWRITTEN` (the `ret` returned normally — the overwrite missed). On CET silicon the same `ret` instead faults `#CP` (`control_protection_fault_body` kill) and `pwned()` never runs; that is the B.3 HW arm. `rop-cet-poc` ships **without** the userspace stack canary (xtask builds it with `-Zstack-protector=none`) so the overwrite reaches `ret` rather than being caught first by `__stack_chk_fail`.
- **`nested-sig-cet-poc-smoke`** runs `nested-sig-cet-poc`, whose outer `SIGUSR1` handler self-raises `SIGUSR2` from inside itself (`kill(self)` on a *different* signal, so it is not masked and nests on the syscall return). With no CET modeled the nesting + `sigreturn` control flow is what's exercised — pass = `NESTED_SIG_POC:before` then `NESTED_SIG_POC:PASS` (both handlers entered and returned, `main` resumed after the outer `kill`), fail = any `NESTED_SIG_POC:FAIL`/`PARTIAL` or `nested-sig-cet-poc: PANIC`. On CET silicon the *fixed* kernel keeps this a clean PASS (the pre-fix single-slot `cet_signal_ssp` design `#CP`-killed the nested handler's `ret`); a `#CP` kill there is the B.3 HW arm. The fix sources the SSP from the live shadow-stack register (`RDSSP`, MSR fallback) at the delivery seed and the `sigreturn` re-sync — see `kernel/src/arch/x86_64/cet.rs` (`live_user_ssp` / `seed_signal_shadow_stack` / `restore_signal_ssp`).

Set on branches touching `userspace/meltdown-poc`, `userspace/rop-cet-poc`, `userspace/nested-sig-cet-poc`, the CET `#CP` handler (`control_protection_fault_body`), the signal seed/restore (`cet::seed_signal_shadow_stack` / `cet::restore_signal_ssp`), or the KPTI/PCID substrate.

## kgdb-smoke

**Env var:** `M3OS_KGDB_REGRESSION=1`

Phase 111 Track C: builds the kernel with the `kgdb` feature and boots it frozen at a wait-for-debugger `int3` (`KGDB:waiting` on COM1). The gate — a **raw GDB-RSP client** (the dev machine has no host `gdb`; a real `gdb target remote` follows the identical wire path) — connects to the COM2→TCP port (a second `-serial tcp:…,server,nowait`; COM1 stays the live console), sends `qSupported`/`?`, sets a `Z0` software breakpoint at `kgdb_probe_target` (`nm` vaddr **+ the fixed 1 TiB PIE base `0x10000000000`** — the Track A charter correction), continues, and asserts: the unsolicited stop reply arrives, `g` reports `RIP` **exactly** at the breakpoint address, and `m` reads back `KGDB_PROBE_MAGIC` (`0xB16B00B5CAFEF00D`). Then it removes the breakpoint, `c`-runs the guest **free**, and sends a bare `0x03` (GDB Ctrl-C, not a framed packet) — the BSP timer tick polls COM2, sees it, and breaks into the stub at the interrupted RIP; the gate asserts the unsolicited **async-break** stop reply and a valid (non-zero) `RIP`. The stub's `KGDB:release … ticks_before=X ticks_after=Y` line proves the SMP all-stop held (every sibling core parked in its NMI handler with its LAPIC timer frozen, so `X == Y`). Exercises the whole Track C stack: the full-GPR `#BP`/`#DB` naked entry (`bp_entry`/`db_entry`), polled COM2, the `kernel_core::gdb_rsp`-driven command loop, the releasable NMI all-stop, and the timer-tick async-break poll (`kgdb_poll_async_break_*`, BSP-only, a per-tick LSR read guarded by `async_break_pending()`; production timers untouched). The stub is arbitrary kernel peek/poke — **OFF in production** (same posture as `panic-test`/`trace`/telnet); the entire `kernel/src/debug/` module and the ring-0 trap routing are `#[cfg(feature = "kgdb")]`. Set on branches touching `kernel/src/debug/`, the `#BP`/`#DB` entry stubs, `arch/x86_64/debug.rs`, or the `smp` kgdb quiesce. Needs binutils `nm` on the host to resolve the breakpoint address.

## ptrace-smoke

**Env var:** `M3OS_PTRACE_REGRESSION=1`

Phase 111 Track D: builds the kernel with the `ptrace` feature — which adds the `sys_ptrace` syscall (`0x1152`), raises the `#BP` IDT gate to **DPL 3** (so a ring-3 `int3` traps to `#BP` instead of `#GP` → kill), and routes a traced tracee's ring-3 debug traps into a stop-and-notify path — then boots and runs `/bin/ptrace-test`. That native binary is a **self-contained tracer/tracee** (needs no `m3gdbserver` or host GDB): it `fork`s, the child calls `PTRACE_TRACEME` + `int3`, and the parent drives the full cycle, printing a `PTRACE_SMOKE:<step> ok` sentinel per step the gate waits on in order — (1) `stop-sigtrap`: `waitpid` reports the tracee's stop as `WIFSTOPPED`/`SIGTRAP`; (2) `getregs-rbx`: `GETREGS` reads the `0xCAFE` marker the child left in `rbx`; (3) `peek-int3`: `PEEKTEXT` reads the `0xCC` byte from the tracee's code; (4) `poke-roundtrip`: `POKETEXT`+`PEEKTEXT` round-trip a word through the tracee's stack scratch (proving cross-address-space write via the tracee's page tables → physmap, which also bypasses RO text protection for planting breakpoints); (5) `setregs`: `SETREGS` rewrites `rbx` to `42`; (6) `setregs-effect`: after `CONT`, the tracee exits with `rbx`, so the parent's second `waitpid` sees exit code `42` — proving the modified register flowed back into the resumed tracee. Internally the stop uses the `fault_kill_trampoline` redirect technique (`ptrace_stop_trampoline`), a blockable ring-0 continuation, and resumes via `restore_and_enter_userspace`. The feature is arbitrary cross-process register/memory peek/poke — **OFF in production** (same posture as `kgdb`/`panic-test`/`trace`); the `kernel/src/process/ptrace.rs` module, the `Process.ptrace` field, the ring-3 trap consumers, and the DPL-3 gate are all `#[cfg(feature = "ptrace")]`. Set on branches touching `kernel/src/process/ptrace.rs`, the `#BP`/`#DB` ring-3 seam in `arch/x86_64/debug.rs`, `sys_ptrace`, or the `waitpid` stop reporting.

## ptrace-gdbserver-smoke

**Env var:** `M3OS_PTRACE_REGRESSION=1` (same env as `ptrace-smoke`)

Phase 111 Track D.3: the native-gdbserver end of userspace debugging. Boots the `ptrace`-featured kernel, logs in over serial, and launches `m3gdbserver 1234 /bin/ptrace-tracee` — a native GDB stub server (`userspace/m3gdbserver`) that `fork`s + `PTRACE_TRACEME` + `execve`s the tracee, which **exec-stops** before its first instruction (the kernel's `execve` exec-stop: a traced `execve` snapshots the new image's entry registers and parks the tracee, notifying the tracer), then translates the **GDB Remote Serial Protocol** it speaks over TCP into `sys_ptrace` requests — reusing the same host-tested `kernel_core::gdb_rsp` wire codec the in-kernel kgdb stub uses. The gate's client is a **raw GDB-RSP driver on the host** (the dev machine has no host `gdb`; a real `gdb target remote :1234` follows the identical wire path) that connects through a QEMU SLIRP `hostfwd` (a fresh host port → guest `1234`, injected by replacing the netdev arg; the guest's static `10.0.2.15` + virtio-net baseline carry the inbound connection). It drives a full session and asserts each response: `qSupported` → `PacketSize`; `?` → the exec-stop (`S05`); `g` → the entry `RIP`; `s` → single-step (RIP advances exactly one instruction — the only path that exercises the ring-3 `#DB` / `on_user_debug` single-step trap through `PTRACE_SINGLESTEP` → `RFLAGS.TF`); `Z0` at the stepped RIP + `c` → the planted-`int3` breakpoint hit with `RIP` = stepped+1 (m3gdbserver plants the `0xCC` via `POKETEXT`, which writes through the physmap to bypass the tracee's read-only code protection); `z0` + `P` (register write) to remove the breakpoint and rewind `RIP`; an `M`/`m` memory round-trip through the tracee's stack; and a final `c` → the tracee runs to `exit(7)` reported as `W07`. This is the closest analog to a real host-`gdb` remote session — it exercises exec-stop, the RSP↔ptrace translation over an IRQ-driven TCP socket (the kernel is alive, unlike the kgdb stub's frozen-kernel COM2), the amd64 register marshalling, and cross-address-space peek/poke — end to end. `m3gdbserver` is a debug tool but harmless in production (it only works against processes it launches under trace, and `sys_ptrace` returns `ENOSYS` without the `ptrace` feature); the gate itself requires the feature. Set on branches touching `userspace/m3gdbserver`, the `execve` exec-stop, or anything in the `ptrace-smoke` list.

## pkgcache-hit-check

**Env var:** `M3OS_PKGCACHE_REGRESSION=1`

Phase 85a: second build of a warmed-cache port performs zero compiler invocations — pure `.m3pkg` hit; requires a musl cross-compiler for the initial warm build

## pkg-smoke

**Env var:** `M3OS_PKG_REGRESSION=1`

Phase 85a: boots m3OS, then exercises the offline `pkg` manager against the bundled `/usr/pkg/` repo — `install`/`list`/`verify`/`upgrade`/`remove` of the dependency-free `libevent` leaf (verifying `/usr/local/lib/libevent.a`), then `pkg install tmux` to prove the dependency solver auto-installs tmux's `libevent` dep; proves the `.m3pkg` round-trips build → image → in-OS install

## git-local-smoke

**Env var:** `M3OS_GIT_REGRESSION=1`

Phase 85b: cross-builds the local-only `git` + zlib into `.m3pkg`s, boots m3OS, `pkg install git` from the bundled `/usr/pkg/` repo — exercising the solver's `DEPS=zlib` auto-install — then drives a scripted local repo workflow: `init` → add/commit → edit + `diff` shows the change → second commit → `log --oneline` shows two commits → `checkout -b feature` + commit → `checkout main` + `merge feature` (both files tracked) → `status` reports a clean tree. Requires a musl cross-compiler for the initial git build

## git-ssh-smoke

**Env var:** `M3OS_GIT_SSH_REGRESSION=1`

Phase 86b: cross-builds the static client-only **dropbear** `ssh` (`dbclient`) into an `ssh.m3pkg`, reuses `git` via `GIT_SSH` (the SSH transport is orthogonal to git's internals — as of Phase 86c that git is the HTTPS-capable build), builds a fresh image with both bundled into `/usr/pkg/` + GitHub's ed25519 host key seeded into `/root/.ssh/known_hosts`, boots m3OS, then over serial: `pkg install ssh` + `pkg install git` succeed, `dbclient -V` / `ssh -V` report `Dropbear v2024.86` (the static client *runs* on m3OS), and `cat /root/.ssh/known_hosts` shows the seeded GitHub `ssh-ed25519` key (TOFU data round-trips the VFS). The live `git clone --depth 1 --single-branch ssh://git@github.com/...` (via `GIT_SSH`) and the host-key-**mismatch reject** negative test are **opt-in** — `M3OS_GIT_SSH_NET=1` runs the mismatch reject (egress to `github.com:22`, no key needed — host key compared at KEX before auth); `M3OS_GIT_SSH_KEY=<path>` stages a GitHub-registered key as `/root/.ssh/id_dropbear` and runs the clone — and **skip-with-reason** when unconfigured (mirroring `tls-smoke`/`dns-smoke` PASS-vs-SKIP). The mismatch-reject step is a `WaitEither` that passes on the real reject (`host key mismatch for`) **or** the formerly-localized non-blocking-`connect` blocker (`Connect failed`) and fails on a silent accept; with non-blocking `connect` now implemented in Phase 86b (`EINPROGRESS`/poll-`POLLOUT`/`getsockopt(SO_ERROR)`, proven by the always-on `connect-smoke` in the main `smoke-test` flow), `M3OS_GIT_SSH_NET=1` **matches the real reject** live against `github.com:22` — dropbear's non-blocking `connect()` establishes, reaches KEX, and the planted bad key is rejected and left on disk. (As of Phase 86c `git` is the HTTPS-capable build; the SSH gate is unaffected since the `ssh://` transport is independent of git's curl linkage.) Requires a musl cross-compiler for the initial dropbear build

## git-https-smoke

**Env var:** `M3OS_GIT_HTTPS_REGRESSION=1`

Phase 86c: cross-builds the HTTPS transport chain — a trimmed client-only static **mbedTLS** 3.6.2 (`sys_getrandom` entropy, no `/dev/urandom`), a static `libcurl --with-mbedtls --with-ca-bundle` 8.15.0, and **git rebuilt WITH curl** (`NO_CURL` removed; `git-remote-https` + `curl_multi_perform` + `mbedtls_ssl_handshake` present, `SSL_CTX_new` absent), bundles curl/mbedtls/ca-certificates/git into `/usr/pkg/`, boots m3OS, then over serial: `pkg install git` pulls the whole `zlib → mbedtls → ca-certificates → curl → git` chain via the solver, `curl --version` reports `mbedTLS/3.6.2` (the static TLS stack *runs* on m3OS), `git config` shows `http.sslVerify=true` + `http.sslCAInfo=/etc/ssl/certs/ca-certificates.crt`, and the installed CA bundle's `Bundle of CA Root Certificates` header round-trips the VFS. The live **bad-cert REJECT** (`self-signed.badssl.com` → TLS fails closed) and the public `git clone https://github.com/octocat/Hello-World.git` (`info/refs` validated by the `application/x-git-upload-pack-advertisement` Content-Type + the `# service=git-upload-pack` pkt-line magic, then a packfile transfer) are **opt-in** — `M3OS_GIT_HTTPS_NET=1` runs both (egress to `:443`, **no secret** — the cert is checked before auth) — and **skip-with-reason** when unconfigured (mirroring `git-ssh-smoke`). PAT auth is configured via `credential.helper store` (documented), not exercised by the anonymous positive arm. Runs at `--timeout 5400` (the ~27 MB install over the slow VFS + packfile transfer). Requires a musl cross-compiler for the initial mbedtls/curl/git build

## python-smoke

**Env var:** `M3OS_PYTHON_REGRESSION=1`

Phase 85c: two-stage cross-builds a **fully static** CPython 3.12 (+ zlib) into a `.m3pkg`, boots m3OS, `pkg install python` from the bundled `/usr/pkg/` repo — exercising the solver's `DEPS=zlib` auto-install — then over serial: `python3 --version` reports 3.12.8, a `-c` run imports `json,re,math,datetime,argparse,hashlib,dataclasses,pathlib,os,secrets` + checks `sys.platform=linux` + a HACL `hashlib.sha256` digest + `os.urandom`/`secrets`, runs the bundled `/usr/src/fibonacci.py`, and round-trips a `/tmp` file write+read. Runs at `--timeout 900` because cold imports over the slow ring-3 VFS take minutes. Python is static — m3OS's `/lib/ld-musl` is a custom loader with no real `libc.so`, so a dynamic interpreter can't run. Requires a musl cross-compiler for the initial build

## coreutils-smoke

**Env var:** `M3OS_COREUTILS_REGRESSION=1`

Phase 94: cross-builds the upstream **uutils/coreutils 0.9.0** `.m3pkg` — the project's **first Rust-cargo port** (`x86_64-unknown-linux-musl`, prebuilt-std, static multicall binary + per-applet symlinks) — boots m3OS, `pkg install coreutils` (`DEPS=` empty), then over serial runs a GNU-compatibility battery: `coreutils --version` (the static Rust musl ET_EXEC runs via the Linux-compat layer), a bare `ls --version` proving the `/usr/local/bin` applet **shadows** the ramdisk `/bin/ls` by PATH precedence, `ls -l` confirming `/usr/local/bin/ls` is a **symlink → coreutils** (the `.m3pkg` symlink round-trip), `ls -la /`, a recursive `cp`/`mv`/`rm` tree (the `rm -r` exercises **`unlinkat`(263)**), a recursive `chmod -R` + `chown -R` and `install -D` (exercising **`fchmodat`(268)/`fchownat`(260)/`mkdirat`(258)** — the fd-relative `uucore::safe_traversal` family, like `unlinkat`, with no musl legacy fallback), `wc -l`, a 50000-line `sort` (rayon-parallel + std threads), `env`, a `sha256sum` digest **cross-checked** against the ramdisk crypto-lib `/bin/sha256sum`, and an **inode-identity battery on the ext2/`vfs_server` root** (the Phase 88 `st_ino` rigor, now exercised on the uutils path: `ln` a hardlink → `stat -c %h` reports `nlink` 2 then 1 across an `rm` of the original name whose content survives via the link, and `stat -c %i … | sort -u | wc -l` confirms hardlinked names share one inode while two distinct files have distinct **non-zero** inodes — the regression guard against the 85d `st_ino=0` collapse); finally `pkg remove coreutils` and a bare `ls` proving the ramdisk floor fallback. Skip-with-reason when the prebuilt-std `x86_64-unknown-linux-musl` Rust target is absent; far lighter than the git-https/clang gates (one multicall binary + symlinks), `--timeout 1800`. Set on branches touching `ports/util/coreutils`, xtask `build_uutils` / coreutils bundling, `rust-toolchain.toml`, the `pkg` installer, the kernel fd-relative `*at` syscalls (`unlinkat`/`fchmodat`/`fchownat`/`mkdirat`), the `link`/`linkat` + `fill_stat`/`st_ino` path, or the static-ELF/Linux-compat paths a Rust musl binary exercises

## clang-smoke

**Env var:** `M3OS_CLANG_REGRESSION=1`

Phase 85d: host-cross-builds an opt-in, X86-only, statically musl-linked **clang + lld** (+ a self-contained libc++) into a `.m3pkg`, builds a fresh image with the `M3OS_WITH_CLANG` feature so it bundles into `/usr/pkg/`, boots m3OS, `pkg install clang` (no deps), then INSIDE m3OS: `clang --version` reports 18.1.8, `clang -print-resource-dir` resolves under `/usr` (relocation contract), `clang -O2 /usr/src/hello.c` compiles + links (lld) + **runs** (`CLANG_C_OK`), `clang++ /usr/src/hello.cpp` links the bundled libc++ + runs (`CLANG_CPP_OK`), and `clang -fuse-ld=lld` links via LLD. Runs at `--timeout 5400` because the ≈125 MB install (the installer reads + SHA-verifies the whole 124 MiB `.m3pkg`, then writes ~1500 files) and each cold 64 MiB-clang invocation over the ~200 KB/s ring-3 VFS take tens of minutes — a deliberately heavy opt-in gate. Clang is static + X86-only; the host clang is the cross-compiler since musl-tools has no C++ compiler. **Phase 88**: the pre-push gate runs it under `M3OS_CLANG_STRESS=1` — a repeated multi-compile that reliably drives clang's `(st_dev, st_ino)` `FileManager` dedup (the exact 85d defect), making this the **stat-identity regression guard** for the `fill_stat` / VFS-reply-metadata / ext2-consolidation work: a reintroduced `st_ino=0` or `fstat≠fstatat` collision resurfaces as `redefinition of 'main'`. Requires host clang + cmake + ninja for the initial build

## rustc-smoke

**Env var:** `M3OS_RUST_REGRESSION=1`

Phase 95: cross-builds a **dynamic** musl `x86_64-unknown-linux-musl` **`rustc`** `.m3pkg` (~368 MB) — the project's first on-device Rust *compiler*, the Rust analog of Phase 85d clang — via the upstream `x.py` bootstrap (host clang as the musl cross; from-source X86 musl LLVM-22; a dedicated `-fPIC` musl libc++ built from the llvm port's libcxx source; per-target `CXXFLAGS=-stdlib=libc++` so `rustc_llvm` links libc++). It is **dynamic, not static**, because rustc's own proc-macro deps can't build on a `crt-static` musl host — so `rust.m3pkg` `DEPS=musl` and the gate bundles `musl.m3pkg` (the Phase 93 `/usr/lib/libc.so`) too. Builds the `M3OS_WITH_RUST` image, boots m3OS, and `pkg install rust` (solver installs `musl` first) **succeeds on-device today** (the always-asserted arm). The INSIDE-m3OS code-generation arm — `rustc --version` 1.96.0 (the dynamic musl rustc loading via the Phase 93 `libc.so` + Phase 85d loader), `rustc --print sysroot` under `/usr` (the prebuilt std relocation contract, mirroring clang's resource dir), and `rustc /usr/src/hello.rs` → the bundled **`rust-lld`** (`-C linker-flavor=ld.lld -C link-self-contained=yes`, since m3OS has no system `cc`/`ld`; rust-lld is itself dynamic against the bundled `libLLVM.so`) → run (`RUSTC_OK`, proc-macro-free by construction) — **PASSES (Phase 95b) under `M3OS_KVM=1`**: `rustc hello.rs` compiles, **multithreaded** rust-lld links it, the native binary runs (`RUSTC_OK`), 0 kernel faults, ~53 s fresh-install. 95b reworked the `ld-musl` loader + kernel mm from whole-file read+copy to a **streaming / demand-paged file-backed `mmap`** (`MAP_LAZY_FILE` + a blocking `vfs_server` read from the page-fault handler) so the ~162 MB `librustc_driver.so` demand-pages instead of being read+copied in full, and cleared a crash chain (a process-page-table fix; a `FIONBIO` `ioctl`; kernel `AT_EXECFN` + loader `DT_RUNPATH`/`$ORIGIN` so rust-lld finds `libLLVM.so`; a **cross-DSO TLS-at-offset-0** loader fix so rust-lld's parallel-reloc worker threads read `llvm::parallel::threadIndex` correctly; and a **thread-group fatal-kill** robustness fix). Running the arm under plain TCG awaits Phase 95c's VFS-throughput work (the multi-hundred-MB install time), so absent `M3OS_KVM=1` the gate is heavy. The gate fails honestly without `RUSTC_OK` — there is no path to PASS otherwise. Runs at `--timeout 5400` (the multi-hundred-MB install + cold rustc load — clang-gate class; far faster under KVM). Builds the `llvm`/`musl` ports first (the reused musl libc++ sysroot + libc.so; a warm pkgcache makes them zero-compiler hits). Skip-with-reason when the host toolchain (clang/cmake/ninja/ld.lld/python3/musl-dev) needed to build rustc is absent. Set on branches touching `ports/lang/rust`, `build_rust`, the `M3OS_WITH_RUST` bundling, the `ld-musl` loader / kernel file-backed-mmap demand-fault path, or `cmd_rustc_smoke`. (`cargo` + proc-macros via on-device `dlopen` against the Phase 93 `libc.so` remains a deferred follow-up, gated separately by `cargo-smoke` / `M3OS_CARGO_REGRESSION`.)

## go-runtime-smoke

**Env var:** `M3OS_GO_REGRESSION=1`

Phase 86d: cross-builds a fully **static** (`CGO_ENABLED=0`, `GOTOOLCHAIN=local`) Go 1.24 program into a `.m3pkg` using the downloaded official toolchain — no musl, no `DEPS`; bundles it via `BUNDLE_ONLY_PORTS`, boots m3OS, `pkg install go`, then over serial runs `/usr/bin/go-runtime-probe http://10.0.2.100:80/`: `GO_HELLO_OK` (the Go runtime starts — scheduler/GC + `getrandom`/`AT_RANDOM` bootstrap), `GO_GOROUTINE_OK` (a `LockOSThread` goroutine completes a channel rendezvous; the runtime's `clone(CLONE_THREAD)` thread-creation path — `sysmon` etc. — is exercised), and `GO_HTTP_OK` (a **plaintext** HTTP GET over the in-kernel TCP stack to a host server reached through a SLIRP `guestfwd` rule at `10.0.2.100:80` — no TLS, no DNS, no real egress). Proves the three Phase 86d kernel blockers are cleared end-to-end: `mmap` `MAP_FIXED` arena commit (Track A), edge-triggered `epoll`/`epoll_pwait` (Track B), `SIGURG`/`tgkill` (Track C). Runs at a long `--timeout` (the ~5.5 MB install + cold static-binary load over the slow ring-3 VFS take minutes). HTTPS-over-Go is deferred (rides 86c → exercised in 86e). Requires the Go toolchain download for the initial build

## gh-smoke

**Env var:** `M3OS_GH_REGRESSION=1`

Phase 86e: cross-builds the static **GitHub CLI** `gh` 2.82.1 from `cli/cli` source with the same pinned Go 1.24.6 toolchain the 86d `go` port uses (`CGO_ENABLED=0`, `go build -trimpath -ldflags '-s -w -X .../internal/build.Version'`, ~55 MB, no `DEPS`), seals it into a `.m3pkg`, builds a fresh image with the opt-in `M3OS_WITH_GH` feature (so it bundles into `/usr/pkg/`, mirroring `M3OS_WITH_CLANG`; default images omit it), boots m3OS, `pkg install gh`, then over serial the **always-on core** proves `gh --version` reports `gh version 2.82.1` — the heavy TLS-capable Go binary RUNS on the 86d runtime. The **authenticated arms** are opt-in: when `GH_TOKEN=<pat>` is set the gate seeds the token at mode `0600` under `/root/.config/gh/` (the value never crosses serial — only the path is sent), exports `GH_TOKEN`/`SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt`, runs `gh auth setup-git` (registering `gh` as the `https://github.com` git credential helper into the 86c curl+TLS+PAT path), a **read** (`gh pr list`) over real HTTPS to `api.github.com`, and asserts `~/.config/gh/hosts.yml` is `-rw-------`; the mutating **write** (`gh issue create`) runs only with `M3OS_GH_WRITE=1` + `M3OS_GH_WRITE_REPO=<owner/repo>`. Absent `GH_TOKEN` the authenticated arms are **skip-with-reason** (a secret can never live in repo/CI — mirroring `git-https-smoke`). Secret hygiene is by construction (token only in 0600 files under `~/.config/gh`, never serial, never `/tmp`). Runs at a clang-gate-class `--timeout 5400` (the ~55 MB install + cold Go runs over the ~200 KB/s VFS take tens of minutes). Requires the Go toolchain + gh-module download for the initial build

## node-smoke

**Env var:** `M3OS_NODE_REGRESSION=1`

Phase 89: cross-builds the fully-static jitless Node.js 22 + npm `.m3pkg` via build_node, builds a fresh `M3OS_WITH_NODE` image so it bundles into `/usr/pkg/`, boots m3OS, `pkg install node`, then over serial proves the **always-on local runtime** (PASSES): `node --version` is `v22.22.3`; `node /usr/src/node-probe.js` emits NODE_HELLO_OK/FS/PROC/EVENTLOOP/**TIMER** (TIMER exercises the A.1 timerfd event-loop wakeup end-to-end); `node -e` loads tls/dns/crypto (NODE_TLSDNS_OK); and **`NODE_EGRESS_OK` — a full libuv `http.get` request/response cycle over the in-kernel TCP stack** to a SLIRP host server at 10.0.2.100:80 (always-on, no real internet). Required **two** kernel fixes: (1) `F_SETFD`→`EBADF` (node's libuv CLOEXEC-all-fds loop busy-spun forever on the old silent-success → startup hang); (2) implementing `FUTEX_REQUEUE`/`FUTEX_CMP_REQUEUE` (the silent no-op deadlocked libuv's threadpool condvar — musl `pthread_cond` requeues cond-waiters onto the mutex — so `http.get`/getaddrinfo hung with `BlockedOnFutex "no waker registered"`; the egress arm is the regression guard). Honors `M3OS_KVM=1` (near-native — node's cold ~56 MB streamed exec + V8 init are ~10–50× faster than TCG) and `M3OS_NODE_FAST_ITER=1` (reuse an installed disk). **The only opt-in arms (`M3OS_NODE_NET=1`)** are the live HTTPS cert-validate + `npm install` — they need real outbound internet (example.com:443, registry.npmjs.org) which repo CI lacks (mirroring `git-https-smoke`'s `M3OS_GIT_HTTPS_NET`); skip-with-reason otherwise. m3OS has no 127.0.0.1 loopback, but the always-on egress arm proves the TCP path regardless. Runs at `--timeout 5400` (the ~120 MB install + cold static-binary load over the slow VFS — far faster under `M3OS_KVM=1`). Absent a host C++ toolchain (clang/lld/python3/make) + the llvm musl sysroot, the gate prints SKIP and returns success.

## userspace-simd-smoke

**Env var:** `M3OS_SIMD_REGRESSION=1`

Phase 86f Track C.3: static `objdump -d` asserts `crypto-test` (x86_64-m3os.json hardware-float, `+sse,+sse2,+aes`) contains XMM-register instruction lines and AES-NI (`aesenc`/`aesenclast`) while the kernel ELF (x86_64-unknown-none soft-float, `-sse`) contains none; then boots m3OS and over serial: `crypto-test` prints `all tests PASSED` (SSE+AES-NI binary runs fault-free on the 16-aligned entry stack — B.2 falsifiable proof), and `crypto-test --bench` emits a `BENCH:aes-ctr:` sentinel (AES-NI path executed in-OS; throughput not asserted — TCG distorts). Set on branches touching `x86_64-m3os.json`, the `build_userspace_bins` target selection, the signal-frame FPU save/restore path, the ABI stack alignment in `setup_abi_stack_with_envp`, or the `crypto-lib` AES path

## pku-smoke

**Env var:** `M3OS_PKU_REGRESSION=1`

Phase 90a Track D.1: boots m3OS and runs the ramdisk-embedded `pku-smoke` binary, which emits one `PKU_SMOKE:<case>:ok` (or `:SKIP` on a no-PKU CPU) sentinel per case + a final `PKU_SMOKE:done`, falsifiably exercising the Track B PKU substrate + the Track C.1 W^X v2 pkey-guarded exception independent of V8: **alloc** (`pkey_alloc(0, PKEY_DISABLE_WRITE)` ≥ 1, free, re-alloc reuses the slot, `pkey_free(0)`/`pkey_free(unallocated)`/`pkey_alloc(bad-rights)` → EINVAL); **exhaust** (exactly 15 allocatable keys then ENOSPC; key 0 reserved); **deny_fault** (the core PKU proof — a write to a page tagged with a write-deny key must trap; m3OS kills an unhandled-fault process, so the write runs in a `fork`ed child and the parent asserts the child was killed by SIGSEGV via `waitpid` status `& 0x7f == 11`); **asym** (per-context PKRU register asymmetry — the parent opens a write window with `WRPKRU` and writes the tagged page successfully, while a forked child closes the window and faults writing the SAME page, two independent PKRU registers, opposite outcomes; then re-asserts the parent window is still open, catching a PKRU leak); **sigframe** (B.4 signal-frame PKRU preservation — open a write window, raise SIGUSR1, the handler `RDPKRU`s the still-open window + writes through it, and the window persists after `sigreturn`); **wx_v2** (the W^X v2 matrix — `pkey_mprotect(RWX, write-deny-key)` and `(RWX, access-deny-key)` SUCCEED (the v2 grant), while `pkey_mprotect(RWX, key=0)`, `(RWX, permissive-key)`, plain `mprotect(RWX)`, and `mmap(RWX)` all → EINVAL; the four reject arms hold and are asserted on a no-PKU CPU too — the unchanged Phase 75 v1 rule). On a no-PKU configuration (TCG without `pku`) the binary auto-detects via a `pkey_alloc` ENOSPC probe and prints `PKU_SMOKE:<case>:SKIP (reason: no PKU — …)` for the five hardware-dependent arms while still asserting the v1 W^X rejections; `M3OS_KVM=1` on a PKU host (Ryzen Zen 4 etc.) exposes real PKU to the guest and runs the full matrix. The gate fails on any `:FAIL`/`PKU_SMOKE:panic` line via `WaitPassOrFail`. Set on branches touching `kernel/src/arch/x86_64/pkru.rs`, the `pkey_*` syscalls (329/330/331) in `kernel/src/arch/x86_64/syscall/mod.rs`, `kernel_core::pkey`, the `wx_decision` W^X v2 guard, the per-task/signal-frame XSAVE component-9 RFBM, the PTE pkey composition in `mm/pkey.rs`, or the fork PKRU/PTE-tag inherit path

## kstack-overflow-smoke

**Env var:** `M3OS_KSTACK_OVERFLOW_REGRESSION=1`

Track D of `docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md`: builds a kernel with the `kstack-overflow-test` feature, boots m3OS **single-core on plain TCG** (no PKU/KVM), and runs the ramdisk-embedded `kstack-overflow-test` binary — a child invokes `SYS_KSTACK_OVERFLOW_TEST` (0x1150) which recurses until it overflows its per-task kernel stack and hits the slot's guard page. The gate asserts the kernel turned the resulting ring-0 guard-page fault into a **SIGSEGV of the child** (`KSTACK_OVF:killed:ok`, `waitpid` status `& 0x7f == 11`) while the **parent kept running** (`KSTACK_OVF:survivor:ok`) → `KSTACK_OVF:done` — i.e. the controlled-kill **recovery** fired (kill the offending task, core returns to the scheduler), NOT the pre-Track-D `hlt_loop` core-wedge. Observed manifestation: a gradual recursion marches RSP into the guard so the overflow surfaces as a **#DF** (the guard-page #PF can't push its frame onto the exhausted stack) caught on the clean DF IST stack; the always-on #PF path covers the real-world large-single-frame manifestation (the cli.js repro). A regression that reverts the recovery to `hlt_loop` hangs the single-core box → the gate times out (a wedged core never reschedules the parent). The probe syscall is feature-gated (ENOSYS in production); the recovery itself (interrupts.rs #PF/#DF handlers, the per-core fault-recovery stack, `hlt_loop`'s offline marking) is always-on. Set on branches touching the page-fault/double-fault handlers, the fault-recovery stack / kill trampoline, the kstack guard-page classifier, or `hlt_loop`

## smp-smoke

**Env var:** `M3OS_SMP_REGRESSION=1`

The permanent **multi-core SMP regression gate** for `docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md`: builds the static Node `.m3pkg`, boots m3OS on **multiple cores** (default `-smp 4`; `M3OS_SMP=<N≥2>` overrides), `pkg install node`, then runs a **futex-heavy libuv-threadpool stress** — `node -e` submitting 256 async `pbkdf2` ops with 16 in flight, each completion resubmitting, saturating the threadpool↔event-loop futex WAIT/WAKE handshake across cores — which must **COMPLETE** (`SMP_STRESS_OK 256`). This guards the five multi-core fixes at once: **(1)** the cross-core **lost-wakeup** (`block_current_until` re-check) — a regression strands a worker `BlockedOnFutex` and the kernel's `no waker registered` watchdog verdict trips the `WaitPassOrFail` fail-fast; **(2)** the **TLB-shootdown panic** survival (Tracks A–D, fail-fast on `KERNEL PANIC`); **(3)** the **CoW/mprotect spurious-fault wrongful-kill** (fail-fast on `process killed`) — under KVM (`M3OS_KVM=1`, real PKU) this also covers **(4)** the W^X-v2 PKU spurious-loop; and **(5)** the **COM1-RX-under-SMP byte-drop** — every injected command (install/version/the `node -e` stress) would garble if the serial RX path regressed, timing out the step. Also fails fast on `RECURSIVE KERNEL PAGE FAULT`. Runs under KVM (near-native + real PKU) or plain TCG; honors `M3OS_NODE_FAST_ITER=1`. SKIPs cleanly without the host C++ toolchain + llvm musl sysroot (mirrors `node-smoke`). Runs at `--timeout 5400`. Set on branches touching the scheduler block/wake path (`block_current_until`/`wake_task_v2`), the page-fault/double-fault handlers, `smp/tlb.rs`, `smp/boot.rs` (`is_online`), `hlt_loop`, or the serial RX path

## node-jit-smoke

**Env var:** `M3OS_NODE_JIT_REGRESSION=1`

Phase 90a Track D.3: the phase's falsifiable payoff — V8 generating real machine code at runtime on m3OS under the W^X v2 invariant, and WASM (the thing jitless permanently ruled out) executing. Cross-builds the **JIT** `build_node` variant (`M3OS_NODE_JIT=1` — drops `--v8-options=--jitless`, applies the three A.1 V8/Node PKU patches (musl `pkey_*` shim + `NodePlatform::GetThreadIsolatedAllocator` override + `KernelHasPkruFix` m3OS-accept), sealed under its own `jit` content key), builds a fresh `M3OS_WITH_NODE` image bundling that JIT `.m3pkg` into `/usr/pkg/`, boots m3OS, `pkg install node`, then over serial proves: (1) **`NODE_JIT_OK`** — `node --allow-natives-syntax /usr/src/node-jit-probe.js` warms a hot loop, calls `%OptimizeFunctionOnNextCall` then `%GetOptimizationStatus`, and asserts the kOptimized bit (`1 << 4`) is set (real optimized/TurboFan machine code — NOT just WASM, which the A.1 findings showed executes even jitless-flagged; the kTurboFanned bit `1 << 6` additionally prints `NODE_JIT_TF`); (2) **`NODE_WASM_OK`** — `new WebAssembly.Instance(new WebAssembly.Module(bytes))` of a trivial `(export "add")` module executes `add(2,3)===5` (the exact capability the Phase 90b yoga.wasm TUI needs); and (3) the **negative/security arm** — the kernel logged `[wx] v2-guarded W+X mapping (pkey=N)` (V8's code-space W+X commit went through the PKU-guarded path; asserted FIRST via a non-consuming `Wait` since `WaitPassOrFail` drains its match, and the v1 unguarded-RWX rejection logs nothing + returns EINVAL → would have aborted V8 before any sentinel printed, so the v2-guarded line present + `NODE_JIT_OK`/`NODE_WASM_OK` passing proves no unguarded W+X grant). The JIT variant **REQUIRES PKU** — per A.1, on a no-PKU CPU V8's `pkey_alloc` ENOSPCs, ThreadIsolation stays disabled, V8 falls back to plain-RWX commits the kernel rejects, and the binary aborts at first code-space commit (it does not degrade to jitless). m3OS sees PKU only under `M3OS_KVM=1` on a PKU host (`-cpu host` surfaces real `pku`+`ospke`; TCG advertises none), so the gate detects no-KVM up front and **skip-with-reason** (mirroring `tls-smoke`/`dns-smoke`/`pku-smoke`), threading `M3OS_KVM` explicitly into the `DeviceSet`. Absent a host C++ toolchain (clang/clang++/ld.lld/python3/make) + the llvm musl sysroot it also SKIPs. Runs at `--timeout 5400` (the ~120 MB install + cold V8 init — far faster under the required KVM). Set on branches touching the `build_node` JIT path, the `wx_decision` W^X v2 guard / `pkey_mprotect`, the PKU substrate, or anything V8 JIT/WASM exercises

## claude-smoke

**Env var:** `M3OS_CLAUDE_REGRESSION=1`

Phase 90b: fetch-and-stages the pinned `@anthropic-ai/claude-code@2.1.112` npm bundle (the LAST `cli.js`-under-Node version — 2.1.113+ repackaged into a per-platform native Bun binary that does not use Node, which would break the `DEPS=node` chain) into a `.m3pkg` via `build_claude_code`, builds a fresh `M3OS_WITH_CLAUDE` image bundling **both** the claude-code `.m3pkg` **and** the node it `DEPS=node`-depends on, boots m3OS, then over serial proves the **always-on offline core** (PASSES): `pkg install claude-code` succeeds with the solver installing **node FIRST** (`pkg install: node: OK` before `pkg install: claude-code: OK` — the dependency-first proof), `claude --version` is `2.1.112` (the `/usr/bin/claude` launcher — a `#!/usr/bin/env node` CJS wrapper that imports `cli.js` in-process, since m3OS's `/bin/sh`=`ion` can't run a shebang script with flag args — cold-loaded over the slow VFS), `claude --help` renders (`Usage: claude`), the vendored `vendor/ripgrep/x64-linux/rg --version` runs (the on-OS proof m3OS's ELF loader handles the **static-pie `ET_DYN`** search binary, B.2), and the A.2 interactive-substrate probes pass (`NODE_SIGINT_OK` self-pipe signal / `NODE_SPAWN_OK` `child_process` fork-exec / `NODE_RAWMODE_OK` termios toggle — the three primitives an interactive agent lives on, closing the deferred Phase 89 A.2 item). Two always-on regression guards back the TUI work: a `SEG_OK 4` `Intl.Segmenter` step (no PKU/network — catches a regression to small-icu) rides the core. **By default the bundled node is the JITLESS variant** → the core is **CI-viable under plain TCG** (no PKU needed); `M3OS_CLAUDE_JIT=1` bundles the **90a JIT node** instead (the embedded-`yoga.wasm` interactive-TUI / runtime-WASM variant) and adds an **automated interactive-TUI render arm** (`claude_tui_render_arm`: launches `claude` in the graphical `term`, screendumps via QMP, and asserts **592 changed band scanlines** vs the empty-prompt baseline — threshold 20, a blank screen ≈ 0; the captured screenshot shows the rendered "Welcome to Claude Code v2.1.112" onboarding logo splash), and IS KVM/PKU-gated (skip-with-reason without `M3OS_KVM=1` on a PKU host). Running the real-world `cli.js` forced two fixes: one kernel **W^X-v2 cross-thread PKU read-recovery** (a sibling worker thread DATA-reading the per-thread-PKRU-guarded V8 code space → `PROTECTION_KEY` fault; the page-fault handler grants read on guarded *executable* pages, writes stay gated → W^X intact) — landed as a documented 90a PKU follow-up, the integration test surfacing the roadmap's pre-flagged SMP-PKU gap — and a node build switch from `--with-intl=small-icu` to **full-icu** (small-icu omits the ICU break-iterator data `Intl.Segmenter` needs for the TUI's grapheme segmentation, which null-deref'd V8's `JSSegments::Create`; the `mremap`/`io_uring`/`capget` syscalls in the earlier trace were red herrings — they correctly return `-ENOSYS` and well-behaved callers fall back). PASSES including the TUI render on the JIT node. The **opt-in authenticated arms** (`M3OS_CLAUDE_NET=1` + a credential seeded at mode 0600 under `/root/.claude/` from the dedicated `M3OS_CLAUDE_TOKEN`=subscription-OAuth (preferred) or `M3OS_CLAUDE_KEY`=API-key — the value never crosses serial, never repo/CI, mirroring `gh-smoke`) run an authenticated `claude -p` round-trip to `api.anthropic.com` over real HTTPS (`CLAUDE_API_OK`), a real-filesystem agent workflow asserted by `cat` (not the model's own claim — `WORKFLOW_FILE_OK`), and the `-rw-------` credential-hygiene check. Honors `M3OS_KVM=1` (near-native, required for the JIT arm) + `M3OS_CLAUDE_FAST_ITER=1` (reuse an installed disk); absent a host C++ toolchain + the llvm musl sysroot it SKIPs. Runs at `--timeout 5400`

## vfs-throughput-smoke

**Env var:** `M3OS_VFS_THROUGHPUT_REGRESSION=1`

Phase 95c Track E.1: boots m3OS on a fresh data disk, runs the ramdisk `vfs-throughput-probe` binary — writes 8 MiB + reads back + verifies the byte pattern, bracketed by `/proc/blkstats` device-block-op snapshots — and asserts `verify=ok` + direction-specific ceilings anchored to the per-block regression it guards (`n_bytes/4096` ≈ 2048 for 8 MiB): `write_calls_delta` ≤ ¾·per-block (1536) and `read_calls_delta` ≤ ½·per-block (1024). Measured baseline write≈649 / read≈134 (write is metadata-dominated: inode+bitmaps+indirect blocks), so the ceilings carry comfortable headroom over the baseline yet trip on a collapse to per-block round-trips — the falsifiable IPC-count guard for the ring-3 VFS coalescing path. **Also asserts `inkernel_root_reads_delta` ≤ 96** — the **ext2-engine-unification** regression guard: post-boot the ring-3 `vfs_server` must be the *sole* root ext2 reader, so the in-kernel `EXT2_VOLUME` engine (counted via `IN_KERNEL_ROOT_READS` → `/proc/blkstats` `inkernel_root_reads`) should serve essentially no root reads; a regression that re-routes root reads back into ring 0 (eroding the single-engine invariant + the safe-write-back precondition) trips this ceiling. Opt-in, no special host toolchain. Set on branches touching `vfs_server` read/write coalescing, `VFS_MAX_PREAD/PWRITE`, the ext2 block-run/allocation path, **or the ext2-engine-unification routing** (`open_ext2_file_routed` in `syscall/mod.rs`, the `IN_KERNEL_ROOT_READS` counters in `kernel/src/fs/ext2.rs`, or `/proc/blkstats` `inkernel_root_reads`)

## vfs-bulkio-smoke

**Env var:** `M3OS_VFS_BULKIO_REGRESSION=1`

Phase 87: boots m3OS, reads `/proc/blkstats` (the per-boot block-request counters added on `blk::read_sectors`/`write_sectors`), `pkg install`s the dependency-free `mbedtls` (~3.8 MiB, read **and written** through the VFS), reads `/proc/blkstats` again, and asserts **both** the `read_calls` **and** `write_calls` deltas stayed under their regression thresholds (≤3,500 read / ≤2,400 write; ~2,856 read + ~1,367 write as-built, down from ~36,200 + ~7,800 pre-Phase-87 — **a ~11x total-I/O reduction**; the read figure rose ~750 in Phase 89 because `chmod`/`chown`/`utimensat` now route through the `vfs_server` for single-owner stat coherence — see `VFS_SETATTR` — and the installer restores a mode on every extracted file). Then `pkg verify mbedtls` re-reads every installed file and SHA-checks it against the recorded hash (`, 0 MISMATCH`) — an always-on **read-back-compare** that guards the batched/zero-fill-skip write path (`allocate_data_block(zero_fill=false)` + `write_block_run`) against a stale-block leak; it runs **after** the post-install snapshot so it does not inflate the measured deltas. Guards the read-side work: contiguous-run coalescing in **both** ext2 readers (`kernel_core::fs::ext2::read_file_data_coalesced`, shared by the kernel engine and the ring-3 `vfs_server`), a **write-through block cache** in `vfs_server` (`Ext2State`; sub-block metadata read-modify-writes — allocation bitmaps, inode-table + directory blocks — hit the cache instead of re-reading), and the 64 KiB `VFS_MAX_PREAD` read cap (decoupled from the 4 KiB request buffer; the bulk reply carries up to `MAX_BULK_LEN`=80 KiB). And the write-side / Track D fairness work: the 64 KiB `VFS_MAX_PWRITE` write cap, deferred sb/BGD metadata flush (`META_FLUSH_THRESHOLD`, drained on SIGTERM at clean shutdown), data-write coalescing + zero-fill skip, and multi-block allocation (`claim_block_run`, one bitmap RMW per contiguous run) — which **eliminated the >1 s WRITE requests** (the `write_calls` assertion is the deterministic guard: more writes per WRITE request ⇒ higher per-request latency). **Prerequisite:** `mbedtls` is a bundle-only port — build it first (`cargo xtask port build mbedtls`, or run `git-https-smoke` once which builds the mbedtls→curl→git chain); the gate fails fast with an actionable message if the artifact is absent rather than booting QEMU. Run on branches touching `kernel/src/fs/ext2.rs`, `kernel/src/blk/`, `vfs_server`'s ext2 read/write path, or the VFS read protocol

## ipv6-smoke

**Env var:** `M3OS_IPV6_REGRESSION=1`

Phase 91: the always-on dual-stack **IPv6** regression gate. Boots m3OS with QEMU SLIRP `ipv6=on` and runs the ramdisk-embedded `ipv6-smoke` ring-3 binary plus the kernel's live-NDP path, asserting (all **CI-deterministic, no real internet**): `[ipv6] link-local configured` — the `fe80::` address is formed from the NIC MAC at init (`IPV6_ADDR_OK`); `[ndp] neighbor advertisement sent` — the guest answers a **live Neighbor Solicitation** from SLIRP over the wire (`NDP_RESOLVE_OK`, the bidirectional NDP proof — packet-capture-confirmed SLIRP solicits the guest's link-local and the guest replies with an NA); and `SMOKE:ipv6-smoke:PASS` covering `IPV6_SMOKE:socket:ok` (AF_INET6 `SOCK_DGRAM`/`SOCK_STREAM` succeed, an unknown family errors — A.6), `:bind:ok` (`bind6` round-trips a 28-byte `sockaddr_in6` through `sockaddr_from_user6` — `IPV6_BIND_OK`), and `:loopback:ok` (a `ping6 ::1` ICMPv6 echo round-trips via the kernel's `::1` internal loopback through the real `handle_icmpv6` request→reply path — `IPV6_LOOPBACK_OK`/`ICMPV6_ECHO_OK`, m3OS having no routed `lo`), `:tcp:ok` (**full dual-stack TCP over IPv6** — an `AF_INET6` listen socket + `connect6(::1)` complete the three-way handshake through the internal loopback and a payload round-trips client→server, exercising the family-aware `TcpConnection`, the IPv6 pseudo-header checksum, and `handle_tcp_v6`), and `:recvmsg:ok` (`sys_recvmsg_inet6` — a v6 UDP datagram looped via `::1` is read with `recvmsg`, asserting `msg_name` is a 28-byte `sockaddr_in6` with `sin6_family==AF_INET6`). Fails on any `:FAIL`/`IPV6_SMOKE:panic`. **SLAAC global-address formation + the RA-driven default route + stateless/stateful DHCPv6 DNS are implemented + host-tested but live-validate only behind the opt-in `M3OS_IPV6_LIVE=1` arm**, which attaches the guest to a real LAN via `M3OS_IPV6_TAP=<ifname>` (a TAP bridged to a segment with a real IPv6 router) instead of SLIRP — because QEMU 8.2.2's libslirp does NDP NS/NA but sends **no Router Advertisements** and runs **no DHCPv6 server** (the guest's RS/Information-Request/DAD-NS all go out correctly-formatted but get no reply); this mirrors the established `*_NET` opt-in pattern. SLAAC was **demonstrated end-to-end against a real home router** this way (the guest formed a real `/64` global from the router's RA), and the run surfaced an **RFC 4861 RS retransmit** (up to 3) now landed; per-run acquisition is best-effort (router RA cadence + deferred MLD). `M3OS_IPV6_DHCPV6=1` additionally asserts the DHCPv6 lease (only when the router runs a DHCPv6 server). The `CURL6_OK` real-internet TCP arm is likewise opt-in (needs a routable global v6 address). IPv4 gates (`smoke-test`, `regression`, `dns-smoke`, `multi-nic-smoke`) are unaffected (the v6 path only adds a `0x86DD` dispatch arm). Set on branches touching `kernel/src/net/dispatch.rs`, the v6 `ipv6`/`icmpv6`/`ndp`/`dhcpv6` modules (kernel + `kernel-core`), `config.rs` v6 state, or the `AF_INET6` socket syscall surface

### Bundle: M3OS_DYNAMIC_C_REGRESSION=1

## dynamic-hello-smoke

**Env var:** `M3OS_DYNAMIC_C_REGRESSION=1`

Phase 93 — **CI-deterministic**, no network/hardware: builds the musl `libc.so` port + a dynamic hello fixture, boots, and asserts a genuinely dynamically-linked C binary (`PT_INTERP=/lib/ld-musl-x86_64.so.1` + `DT_NEEDED libc.so`) prints `DYNAMIC_HELLO:ok` (printf+malloc) and `DLOPEN:ok` (the loader's `dlopen`+`dlsym`+call mechanism `ctypes`/`lib-dynload` use); proves Track A `libc.so` + Track B loader (TLS/TCB, COPY-reloc-order, weak symbols, `/lib` search) + Track C `mremap` compose; **Phase 95b** added two multithreaded arms run after the hello fixture: `DYNAMIC_TLS:ok` (a `libfoo.so` `__thread` written general-dynamic + read initial-exec on N pthreads — the rust-lld worker-`threadIndex` cross-DSO TLS-at-offset-0 loader fix) and `THREAD_FAULT:ok` (the **thread-group fatal-fault** / `addr=0x8` kernel fix — a NULL-deref in one thread of a multithreaded process must kill the WHOLE group: a `leader-ok` arm faults the group-leader thread and a `worker-ok` arm faults a non-leader worker while siblings sit `BlockedOnFutex`, each reaped `WIFSIGNALED && SIGSEGV`; default `-smp 4` exercises the SMP shared-page-table-free race a regression turns into a kernel NULL+8 deref / sibling deadlock)

## dynamic-python-smoke

**Env var:** `M3OS_DYNAMIC_C_REGRESSION=1`

The heavy opt-in arm: builds the dynamic CPython + libffi, an `M3OS_WITH_DYNAMIC_PYTHON` image, `pkg install python-dynamic` (solver installs `musl`/libc.so first via `DEPS=musl`), then asserts a dynamic `python3` boots (`Python 3.12.8`), imports a `lib-dynload` `.so` via `dlopen` (`DYNPY:import-ok`), and `ctypes.CDLL('/usr/lib/libc.so')` opens + calls `strlen` (`CTYPES:ok`); runs at `--timeout 5400` for the ~30-min cold cross-build + slow-VFS install. Set on branches touching `userspace/ld-musl-x86_64.so.1`, `ports/lib/musl`|`libffi`, `ports/lang/python-dynamic`, xtask `build_musl`/`build_libffi`/`build_python_dynamic` or the `dynamic-*` gates, the kernel `mremap`/`arch_prctl`/startup paths a dynamic libc exercises, or the fatal-fault thread-group teardown (`fault_kill_trampoline`/`terminate_thread_group_and_exit`/`do_full_process_exit`) — the `dynamic-tls`/`thread-fault` fixtures require a musl cross-compiler, else the gate SKIPs

## acpi-smoke

**Env var:** `M3OS_ACPI_REGRESSION=1`

Boots QEMU q35 and asserts the full Phase 101 ACPI pipeline at runtime:
`acpid` fetches FACP/DSDT via `SYS_ACPI_TABLE_GET` and builds the AML
namespace from the live firmware bytecode; the E.3 `RegionSpace` boot
self-probes pass (a `SystemIO` read of the FADT's PM1a status port and a
`SystemMemory` read of the DSDT signature through the
`SYS_ACPI_{IO,MEM}_*` syscalls); the SCI routes and the ACPI-enable
handshake completes; then the gate logs in on the serial console, starts
the `acpi-sub-smoke` test subscriber (service-registry `Subscribe` — the
D.5/E.4 push path), fires a QMP `system_powerdown`, and asserts the
power-button event traversed kernel demux → acpid → the subscribed
client (`ACPI_SUB:event path=\FIXED.PWRBTN code=0x80`).

## pkg-net-smoke

**Env var:** `M3OS_PKG_NET_REGRESSION=1`

## suspend-smoke

**Env var:** `M3OS_SUSPEND_REGRESSION=1`

Phase 110 A.4 addition: the whole round trip now runs **with KPTI live**
(default `auto` lane), and the resume path is KPTI-load-bearing — suspend
**keeps** the AP per-core allocations (no free-at-park: every KPTI user half
maps each core's PerCoreData/GDT/TSS/IST-top pages at their live addresses,
so they must be address-stable for the life of every process),
`init_ap_per_core` reuses a populated slot in place and clears the reused
GDT's TSS busy bit (the pre-suspend `ltr` left type 0xB; the AP re-boot
`ltr`s before its IDT loads), and the entry-set builder maps every
*allocated* core so a process exec'd on the resumed BSP before the APs are
back online still carries their structures.

Boots QEMU (default machine, S3 enabled) and drives the **full ACPI S3
suspend-to-RAM round trip**: acpid registers `\_S3` SLP_TYP
(`SYS_ACPI_REGISTER_S3`); `m3ctl power suspend` → powerd `\_PTS(3)` →
`SYS_POWER_ENTER_SLEEP` → kernel quiesce (filesystem sync, cooperative
AP park at the scheduler-loop boundary, virtio-blk in-flight drain +
ring reset, PCI config snapshot) → FACS **X** waking vector at the
trampoline page's 32-bit shim → `PM1a_CNT <- SLP_TYP|SLP_EN` → QEMU run
state `suspended`. A QMP `system_wakeup` then re-enters through OVMF's
S3 path: shim → long mode on the kernel CR3 → minimal register-state
re-init (GDT with the TSS busy-bit cleared, IDT, GS bases, syscall
MSRs, TSC monotonic rebase) → long-jump back into the suspended
syscall → heavyweight re-init in task context (PIC/APIC, PCI config
restore, SCI reroute + PWRBTN re-arm, virtio-blk/net device
re-handshake against the retained rings, PS/2 keyboard+mouse re-init, AP reboot via INIT-SIPI-SIPI)
→ powerd drains the wake-side event burst (the wake sets `PWRBTN_STS`;
those events are artifacts, not poweroff requests) → `\_WAK(3)` →
`POWERD:resume` → `suspend: resumed`. Post-resume liveness: a fresh
shell echo, a disk write/read through the re-initialized virtio-blk,
and the power button still driving the D.3 poweroff chain to a
**guest-initiated QEMU exit**. `power-smoke` is the complementary
S3-disabled fail-closed lane.

## usb-root-smoke

**Env var:** `M3OS_USB_ROOT_REGRESSION=1`

The Phase 106 M1 milestone, live in CI: builds the combined single-disk
USB image (`cargo xtask image --combined` — one GPT disk carrying the
ESP FAT and the ext2 rootfs; the layout is also pinned host-side by a
unit test that mirrors the kernel's GPT probe byte-for-byte) and boots
QEMU with it attached **only** as a USB mass-storage stick behind
qemu-xhci — no virtio disk, no AHCI controller. OVMF boots the stick's
ESP; the kernel's virtio root mount fails; init's bootstrap walks the
AHCI stage (absent) then forks `/drivers/xhci` + `/drivers/usb-storage`;
`usb0.block` registers and `blk::remote` root slot 0 adopts it
last-resort (nvme → ahci → usb priority, `/drivers/` owner gate
enforced); the root mount's `gpt_ext2_scan` finds the ext2 partition
after the ESP; and the boot proceeds to a **writable** USB root serving
the on-disk `/etc/services.d`. The gate logs in over serial and proves
writability with a file write + read-back through the ring-3
usb-storage path.

## nvme-rw-smoke

**Env var:** `M3OS_NVME_REGRESSION=1`

The NVMe analog of `ahci-rw-smoke`. Boots with the **real** ext2 rootfs
routed behind a QEMU `nvme` controller (`DeviceSet { nvme_root: true }`
→ `-drive if=none,id=nvmeroot0` + `-device nvme,drive=nvmeroot0`), so
`/` is served by the ring-3 `nvme_driver` over `nvme.block` — init's
Stage-1 NVMe arm logs `init: / mounted (ext2 via ring-3 nvme.block)`.
After login, `ext2-coherence-smoke` writes a 200 KiB file and a fresh
process byte-verifies the read-back, exercising `blk::remote::
write_sectors` → `do_write_ipc` → the ring-3 `nvme_driver` `handle_write`.
Always-on in CI (default smoke only exercises in-kernel virtio-blk);
SKIP-with-reason without a musl cross-compiler (the C self-test is then
a 0-byte placeholder). Note: the driver's bring-up LBA-0 self-test is
now **non-destructive** (save + restore) so it no longer clobbers the
MBR of a real rootfs.

## nvme-persist-smoke

**Env var:** `M3OS_NVME_REGRESSION=1`

The NVMe analog of `ahci-persist-smoke`: a two-boot reboot-persistence
gate against the SAME nvme-routed ext2 disk. Boot 1 writes a marker to
`/` over `nvme.block` and idles past one periodic write-back flush
(draining deferred ext2 metadata and issuing `BLK_FLUSH`); QEMU is torn
down; boot 2 re-mounts ext2 fresh and re-reads the marker. Asserts boot
1 logged no `[blk] remote block flush failed`. Always-on (echo/cat only
— no musl). The per-request DMA-alloc kernel log was demoted INFO →
DEBUG so the NVMe I/O path no longer floods the serial console.

## nvme-install-smoke

**Env var:** `M3OS_NVME_INSTALL_REGRESSION=1`

The Phase 106 end-to-end installer proof (Track C / E.2): a two-boot
gate. Boot 1 boots the combined GPT(ESP+ext2) USB image (`image
--combined`) as a `usb-storage` stick with a **blank** NVMe target also
attached — when init's Stage-1 NVMe arm adopts the ext2-less NVMe, the
C.3 root-slot release + skip fires and the USB root mounts (the race is
timing-dependent, so the gate asserts login-reached, not the release
line). After a serial login, `/sbin/installer` runs the whole Track C
surface live: the `0x117x` capability-gated raw block syscalls
(installer exec-path trust), the GPT span parse (copy `0..=alt_lba`,
never a whole stick), the target resolve + size probe, and the sparse
`dd`-copy USB→NVMe + `BLK_FLUSH` + `reboot`. Boot 2 relaunches with
**only** the written NVMe and asserts `init: / mounted (ext2 via ring-3
nvme.block)` plus a live shell. The copy's read side rides the Phase
106 usb-storage **shm bounce path** (64 KiB `SubmitShmTransfer` data
stages — 2 SCSI commands per 256-sector request instead of 37 inline
sub-requests), which is what makes a ~1 GiB image copy finish inside
the gate window under TCG. Opt-in (two full QEMU boots + an image copy:
minutes per run).

The same env var also runs **`nvme-install-part-smoke`** — the Phase
106 C.4 partition-aware arm. Boot 1 runs `/sbin/installer --part`
instead: CRC-verified source-GPT parse (`kernel-core::fs::gpt`), the
Track D **first-user prompts** (root password / username / user
password on the console, echo off, answered by the gate — the created
account replaces the image's well-known seeded credentials, which the
filtered populate never copies), target capacity probe (LBA bisection
over the raw read syscall), a **fresh GPT sized to the target** (same
ESP span, Linux partition grown to the last usable LBA), a sparse
same-span ESP copy, an on-device `format_ext2` of the grown partition,
a file-level populate (`ext2_populate::populate_from_reader_filtered`
over a write-back cache + run coalescer), and fresh
`/etc/passwd`/`/etc/shadow`/`/etc/group` + `/home/<user>` written via
the existing passwd-lib `$sha256i$` chain. Between the boots the gate
verifies **host-side** what the installer wrote: the
`INSTALLER:gpt-written root_sectors=` sentinel must equal the target's
usable tail (cross-checked against the source image's own GPT via the
independent `gpt` crate), the installed disk's GPT must parse with that
crate and carry the grown span + an ESP, the populate sentinel must
report `skipped=0`, and — when the host has `e2fsck` — the extracted
rootfs partition must check clean before boot 2 ever mounts it. Boot 2
then logs in **as the installer-created user** and asserts the account
row in the installed `/etc/passwd` (uid 1000, `/home/<user>`,
`/bin/ion`).

## power-smoke

**Env var:** `M3OS_POWER_REGRESSION=1`

Boots QEMU q35 and asserts the Phase 103 pipeline on the platform QEMU
can model (the desktop/VM posture): `powerd` finds acpid, walks the
namespace for `PNP0C0A`/`ACPI0003` (absent on q35) and thermal zones
(`ACPI_LIST_TZ` — q35 declares none), probes the kernel cpufreq
mechanism (`SYS_POWER_CPUFREQ_STATUS` — no HWP under QEMU), and
announces `POWERD:ready battery=none ac=assumed-online zones=0
mech=none backlight=none sleep=s3+s4` (the F.1 sleep discovery runs
against the live q35 DSDT — `\_S3`/`\_S4` declared, no S0ix); the ring-3 conservative governor's first 1 s
tick reports `POWERD:governor mode=conservative target=` (proving the
recv-timeout wake, the CPU-times load sample, and the
`SYS_POWER_SET_PERF` no-op apply); after a serial login, `m3ctl power
status` renders the battery/thermal/governor/backlight posture over the
`power` IPC service and `m3ctl backlight 50` is rejected cleanly on the
panel-less VM (`backlight: no device` — the Track B set path);
`m3ctl power suspend` **fails closed** (`POWERD:suspend rejected
reason=resume-path-unimplemented` — no S3 resume path exists, and
entering S3 without one never wakes) with the session verified alive
afterward (an echo round-trip); then a
QMP `system_powerdown` power button traverses acpid → powerd's event
subscription (`POWERD:event path=\FIXED.PWRBTN code=0x80`) and — Track
D.3 — drives the full graceful-poweroff chain: `POWERD:poweroff` →
`/bin/shutdown` → SIGTERM to init (reverse-dependency service
teardown) → `sys_reboot(POWER_OFF)` → kernel sync → the ACPI S5 write
acpid registered at boot (`[acpi] S5 poweroff: PM1a_CNT`). The gate's
final assertion is a **guest-initiated QEMU exit** (bounded `try_wait`,
exit 0; no `child.kill()` on the success path). The populated
thermal-zone and backlight paths are covered host-side by
hand-assembled DSDT fixtures (`kernel-core/tests/acpi_thermal_zone.rs`,
`kernel-core/tests/acpi_backlight.rs` — the latter proving the
`_BCM(Arg0)`+`Store` set → `_BQC` read-back round trip); the live
battery/brightness/HWP-MSR/lid arms have no QEMU model and are
hardware-only (`Validated-on-HW` per the charter's Track G).
