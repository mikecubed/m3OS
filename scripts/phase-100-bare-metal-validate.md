# Phase 100 — Bare-Metal GUI Session: Bare-Metal Validation Runbook

> Started as the Track B (write-combining) runbook; §§0–3 cover the WC arm. §4
> is the combined Phase-100 sentinel index + the five-arm recorded-run checklist
> (Tracks B/C/D/E) referenced by the Track E.2 acceptance.

**Aligned Roadmap Phase:** Phase 100 Track B (B.1/B.2/B.3)
**Status:** Implemented (HW-unvalidated) — awaiting recorded run on Dell Precision 5560
**Reference machine:** Dell Precision 5560 / Tiger Lake (Intel Iris Xe Graphics)
**Protocol baseline:** `docs/appendix/bare-metal-validation.md`

## Why this exists

Track B's core premise is that mapping the compositor framebuffer as Write-Combining
(WC) instead of write-back (WB) materially reduces blit latency on real MMIO.  QEMU
models the framebuffer as host RAM, so WC and WB are indistinguishable in emulation —
the improvement is a hardware-only measurement.  This runbook documents the
reproducible method for capturing a full-screen-fill timing on the physical laptop and
recording the WC-vs-WB ratio as the evidence artifact that qualifies the phase as
`Validated-on-HW (run N, date)`.

## Phase context

Phase 96 reprogrammed `IA32_PAT` index 2 to WC (`PAT_WITH_WC = 0x0007_0406_0001_0406`)
and remapped the **kernel console** framebuffer with `NO_CACHE`.  Track B applies the
same WC slot to the **userspace compositor** framebuffer in `sys_framebuffer_mmap`
(adds `PageTableFlags::NO_CACHE` so the 4 KiB leaf selects PAT index 2: PCD=1, PWT=0,
PAT-bit=0).  The present path (`sys_framebuffer_pageflip`) issues an `sfence` before
signalling the flip so weakly-ordered WC stores are globally visible.

## 0. Prerequisites (QEMU-verifiable — run before bare-metal iteration)

Before travelling to the reference machine, confirm the WC attribute is correctly
encoded in QEMU:

```bash
cd <repo>
cargo xtask run 2>&1 | grep '\[fb-wc\]'
# Expected:
# [fb-wc] user FB leaf flags: PCD=1 PWT=0 PAT=0 (WC idx2)
```

A `PCD=1 PWT=0 PAT=0` sentinel proves the PTE encodes PAT index 2.  Any other
combination is a kernel bug; fix it before proceeding to bare-metal iteration.
`PAT=1` (HUGE_PAGE bit set) would decode index 6 (UC-); `PWT=1` would decode
index 3 (UC).

## 1. Blit-latency measurement method

The timing is captured as kernel log output from a userspace timing call embedded in
the compositor's full-screen-fill loop.  The compositor (`display_server`) writes a
timed blit into the WC-mapped framebuffer and prints the elapsed nanoseconds over the
serial/log sink.  Two runs are needed: one with the WC mapping (this phase) and one
with the write-back baseline (revert `NO_CACHE` from the PTE flags, rebuild, reboot).

### 1a. The timing sentinel (already emitted by `display_server`)

`display_server` already emits this sentinel — no instrumentation needs to be
added at HW-iteration time. It brackets its initial whole-screen `fill_background`
with `monotonic_nanos()` (the ring-3 `CLOCK_MONOTONIC` `sys_clock_gettime` path)
and prints, once per boot:

```text
[fb-blit] full-screen fill elapsed_ns=<N> pixels=<P>
```

The implementation is the initial-fill block in
`userspace/display_server/src/main.rs` (look for `monotonic_nanos()` bracketing
`fill_background`). The sentinel is printed to stdout/syslog, captured over the
`usb-logsink` boot.log and the network sink, and its **presence** is gate-checked
by `compositor-stress` so it cannot silently regress.

> **Note:** the absolute `elapsed_ns` is meaningful only on real MMIO. On QEMU the
> framebuffer is host RAM, so the WC and write-back numbers are indistinguishable
> — the line still prints (and is asserted present), but the WC-vs-WB *ratio*
> below is the Dell-only measurement.

### 1b. On the reference machine

Boot the physical Dell Precision 5560 from the USB image (both WC build and WB
baseline build):

```bash
# Build and write the image (WC build — this phase):
cargo xtask image
sudo dd if=target/disk.img of=/dev/sdX bs=4M status=progress && sync

# Boot the laptop from the USB key (UEFI boot menu: F12 on Dell).
# Capture logs:
#   Pre-network: amtterm <laptop-ip> | tee m3os-sol-wc.log
#   Post-network: sudo scripts/m3os-logsink.sh --port 514 --log m3os-wc-run.log

# After compositor starts, let it render one full-screen frame (e.g. greeter login).
# Find the sentinel in the captured log:
grep '\[fb-blit\]' m3os-wc-run.log
```

Repeat with the write-back baseline (revert `PageTableFlags::NO_CACHE` from the
`sys_framebuffer_mmap` flags, rebuild, reimage, reboot, capture to `m3os-wb-run.log`):

```bash
grep '\[fb-blit\]' m3os-wb-run.log
```

### 1c. Recording the result

The WC/WB ratio is:

```
ratio = elapsed_ns(WB) / elapsed_ns(WC)
```

On real MMIO (LPDDR5 panel + Tiger Lake iGPU MMIO path) the expected order of
magnitude is 10–50×.  Any ratio below 2× on real hardware is a signal that the WC
attribute is not being applied to the actual physical framebuffer address (e.g. the
bootloader-provided physical address is RAM-backed rather than device-MMIO).

## 2. Log capture paths

| Path | When | Command |
|---|---|---|
| AMT Serial-over-LAN | Pre-network / early-boot | `amtterm <laptop-ip> \| tee m3os-sol.log` |
| `usb-logsink` boot.log | Always (USB key present) | `cat /mnt/usb0/boot.log` after boot |
| Network syslog sink | Post-DHCP | `sudo scripts/m3os-logsink.sh --port 514 --log m3os-live.log` |

See `scripts/ure-vfio-validate.md §2` and `§3` for the full AMT SOL and network-sink
setup runbooks.

## 3. WC attribute QEMU-falsifiable assertions

These assertions are verifiable in QEMU and must pass before bare-metal iteration:

- **PTE leaf flags sentinel:** boot serial output contains
  `[fb-wc] user FB leaf flags: PCD=1 PWT=0 PAT=0 (WC idx2)` — confirms PAT
  index 2 is selected at mapping time.
- **No assertion failure / kernel panic** on `sys_framebuffer_mmap` or
  `sys_framebuffer_pageflip` — the `sfence` and VMA record are non-disruptive.
- **Existing GUI gates still pass** — `compositor-stress`, `less-render-probe`,
  `tiling-smoke` all pass, confirming no regression from the WC flag addition.

## 4. Full Phase-100 sentinel index + recorded-run checklist (all five arms)

### Sentinel index (grep these in the captured log: `usb-logsink` boot.log / AMT SOL / network sink)

| Track | Sentinel (greppable) | Proves | CI status |
|---|---|---|---|
| B | `[fb-wc] user FB leaf flags: PCD=1 PWT=0 PAT=0 (WC idx2)` | user FB mapped Write-Combining (PAT idx 2) | ✅ **gated** (`compositor-stress` waits; fails on any other PCD/PWT/PAT) |
| B | `[fb-blit] full-screen fill elapsed_ns=<N> pixels=<P>` (WC vs WB builds — see §1) | WC blit faster than write-back | ✅ **presence-gated** (`compositor-stress`); WC-vs-WB **ratio** ⏳ HW-only (QEMU RAM-FB≈WB) |
| E.1 | `RENDER_FP frame=<n> rows_nonblank=<R> rows_changed=<C> hash=0x<hex>` | the panel rendered content (`rows_nonblank>0`) vs background-only/black (`=0`); `≥200` @1080p signals the greeter dialog | ✅ **gated** (`compositor-stress` requires the line **and** `rows_nonblank>0` content); `≥200` greeter-dialog magnitude is the HW arm |
| C.1 | `USB_HID:pointer-injected count=<n>` | a real USB mouse's reports were decoded + injected (non-zero count) | ✅ **gated** (`usb-smoke` waits after `USB_HID:mouse`); dock-hub topology HW |
| C.2 | `INPUT:pointer-focus-change surface=<id>` | focus follows a button-down over a `Toplevel` (focus-on-click) | dispatch host-tested; real-click firing HW |
| D.2 | `USB_HID:idle ticks=<n> backoff_ns=<n>` | `usb-hid` reached the idle-backoff plateau (no busy-spin) | ⏳ HW/long-run idle measurement |
| D.3 | `USB_HUB:idle ticks=<n> backoff_ns=<n>` | the hub walker reached the idle-backoff plateau (change bits acked → backoff engages) | ✅ **gated** (`usb-hub-smoke` waits `USB_HUB:idle` on a populated hub); long-run flat-CPU ⏳ HW |

### Photo evidence convention (E.2)

For "the screen shows the greeter" — where an on-device sentinel cannot cover panel
colour/backlight — commit a **dated** photo of the panel under the phase evidence
directory (kept small), referenced by path from the task doc. Pair it with the
`RENDER_FP` sentinel line from the same boot's captured log so the photo is
corroborated by a falsifiable in-log fingerprint, not asserted from memory.

### Five-arm recorded-run checklist (all must clear together for `Validated-on-HW`)

Boot the Dell Precision 5560 from the USB image (`cargo xtask image` → `dd` → UEFI
boot). On the diskless USB boot, `init` takes the builtin-defaults path and (no
`/proc/m3os-boot-mode=serial` override) defaults to **graphical**, so the greeter
should come up. Capture pre-network logs over AMT SOL and post-network over the
network sink + `usb-logsink` boot.log, then confirm:

- [ ] **Greeter renders on the panel** — `RENDER_FP … rows_nonblank≥200` in the log **and** a dated panel photo.
- [ ] **USB mouse moves the cursor + focus follows** — non-zero `USB_HID:pointer-injected count=<n>`; cursor motion shows as small-`rows_changed` `RENDER_FP` lines; a click over a window emits `INPUT:pointer-focus-change surface=<id>`.
- [ ] **WC blit-latency win** — `[fb-blit] … elapsed_ns` on a WC build materially below the write-back baseline build (record the ratio in the Results table).
- [ ] **USB keyboard works in text mode** — boot with `/proc/m3os-boot-mode=serial` (or before the compositor claims the FB): typing on a USB keyboard echoes at the framebuffer login (`stdin_feeder` USB `KBD_EVENT_PULL` drain).
- [ ] **Idle-CPU is flat** — after input settles, `USB_HID:idle` / `USB_HUB:idle` plateau sentinels appear and a CPU-occupancy probe shows no core pinned hot.

## Results

> **Placeholder — to be filled in after the first recorded run on the Dell Precision 5560.**

| Run | Date | Build SHA | Machine | WC elapsed_ns | WB elapsed_ns | Ratio | Log artifact |
|---|---|---|---|---|---|---|---|
| 1 | (pending) | — | Dell Precision 5560 / Tiger Lake | — | — | — | — |

**Status after run 1:** `Validated-on-HW (run 1, YYYY-MM-DD)` — `Dell Precision 5560 / Tiger Lake`
