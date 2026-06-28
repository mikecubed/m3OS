# Phase 100 Track B — Write-Combining User Framebuffer: Bare-Metal Validation Runbook

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

### 1a. In `display_server` — emit the timing sentinel

In the compositor's page-fill / damage-blit path (the code path that writes
full-screen pixels before calling `sys_framebuffer_pageflip`), bracket the write loop
with timing calls:

```rust
// Pseudocode — adapt to the actual blit loop location
let t0 = crate::time::monotonic_ns(); // or sys_clock_gettime
for y in 0..height {
    let row = &mut fb[y * stride .. y * stride + width * 4];
    row.fill(color);                  // the full-screen fill
}
let elapsed = crate::time::monotonic_ns() - t0;
// Emit over log sink — visible in usb-logsink/AMT SOL/network sink:
log::info!("[fb-blit] full-screen fill elapsed_ns={} pixels={}", elapsed, width * height);
```

The sentinel line `[fb-blit] full-screen fill elapsed_ns=<N>` is captured over the
`usb-logsink` boot.log and the network sink.

> **Note:** the compositor is a ring-3 process; use the `sys_clock_gettime`
> syscall (`CLOCK_MONOTONIC_RAW`) for the timing, not any kernel-internal
> monotonic path.  The sentinel is printed to stdout/syslog, which `usb-logsink`
> captures.

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

## Results

> **Placeholder — to be filled in after the first recorded run on the Dell Precision 5560.**

| Run | Date | Build SHA | Machine | WC elapsed_ns | WB elapsed_ns | Ratio | Log artifact |
|---|---|---|---|---|---|---|---|
| 1 | (pending) | — | Dell Precision 5560 / Tiger Lake | — | — | — | — |

**Status after run 1:** `Validated-on-HW (run 1, YYYY-MM-DD)` — `Dell Precision 5560 / Tiger Lake`
