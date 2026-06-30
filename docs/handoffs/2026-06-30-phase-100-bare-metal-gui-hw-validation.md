# Handoff — Phase 100 Bare-Metal GUI Session: USB-HID bring-up on real hardware

**Date:** 2026-06-30
**Branch:** `feat/phase-100-bare-metal-gui-session` → PR **#272** (base `main`)
**HEAD at handoff:** `d094d87a` (local == origin; tree clean)
**Reference machine:** Dell Precision 5560 / Tiger Lake. **Two xHCI controllers.** USB mouse + 2 USB keyboards behind a **dock hub**; the built-in trackpad is **I2C-HID serviced via the kernel PS/2 path** (NOT usb-hid); the built-in keyboard is **PS/2** (kernel-serviced). Boots **diskless from a USB stick**; network comes up (DHCP) and `sshd` is the only off-box channel.
**Acceptance runbook:** `scripts/phase-100-bare-metal-validate.md` (§4 = the five-arm checklist).

This session chased one symptom — *the USB keyboard does not type in the GUI* — down through five layered root causes, fixing each with the bare-metal log as the oracle. The work is **not finished**: usb-hid now enumerates correctly, but the keyboard/mouse are **tier-2 devices behind the dock hub** and the **usbhub** fix that should surface them is built and pushed but **not yet HW-confirmed** — a capture is pending (§1).

---

## 1. RESUME HERE — open issue: tier-2 (behind-hub) keyboard/mouse not yet enumerated

**State of the chain (HW-confirmed up to here):** usb-hid spawns, waits out the busy server, and enumerates the **root-level** devices — but those are only the **dock hub** (`class=9`), the **RTL8156 USB-Ethernet** dongle (`vid=0x0bda pid=0x8156`), and a **mass-storage** device (`class=8`, the boot stick). The keyboard (HID `class=3`) and mouse are plugged **into** the dock hub = **tier-2**, and were never surfaced.

**Why (diagnosed):** surfacing tier-2 devices is **usbhub**'s job (it resets each downstream port and sends `EnumerateChild` to the xHCI server). But usbhub used a **blocking `ipc_call_buf` with no timeout**, so during the long busy-server window at boot (usb-hid had to retry its own enumeration **15×** before the single-threaded server answered) usbhub almost certainly **parked forever in `BlockedOnReply` on its first `NextAttach`** — the exact a90aa2ca wedge usb-hid was hardened against but usbhub never was — and never enumerated the hub at all.

**Fix shipped this session (commit `d094d87a`, awaiting HW confirmation):** gave usbhub the same hardening usb-hid got — bounded `ipc_call_buf_timeout` (3 s) + a retry-while-busy initial hub scan (bounded 15 s) — **plus full dmesg diagnostics** for the whole tier-2 path.

**The capture that decides the next step** (filter on the m3OS side — see §6):
```bash
echo 'dmesg | grep usbhub'  | ssh root@<ip> > usbhub.log
echo 'dmesg | grep usb-hid' | ssh root@<ip> > usbhid.log   # press keys first
```
Branch on `usbhub.log`:
- `bound hub` → `has N downstream ports` → `port X device connected` → `reset+enabled` → **`child enumerated … class=3`** → keyboard surfaced; `usbhid.log` should show `role=KEYBOARD` and keys should echo → **DONE** (also unblocks acceptance arm 4).
- …`reset+enabled` → **`child enumerate FAILED … (reply=…)`** → the **server-side `EnumerateChild`** (tier-2 Enable-Slot/Address-Device with route string) is the next bug. Chase it in `xhci/src/server.rs` (the `EnumerateChild` request handler) and `controller.rs` (`enumerate_port` / route-string addressing). The `(reply=timeout/transport | empty-attach | wrong-reply)` tag says which.
- `has N downstream ports` but **no `device connected`** → the hub isn't reporting the keyboard as connected (port power / connect-detect / it's on a *deeper* hub tier). Look at `enumerate_hub` port-status decode and whether the dock chains hubs.
- **No `bound hub`** at all (only `spawned`/`retrying`) → usbhub still can't get the hub from the server → server-side attach-table / `NextAttach` issue.

---

## 2. Fixed this session (commits, newest first — all on PR #272, each passed `cargo xtask check`)

| Commit | What | Why it mattered on HW |
|---|---|---|
| `d094d87a` | **usbhub: bounded RPC + retry-while-busy + dmesg diagnostics.** `usb_call`→`ipc_call_buf_timeout` (3 s); `enumerate_hubs_once` + retry loop; `klog` mirrors the whole hub/tier-2 lifecycle. | usbhub parked forever on a busy server → never enumerated the hub → tier-2 keyboard/mouse invisible. **Awaiting HW confirm (§1).** |
| `f86946a9` | **usb-hid: retry enumeration while the server is busy.** `usb_call_status` (Reply/TimedOut/Failed) + `enumerate_once` + bounded retry loop (15 s). Clean empty reply still exits fast (QEMU). | usb-hid exited "no HID devices" on the **first** timed-out `NextAttach`. **HW-confirmed fixed** (it now retries and enumerates root devices). |
| `3e7c0b8f` | **xhci: `poll_yield` must sleep ≥1 tick.** Was `nanosleep_for(0, 100_000)` (100 µs); the kernel busy-spins any **sub-millisecond** sleep (no deschedule), so `poll_yield(POLL_ITERS_1S)` pinned a core for a full second. Now 1 ms/iter, iter counts cut to keep the same wall-clock budgets. | Two controllers × ~1 s bring-up busy-spin = the `cpu-hog …/drivers/xhci …Running` storm that starved the HID driver. **Reduced but a residual ~2 s cpu-hog remains** — see §4.1. |
| `1f29d53a` | **scripts/phase-100-write-usb.sh: don't reject a valid disk when `[[ -b ]]` is flaky.** Make lsblk authoritative for device-type; fall back to lsblk RM/HOTPLUG for the removable check. | `sudo bash …write-usb.sh /dev/sda` aborted "not a block device" even though lsblk listed it and `dd` worked (mount-namespaced sudo / udev timing). |
| `7da75776` | **usb-hid + usbhub: mirror lifecycle into dmesg** via `serial_print`/`klog` (→ `sys_debug_print` → `[userspace] …` → dmesg ring). | **The unblock.** Driver fd-1 output is NOT in `/proc/kmsg`; without this, the bare-metal failure was invisible over SSH (the original handoff's "grep dmesg for the role line" plan never worked). |
| `f61bd3af` | **xhci: ack ALL PORTSC RW1C change bits** in `on_port_status_change` (was CSC only) via `PORTSC_RW1C_MASK`. | Standard xHCI discipline; clears PLC/PEC/CEC a USB-3 dock raises. Correct, but was **not** the storm root (the busy-spin was — `3e7c0b8f`). |
| `149b7210` | **usb-hid: classify non-boot keyboards.** A Report-Protocol keyboard (subclass 0) fell through `classify_role` to `Ignore`. Now proto==1 *or* HID Usage-Page-0x07 fields → `BootKeyboard` + `SET_PROTOCOL(0)`. Mouse path stays Boot-subclass-gated. | Necessary for any non-boot USB keyboard to inject keys. Not yet exercised (no keyboard has reached classification on HW yet — blocked by §1). |

---

## 3. Hard-won facts that corrected the original handoff's assumptions

1. **"The mouse works" did NOT prove usb-hid's USB path.** The working pointer is the **I2C trackpad via the kernel PS/2 path**, not usb-hid. There is **no I2C-HID driver** in the tree (only `kernel/src/arch/x86_64/ps2.rs`). So usb-hid's USB poll path was never validated by the trackpad — and in fact usb-hid was *dying*, not polling.
2. **Driver stdout (fd 1) is invisible in `dmesg`.** `/proc/kmsg` = the kernel ring, fed only by `log::`/`serial_println!` (`_kernel_print`) — and by **`sys_debug_print`** (syscall #12, `syscall_lib::serial_print`), which logs `[userspace] <msg>`. Ring-3 `write_str(STDOUT, …)` goes to the console/serial, **not** the ring. **To make a driver observable on bare metal, route key lines through `serial_print`/`klog`.**
3. **A sub-millisecond `nanosleep` is a TSC busy-spin, not a yield** (`sys_nanosleep`, `kernel/src/arch/x86_64/syscall/mod.rs` ~L4290/4342). Anything that "sleeps 100 µs to be cooperative" actually **pins the core**. Sleep **≥ 1 ms** (one tick) to deschedule. This footgun caused the xHCI cpu-hog.
4. **The `cpu-hog` warning is purely diagnostic** (logs any task holding a core ≥ 200 ms; `scheduler.rs` ~L5838). It does **not** kill. `final_state=Dead`/`Running` is the task's state at switch-out; `ran~Nms` is **continuous** CPU this dispatch (`start_tick` resets every dispatch).

---

## 4. Known residual issues (not yet fixed)

1. **Residual ~2 s xHCI `cpu-hog`** (`final_state=Running`) persists after the `poll_yield` fix, so there is a *second* busy-spin. Prime suspect: the command-completion wait `COMPLETION_SPIN_POLLS = 4000` busy-spin phase (`controller.rs` ~L846/873, also `wait_for_transfer_event`/`wait_for_bulk_out_event` at ~L1166/2204/2288) — each iteration drains the event ring before the loop switches to 1 ms sleeps; under a flooded ring the 4000-spin phase alone can run ~2 s. If §1's fix doesn't fully settle the server, make these spin phases yield sooner (smaller `COMPLETION_SPIN_POLLS`, or switch to the 1 ms-sleep arm earlier). Affects acceptance arm 5 (idle CPU).
2. **`f61bd3af` (PORTSC ack-all) is plausibly unnecessary** now that the busy-spin is understood — keep it (it's correct xHCI discipline) but don't assume it fixed anything.
3. **Greeter bg image / `ion` RO-fs warnings** — cosmetic, from the earlier session (original handoff §4.2/§4.3); unchanged.

---

## 5. Key code locations touched / relevant

- **usb-hid** `userspace/drivers/usb-hid/src/main.rs`: `classify_role` + `fields_have_keyboard` (~L140/200); `CallStatus`/`usb_call_status`/`usb_call` (~L294); `enumerate_once` + retry loop in `program_main` (~L1110/1170); `INITIAL_ENUM_BUDGET_MS`/`ENUM_RETRY_SLEEP_NS` (~L286); `klog` + bound-role/key/exit mirrors throughout.
- **usbhub** `userspace/drivers/usbhub/src/main.rs`: `klog`/`monotonic_*`/`CallStatus`/`usb_call_status` (~L158); `enumerate_hubs_once` + retry loop in `program_main` (~L440/456); tier-2 `EnumerateChild` + diagnostics in `enumerate_hub` (~L360-420); steady-state `hub_ports_have_change` mask candidate (§unfixed, ~L244).
- **xhci** `userspace/drivers/xhci/src/controller.rs`: `poll_yield` + `POLL_ITERS_*` (~L154/161); `on_port_status_change` PORTSC ack-all (~L2463); command-wait spin phases `COMPLETION_SPIN_POLLS` (~L98/846); `EnumerateChild` path begins server-side (`xhci/src/server.rs` `handle_request`).
- **kernel** `sys_nanosleep` sub-ms busy-spin (`arch/x86_64/syscall/mod.rs` ~L4290/4342); `sys_debug_print` (~L2836); dmesg ring (`serial.rs` `_kernel_print`/`dmesg_snapshot`).
- **PORTSC bits**: `kernel-core/src/usb/xhci/port.rs` (`PORTSC_RW1C_MASK`, `portsc_clear_change`).

---

## 6. Workflow & environment facts (critical — these wasted time this session)

- **Build the bootable image:** `cargo xtask image` → `target/x86_64-unknown-none/release/boot-uefi-m3os.img`. The user flashes from **their** host (`git pull && cargo xtask image` there), not this one.
- **Write USB:** `scripts/phase-100-write-usb.sh /dev/sdX` (now lsblk-authoritative). On the reference machine the USB is `/dev/sda` (NVMe system disk is `nvme0n1`). Direct fallback: `sudo dd if=…/boot-uefi-m3os.img of=/dev/sda bs=4M conv=fsync status=progress && sync`.
- **Capture logs — FILTER ON THE M3OS SIDE.** A full `echo dmesg | ssh … > log` **truncates** (the SSH PTY / streaming `/proc/kmsg` gets cut mid-boot — we lost two captures to this). m3OS `grep` is **fixed-string, single-pattern** (`coreutils-rs/src/grep.rs`): `echo 'dmesg | grep usbhub' | ssh root@<ip> > x.log`. Each `dmesg` re-reads the full ring (per-fd cursor from oldest). The password prompts on your tty (ssh reads it from /dev/tty).
- **`[userspace]` is the new-image gate.** If `grep '\[userspace\]'` is empty, the booted USB is an OLD image — re-flash. (We burned two cycles on stale images.)
- **QEMU cannot reproduce any of this** — no real HID/hub/multi-controller. usb-hid prints "no HID devices … exiting" and the device-less paths are deliberately fast (the retry loops only trigger on a *timeout*, not a clean empty reply, so QEMU gates are unaffected). All USB-device bugs need the Dell.
- **sshd serves an interactive shell only** (rejects exec/scp) — hence the `echo 'cmd' | ssh` idiom.

---

## 7. Suggested resume order
1. Get §1's `usbhub.log` + `usbhid.log` → branch on the tier-2 outcome. Most likely next fix is **server-side `EnumerateChild`** if usbhub reaches `reset+enabled` but `child enumerate FAILED`.
2. Once the keyboard types: confirm the `149b7210` classifier actually fires (`role=KEYBOARD`) and arm 4 (USB keyboard in text mode) passes.
3. Kill the residual ~2 s xHCI cpu-hog (§4.1) → re-check arm 5 (idle CPU flat) via `/proc/loadavg` + absence of `cpu-hog`.
4. Arms 1 (greeter photo artifact) and 3 (WC blit-latency ratio) from the original runbook remain.

> The original handoff's §1 (USB keyboard classification) is superseded: classification was *a* bug (`149b7210`) but the dominant blockers were the busy-spin storm and the usbhub wedge. `.agent/SESSION.md` is stale — ignore it; this doc supersedes it.
