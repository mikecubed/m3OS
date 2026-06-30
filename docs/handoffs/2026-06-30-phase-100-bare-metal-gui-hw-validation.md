# Handoff — Phase 100 Bare-Metal GUI Session: hardware validation & bring-up fixes

**Date:** 2026-06-30
**Branch:** `feat/phase-100-bare-metal-gui-session` → PR **#272** (base `main`)
**HEAD at handoff:** `a90aa2ca` (local == origin; tree clean)
**Reference machine:** Dell Precision 5560 / Tiger Lake. USB mouse + 2 USB keyboards behind a **dock hub**; built-in keyboard is **PS/2** (kernel-serviced, independent of usb-hid).
**Acceptance runbook:** `scripts/phase-100-bare-metal-validate.md` (§4 = the five-arm checklist).

This session took Phase 100 from "boots to a greeter" to a **usable bare-metal GUI session** by fixing a string of diskless-only bugs found on real hardware, and resolving all PR #272 review threads. One functional bug remains open (USB keyboard input in the GUI) with the diagnosis done and a single data point pending.

---

## 1. RESUME HERE — open issue: USB keyboard does not type in the GUI

**Symptom:** In GUI mode, the **built-in PS/2 keyboard types into the terminal, but USB keyboards do not** (both of two USB keyboards; their LED lights, but no keys echo). Mouse works (cursor + click-focus).

**Diagnosis (done):** Not display_server, not term, not kbd_server — the PS/2 keyboard proves the whole downstream chain works. The break is **usb-hid is not injecting the USB keyboard's keystrokes.** Root in the code:
`userspace/drivers/usb-hid/src/main.rs` → `classify_role` (~L140-165) classifies an interface as a keyboard **only if it declares the Boot subclass** (`interface_sub_class == SUBCLASS_HID_BOOT`). A **Report-protocol keyboard (subclass 0)** with plain keyboard usages and no pointer/consumer fields falls through to **`DeviceRole::Ignore`** — and **there is no `ReportKeyboard` role / decoder at all**, so its keys are never decoded or injected. (The LED is likely BIOS-retained NumLock, not m3OS.)

**The one data point needed to pick the fix** — usb-hid logs the bound role (main.rs:1051-1064). On the Dell, with the USB keyboard attached:
```bash
echo dmesg | ssh root@<m3os-ip> > k.log     # see §6 for why dmesg, not cat /proc/kmsg
grep -a 'usb-hid: bound' k.log              # → "usb-hid: bound vid=.. pid=.. class=3 proto=.. role=.."
grep -a 'USB_HID:key' k.log                 # should be ABSENT (confirms no key inject)
```
Branch on the keyboard's `role=`:
- **`role=IGNORE` / `role=CONSUMER` / no `proto=1` device** (most likely, per the code) → **classification/decoder bug.** Fix in usb-hid: add keyboard support for the non-boot path. Two viable shapes:
  1. In `classify_role`, treat `interface_protocol == PROTOCOL_HID_KEYBOARD` as a keyboard regardless of subclass (and/or detect HID Usage Page 0x07 *Keyboard* in the parsed `ReportField`s), then **force boot protocol** via `boot_protocol_init` (`SET_PROTOCOL(0)`) so the device emits the standard 8-byte boot report that `BootKeyboardDecoder` already handles. Lowest-effort; relies on the keyboard supporting boot protocol (almost all do).
  2. Add a real `ReportKeyboard` role + a report-descriptor keyboard decoder (Usage Page 0x07). More work; needed only if a keyboard rejects `SET_PROTOCOL(0)`.
  ⚠️ The current `classify_role` comment (L141-147) deliberately refused to drive non-boot interfaces as boot devices (Phase 92, to avoid mis-driving tablets/media strips). Keep that intent for **protocol 0/2**; only relax for **protocol 1 (Keyboard)** / explicit keyboard usages.
- **`role=KEYBOARD`** → it *is* bound as a boot keyboard; the break is the **interrupt-IN poll/decode** (key reports not arriving — suspect the dock-hub endpoint isn't armed/read by the xHCI server for that device). Different fix; chase `poll_keyboard` → `poll_report` (PollInterruptIn) → the xHCI server's interrupt-IN arming for hub-attached devices.

This also unblocks **acceptance arm 4** (USB keyboard in text mode) — same usb-hid injection path.

---

## 2. Fixed this session (commits, newest first)

All on the PR #272 branch; each passed `cargo xtask check` (pre-commit hook) + pre-push gates.

| Commit | What | Why |
|---|---|---|
| `a90aa2ca` | **Bound usb-hid's xHCI RPC** (the "pid 7 wedge" fix). New kernel syscall `ipc_call_buf_timeout` (0x111D, dispatch opcode 30) = bulk variant of `ipc_call_timeout`; usb-hid's `usb_call` uses it with a 1 s budget and returns `None` on timeout (retries instead of parking). Watchdog `StuckNoWaker` warning rate-limited to ≤1/10 s. | usb-hid parked forever in `BlockedOnReply` (no waker) when the single-threaded xHCI server was monopolized by the dock-hub re-enumeration storm → watchdog flooded the log, mouse froze. **HW-confirmed fixed.** |
| `982e3b18` | **Embed the 2.1 MB Nerd Font in the ramdisk** at `/usr/share/fonts/m3os/term.ttf` (new `/usr/share/fonts/m3os/` tree in `kernel/src/fs/ramdisk.rs`). | Diskless had no font asset → term used the static 8×16 bitmap. **HW-confirmed: terminal font now correct.** |
| `f616dc54` | term: `blit_glyph_view` scales a sub-cell glyph to fill the 24×48 cell (gap-free fallback); trim the vfs wait 5 s→2 s. | Belt-and-suspenders for the font gaps; faster terminal. |
| `48565f18` | **Round 2 desktop fixes:** (a) terminal renders — bounded term's `wait_for_shell_dependencies` vfs wait so a diskless boot (no vfs_server) proceeds via the kernel fs fallback instead of blocking forever; (b) launcher → **centered Overlay layer** (was a tiled Toplevel); (c) bar waits for login on diskless — init writes `/run/m3os-graphical-only`, bar/clients gate on it. | Terminal was a blank dwindle tile; launcher tiled instead of modal; bar appeared before login. **HW-confirmed fixed.** |
| `d62fdad5` | **Round 1 desktop fixes:** add `bar`/`wallpaper`/`notifyd` to init `BUILTIN_CONFIGS` (diskless had no bar/wallpaper); clamp term's initial surface to the real framebuffer (was hardcoded 1920×1200); scale the launcher to the panel. | No bar/wallpaper on diskless; term oversized; launcher tiny. |
| `47a6544e` | `scripts/phase-100-write-usb.sh` (safety-checked USB writer) + fix the runbook's `dd` source path. | The boot image is `boot-uefi-m3os.img`, not the data `disk.img`. |
| `f80fc49c`, `679ec8b3` | Resolved all 7 PR #272 Copilot review threads (5 + 2). | — |

(`4053bb95` between them is another session's xtask rust-port fix, pulled in via rebase.)

**PR #272 review state:** all 7 inline threads replied + resolved (0 unresolved).

---

## 3. Phase 100 acceptance arms (runbook §4) — status

- [x] **Arm 2 — mouse moves cursor + focus-follows-click** — HW-confirmed (cursor moves; clicking changes focus).
- [~] **Arm 1 — greeter renders** — functionally confirmed (login works); still need the *artifact*: a dated panel photo + the matching `RENDER_FP … rows_nonblank≥200` log line, committed under a phase evidence dir.
- [ ] **Arm 3 — WC blit-latency win** — NOT done. Two-build measurement: note `[fb-blit] elapsed_ns` on the WC build (current), then build a write-back baseline (revert `PageTableFlags::NO_CACHE` in `sys_framebuffer_mmap`), reflash, note its `elapsed_ns`, record the ratio (expect 10–50×) in the runbook Results table. *(Offered to add a `cargo xtask image --wb-baseline` flag to avoid hand-editing — not yet built.)*
- [ ] **Arm 4 — USB keyboard in text mode** — BLOCKED by the §1 usb-hid keyboard bug (same injection path). Fix §1 first.
- [ ] **Arm 5 — idle-CPU flat** — NOT validated. Check `cat /proc/loadavg` after the desktop idles, and grep a full `dmesg` for `USB_HID:idle` / `USB_HUB:idle` plateau sentinels and absence of `cpu-hog`. ⚠️ Likely to still show a hot core / storm — see §4.1 (we bounded usb-hid but did **not** stop the usbhub storm).

---

## 4. Known residual issues (not yet fixed)

1. **usbhub dock-hub re-enumeration storm (ROOT of the wedge).** The xHCI server is single-threaded and shared; `usbhub` re-enumerates on a dock-hub port-change bit that apparently never clears, monopolizing the server (`xhci/src/server.rs:483-493` runs `process_port_events` before answering clients; `usbhub/src/main.rs:506-548`). `a90aa2ca` made usb-hid *survive* this; it did not stop it. **Now diagnosable** (watchdog no longer floods). Targeted fix would be in `usbhub` (don't re-enumerate on an un-clearable change bit; verify `clear_port_change_bits` quiesces the dock port). Affects arm 5.
2. **Greeter background image missing on diskless** — cosmetic; the greeter's bg PNG is a data-disk asset not in the ramdisk. Could embed it like the font if desired.
3. **`ion: could not create config/history file: Read-only file system`** — the diskless ramdisk root is read-only; harmless shell warnings.

---

## 5. Key code locations touched / relevant

- usb-hid keyboard classification: `userspace/drivers/usb-hid/src/main.rs` — `classify_role` (~L140), `DeviceRole` (~L118), `build_device`/bind log (~L1034-1064), `poll_keyboard` (~L376), `inject_key` (~L318), `usb_call` bounded RPC (~L225-260), `boot_protocol_init` (~L242).
- New IPC syscall: `kernel/src/ipc/mod.rs` — `ipc_call_buf_timeout` fn + dispatch opcode 30; `kernel/src/arch/x86_64/syscall/mod.rs` — `IPC_LAST = 0x111D`; `userspace/syscall-lib/src/lib.rs` — `SYS_IPC_CALL_BUF_TIMEOUT` + `ipc_call_buf_timeout()` wrapper.
- Watchdog rate-limit: `kernel/src/task/scheduler.rs` — `LAST_STUCK_WARN_TICK` / `STUCK_WARN_INTERVAL_TICKS` near the `StuckNoWaker` arm (~L6724).
- Input routing (verified correct): dispatcher `kernel-core/src/input/dispatch.rs` `route_key_down` (exclusive-layer checked before focus); per-client delivery `userspace/display_server/src/main.rs` (`LABEL_CLIENT_EVENT_PULL`, ~L599-642, Outbound queue ~L1397-1426); term key reception `userspace/term/src/main.rs` (~L385-393, `pull_one_event` ~L790-833).
- Diskless service set: `userspace/init/src/main.rs` `BUILTIN_CONFIGS` (~L1395) + graphical-only filter; `/run/m3os-graphical-only` marker writer.
- Font embed: `kernel/src/fs/ramdisk.rs` `TERM_TTF` + `USR_ENTRIES`/`SHARE_ENTRIES`.

---

## 6. Workflow & environment facts (important for a fresh session)

- **Build the bootable image:** `cargo xtask image` → `target/x86_64-unknown-none/release/boot-uefi-m3os.img` (this is the artifact to flash; the separate `disk.img` is the data disk, NOT used by the diskless USB boot).
- **Write USB:** `scripts/phase-100-write-usb.sh /dev/sdX` (validates whole-disk/removable/not-root, asks to confirm).
- **Validation gate:** `cargo xtask check` (clippy -D warnings + rustfmt + host tests). Pre-commit runs it; pre-push adds smoke + regression.
- **Reproducing diskless bugs in QEMU** — these bugs are diskless-only; `cargo xtask run` always regenerates `disk.img` and boots **serial** mode, so it can't reproduce the diskless **graphical** path. Boot the raw image with **no data disk**:
  ```bash
  qemu-system-x86_64 -bios /usr/share/ovmf/OVMF.fd \
    -drive format=raw,file=target/x86_64-unknown-none/release/boot-uefi-m3os.img \
    -serial file:/tmp/diskless.log -m 2048 -smp 4 \
    -cpu qemu64,+xsave,+avx,+xsaveopt,+smep,+smap,+aes \
    -display none -vga std -no-reboot
  ```
  ⚠️ **QEMU has no real HID devices** — usb-hid prints "no HID devices attached — exiting cleanly" and exits. So QEMU reproduces the boot path / bar timing / term rendering, but **NOT** the USB-device bugs (the wedge, the keyboard). Those need the Dell.
- **Getting logs off the Dell — SSH only.** m3OS sshd serves an **interactive shell only** (it rejects `exec` and sftp/scp). So:
  - `echo dmesg | ssh root@<ip> > log` — runs `dmesg` on m3OS, output to a host file. The password prompts on your terminal (ssh reads it from /dev/tty, not stdin).
  - Use the **`dmesg` command, not `cat /proc/kmsg`** — `cat` returns only the first ~4 KB of the ring (one read chunk → oldest-only); `dmesg` loops to EOF.
  - Filter with `grep -a` (the SSH PTY injects control bytes).
  - The USB `usb-logsink` boot.log does **not** work (the flashed USB has no ext2 partition to write to).
- **HW topology reminder:** built-in keyboard = PS/2 (kernel path → kbd_server, works in GUI). USB mouse/keyboard = behind the dock hub → usb-hid. Network comes up (DHCP); `sshd` runs → that's the log channel.

---

## 7. Notes
- `.agent/SESSION.md` is stale (left from a `/flow:pr-resolve` run early in the session) — ignore it; this doc supersedes it.
- Suggested resume order: (1) get the `usb-hid: bound … role=` line from the Dell → fix the USB keyboard (§1), which also unblocks arm 4; (2) capture idle `dmesg` + `/proc/loadavg` → decide whether to fix the usbhub storm (§4.1) for arm 5; (3) arm 3 WC ratio (two builds); (4) arm 1 photo artifact → flip runbook status toward `Validated-on-HW`.
