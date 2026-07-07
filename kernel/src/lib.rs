//! m3OS kernel library crate.
//!
//! Phase 61 Track 0a split the kernel into a library (this crate) and a
//! thin binary (`kernel/src/main.rs`). The binary now owns only the
//! `entry_point!` macro and the `#[panic_handler]` / `#[alloc_error_handler]`
//! attributes (which must live in the binary that links them); everything
//! else — modules, boot sequence, helper tasks — lives here so that
//! integration tests under `kernel/tests/*.rs` can `use kernel::...` to
//! reach scheduler / SMP / pipe internals.

#![no_std]
// Phase 110 A.3b — the `abi_x86_interrupt` feature was dropped once every
// interrupt/exception vector became a naked-asm entry stub (KPTI needs to own
// the `iretq` for the CR3 exit switch), so no `extern "x86-interrupt"` fn
// remains in the kernel.
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::testing::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]
// Phase 61 Track 0a — kernel internals are exposed as a `pub mod` set so
// integration tests under `kernel/tests/*.rs` can reach them. Items that
// were previously module-private are now technically part of the kernel
// crate's public surface, which surfaces a number of clippy lints that are
// not meaningful for an internal API. Suppress them at the crate level
// rather than rewriting every module's API for a pure visibility change.
#![allow(
    clippy::missing_safety_doc,
    clippy::result_unit_err,
    clippy::new_without_default,
    clippy::len_without_is_empty,
    clippy::not_unsafe_ptr_arg_deref
)]

extern crate alloc;

pub mod acpi;
pub mod arch;
pub mod blk;
/// Phase 111 Track C — in-kernel GDB stub (kgdb). Feature-gated: the stub is
/// arbitrary kernel peek/poke, OFF in production.
#[cfg(feature = "kgdb")]
pub mod debug;
pub mod epoll;
pub mod eventfd;
pub mod fb;
pub mod flock;
pub mod fs;
pub mod fwcfg;
pub mod iommu;
pub mod ipc;
pub mod mitigations;
pub mod mm;
pub mod net;
pub mod panic_diag;
pub mod pci;
pub mod pipe;
pub mod process;
pub mod pty;
pub mod rtc;
pub mod serial;
pub mod signal;
pub mod smp;
pub mod stdin;
pub mod syscall;
pub mod task;
pub mod test_prelude;
#[cfg(test)]
pub mod testing;
pub mod time;
pub mod timerfd;
pub mod trace;
pub mod tty;

use alloc::{boxed::Box, string::String, vec, vec::Vec};
use bootloader_api::BootInfo;

/// Framebuffer geometry captured at boot entry so `post_marker` can paint from
/// anywhere (including inside `mm::init`) without threading the pointer through.
#[derive(Clone, Copy)]
struct PostFb {
    base: usize,
    width: usize,
    height: usize,
    stride_bytes: usize,
    bpp: usize,
}

static POST_FB: spin::Once<PostFb> = spin::Once::new();

/// Master switch for the bare-metal bring-up diagnostics: the `post_marker`
/// POST squares, the `[timer] lapic_ticks_per_ms` framebuffer line, and init's
/// AHCI-retry dots. **Default OFF.** These debugged the Phase 96 Tiger Lake
/// early-boot hang and are invisible on a normal boot (the fb console overwrites
/// the strip immediately), but they stay compiled out unless a future bare-metal
/// bring-up needs them again — flip to `true` and rebuild. Same default-off-const
/// idiom as `net::dhcp::FB_NET_HEARTBEAT` / the xhci driver's `VERBOSE_ENUM`.
pub(crate) const BRINGUP_DIAG: bool = false;

/// Record the framebuffer for `post_marker`. Called once at boot entry.
fn post_fb_set(ptr: *mut u8, info: &bootloader_api::info::FrameBufferInfo) {
    let bpp = info.bytes_per_pixel.max(1);
    POST_FB.call_once(|| PostFb {
        base: ptr as usize,
        width: info.width,
        height: info.height,
        stride_bytes: info.stride * bpp,
        bpp,
    });
}

/// Bring-up diagnostic — paint a small solid square at grid slot `step` as a
/// serial-free "POST code". Slots tile left-to-right, 16 per row (`step / 16`
/// chooses the row): row 0 (slots 0–15) holds top-level boot steps, row 1
/// (slots 16+) a subsystem's internal steps.
///
/// Early boot logs go only to the COM1 UART, invisible on a machine without a
/// serial port or AMT capture (the Phase 96 Tiger Lake laptop). These squares
/// make a bare-metal early-boot hang *visible*: the **last square shown is the
/// last step that completed** — the hang is in the step after it. Harmless on
/// success (the fb console + compositor overwrite the strip immediately).
/// Format-agnostic (same byte to every channel, shows on RGB or BGR);
/// `write_volatile` keeps it from being elided.
#[inline(never)]
pub(crate) fn post_marker(step: usize) {
    // Gated off by default via BRINGUP_DIAG, folded into the framebuffer-presence
    // guard so the body stays a single conditional early-return (a positive guard
    // also sidesteps the const-false `unreachable_code` lint).
    let Some(fb) = POST_FB.get().filter(|_| BRINGUP_DIAG) else {
        return;
    };
    const SQ: usize = 28;
    const GAP: usize = 8;
    const PER_ROW: usize = 16;
    let col = step % PER_ROW;
    let row = step / PER_ROW;
    let x0 = GAP + col * (SQ + GAP);
    let y0 = GAP + row * (SQ + GAP);
    if x0 + SQ > fb.width || y0 + SQ > fb.height {
        return;
    }
    // Distinct brightness per column (0x48, 0x70, 0x98, …) so neighbours differ.
    let byte: u8 = 0x48u8.wrapping_add((col as u8).wrapping_mul(0x28));
    let base = fb.base as *mut u8;
    for y in y0..y0 + SQ {
        // SAFETY: the bootloader mapped + rendered to this framebuffer before
        // jumping to the kernel, and mm::init preserves that mapping; the square
        // is bounds-checked against width/height above.
        let line = unsafe { base.add(y * fb.stride_bytes) };
        for x in x0..x0 + SQ {
            let px = unsafe { line.add(x * fb.bpp) };
            for b in 0..fb.bpp {
                unsafe { px.add(b).write_volatile(byte) };
            }
        }
    }
}

/// Top-level kernel boot entry. Called from the binary `main.rs` which owns
/// the `entry_point!` macro. Performs all pre-task init (serial, GDT/IDT,
/// frame allocator, heap, framebuffer, ACPI, IOMMU, RTC, SMP, scheduler,
/// IPC, networking, userspace init) and then enters the BSP idle loop.
pub fn kernel_main_entry(boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    serial::init_logger();

    serial_println!("[m3os] Hello from kernel! v{}", env!("CARGO_PKG_VERSION"));
    log::info!("Kernel initialized");

    // P9-T001: parse framebuffer info before mm::init consumes boot_info.
    // `mm::init` takes `&'static mut BootInfo` which borrows the whole struct
    // for 'static, so we must extract the raw pointer + layout first.  Hoisted
    // above arch::init so the bring-up POST markers can paint from the very
    // first init step (see `post_marker`).
    let fb_parts: Option<(*mut u8, bootloader_api::info::FrameBufferInfo)> =
        boot_info.framebuffer.as_mut().map(|fb| {
            let info = fb.info();
            // SAFETY: boot_info is &'static mut so the framebuffer memory is
            // valid for the kernel lifetime.  We extract a raw pointer here
            // and hand it to fb::init_from_parts after mm::init returns;
            // no other code accesses the framebuffer between these two points.
            let ptr: *mut u8 = fb.buffer_mut().as_mut_ptr();
            (ptr, info)
        });

    // BRING-UP DIAGNOSTIC (serial-free POST codes) — see `post_marker`. Record
    // the framebuffer so any code (incl. mm::init) can paint progress squares.
    // The last square shown on a hung bare-metal boot is the last step done.
    if let Some((ptr, info)) = fb_parts.as_ref() {
        post_fb_set(*ptr, info);
    }
    post_marker(0); // square 0 = kernel entry reached + framebuffer paintable

    // Load GDT/IDT — no IRQs yet.
    arch::init();
    post_marker(1); // GDT/IDT loaded

    // P15-T001: extract RSDP address before mm::init consumes boot_info.
    let rsdp_addr: Option<u64> = boot_info.rsdp_addr.into_option();

    mm::init(boot_info);
    post_marker(2); // mm::init returned (frame alloc + heap + buddy)

    // Map the kernel-stack pool with guard pages. Must precede any code
    // that claims a slot — Task::new, AP boot, per-process syscall-stack
    // setup, and the test harness's task spawns all go through
    // `kstack::alloc` / `alloc_leaked_top`.
    task::kstack::init();
    post_marker(3); // kernel-stack pool mapped

    // Phase 86a Track A.3 — Seed the CSPRNG from hardware entropy as early as
    // possible in the boot sequence.
    //
    // PLACEMENT RATIONALE — why here (after kstack::init, before everything else):
    //   - mm::init must have completed so the kernel heap is available (the spin
    //     Mutex in ChaChaDrbg requires no alloc, but log::info! may).
    //   - Must be before net::virtio_net::init so the TCP ISN helper draws from
    //     a seeded DRBG on the first connect/listen.
    //   - Must be before task::spawn(init_task) so AT_RANDOM bytes in every
    //     execve'd binary come from a seeded DRBG.
    //   - The #[cfg(test)] harness below may skip the rest of boot, but it never
    //     calls getrandom/exec/tcp, so running seed_csprng_early() before it is
    //     harmless.
    //
    // PRE-SEED CONSUMER AUDIT (Phase 86a) — referenced by symbol, not line, so
    // it does not drift:
    //   • Kernel stack canary:   m3OS uses panic-strategy=abort; there is no
    //       -Z stack-protector enabled; no canary is drawn before this point.
    //       Status: N/A.
    //   • AT_RANDOM (the rand16 fill in mm::elf::load_elf):
    //       Drawn at execve time, which is after init_task spawns and after this
    //       seed point.  Status: MOVED AFTER SEED.
    //   • TCP ISN (net::tcp::tcp_isn, used at both the active `connect` site and
    //       the passive `TcpState::Listen` SYN-ACK site):
    //       TCP connect/listen happens after net::virtio_net::init, which is
    //       after this seed point.  Status: MOVED AFTER SEED.
    //   • /dev/urandom + /dev/random (FdBackend::DevUrandom read handler):
    //       Served from this DRBG; reads only happen once userspace runs, well
    //       after this seed point.  Status: MOVED AFTER SEED.
    //   • DNS transaction ID:
    //       musl's resolver runs in userspace (ring 3) via getrandom.  getrandom
    //       is only called after userspace processes start (after init_task),
    //       which is well after this seed point.  Status: MOVED AFTER SEED.
    //   • Degraded path (no RDSEED/RDRAND): DRBG stays Early; fill_insecure is
    //       used by AT_RANDOM, the TCP ISN fallback, and /dev/urandom; boot
    //       reaches login prompt without deadlock.  Status: ACCEPTED DEGRADED.
    arch::x86_64::syscall::seed_csprng_early();
    post_marker(4); // CSPRNG seeded (RDSEED/RDRAND)

    // When built with `cargo test`, run the generated test harness and exit.
    // Placed after mm::init so that tests can use heap allocations.
    // `tmpfs::init()` is deferred to after this block so its heap / frame
    // usage doesn't perturb the frame-allocator baseline that some tests
    // snapshot.
    #[cfg(test)]
    test_main();

    // Phase 54: populate tmpfs with /tmp and /run top-level directories.
    // Must run after heap init so tmpfs allocations succeed, before any
    // task that opens files under those paths.
    fs::tmpfs::init();
    post_marker(5); // tmpfs up — next: framebuffer console

    // P9-T002: initialise framebuffer text console (fixed-font renderer).
    if let Some((buf_ptr, mut info)) = fb_parts {
        // Cap an over-large bootloader-selected framebuffer down to
        // FB_CAP (1920×1080). With QEMU `-vga std` the UEFI GOP path
        // greedily selects the LARGEST mode that fits in VRAM (up to 4K
        // with `vgamem_mb=32`), and the compositor has no GPU — it
        // software-composites every pixel, so 4K (8.3 MP) is the dominant
        // GUI cost. Reprogramming Bochs VBE to 1080p (2.07 MP) cuts that
        // ~4× and still leaves VRAM for double-buffering (2×1080×1920×4 =
        // 16.6 MiB < 32 MiB). A VBE mode set does not move the LFB, so
        // `buf_ptr` and the bootloader mapping (sized for the larger mode)
        // stay valid. Non-Bochs framebuffers (real UEFI hardware) decline
        // and keep their native mode.
        const FB_CAP_W: usize = 1920;
        const FB_CAP_H: usize = 1080;
        if info.width > FB_CAP_W || info.height > FB_CAP_H {
            let bpp_bits = (info.bytes_per_pixel * 8) as u16;
            // SAFETY: ring 0; runs before init_from_parts/enable_doublebuffer
            // touch the framebuffer. mm::init does not touch the FB region.
            match unsafe { fb::vbe::set_mode(FB_CAP_W as u16, FB_CAP_H as u16, bpp_bits) } {
                Ok((w, h)) => {
                    let (w, h) = (w as usize, h as usize);
                    info.width = w;
                    info.height = h;
                    // Bochs VBE sets VIRT_WIDTH = XRES on the mode set, so the
                    // linear stride is exactly `width` pixels with no padding.
                    info.stride = w;
                    // Single visible buffer; enable_doublebuffer() doubles this.
                    info.byte_len = w * h * info.bytes_per_pixel;
                    log::info!(
                        "[fb] capped framebuffer to {}x{} via Bochs VBE (compositor perf)",
                        w,
                        h
                    );
                }
                Err(e) => {
                    log::info!(
                        "[fb] framebuffer mode cap skipped ({:?}); using bootloader mode",
                        e
                    );
                }
            }
        }
        // SAFETY: buf_ptr is derived from boot_info.framebuffer which is
        // &'static mut; the mapping outlives the kernel.  mm::init does not
        // touch the framebuffer region.
        if unsafe { fb::init_from_parts(buf_ptr, info) } {
            log::info!("[fb] framebuffer console initialised");
            // Make the framebuffer write-combining. The bootloader maps it
            // uncacheable, so on real hardware every pixel write is a separate
            // bus transaction (~0.2 s per scrolled line — the bare-metal console
            // lag). Program a WC PAT slot (also done on every AP) and remap the
            // FB region to it: the CPU then batches pixel writes into burst
            // transactions. QEMU's RAM-backed FB is unaffected either way.
            arch::x86_64::pat::init();
            if let Some((base, len)) = fb::framebuffer_region() {
                // SAFETY: pat::init() ran on this (BSP) core just above; `base`
                // is the live FB mapping and WC is sound for a framebuffer.
                let leaves = unsafe { arch::x86_64::pat::set_range_write_combining(base, len) };
                log::info!(
                    "[fb] framebuffer remapped write-combining ({} leaves, {} KiB)",
                    leaves,
                    len / 1024
                );
            }
            // Update TTY0 winsize to match the actual framebuffer dimensions.
            if let Some((rows, cols)) = fb::console_text_size() {
                let mut tty = tty::TTY0.lock();
                tty.winsize.ws_row = rows;
                tty.winsize.ws_col = cols;
                log::info!("[fb] TTY winsize set to {}x{}", rows, cols);
            }
        } else {
            log::warn!("[fb] framebuffer too small for text console");
        }
    } else {
        log::warn!("[fb] no framebuffer provided by bootloader");
    }
    post_marker(6); // fb console init done (this step cleared the screen)

    // P15: ACPI table discovery — parse RSDP, RSDT/XSDT, MADT, FADT.
    acpi::init(rsdp_addr);
    post_marker(7); // ACPI tables parsed (real-firmware path)

    // Read the launch-time boot-mode override from QEMU fw_cfg (if present) so
    // `/proc/m3os-boot-mode` reflects it before `init` makes its greeter-vs-serial
    // decision. Safe no-op on real hardware (no fw_cfg).
    fwcfg::init();

    // Phase 55a (B): IOMMU discovery — consume decoded DMAR / IVRS tables,
    // build unit descriptor list, device-to-unit map, and reserved-region
    // set. No hardware bring-up yet (Tracks C / D / E follow).
    iommu::init();
    post_marker(8); // IOMMU (VT-d/DMAR) discovery done

    // Phase 34: Read RTC and establish boot wall-clock time.
    rtc::init_rtc();

    // Smoke-test heap allocations (P2-T007)
    let boxed = Box::new(42u64);
    log::info!("[mm] Box::new(42) = {}", *boxed);

    let v: Vec<u32> = vec![1, 2, 3];
    log::info!("[mm] Vec alloc ok, len={}", v.len());

    let s = String::from("heap works");
    log::info!("[mm] String alloc ok: {}", s);

    // P15: Enumerate PCI buses and log discovered devices.
    pci::init();
    post_marker(9); // PCI enumeration done (real device tree)

    // Phase 24: Initialize virtio-blk driver.
    blk::init();

    // Phase 56 Track B.2 — initialise the PS/2 AUX port (mouse) before the
    // PIC unmasks IRQ12. The init flow polls the 8042 directly via port I/O,
    // so it is safe to run with the PIC still masked. After
    // `enable_interrupts()` the IRQ12 line is live and the mouse handler
    // begins draining packets into the lock-free ring.
    // Phase 96: explicitly enable the PS/2 *keyboard* port + IRQ1. Previously the
    // boot only ran `init_mouse` and assumed firmware left the keyboard enabled;
    // on a laptop where the built-in keyboard is EC-emulated 8042 this makes the
    // port + IRQ1 live. Safe no-op effect on a pure I2C-HID laptop (no real 8042
    // keyboard). The USB keyboard path (xHCI + usb-hid) is independent.
    match unsafe { arch::x86_64::ps2::init_keyboard() } {
        Ok(()) => log::info!("[ps2] keyboard initialised (IRQ1 ready)"),
        Err(e) => log::warn!(
            "[ps2] keyboard init failed: {:?} — booting without PS/2 kbd",
            e
        ),
    }
    match unsafe { arch::x86_64::ps2::init_mouse() } {
        Ok(()) => log::info!("[ps2] mouse initialised (IRQ12 ready)"),
        Err(e) => log::warn!("[ps2] mouse init failed: {:?} — booting without mouse", e),
    }

    // Enable PIC and unmask IRQs now that all subsystems are initialized.
    unsafe { arch::enable_interrupts() };
    log::info!("[arch] interrupts enabled");

    // Phase 15: switch from PIC to APIC interrupt routing.
    // Only attempt APIC init if ACPI MADT data is available; otherwise the
    // kernel falls back to the legacy PIC (which is already running).
    if acpi::io_apic_address().is_some() {
        arch::x86_64::apic::init();
    } else {
        log::warn!("[apic] MADT/I/O APIC not found — staying on legacy PIC");
    }
    post_marker(10); // interrupts enabled + APIC routing init done

    // Phase 25: Initialize per-core data structures for the BSP.
    // Always called — gs_base must be set for the scheduler. If no MADT is
    // available, init_bsp_per_core() falls back to single-core BSP-only mode.
    smp::init_bsp_per_core();
    post_marker(11); // BSP per-core (GS base) init done

    // Phase 57e Track J — enable XSAVE/AVX state preservation on the BSP.
    //
    // CPUID probe runs first so the kernel can panic with a clear message on
    // pre-2011 CPUs (no OSXSAVE).  `enable_xsave_state` then sets CR4.OSXSAVE
    // and writes XCR0 = x87+SSE+AVX = 0x7.
    //
    // Ordering matters: this must happen *before* `smp::boot::boot_aps()` so
    // every AP picks up CR4.OSXSAVE = 1 from the trampoline's `DATA_CR4` slot
    // (the trampoline reads BSP CR4 at install time, see kernel/src/smp/boot.rs).
    // APs still need to set XCR0 themselves — XCR0 is per-core and not part
    // of CR4 — handled in `ap_entry`.
    let xsave = arch::x86_64::cpuid::probe();
    log::info!(
        "[xsave] supported components={:#x} max_area={} probe_xcr0_area={} xsaveopt={}",
        xsave.supported_components,
        xsave.max_area_size,
        xsave.area_size_at_mask,
        xsave.xsaveopt
    );
    // `enable_xsave_state` updates CR4 and writes XCR0 via xsetbv.  Its
    // safety contract requires IRQs disabled or single-threaded execution.
    // The BSP is single-threaded here (boot_aps has not run), but interrupts
    // are already enabled (line above), so wrap the privileged-register
    // update in `without_interrupts` to honor the contract unconditionally
    // and prevent an IRQ from observing partial CR4/XCR0 state.
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        arch::x86_64::cpuid::enable_xsave_state()
    });
    // Validate the static `XSAVE_AREA_SIZE` against the *enabled* mask size
    // (CPUID 0Dh.0.EBX re-read after `xsetbv`).  `area_size_at_mask` from the
    // probe was captured before XCR0 was set, so it reflects the reset mask and
    // is unsuitable for the size assertion.
    //
    // Phase 90a B.1: on a PKU CPU `enable_xsave_state` folded the PKRU
    // component (9) into XCR0, so `enabled_area` now includes the PKRU region;
    // the static `XSAVE_AREA_SIZE` (2752) was grown to fit it.  The assertion is
    // the per-core re-validation requirement — it fires loudly if the grown
    // layout ever exceeds the static buffer, rather than letting a later
    // xsave64 corrupt the adjacent task's state.
    let enabled_area = arch::x86_64::cpuid::enabled_area_size();
    log::info!(
        "[xsave] post-enable XCR0 area={enabled_area} (static={})",
        arch::x86_64::cpuid::XSAVE_AREA_SIZE
    );
    assert!(
        arch::x86_64::cpuid::XSAVE_AREA_SIZE >= enabled_area,
        "XSAVE_AREA_SIZE ({}) is smaller than CPUID-required area for enabled XCR0 ({})",
        arch::x86_64::cpuid::XSAVE_AREA_SIZE,
        enabled_area
    );

    // Phase 90a B.1 — report the BSP's PKU state (CR4.PKE + XCR0 component 9).
    // `pku_usable` consults the host-tested decode; `cr4_pke_enabled` reads the
    // live register so the log confirms the bit actually landed on this core.
    {
        let pku_features = arch::x86_64::cpuid::pku_features();
        log::info!(
            "[sec] BSP PKU usable={} CR4.PKE {} (cpu_pku={}, xsave_component9={})",
            pku_features.pku_usable(),
            if arch::x86_64::cpuid::cr4_pke_enabled() {
                "enabled"
            } else {
                "off"
            },
            pku_features.pku,
            pku_features.pkru_component_supported,
        );
    }
    post_marker(12); // XSAVE/AVX enable + size assert + PKU report done (real-silicon-first path)

    // Phase 77 Track B — enable CR4.SMEP (bit 20) + CR4.SMAP (bit 21) on the
    // BSP when the CPU supports them.  Ordering matters for the same reason as
    // XSAVE above: this runs *before* `boot_aps()` so the trampoline's captured
    // `DATA_CR4` carries the bits and every AP inherits them on CR4 reload.
    let (smep_on, smap_on) = x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        arch::x86_64::cpuid::enable_smep_smap()
    });
    // Clear EFLAGS.AC *outside* the `without_interrupts` bracket above (whose
    // `popf` would otherwise restore the firmware AC and silently disable SMAP
    // for the BSP's boot/idle context). Persistent for this context; syscall
    // entry clears AC via SFMASK and APs clear it in `ap_entry`.
    unsafe {
        arch::x86_64::cpuid::clear_ac_for_smap();
    }
    let (smep_sup, smap_sup) = arch::x86_64::cpuid::probe_smep_smap();
    log::info!(
        "[sec] BSP CR4.SMEP {} (supported={}), CR4.SMAP {} (supported={})",
        if smep_on { "enabled" } else { "off" },
        smep_sup,
        if smap_on { "enabled" } else { "off" },
        smap_sup,
    );

    // Phase 77 Track B (debug-only): prove SMEP/SMAP actually fault a ring-0
    // access to a user page. Feature-gated; absent in production builds.
    #[cfg(feature = "smep-smap-test")]
    x86_64::instructions::interrupts::without_interrupts(|| {
        arch::x86_64::smap_test::run_boot_self_test();
    });

    // Phase 111 Track B (debug-only): prove the #BP RIP-fixup + RFLAGS.TF
    // single-step substrate works end to end (int3 from kernel context, one
    // #DB per step). Feature-gated; absent in production builds.
    #[cfg(feature = "debug-substrate-test")]
    arch::x86_64::debug::run_boot_self_test();

    // Phase 77 Track E: apply microcode on the BSP first (before APs are
    // woken). A no-op clean skip on QEMU / non-AMD CPUs (no MSR write unless a
    // strictly-newer matching patch is found in the embedded blob).
    arch::x86_64::microcode::apply_microcode_on_cpu(0);

    // Phase 84 Track D.2 / C.2 — decide the Spectre mitigation policy and apply
    // the boot-time-applicable mitigations (eIBRS set-once) on the BSP. Runs
    // before `boot_aps()`; each AP re-applies its own per-core eIBRS in the AP
    // boot path. The `mitigations=off|auto|full` level is a build-time default
    // (see the `mitigations` module — m3OS has no kernel boot cmdline).
    x86_64::instructions::interrupts::without_interrupts(|| {
        crate::mitigations::init_bsp();
        // Phase 110 Track A.1 — prove the KPTI user-half PML4 builder maps only
        // the user lower half + minimal entry set, and no kernel-secret leaf, on
        // real hardware page tables (QEMU cannot exercise Meltdown itself). A
        // throwaway pair, built + walked + freed; emits a `KPTI_SELFTEST:`
        // sentinel. Inert w.r.t. the live CR3 (KPTI_WIRED is still false).
        crate::mm::kpti::self_test();
    });

    // Phase 103 Track E: probe HWP and opt in on the BSP (IA32_PM_ENABLE is
    // package-scope, so the one write covers APs). QEMU CPU models expose no
    // HWP — this logs the posture and the cpufreq mechanism stays a no-op.
    arch::x86_64::cpufreq::init_bsp();

    // Phase 16: Initialize NIC drivers.  Phase 55b E.5: the in-kernel e1000
    // driver has been deleted; device-specific 82540EM code now lives in
    // `userspace/drivers/e1000`. The kernel registers only virtio-net here;
    // the ring-3 e1000 driver registers its `RemoteNic` facade via IPC on
    // startup.
    net::virtio_net::init();
    post_marker(13); // SMEP/SMAP + microcode + Spectre mitigations + virtio-net done

    // Trigger a breakpoint to verify the IDT is working (P3-T007).
    if cfg!(debug_assertions) {
        x86_64::instructions::interrupts::int3();
        log::info!("[arch] breakpoint exception handled OK");
    }

    // Verify timer IRQ is firing (P3-T008) — debug builds only.
    if cfg!(debug_assertions) {
        let start = arch::x86_64::interrupts::tick_count();
        let mut ticked = false;
        for _ in 0..10_000_000u32 {
            // Debug-only, init-time: bounded by 10 M iterations × spin_loop hint
            // (~500 ms worst case at 50 ns/iter).  Never executed in release builds.
            // Not attributable to any user workload.
            core::hint::spin_loop();
            if arch::x86_64::interrupts::tick_count().wrapping_sub(start) >= 1 {
                ticked = true;
                break;
            }
        }
        let ticks = arch::x86_64::interrupts::tick_count();
        if ticked {
            log::info!("[arch] timer ticks after wait: {}", ticks);
        } else {
            log::warn!("[arch] no timer ticks observed — IRQs may not be firing");
        }
    }

    // Phase 25: Boot Application Processors.
    // Only if SMP was initialized and there are APs to boot.
    if smp::is_per_core_ready() && smp::core_count() > 1 {
        // 2026-05-25 — pre-reserve scheduler Vec capacity + warm the slab
        // caches before APs come online so that concurrent
        // `spawn_idle_for_core` calls from APs cannot trigger
        // `grow_heap` from inside `scheduler_lock`. See
        // `task::scheduler::reserve_for_smp_boot` for the full rationale.
        task::scheduler::reserve_for_smp_boot();
        smp::boot::boot_aps();
    }
    post_marker(14); // SMP boot_aps done — all APs online (AP rendezvous survived)

    // Phase 111 Track C — wait-for-debugger entry (kgdb feature only). Freezes
    // here (all-stop) until a host GDB-RSP client attaches on COM2 and
    // continues; then a planted breakpoint at `kgdb_probe_target` demonstrates
    // the full hit/inspect cycle. Absent in production. Placed after SMP boot
    // so the all-stop quiesce has real sibling cores to freeze.
    #[cfg(feature = "kgdb")]
    {
        debug::gdbstub::kgdb_break();
        debug::gdbstub::kgdb_probe_target();
    }

    task::spawn(init_task, "init");
    task::spawn_idle(idle_task);

    post_marker(15); // pre-task kernel init complete — entering scheduler
    // Bare-metal diagnostic: surface the LAPIC timer calibration on the
    // framebuffer (serial is invisible without a serial port). A sane value is
    // ~1500–60000 ticks/ms; the `6250 (default)`-class fallback or a wildly
    // different number flags a bad PIT-ch2 calibration (→ wrong nanosleep/timer
    // pacing). Printed just before the scheduler so it sits right above init's
    // first output. Guarded: `lapic_ticks_per_ms` panics if APIC wasn't inited.
    // The calibration value also goes to the always-on kernel log
    // (`[apic] LAPIC timer calibration: … ticks/ms`); this is just the
    // bare-metal fb mirror, gated with the other bring-up diagnostics.
    if BRINGUP_DIAG && acpi::io_apic_address().is_some() {
        crate::fb::write_fmt(format_args!(
            "[timer] lapic_ticks_per_ms={}\n",
            arch::x86_64::apic::lapic_ticks_per_ms()
        ));
    }
    log::info!("[kernel] entering scheduler — init will start service set");
    task::run()
}

// ---------------------------------------------------------------------------
// Phase 7 service tasks
// ---------------------------------------------------------------------------

/// init task: creates service endpoints, registers them, spawns servers,
/// then loads the userspace `/sbin/init` as PID 1.
fn init_task() -> ! {
    post_marker(21); // scheduler running; kernel-side init_task started
    // Phase 7: console service endpoint.
    let console_ep = ipc::endpoint::ENDPOINTS.lock().create();
    ipc::registry::register("console", console_ep)
        .expect("[init] failed to register console service");
    log::info!("[init] service registry: console={:?}", console_ep);

    // Ring-3 NIC ingress endpoint (Phase 55c Track E, restored 2026-04-24).
    //
    // The original Track E design spawned a dedicated `remote_nic_ingress_task`
    // that sat blocked on `recv_msg` until a ring-3 NIC sent an RX frame.
    // Merely having that task in the scheduler reliably starved PID 1's reap
    // loop in `serverization-fallback` because the busy-yield loop inside
    // `sys_nanosleep`'s long-sleep branch interacts with the extra task slot
    // badly enough on core 0 that `service stop <name>` blew past its 30 s
    // budget. See `docs/post-mortems/2026-04-24-ingress-task-starvation.md`.
    //
    // The fix: keep the ingress IPC contract intact (the driver still sends
    // `NET_RX_FRAME`/`NET_LINK_STATE` to `net.nic.ingress` via `send_buf`)
    // but fold the receive into `net_task` via the new `recv_msg_nowait`
    // primitive. The `set_endpoint_pending_send_hook` call below wires
    // driver-side enqueues to wake `net_task`, which drains the queue on its
    // next iteration and wakes the (now blocked-on-send) driver task. No
    // dedicated receiver task is on the run queue, so PID 1 sees no extra
    // contention.
    let ingress_ep = ipc::endpoint::ENDPOINTS.lock().create();
    ipc::registry::register("net.nic.ingress", ingress_ep)
        .expect("[init] failed to register net.nic.ingress service");
    ipc::endpoint::set_endpoint_pending_send_hook(ingress_ep, wake_net_for_ingress);
    log::info!("[init] service registry: net.nic.ingress={:?}", ingress_ep);

    // Phase 52: kbd endpoint creation and registration moved to the userspace
    // kbd_server service (kernel/initrd/etc/services.d/kbd.conf).  The kernel
    // no longer pre-registers or spawns a ring-0 kbd_server_task.

    // Phase 54: fat_server and vfs_server are now userspace processes.
    // Ring-0 endpoint pre-registration and task spawning removed —
    // the userspace crates register themselves on startup via IPC.

    // Spawn Phase 7 service tasks.
    task::spawn(console_server_task, "console");
    // No dedicated `remote_nic_ingress_task` — the ingress queue is drained
    // by `net_task` via `recv_msg_nowait` (see the ingress endpoint setup
    // above and the drain loop in `net_task`).
    // Phase 52: kbd_server_task removed — userspace kbd_server handles IRQ1.

    // Spawn the shared network processing task. Both NIC backends rely on it:
    // virtio-net wakes it from the kernel IRQ path, and the ring-3 e1000 path
    // uses it to flush RemoteNic TX plus process ingress-triggered wakeups.
    task::spawn(net_task, "net");

    // Phase 52: stdin_feeder_task removed — userspace stdin_feeder reads from
    // the userspace kbd_server via IPC and pushes to stdin via stdin_push syscall.

    // Phase 21: serial stdin feeder — reads bytes from COM1, feeds stdin buffer.
    // COM1 IRQ4 is routed to the BSP, so keep the feeder on the same core and
    // avoid cross-core scheduler wakeups from interrupt context.
    task::spawn_on_current_core(serial_stdin_feeder_task, "serial-stdin");

    // Phase 20: load /sbin/init from ramdisk as userspace PID 1.
    post_marker(22); // kernel service tasks spawned — exec'ing userspace PID 1
    spawn_userspace_init();

    // Phase 60 Track C — One-shot diagnostic.  After every kernel-side
    // spawn() and the userspace init handoff, log task_cache and
    // xsave_cache state so `cargo xtask run` traces carry a verbatim
    // record of production-path slab activity.  Matches the data captured
    // for the Phase 60 measurement handoff
    // (`docs/handoffs/60c-slab-heap-measurement.md`).
    {
        let caches = mm::slab::caches();
        let task_stats = caches.task_cache.lock().stats();
        let xsave_stats = caches.xsave_cache.lock().stats();
        let heap = mm::heap::heap_stats();
        log::info!(
            "[phase60] task_cache slabs={} active={} free={} | xsave_cache slabs={} active={} free={} | heap used={}KiB free={}KiB slab_pages={}",
            task_stats.total_slabs,
            task_stats.active_objects,
            task_stats.free_slots,
            xsave_stats.total_slabs,
            xsave_stats.active_objects,
            xsave_stats.free_slots,
            heap.used_bytes / 1024,
            heap.free_bytes / 1024,
            heap.slab_pages,
        );
    }

    log::info!("[init] service set started — yielding");
    // Phase 57e Bug #6 (closed): the original `loop { task::yield_now(); }`
    // under preempt-full got stuck in `preempt_enable`'s zero-crossing
    // synchronous-yield branch.  Closed in 8b44442 (Bug #12 part 5) when
    // the eager-yield branch was removed globally.
    //
    // Phase 57e Bug #11 (closed): a `loop { enable_and_hlt() }` halts BSP
    // and starves BSP-resident services if the only path back to the
    // scheduler is timer-driven kernel-mode preemption.  Closed by both
    // closing Bug #6 and (under voluntary) cooperatively yielding.
    //
    // Phase 57e Bug #12 part 7 (a1bfe17 + this commit): drop the hlt
    // that was added in eb1f13d.  hlt parks the BSP between yields,
    // adding up to 1 ms of latency on every same-core wake (BSP can
    // only re-yield after the next IRQ).  Voluntary mode had this
    // pattern correct at 052010a: just yield_now(), no hlt.  Match
    // voluntary's pattern in both modes — yes, it's a busy-yield loop
    // (BSP doesn't sleep when fully idle), but the busy-yield is what
    // kept voluntary lag-free, and the scheduler sees BSP's queue
    // immediately on each iteration.  Power-saving tradeoffs can be
    // revisited once the latency model is settled.
    loop {
        task::yield_now();
    }
}

// `remote_nic_ingress_task` (Phase 55c Track E original design) was deleted
// 2026-04-24. The dedicated kernel receiver task starved PID 1's reap loop
// just by sitting blocked on `recv_msg` — see the post-mortem at
// `docs/post-mortems/2026-04-24-ingress-task-starvation.md`. The replacement
// is `drain_remote_nic_ingress` (called from `net_task` below) which uses
// the new `recv_msg_nowait` primitive to drain the ingress endpoint without
// adding a task to the run queue.

/// Load `/sbin/init` from the ramdisk and launch it as userspace PID 1.
fn spawn_userspace_init() {
    use mm::elf::load_elf_into;

    let data = fs::ramdisk::get_file("sbin/init").expect("[init] /sbin/init not found in ramdisk");

    if data.is_empty() {
        panic!("[init] /sbin/init is empty — not built?");
    }

    log::info!("[init] loading /sbin/init: {} bytes", data.len());

    let new_cr3 = mm::new_process_page_table().expect("[init] out of frames for /sbin/init");
    let phys_off = mm::phys_offset();

    let argv: &[&[u8]] = &[b"/sbin/init"];
    let envp: &[&[u8]] = &[
        b"PATH=/usr/local/bin:/bin:/sbin:/usr/bin",
        b"HOME=/",
        b"TERM=m3os-term",
    ];

    let (loaded, user_rsp) = {
        let mut mapper = unsafe { mm::mapper_for_frame(new_cr3) };
        let loaded = unsafe { load_elf_into(&mut mapper, phys_off, data, "/sbin/init") }
            .expect("[init] ELF load failed for /sbin/init");
        let user_rsp = unsafe {
            mm::elf::setup_abi_stack_with_envp(
                loaded.stack_top,
                &mapper,
                phys_off,
                argv,
                envp,
                loaded.aux_info(),
                Some(b"/sbin/init"),
            )
        }
        .expect("[init] ABI stack setup failed for /sbin/init");
        (loaded, user_rsp)
    };

    log::info!(
        "[init] /sbin/init loaded: entry={:#x} rsp={:#x}",
        loaded.entry,
        user_rsp,
    );

    let pid = process::spawn_process_with_cr3(
        0,
        loaded.entry,
        user_rsp,
        x86_64::PhysAddr::new(new_cr3.start_address().as_u64()),
        0,
        0,
    );
    log::info!("[init] /sbin/init registered as pid {}", pid);

    // PID 1 is loaded directly here, not through `execve`, so it never
    // passes the path that populates `comm` / `exec_path` / `cmdline` from
    // the binary basename. Set them explicitly so `/proc/1/{comm,stat,
    // cmdline}`, `ps`, and `htop` show "init" instead of the "unknown"
    // fallback `proc_name` uses when all three are empty.
    {
        let mut table = process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.set_comm(b"init");
            proc.exec_path = String::from("/sbin/init");
            proc.cmdline = vec![String::from("/sbin/init")];
        }
    }

    task::spawn_fork_task(
        process::make_fork_ctx_zeroed(pid, loaded.entry, user_rsp),
        "userspace-init",
    );
}

/// Console server: receives IPC write requests, logs to serial, replies with ack.
///
/// # Data path
///
/// Callers pass a kernel-space pointer and length in the IPC message.  The
/// server **validates** the pointer range (non-null, bounded length, no
/// overflow) and then copies the bytes into a local buffer before use.
/// This eliminates the previous `from_raw_parts` shortcut that directly
/// aliased caller memory.
///
/// When this service is eventually extracted to a ring-3 process, the
/// validated copy will be replaced by `copy_from_user` which additionally
/// walks the caller's page tables.
///
/// # IPC protocol (label = CONSOLE_WRITE)
///
///   data\[0\] = pointer to UTF-8 string bytes (kernel address)
///   data\[1\] = byte length (must be 1..=4096)
///
/// Reply: label = 0 on success, `u64::MAX` on error.
///
/// # Service lifecycle (Phase 46)
///
/// Follows the standard service lifecycle: registers its endpoint via the
/// service registry, enters a recv/reply_recv loop, and is restart-safe
/// (the registry supports re-registration, and `cleanup_task_ipc` from
/// Track E cleans up endpoint/notification state if this task dies;
/// callers blocked in `BlockedOnReply` remain stuck — see docs/06-ipc.md).
fn console_server_task() -> ! {
    let my_id = task::current_task_id().expect("[console] no task id");

    // Look up this server's endpoint via the service registry.
    let ep_id = ipc::registry::lookup("console").expect("[console] endpoint not in registry");

    task::set_server_endpoint(my_id, ep_id);

    // Insert an endpoint capability at handle 0.
    let ep_handle = task::insert_cap(my_id, ipc::Capability::Endpoint(ep_id))
        .expect("[console] failed to insert endpoint cap");
    debug_assert_eq!(
        ep_handle, 0,
        "[console] endpoint cap not at expected handle 0"
    );

    log::info!("[console] ready");

    // First receive.
    let reply_cap_handle: ipc::CapHandle = 1;
    let mut msg = ipc::endpoint::recv_msg(my_id, ep_id);

    loop {
        let reply_msg = match msg.label {
            CONSOLE_WRITE => {
                let ptr = msg.data[0];
                let len = msg.data[1] as usize;
                if ptr == 0
                    || len == 0
                    || len > MAX_CONSOLE_WRITE_LEN
                    || ptr.checked_add(len as u64).is_none()
                {
                    ipc::Message::new(u64::MAX)
                } else {
                    // Validated kernel-space copy: current callers are kernel tasks
                    // sharing the kernel address space. When this service moves to
                    // ring 3, callers will use copy_from_user instead.
                    let mut buf = alloc::vec![0u8; len];
                    unsafe {
                        core::ptr::copy_nonoverlapping(ptr as *const u8, buf.as_mut_ptr(), len);
                    }
                    if let Ok(text) = core::str::from_utf8(&buf) {
                        crate::serial::_print(format_args!("{}", text));
                        fb::write_str(text);
                        ipc::Message::new(0)
                    } else {
                        log::warn!("[console] received invalid UTF-8; rejecting write request");
                        ipc::Message::new(u64::MAX)
                    }
                }
            }
            _ => {
                // Unknown operation — reply with error label.
                ipc::Message::new(u64::MAX)
            }
        };

        // Consume the one-shot reply cap inserted by recv_msg.
        let caller_id = match task::task_cap(my_id, reply_cap_handle) {
            Ok(ipc::Capability::Reply(id)) => id,
            _ => {
                // Sender used send() rather than call() — no reply cap was inserted.
                // Log a warning and recv the next message without replying.
                log::warn!("[console] no reply cap at handle 1; sender used send rather than call");
                msg = ipc::endpoint::recv_msg(my_id, ep_id);
                continue;
            }
        };
        let _ = task::remove_task_cap(my_id, reply_cap_handle);

        // Reply and immediately wait for the next message.
        msg = ipc::endpoint::reply_recv_msg(my_id, caller_id, ep_id, reply_msg);
    }
}

// Phase 52: kbd_server_task removed — the userspace kbd_server
// (kernel/initrd/etc/services.d/kbd.conf) now owns IRQ1, scancode translation,
// and KBD_READ IPC handling.

/// Console IPC operation label: write a UTF-8 string to the serial console.
///
/// data[0] = kernel pointer to string bytes, data[1] = byte length (max 4096).
const CONSOLE_WRITE: u64 = 0;
const MAX_CONSOLE_WRITE_LEN: usize = 4096;

// ---------------------------------------------------------------------------
// Phase 8 storage tasks
// ---------------------------------------------------------------------------

// Phase 54: ring-0 fat_server_task and vfs_server_task removed.
// These are now userspace processes (userspace/fat_server, userspace/vfs_server).

/// Idle task: halts the CPU between timer ticks, then explicitly yields.
///
/// Under voluntary preemption (the only mode after Phase 57e was deferred
/// — see `docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md`),
/// the timer IRQ handler only sets `reschedule`; it does not dispatch.
/// A pure `enable_and_hlt` loop would therefore hlt again as soon as the
/// IRQ returned, instead of running the task that the IPI was meant to
/// wake.  The explicit `yield_now` after the wake is load-bearing — it
/// turns the IRQ-set `reschedule` into an actual scheduler dispatch.
///
/// History: an earlier `preempt-full` mode handled this on IRQ-return via
/// `check_and_preempt_kernel`, but that path was removed when 57e was
/// deferred (commit a1bfe17 retired timer-driven kernel-mode preemption,
/// commit d8278ca retired the feature flag).
fn idle_task() -> ! {
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
        task::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Phase 21 — serial stdin feeder
// Phase 57a H.1 — migrated to notification-based wait (block_current_until)
// ---------------------------------------------------------------------------

/// Read bytes from the IRQ-driven serial ring buffer and feed them into the
/// kernel stdin buffer with canonical editing, echo, and signal support.
///
/// # Phase 57a H.1 — notification-based park
///
/// The previous implementation used an `enable_and_hlt` halt-loop to wait for
/// the next COM1 RX IRQ.  This parks the **entire CPU core** until any
/// interrupt arrives, which prevented co-scheduled tasks (notably `kbd_server`
/// when both were placed on AP3 by the scheduler) from running.
///
/// The new implementation mirrors `net_task`:
///
/// 1. Register this task's [`TaskId`] with [`serial::set_feeder_task_id`] so
///    the COM1 RX ISR can wake it via [`task::scheduler::wake_task_v2`].
/// 2. At the top of each drain loop, clear `STDIN_FEEDER_WOKEN` with a `swap`
///    (edge-triggered, same as `NIC_WOKEN` in `net_task`).
/// 3. Drain all pending bytes from the ring buffer.
/// 4. If no bytes were available, park via
///    `block_current_until(&STDIN_FEEDER_WOKEN, Some(now + 1ms))`.  The scheduler
///    can now dispatch other tasks on this core while the feeder is blocked,
///    and the short deadline guarantees a missed IRQ wake cannot make console
///    input appear dead.
/// 5. On wake (ISR set the flag + IPI'd us), loop back to step 2.
fn serial_stdin_feeder_task() -> ! {
    // Enable UART Receive Data Available interrupt (IER bit 0).
    unsafe {
        x86_64::instructions::port::Port::new(0x3F9u16).write(0x01u8);
    }

    // H.1: register our TaskId so the COM1 RX ISR can issue wake_task_v2.
    let my_id = task::scheduler::current_task_id()
        .expect("[serial-stdin] no task id — feeder must run inside the scheduler");
    crate::serial::set_feeder_task_id(my_id);

    log::info!("[serial-stdin] feeder ready (notification-based, echo + signals)");

    loop {
        // Edge-triggered drain gate: clear the wake flag up-front so any IRQ
        // that fires during draining is still observed on the next iteration.
        // This is the same swap pattern used by net_task on NIC_WOKEN.
        crate::serial::STDIN_FEEDER_WOKEN.store(false, core::sync::atomic::Ordering::Release);

        // Drain all bytes currently in the ring buffer.
        let mut drained = false;
        while let Some(byte) = crate::serial::serial_rx_pop() {
            drained = true;

            // Delegate to the unified LineDiscipline in TTY0.
            let mut eof_signal = false;
            let result = {
                let mut t = tty::TTY0.lock();
                t.ldisc.process_byte(byte, &mut |data| {
                    if data.is_empty() {
                        eof_signal = true;
                    } else {
                        for &b in data {
                            stdin::push_char(b);
                        }
                    }
                })
            };
            if eof_signal {
                stdin::signal_eof();
            }

            // Handle the result.
            match result {
                kernel_core::tty::LdiscResult::Consumed => {}
                kernel_core::tty::LdiscResult::Signal(sig) => {
                    let fg = process::FG_PGID.load(core::sync::atomic::Ordering::Relaxed);
                    if fg != 0 {
                        let name = match sig {
                            2 => "^C",
                            20 => "^Z",
                            3 => "^\\",
                            _ => "",
                        };
                        serial_echo(name);
                        serial_echo("\n");
                        process::send_signal_to_group(fg, sig as u32);
                    } else {
                        stdin::push_char(byte);
                    }
                }
                kernel_core::tty::LdiscResult::Pushed { ref echo }
                | kernel_core::tty::LdiscResult::LineComplete { ref echo } => {
                    if let Some(count) = echo.erase_count() {
                        for _ in 0..count {
                            serial_echo("\x08 \x08");
                        }
                    } else if !echo.is_empty() {
                        let echo_bytes = echo.as_slice();
                        if let Ok(s) = core::str::from_utf8(echo_bytes) {
                            serial_echo(s);
                        }
                    }
                }
            }
        }

        // If we drained at least one byte this iteration, loop immediately to
        // catch any bytes that arrived during processing before parking.
        if drained {
            continue;
        }

        // Ring buffer was empty — park briefly until the COM1 RX ISR signals
        // us, with a 1 ms deadline as a safety net. The IRQ wake is the fast
        // path; the deadline prevents console input from getting stuck if an
        // interrupt-side wake races or is otherwise lost.
        let deadline = arch::x86_64::interrupts::tick_count().saturating_add(1);
        let _ = task::scheduler::block_current_until(
            task::TaskState::BlockedOnRecv,
            &crate::serial::STDIN_FEEDER_WOKEN,
            Some(deadline),
        );
    }
}

/// Echo a string back to the serial port (COM1).
fn serial_echo(s: &str) {
    for &b in s.as_bytes() {
        unsafe {
            // Wait for transmit holding register to be empty.
            while x86_64::instructions::port::Port::<u8>::new(0x3FD).read() & 0x20 == 0 {}
            x86_64::instructions::port::Port::new(0x3F8).write(b);
        }
    }
}

// ---------------------------------------------------------------------------
// Network task (P16-T055)
// ---------------------------------------------------------------------------

/// Background task that processes incoming network frames.
///
/// Phase 55b E.5: the virtio-net driver installs its RX IRQ through the HAL
/// (`install_msi_irq` / `install_intx_irq`); the ISR sets
/// [`net::virtio_net::NET_IRQ_WOKEN`] and wakes this task. The ring-3 e1000
/// driver (`userspace/drivers/e1000`) delivers frames via `RemoteNic::inject_rx_frame`
/// which also sets [`net::NIC_WOKEN`]. Between IRQs the task parks via
/// [`task::scheduler::block_current_unless_woken`]; on wake it drains all
/// pending frames through the network dispatch stack.
fn net_task() -> ! {
    // Prioritize protocol dispatch slightly above normal userspace so queued
    // ring-3 NIC RX/TX work drains promptly once the ingress send wakes us.
    let _ = task::sys_nice(-8, 0);

    // Register this task's id with the virtio-net ISR so it can wake us.
    // The ring-3 e1000 driver wakes the task via the per-endpoint pending-send
    // hook installed by `init_task` for `net.nic.ingress`, which calls
    // `wake_net_task()` on the same `NIC_WOKEN` flag this task parks on.
    let my_id = task::scheduler::current_task_id().expect("[net] no task id");
    net::virtio_net::set_net_task_id(my_id);
    let ingress_ep = ipc::registry::lookup_endpoint_id("net.nic.ingress");
    log::info!(
        "[net] network processing task started (ingress={:?})",
        ingress_ep,
    );

    loop {
        // Clear the unified wake flag up front so any edge set between now
        // and park is still observable.
        net::NIC_WOKEN.store(false, core::sync::atomic::Ordering::Release);
        let mut any =
            net::virtio_net::NET_IRQ_WOKEN.swap(false, core::sync::atomic::Ordering::Acquire);
        // Edge-triggered ingress drain: only call `recv_msg_nowait` when
        // the pending-send hook has explicitly signaled work. Skipping the
        // unconditional drain keeps `net_task`'s park loop free of
        // `ENDPOINTS.lock()` acquisitions when no ring-3 NIC is publishing.
        let mut drained_ingress =
            if net::INGRESS_HAS_WORK.swap(false, core::sync::atomic::Ordering::AcqRel) {
                drain_remote_nic_ingress(my_id, ingress_ep)
            } else {
                0
            };
        let mut drained_remote_rx = net::remote::RemoteNic::drain_rx_queue();
        let mut drained_remote_tx = net::remote::RemoteNic::drain_tx_queue();
        while any || drained_ingress != 0 || drained_remote_rx != 0 || drained_remote_tx != 0 {
            net::dispatch::process_rx();
            drained_ingress =
                if net::INGRESS_HAS_WORK.swap(false, core::sync::atomic::Ordering::AcqRel) {
                    drain_remote_nic_ingress(my_id, ingress_ep)
                } else {
                    0
                };
            drained_remote_rx = net::remote::RemoteNic::drain_rx_queue();
            any = net::virtio_net::NET_IRQ_WOKEN.swap(false, core::sync::atomic::Ordering::Acquire);
            drained_remote_tx = net::remote::RemoteNic::drain_tx_queue();
        }
        // Phase 77 Track D.2 — service the TCP retransmission timers once per
        // pass. Runs in this (task) context so the replayed segments go out the
        // normal `ipv4::send` path; the periodic deadline below guarantees it
        // fires even when no NIC event wakes the task.
        net::tcp::tcp_tick();

        // Phase 91 — step the IPv6 maintenance state machine (link-local
        // formation on first run, router solicitation, DHCPv6). Rides the same
        // periodic ~200 ms deadline so it advances on an otherwise idle link.
        net::ipv6::v6_tick();

        // Phase 96 R4 — drive the DHCP client one step (no-op unless a RemoteNic
        // is registered and not yet bound). Runs after `process_rx` so an OFFER/
        // ACK queued on UDP:68 this pass is consumed immediately.
        net::dhcp::tick();

        // Park on the unified flag: the virtio-net ISR, RemoteNic, and the
        // ingress pending-send hook all set it, so a wake from any path
        // reliably unblocks the task.
        //
        // F.6: under sched-v2 use block_current_until (v2 CAS primitive).
        // Phase 77 Track D.2: a ~200 ms deadline turns the park into a periodic
        // wake so the RTO scan above runs even on an otherwise idle link.
        //
        // Phase 96: TX is now fire-and-forget (`send_tx_owned`) — queued frames
        // carry their own bytes and the net task never blocks waiting for the
        // polled driver to pick one up, so there is no in-flight state to
        // fast-poll. `send_frame` already wakes this task (`wake_net_task`) the
        // moment TCP queues an outbound frame, so the 200 ms deadline only has
        // to backstop the periodic RTO scan on an idle link.
        {
            const TCP_RTO_TICK_INTERVAL_MS: u64 = 200;
            let deadline = crate::arch::x86_64::interrupts::tick_count()
                .saturating_add(TCP_RTO_TICK_INTERVAL_MS);
            let _ = task::scheduler::block_current_until(
                task::TaskState::BlockedOnRecv,
                &net::NIC_WOKEN,
                Some(deadline),
            );
        }
    }
}

/// Pending-send hook installed on the `net.nic.ingress` endpoint.
///
/// Sets `INGRESS_HAS_WORK` (an edge-triggered drain gate) and wakes the
/// shared net task. The gate keeps `net_task`'s no-traffic park loop free
/// of `ENDPOINTS.lock()` acquisitions: without it, calling
/// `recv_msg_nowait` on every wake amplifies PID 1 starvation under
/// `serverization-fallback`'s nanosleep busy-yield.
fn wake_net_for_ingress() {
    net::INGRESS_HAS_WORK.store(true, core::sync::atomic::Ordering::Release);
    net::virtio_net::wake_net_task();
}

/// Drain queued ingress messages from the ring-3 NIC driver.
///
/// Returns the number of messages processed. This replaces the dedicated
/// `remote_nic_ingress_task` that Phase 55c Track E originally spawned —
/// keeping that task blocked on `recv_msg` introduced PID 1 starvation
/// (see `docs/post-mortems/2026-04-24-ingress-task-starvation.md`).
fn drain_remote_nic_ingress(
    my_id: task::TaskId,
    ingress_ep: Option<ipc::endpoint::EndpointId>,
) -> usize {
    use kernel_core::driver_ipc::net::{NET_LINK_STATE, NET_RX_FRAME};

    let Some(ep_id) = ingress_ep else {
        return 0;
    };
    let mut count = 0usize;
    while let Some(msg) = ipc::endpoint::recv_msg_nowait(my_id, ep_id) {
        if msg.label == u64::MAX {
            // Sentinel from cleanup: endpoint closed mid-drain. Stop.
            break;
        }
        let bulk = task::take_bulk_data(my_id).unwrap_or_default();
        match msg.label {
            label if label == NET_RX_FRAME as u64 => {
                if bulk.is_empty() {
                    log::warn!("[net-ingress] NET_RX_FRAME arrived without bulk payload");
                } else {
                    net::remote::RemoteNic::inject_rx_frame(&bulk);
                }
            }
            label if label == NET_LINK_STATE as u64 => {
                if bulk.is_empty() {
                    log::warn!("[net-ingress] NET_LINK_STATE arrived without payload");
                } else {
                    net::remote::RemoteNic::handle_link_state(&bulk);
                }
            }
            label => {
                log::warn!("[net-ingress] unexpected message label {}", label);
            }
        }
        count += 1;
    }
    count
}

// ---------------------------------------------------------------------------
// Kernel utilities
// ---------------------------------------------------------------------------

pub fn hlt_loop() -> ! {
    // Track B (SMP TLB-shootdown survivability,
    // docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md): every
    // caller of `hlt_loop` is a terminal dead-end — the panic handler
    // (`handle_panic`), the recursive-#PF cascade, the kstack-overflow / kernel
    // page-fault arm, #GP, and #DF all funnel here and never return. Mark this
    // core offline so the TLB-shootdown target loops (`smp::tlb`) stop counting
    // it as a core that must acknowledge. `is_online` is otherwise write-once
    // (→ `true` at AP bringup), so a wedged core was never excluded: a sibling
    // issuing a routine `mprotect`/`mmap` shootdown would wait the full ack
    // window on an ACK that can never come. A halted core never runs userspace
    // again, so abandoning its (now stale) TLB is correct. `try_per_core()` is
    // ISR-safe and returns `None` (no-op) if per-core data isn't up yet, so
    // this is safe on the earliest-boot halt paths too.
    if let Some(pc) = crate::smp::try_per_core() {
        pc.is_online
            .store(false, core::sync::atomic::Ordering::Release);
    }
    loop {
        x86_64::instructions::hlt();
    }
}

/// Kernel panic dispatcher invoked by the binary's `#[panic_handler]`. In
/// `cfg(test)` builds, delegates to the test runner's panic handler so QEMU
/// exits with the failure code; otherwise prints the panic banner, dumps
/// the crash context and trace rings, then halts.
pub fn handle_panic(info: &core::panic::PanicInfo) -> ! {
    #[cfg(test)]
    testing::test_panic_handler(info);

    #[cfg(not(test))]
    {
        // Phase 99 (Track C.1) — quiesce sibling cores before printing so the
        // banner + crash dump land on a quiet COM1 instead of SMP-interleaved
        // garbage (docs/handoffs/2026-06-05-4gib-smp-panic-corrupted-output.md).
        // If another core already owns the panic, park without printing so a
        // second panic during the dump cannot re-corrupt the banner. Bounded:
        // a wedged sibling that never acks times the grace window out rather
        // than hanging the panic path. Single-core / pre-SMP boot is a no-op
        // (returns `true` immediately, no NMIs sent).
        if !crate::smp::panic_quiesce_aps() {
            hlt_loop();
        }
        if let Some(location) = info.location() {
            serial::_panic_print(format_args!(
                "KERNEL PANIC at {}:{}\n",
                location.file(),
                location.line()
            ));
            // Also surface the panic on the framebuffer console: the serial
            // banner is invisible on bare metal (no serial cable), so a kernel
            // panic would otherwise look like a silent freeze. This is the only
            // way to read a crash location off a physical screen.
            crate::fb::write_fmt(format_args!(
                "\nKERNEL PANIC at {}:{}\n  {}\n",
                location.file(),
                location.line(),
                info.message(),
            ));
        } else {
            serial::_panic_print(format_args!("KERNEL PANIC at unknown location\n"));
            crate::fb::write_fmt(format_args!(
                "\nKERNEL PANIC (unknown location)\n  {}\n",
                info.message()
            ));
        }
        serial::_panic_print(format_args!("  {}\n", info.message()));
        panic_diag::dump_crash_context();
        trace::dump_trace_rings();
        // Phase 111 Track C.4 — a bare-metal panic drops into the in-kernel GDB
        // stub (kgdb feature) for live post-mortem instead of a dead halt. The
        // banner + crash dump have already printed on the (now quiesced) COM1;
        // the stub then owns COM2 until the operator detaches, after which we
        // fall through to the halt below.
        #[cfg(feature = "kgdb")]
        debug::gdbstub::enter_from_panic();
        hlt_loop();
    }
}

/// Allocation-failure handler invoked by the binary's `#[alloc_error_handler]`.
/// Called only after the bootstrap/size-class allocator has already attempted
/// allocator-local reclaim and any eligible heap-growth retry — if we reach
/// here the allocation truly is out of options.
pub fn handle_alloc_error(layout: alloc::alloc::Layout) -> ! {
    // Print frame-allocator state before panicking so the post-mortem
    // log shows exactly how exhausted the buddy was — distinguishes
    // "kernel heap fragmented but buddy has free frames" from "buddy
    // is fully empty, every byte of system RAM is accounted for
    // somewhere". The current calling task's pid is also useful: if
    // it's a fresh fork child trying to allocate its kernel stack
    // we know the culprit is process-creation cost.
    let (free, total) = crate::mm::frame_allocator::free_frame_count();
    let pid = crate::process::current_pid();
    serial::_panic_print(format_args!(
        "[alloc_error] layout={:?} buddy free={}/{} pages ({} MiB free / {} MiB total) pid={}\n",
        layout,
        free,
        total,
        (free * 4) / 1024,
        (total * 4) / 1024,
        pid,
    ));
    // Bare-metal-visible copy (serial is invisible without a cable).
    crate::fb::write_fmt(format_args!(
        "\n[alloc_error] {} MiB free / {} MiB total, pid={}\n",
        (free * 4) / 1024,
        (total * 4) / 1024,
        pid,
    ));
    panic!(
        "kernel OOM: failed to allocate {:?} after heap growth retry",
        layout
    );
}

// ---------------------------------------------------------------------------
// In-QEMU unit tests (run via `cargo xtask test`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::serial_println;

    #[test_case]
    fn trivial_assertion() {
        assert_eq!(1 + 1, 2);
    }

    #[test_case]
    fn serial_output_works() {
        serial_println!("serial output from test");
    }

    // -----------------------------------------------------------------------
    // Phase 33 — Memory subsystem tests
    // -----------------------------------------------------------------------

    /// C.1/C.2: Verify the post-cutover allocator can satisfy large runtime
    /// allocations without disturbing the bootstrap heap accounting.
    #[test_case]
    fn heap_grows_on_oom() {
        use crate::mm::{
            frame_allocator::frame_stats,
            heap::{HEAP_INITIAL_SIZE, heap_stats},
        };
        use alloc::vec::Vec;

        let before = heap_stats();
        let frames_before = frame_stats();
        assert!(before.total_size >= HEAP_INITIAL_SIZE);
        assert!(
            before.size_class_active,
            "size-class allocator was not activated before runtime tests",
        );

        // Allocate a series of 256 KiB blocks to push past initial heap.
        let block_size = 256 * 1024;
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        // Allocate enough to exceed the initial heap by 1 MiB.
        let target = HEAP_INITIAL_SIZE + (1024 * 1024);
        let mut total_allocated = 0usize;
        while total_allocated < target {
            let mut block = Vec::with_capacity(block_size);
            // Touch the memory to ensure it's actually mapped.
            block.resize(block_size, 0xAB);
            assert_eq!(block[0], 0xAB);
            assert_eq!(block[block_size - 1], 0xAB);
            blocks.push(block);
            total_allocated += block_size;
        }

        let after = heap_stats();
        let frames_after = frame_stats();
        // Runtime allocations should increase allocator activity and consume
        // backing pages without requiring the bootstrap heap itself to grow.
        assert!(
            after.alloc_count > before.alloc_count,
            "allocator did not record new allocations: before={} after={}",
            before.alloc_count,
            after.alloc_count
        );
        assert!(
            after.page_backed_pages > before.page_backed_pages,
            "page-backed allocation count did not increase: before={} after={}",
            before.page_backed_pages,
            after.page_backed_pages
        );
        assert!(
            frames_after.allocated_frames > frames_before.allocated_frames,
            "frame usage did not increase: before={} after={}",
            frames_before.allocated_frames,
            frames_after.allocated_frames
        );
        serial_println!(
            "allocator grew via page-backed path: bootstrap={} KiB large_pages={} alloc_delta={}",
            after.total_size / 1024,
            after.page_backed_pages,
            after.alloc_count - before.alloc_count
        );

        // Drop all blocks — backing pages should return to the frame allocator.
        drop(blocks);
        let final_stats = heap_stats();
        let frames_final = frame_stats();
        assert!(
            frames_final.allocated_frames < frames_after.allocated_frames,
            "dropping blocks did not release backing pages: after={} final={}",
            frames_after.allocated_frames,
            frames_final.allocated_frames
        );
        assert_eq!(
            final_stats.page_backed_pages, before.page_backed_pages,
            "dropping blocks did not restore the page-backed allocation count: before={} final={}",
            before.page_backed_pages, final_stats.page_backed_pages
        );
        assert!(final_stats.free_bytes > 0);
    }

    /// B: Verify buddy allocator manages frames correctly — alloc and free
    /// cycle doesn't leak.
    #[test_case]
    fn buddy_alloc_free_no_leak() {
        use crate::mm::frame_allocator;

        // Pre-allocate the storage Vec so that its slab page consumption
        // happens before we snapshot the free count.  Without this, the
        // size-class allocator's first slab page allocation would appear
        // as a frame leak.
        let mut frames = alloc::vec::Vec::with_capacity(16);

        let before = frame_allocator::available_count();

        // Allocate 16 frames.
        for _ in 0..16 {
            let frame = frame_allocator::allocate_frame().expect("frame alloc failed");
            frames.push(frame.start_address().as_u64());
        }

        let during = frame_allocator::available_count();
        assert!(
            during <= before - 16,
            "free count should have dropped by at least 16: before={} during={}",
            before,
            during
        );

        // Free all frames.
        for phys in frames {
            frame_allocator::free_frame(phys);
        }

        let after = frame_allocator::available_count();
        assert_eq!(
            after, before,
            "frame leak: before={} after={}",
            before, after
        );
    }

    /// B.4: Verify contiguous multi-page allocation works.
    #[test_case]
    fn contiguous_alloc_works() {
        use crate::mm::frame_allocator;

        let before = frame_allocator::available_count();

        // Allocate 4 contiguous pages (order 2).
        let frame = frame_allocator::allocate_contiguous(2).expect("contiguous alloc failed");
        let base = frame.start_address().as_u64();

        // Verify alignment: base must be 16 KiB aligned (4 pages).
        assert_eq!(
            base % (4096 * 4),
            0,
            "contiguous block not properly aligned"
        );

        // Free and verify no leak.
        frame_allocator::free_contiguous(base, 2);
        let after = frame_allocator::available_count();
        assert_eq!(
            after, before,
            "contiguous frame leak: before={} after={}",
            before, after
        );
    }

    /// D.4: Verify allocate_frame_zeroed returns a fully zeroed frame.
    #[test_case]
    fn allocate_frame_zeroed_returns_zeros() {
        use crate::mm::frame_allocator;

        // First, allocate a raw frame, write a non-zero pattern, and free it
        // so the buddy pool contains a "dirty" frame.
        let dirty = frame_allocator::allocate_frame().expect("alloc dirty frame");
        let phys = dirty.start_address().as_u64();
        let phys_off = crate::mm::phys_offset();
        let ptr = (phys_off + phys) as *mut u8;
        unsafe { core::ptr::write_bytes(ptr, 0xAB, 4096) };
        frame_allocator::free_frame(phys);

        // Now allocate via the zeroed path.
        let zeroed = frame_allocator::allocate_frame_zeroed().expect("alloc zeroed frame");
        let z_phys = zeroed.start_address().as_u64();
        let z_ptr = (phys_off + z_phys) as *const u8;
        let data = unsafe { core::slice::from_raw_parts(z_ptr, 4096) };
        assert!(
            data.iter().all(|&b| b == 0),
            "allocate_frame_zeroed returned non-zero content at frame {:#x}",
            z_phys
        );
        frame_allocator::free_frame(z_phys);
    }

    /// D.4: Stale-mapping reuse — dirty frames recycled through multiple
    /// alloc/free cycles must still be zeroed by allocate_frame_zeroed.
    /// Catches regressions where a new allocator path skips zeroing.
    #[test_case]
    fn zero_exposure_stale_reuse_cycles() {
        use crate::mm::frame_allocator;
        let phys_off = crate::mm::phys_offset();

        // Run 4 rounds with different poison patterns to defeat coincidence.
        let patterns: [u8; 4] = [0xDE, 0x55, 0xFF, 0x01];
        for (round, &pattern) in patterns.iter().enumerate() {
            let dirty = frame_allocator::allocate_frame().expect("alloc dirty");
            let phys = dirty.start_address().as_u64();
            unsafe {
                core::ptr::write_bytes((phys_off + phys) as *mut u8, pattern, 4096);
            }
            frame_allocator::free_frame(phys);

            let zeroed = frame_allocator::allocate_frame_zeroed().expect("alloc zeroed");
            let z_phys = zeroed.start_address().as_u64();
            let data =
                unsafe { core::slice::from_raw_parts((phys_off + z_phys) as *const u8, 4096) };
            assert!(
                data.iter().all(|&b| b == 0),
                "round {}: stale reuse leak at frame {:#x} (pattern {:#x})",
                round,
                z_phys,
                pattern
            );
            frame_allocator::free_frame(z_phys);
        }
    }

    /// D.4: map_user_pages end-to-end — exercises the real `map_user_pages`
    /// function (which calls `allocate_frame_zeroed` internally).  Poisons
    /// frames first so that any failure to zero would leave stale data.
    /// Verifies the mapped physical frames are clean via the physical offset.
    #[test_case]
    fn zero_exposure_map_user_pages_e2e() {
        use crate::mm::frame_allocator;
        use x86_64::structures::paging::{Mapper, PageTableFlags, Translate};
        let phys_off = crate::mm::phys_offset();

        // Poison 4 frames and return them to the pool.
        for pattern in [0xCC_u8, 0xDD, 0xEE, 0xFF] {
            let f = frame_allocator::allocate_frame().expect("alloc poison");
            let phys = f.start_address().as_u64();
            unsafe { core::ptr::write_bytes((phys_off + phys) as *mut u8, pattern, 4096) };
            frame_allocator::free_frame(phys);
        }

        // Call the real map_user_pages which allocates via allocate_frame_zeroed.
        const TEST_VBASE: u64 = 0x0000_7FFE_0000_0000;
        const N_PAGES: u64 = 4;
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;

        let mut mapper = unsafe { crate::mm::paging::get_mapper() };
        unsafe {
            crate::mm::user_space::map_user_pages(&mut mapper, TEST_VBASE, N_PAGES, flags)
                .expect("map_user_pages failed");
        }

        // Read back each mapped frame via physical offset and verify zero.
        let mut frame_addrs = [0u64; N_PAGES as usize];
        for i in 0..N_PAGES {
            let vaddr = x86_64::VirtAddr::new(TEST_VBASE + i * 4096);
            let paddr = mapper
                .translate_addr(vaddr)
                .expect("page not mapped after map_user_pages");
            frame_addrs[i as usize] = paddr.as_u64() & !0xFFF;
            let data = unsafe {
                core::slice::from_raw_parts((phys_off + frame_addrs[i as usize]) as *const u8, 4096)
            };
            assert!(
                data.iter().all(|&b| b == 0),
                "map_user_pages: stale data in page {} (frame {:#x})",
                i,
                frame_addrs[i as usize]
            );
        }

        // Cleanup: unmap and free.
        for i in 0..N_PAGES {
            let vaddr = x86_64::VirtAddr::new(TEST_VBASE + i * 4096);
            let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(vaddr);
            if let Ok((_frame, flush)) = mapper.unmap(page) {
                flush.flush();
            }
            frame_allocator::free_frame(frame_addrs[i as usize]);
        }
    }

    /// D.4: resolve_cow_fault end-to-end — sets up a real CoW-marked page in
    /// the current address space, calls the real `resolve_cow_fault`, and
    /// verifies that the new frame contains the parent's data with no stale
    /// content leakage.
    #[test_case]
    fn zero_exposure_resolve_cow_e2e() {
        use crate::mm::frame_allocator;
        use x86_64::structures::paging::{Mapper, PageTableFlags, Translate};
        let phys_off = crate::mm::phys_offset();

        // Poison a frame and return it so the pool has stale data for the CoW
        // destination.
        let stale = frame_allocator::allocate_frame().expect("alloc stale");
        let stale_phys = stale.start_address().as_u64();
        unsafe { core::ptr::write_bytes((phys_off + stale_phys) as *mut u8, 0xBE, 4096) };
        frame_allocator::free_frame(stale_phys);

        // Allocate the "parent" frame and fill with a distinguishable pattern.
        let parent = frame_allocator::allocate_frame().expect("alloc parent");
        let parent_phys = parent.start_address().as_u64();
        let parent_ptr = (phys_off + parent_phys) as *mut u8;
        for i in 0u16..4096 {
            unsafe { parent_ptr.add(i as usize).write((i & 0xFF) as u8) };
        }

        // Bump refcount to 2 so resolve_cow_fault takes the copy path.
        frame_allocator::refcount_inc(parent_phys);

        // Map the parent frame at a test user-space address with CoW flags:
        // PRESENT | USER_ACCESSIBLE | BIT_9 (CoW marker) | !WRITABLE.
        const COW_TEST_VADDR: u64 = 0x0000_7FFD_0000_0000;
        let cow_flags = PageTableFlags::PRESENT
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::BIT_9;
        let vaddr = x86_64::VirtAddr::new(COW_TEST_VADDR);
        unsafe {
            crate::mm::paging::map_current_user_page_locked(vaddr, parent, cow_flags)
                .expect("map CoW page failed");
        }

        // Call the real resolve_cow_fault — this allocates a new frame, copies
        // parent data, and remaps the PTE as writable.
        let resolved = crate::arch::x86_64::interrupts::resolve_cow_fault(COW_TEST_VADDR);
        assert!(resolved, "resolve_cow_fault returned false");

        // Find the new physical frame via translation.
        let mapper = unsafe { crate::mm::paging::get_mapper() };
        let new_paddr = mapper
            .translate_addr(vaddr)
            .expect("page not mapped after resolve_cow_fault");
        let new_phys = new_paddr.as_u64() & !0xFFF;

        // The new frame must differ from the parent (a copy was made).
        assert_ne!(
            new_phys, parent_phys,
            "resolve_cow_fault should have allocated a new frame"
        );

        // Verify every byte in the new frame matches the parent's pattern.
        let new_data =
            unsafe { core::slice::from_raw_parts((phys_off + new_phys) as *const u8, 4096) };
        for (i, &byte) in new_data.iter().enumerate() {
            let expected = (i & 0xFF) as u8;
            assert_eq!(
                byte, expected,
                "CoW copy mismatch at offset {}: got {:#x}, expected {:#x} (new frame {:#x})",
                i, byte, expected, new_phys
            );
        }

        // Cleanup: unmap the page and free both frames.
        let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(vaddr);
        drop(mapper);
        let mut mapper = unsafe { crate::mm::paging::get_mapper() };
        if let Ok((_f, flush)) = mapper.unmap(page) {
            flush.flush();
        }
        frame_allocator::free_frame(new_phys);
        // Parent frame had refcount bumped to 2; resolve_cow_fault decremented
        // to 1 via free_frame.  Decrement once more to actually free.
        frame_allocator::free_frame(parent_phys);
    }

    /// D.4: munmap + reuse — after freeing a batch of dirty frames
    /// (simulating munmap), every subsequent zeroed allocation must be clean.
    #[test_case]
    fn zero_exposure_munmap_reuse_batch() {
        use crate::mm::frame_allocator;
        let phys_off = crate::mm::phys_offset();

        const BATCH: usize = 8;
        let mut freed_addrs = [0u64; BATCH];

        // Allocate BATCH frames, poison each with a distinct pattern, free all.
        for (i, slot) in freed_addrs.iter_mut().enumerate() {
            let f = frame_allocator::allocate_frame().expect("alloc batch");
            let phys = f.start_address().as_u64();
            unsafe {
                core::ptr::write_bytes((phys_off + phys) as *mut u8, (0xA0 + i as u8), 4096);
            }
            *slot = phys;
        }
        for &phys in &freed_addrs {
            frame_allocator::free_frame(phys);
        }

        // Re-allocate BATCH frames via the zeroed path and verify each.
        for i in 0..BATCH {
            let z = frame_allocator::allocate_frame_zeroed().expect("alloc zeroed batch");
            let z_phys = z.start_address().as_u64();
            let data =
                unsafe { core::slice::from_raw_parts((phys_off + z_phys) as *const u8, 4096) };
            assert!(
                data.iter().all(|&b| b == 0),
                "munmap reuse batch[{}]: stale data at frame {:#x}",
                i,
                z_phys
            );
            frame_allocator::free_frame(z_phys);
        }
    }

    /// D.4: Contiguous-block zeroed allocation — multi-page allocations
    /// via allocate_contiguous_zeroed must zero every page in the block,
    /// even when backing frames previously held data.
    #[test_case]
    fn zero_exposure_contiguous_zeroed() {
        use crate::mm::frame_allocator;
        let phys_off = crate::mm::phys_offset();
        let page_size = 4096u64;

        // Allocate and poison a 4-page contiguous block (order 2), then free it.
        let dirty = frame_allocator::allocate_contiguous(2).expect("alloc dirty contig");
        let base = dirty.start_address().as_u64();
        for i in 0..4u64 {
            unsafe {
                core::ptr::write_bytes(
                    (phys_off + base + i * page_size) as *mut u8,
                    0xFE,
                    page_size as usize,
                );
            }
        }
        frame_allocator::free_contiguous(base, 2);

        // Re-allocate via the zeroed path.
        let zeroed = frame_allocator::allocate_contiguous_zeroed(2).expect("alloc zeroed contig");
        let z_base = zeroed.start_address().as_u64();
        for i in 0..4u64 {
            let data = unsafe {
                core::slice::from_raw_parts(
                    (phys_off + z_base + i * page_size) as *const u8,
                    page_size as usize,
                )
            };
            assert!(
                data.iter().all(|&b| b == 0),
                "contiguous zeroed: stale data in page {} of block at {:#x}",
                i,
                z_base
            );
        }
        frame_allocator::free_contiguous(z_base, 2);
    }

    /// C: Verify slab cache allocation and deallocation.
    #[test_case]
    fn slab_cache_alloc_free() {
        let caches = crate::mm::slab::caches();
        let mut fd_cache = caches.fd_cache.lock();

        let stats_before = fd_cache.stats();
        let mut page_counter = 0usize;

        // Allocate 10 objects from the FD cache (64-byte slots).
        let mut addrs = alloc::vec::Vec::new();
        for _ in 0..10 {
            let addr = fd_cache
                .allocate(&mut || {
                    // Page allocator callback: use frame allocator.
                    let frame = crate::mm::frame_allocator::allocate_frame()?;
                    page_counter += 1;
                    Some((crate::mm::phys_offset() + frame.start_address().as_u64()) as usize)
                })
                .expect("slab alloc failed");
            addrs.push(addr);
        }

        let stats_during = fd_cache.stats();
        assert_eq!(
            stats_during.active_objects,
            stats_before.active_objects + 10
        );

        // Free all objects.
        for addr in addrs {
            fd_cache.free(addr as usize);
        }

        let stats_after = fd_cache.stats();
        assert_eq!(stats_after.active_objects, stats_before.active_objects);
        serial_println!(
            "slab test: allocated {} objects using {} page(s)",
            10,
            page_counter
        );
    }

    /// Phase 60 Track C — Heap-relief diagnostic.
    ///
    /// Snapshots `task_cache` / `xsave_cache` activity once the test
    /// scheduler has spawned its workload so the serial output from
    /// `cargo xtask test` carries a verbatim record for the Phase 60
    /// measurement handoff (`docs/handoffs/60c-slab-heap-measurement.md`).
    #[test_case]
    fn phase60_heap_relief_stats_dump() {
        let caches = crate::mm::slab::caches();
        let task_stats = caches.task_cache.lock().stats();
        let xsave_stats = caches.xsave_cache.lock().stats();
        serial_println!(
            "[phase60] task_cache slabs={} active={} free={} | xsave_cache slabs={} active={} free={}",
            task_stats.total_slabs,
            task_stats.active_objects,
            task_stats.free_slots,
            xsave_stats.total_slabs,
            xsave_stats.active_objects,
            xsave_stats.free_slots,
        );
    }

    /// Phase 60 B.2 — Verify the new `xsave_cache` slot allocates and frees.
    ///
    /// Independent of the `task_cache` migration: the production
    /// `xsave_cache` is exercised on every task spawn via
    /// `alloc_task_slot`, but this smoke test confirms the named-cache
    /// API remains usable at the slab level after the new member was added
    /// to `KernelSlabCaches`.
    #[test_case]
    fn xsave_slab_cache_alloc_free() {
        let caches = crate::mm::slab::caches();
        let mut xsave_cache = caches.xsave_cache.lock();

        let stats_before = xsave_cache.stats();

        // Allocate 4 objects (one full page worth at 832-byte slots — page
        // is 4096, so 4096/832 = 4 slots per page with leftover padding).
        let mut addrs = alloc::vec::Vec::new();
        for _ in 0..4 {
            let addr = xsave_cache
                .allocate(&mut || {
                    let frame = crate::mm::frame_allocator::allocate_frame()?;
                    Some((crate::mm::phys_offset() + frame.start_address().as_u64()) as usize)
                })
                .expect("xsave_cache alloc failed");
            addrs.push(addr);
        }

        let stats_during = xsave_cache.stats();
        assert_eq!(stats_during.active_objects, stats_before.active_objects + 4);

        for addr in addrs {
            xsave_cache.free(addr as usize);
        }

        let stats_after = xsave_cache.stats();
        assert_eq!(stats_after.active_objects, stats_before.active_objects);
        serial_println!(
            "xsave slab test: allocated {} objects (slot size {} bytes)",
            4,
            crate::mm::slab::XSAVE_CACHE_SLOT_SIZE,
        );
    }

    /// F: Verify frame statistics are consistent.
    #[test_case]
    fn frame_stats_consistent() {
        let stats = crate::mm::frame_allocator::frame_stats();

        assert!(stats.total_frames > 0, "no frames reported");

        // Linux-like accounting: total = available + allocated,
        // where available = free (buddy) + per_cpu_cached.
        assert_eq!(
            stats.total_frames,
            stats.available_frames + stats.allocated_frames,
            "frame count mismatch: total={} available={} alloc={}",
            stats.total_frames,
            stats.available_frames,
            stats.allocated_frames
        );
        assert_eq!(
            stats.available_frames,
            stats.free_frames + stats.per_cpu_cached,
            "available mismatch: available={} free={} cached={}",
            stats.available_frames,
            stats.free_frames,
            stats.per_cpu_cached
        );

        // Per-order free counts should sum to free_frames (buddy-only, no per-CPU).
        let order_sum: usize = stats
            .free_by_order
            .iter()
            .enumerate()
            .map(|(order, &count)| count * (1 << order))
            .sum();
        assert_eq!(
            order_sum, stats.free_frames,
            "buddy order sum ({}) != free_frames ({})",
            order_sum, stats.free_frames
        );
        serial_println!(
            "frame stats: total={} free={} available={} allocated={} per_cpu_cached={}",
            stats.total_frames,
            stats.free_frames,
            stats.available_frames,
            stats.allocated_frames,
            stats.per_cpu_cached
        );
    }

    /// F: Verify meminfo syscall returns non-empty data (heap stats).
    #[test_case]
    fn heap_stats_nonzero() {
        let stats = crate::mm::heap::heap_stats();
        assert!(stats.total_size > 0, "heap total_size is 0");
        assert!(stats.alloc_count > 0, "no allocations recorded");
        assert!(
            stats.total_size >= stats.free_bytes,
            "free > total: free={} total={}",
            stats.free_bytes,
            stats.total_size
        );
    }

    // -----------------------------------------------------------------------
    // Phase 55b Track B.1 — sys_device_claim integration tests
    // -----------------------------------------------------------------------
    //
    // These run in the pre-`kernel_main` test harness, so `test_main()` is
    // invoked before `pci::init()`. The test forces PCI enumeration itself
    // so that a real BDF is available for the claim path. The assertions
    // cover first-claim success, duplicate-claim returns `Busy`, and
    // release re-opens the slot for a new PID.

    /// Track B.1: first claim succeeds; second claim on the same BDF by a
    /// different PID returns `Busy`; releasing for the owning PID restores
    /// the slot so a third PID can claim it.
    ///
    /// Cross-references the pure-logic assertions in
    /// `kernel_core::device_host::registry_logic::tests` — this test adds
    /// the kernel-side invariant that the `PciDeviceHandle` (and its
    /// IOMMU domain) round-trips through the registry correctly.
    #[test_case]
    fn device_host_claim_first_succeeds_duplicate_returns_busy() {
        use crate::syscall::device_host::{
            TestClaimError, test_owner_of, test_release_for_pid, test_try_claim_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        // Ensure the PCI bus has been scanned so a real device is available.
        // `pci::init` is idempotent on repeat calls — the second scan finds
        // the already-populated static list and logs the same devices.
        crate::pci::init();

        // Find the first unclaimed device so the test stays decoupled from
        // whatever QEMU happens to attach. If QEMU produces no PCI device
        // at all (very unusual), skip the test rather than fail.
        let mut key: Option<DeviceCapKey> = None;
        let mut idx = 0;
        while let Some(dev) = crate::pci::pci_device(idx) {
            let k = DeviceCapKey::new(0, dev.bus, dev.device, dev.function);
            if test_owner_of(k).is_none() {
                // Also check that it's not already claimed by an in-kernel
                // driver — if it is, claim_pci_device_by_bdf would return
                // `AlreadyClaimed` which the test would interpret as Busy.
                key = Some(k);
                break;
            }
            idx += 1;
        }
        let Some(key) = key else {
            serial_println!("device_host test skipped: no free PCI device in QEMU");
            return;
        };

        serial_println!(
            "device_host test using BDF {:04x}:{:02x}:{:02x}.{}",
            key.segment,
            key.bus,
            key.dev,
            key.func
        );

        // Use PID values in a range the kernel does not actually schedule —
        // current_pid() for the test runner is 0, so picking high sentinels
        // avoids any collision with real PIDs.
        const PID_A: crate::process::Pid = 0xC0FF_EE01;
        const PID_B: crate::process::Pid = 0xC0FF_EE02;
        const PID_C: crate::process::Pid = 0xC0FF_EE03;

        // Pre-clean in case a prior test left state (should not happen, but
        // defensive since the registry is a static global).
        let _ = test_release_for_pid(PID_A);
        let _ = test_release_for_pid(PID_B);
        let _ = test_release_for_pid(PID_C);

        // 1) First claim succeeds, recorded under PID_A.
        match test_try_claim_for_pid(PID_A, key) {
            Ok(()) => {}
            Err(e) => {
                // `AlreadyClaimed` here means an in-kernel driver beat us
                // to the slot during the pre-scan race — skip gracefully.
                if matches!(e, TestClaimError::Busy) {
                    serial_println!(
                        "device_host test skipped: BDF {:02x}:{:02x}.{} already claimed in kernel",
                        key.bus,
                        key.dev,
                        key.func,
                    );
                    return;
                }
                panic!(
                    "first claim failed unexpectedly: {:?} for BDF {:02x}:{:02x}.{}",
                    e, key.bus, key.dev, key.func
                );
            }
        }
        assert_eq!(
            test_owner_of(key),
            Some(PID_A),
            "ownership should track PID_A after first claim",
        );

        // 2) A second claim on the same BDF — whether by PID_A or PID_B —
        //    returns Busy. B.1 acceptance race: "exactly one succeeds".
        assert_eq!(
            test_try_claim_for_pid(PID_A, key),
            Err(TestClaimError::Busy),
            "same-PID duplicate claim must be Busy",
        );
        assert_eq!(
            test_try_claim_for_pid(PID_B, key),
            Err(TestClaimError::Busy),
            "cross-PID duplicate claim must be Busy",
        );
        assert_eq!(
            test_owner_of(key),
            Some(PID_A),
            "original owner's claim must survive the duplicate attempt",
        );

        // 3) PID_A exits (simulate via release_for_pid). Slot is now free
        //    and a fresh PID_C can claim it — this is the Phase 46 / 51
        //    supervisor-restart path exercised at the registry level.
        let freed = test_release_for_pid(PID_A);
        assert_eq!(freed, 1, "release_for_pid must free exactly one entry");
        assert_eq!(test_owner_of(key), None, "slot must be free after release");

        match test_try_claim_for_pid(PID_C, key) {
            Ok(()) => {}
            Err(e) => panic!("reclaim by PID_C failed: {:?}", e),
        }
        assert_eq!(test_owner_of(key), Some(PID_C));

        // 4) Double-release of an already-released PID must not panic; it
        //    returns zero freed slots (tests the -EBADF acceptance clause
        //    at the registry level).
        let double = test_release_for_pid(PID_A);
        assert_eq!(double, 0, "double-release must be safe and return 0");

        // Cleanup for a tidy global registry — the next test in the suite
        // should see the state it started with.
        let _ = test_release_for_pid(PID_C);
        assert_eq!(test_owner_of(key), None);

        serial_println!("device_host B.1 integration test passed");
    }

    // -----------------------------------------------------------------------
    // Phase 55b Tracks B.2 / B.3 / B.4 — device-host syscall integration tests
    // -----------------------------------------------------------------------

    /// Pick a free PCI BDF for the test. Returns `None` when no free device
    /// is available (test is skipped in that case).
    #[cfg(test)]
    fn pick_free_pci_bdf() -> Option<kernel_core::device_host::DeviceCapKey> {
        use crate::syscall::device_host::test_owner_of;
        use kernel_core::device_host::DeviceCapKey;
        crate::pci::init();
        let mut idx = 0;
        while let Some(dev) = crate::pci::pci_device(idx) {
            let k = DeviceCapKey::new(0, dev.bus, dev.device, dev.function);
            if test_owner_of(k).is_none() {
                return Some(k);
            }
            idx += 1;
        }
        None
    }

    // -- Track B.2 — sys_device_mmio_map integration tests -------------------

    /// Track B.2: recording an MMIO mapping under a claimed device, then
    /// calling `test_release_for_pid`, must clear both the claim slot and
    /// the MMIO registry entries it owned.
    #[test_case]
    fn device_host_mmio_release_cascades_to_mmio_entries() {
        use crate::syscall::device_host::{
            TestClaimError, test_mmio_count_for_pid, test_owner_of, test_record_mmio,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("device_host B.2 cascade test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE10;
        let _ = test_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "device_host B.2 cascade test skipped: BDF already in use by kernel driver"
                );
                return;
            }
            Err(e) => panic!("claim failed: {:?}", e),
        }

        // Record two MMIO entries under the same device — mimics a driver
        // that maps BAR0 and BAR2. Neither needs a real page table for the
        // cascade assertion.
        test_record_mmio(PID, key, 0, 0x1000, 0xdead_0000).expect("BAR0 mmio recorded");
        test_record_mmio(PID, key, 2, 0x2000, 0xdead_2000).expect("BAR2 mmio recorded");
        assert_eq!(
            test_mmio_count_for_pid(PID),
            2,
            "two MMIO entries should be present after recording"
        );

        // Release the claim — cleanup cascade must wipe both MMIO entries.
        let freed = test_release_for_pid(PID);
        assert_eq!(freed, 1, "expected 1 claim released");
        assert_eq!(
            test_mmio_count_for_pid(PID),
            0,
            "MMIO entries must be cleared by the cascade",
        );
        assert_eq!(test_owner_of(key), None);

        serial_println!("device_host B.2 cascade test passed");
    }

    /// Track B.2: the 33rd MMIO-map request against a single device-cap
    /// returns `CapacityExceeded` without corrupting the registry.
    #[test_case]
    fn device_host_mmio_capacity_cap_is_enforced() {
        use crate::syscall::device_host::{
            MAX_MMIO_PER_DEVICE, TestClaimError, TestMmioError, test_mmio_count_for_pid,
            test_record_mmio, test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("device_host B.2 capacity test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE11;
        let _ = test_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "device_host B.2 capacity test skipped: BDF already in use by kernel driver"
                );
                return;
            }
            Err(e) => panic!("claim failed: {:?}", e),
        }

        // Fill the per-device MMIO slot cap. BAR indices wrap 0..6 to stay
        // valid — the registry key is (pid, key, bar_index, user_va), so
        // the synthetic `user_va` values keep entries distinct.
        for i in 0..MAX_MMIO_PER_DEVICE {
            let bar_index = (i % 6) as u8;
            let user_va = 0xdead_0000 + (i as u64) * 0x1000;
            test_record_mmio(PID, key, bar_index, 0x1000, user_va)
                .unwrap_or_else(|e| panic!("record {i} failed: {:?}", e));
        }
        assert_eq!(test_mmio_count_for_pid(PID), MAX_MMIO_PER_DEVICE);

        // One more should be rejected with CapacityExceeded.
        let one_over = test_record_mmio(PID, key, 0, 0x1000, 0xbeef_0000);
        assert_eq!(one_over, Err(TestMmioError::CapacityExceeded));
        // Registry unchanged.
        assert_eq!(test_mmio_count_for_pid(PID), MAX_MMIO_PER_DEVICE);

        let freed = test_release_for_pid(PID);
        assert_eq!(freed, 1);
        assert_eq!(test_mmio_count_for_pid(PID), 0);

        serial_println!("device_host B.2 capacity test passed");
    }

    /// Track B.2: MMIO entry recorded against a device not claimed by the
    /// caller returns a `NotClaimed` error. This is the registry-level
    /// analogue of the cross-device negative test in F.3.
    #[test_case]
    fn device_host_mmio_record_without_claim_fails() {
        use crate::syscall::device_host::{TestMmioError, test_record_mmio, test_release_for_pid};
        use kernel_core::device_host::DeviceCapKey;

        // Use a deliberately-bogus BDF that no real PCI device should occupy
        // (b:d.f = FF:1F.7 on segment 0xFFFF).
        let key = DeviceCapKey::new(0xFFFF, 0xFF, 0x1F, 7);

        const PID: crate::process::Pid = 0xC0FF_EE12;
        let _ = test_release_for_pid(PID);

        let err = test_record_mmio(PID, key, 0, 0x1000, 0xdead_3000);
        assert_eq!(
            err,
            Err(TestMmioError::NotClaimed),
            "recording MMIO without a prior claim must fail with NotClaimed",
        );

        serial_println!("device_host B.2 no-claim test passed");
    }

    /// Track B.2: pure-logic bounds checks are host-tested in `kernel-core`,
    /// but this smoke test asserts the re-export surface is reachable from
    /// the kernel crate so downstream drivers see the same API.
    #[test_case]
    fn device_host_mmio_bounds_helpers_reachable_from_kernel() {
        use kernel_core::device_host::{
            MAX_MMIO_BAR_BYTES, MmioBoundsError, MmioCacheMode, build_mmio_window,
            cache_mode_for_bar, validate_mmio_bar_size,
        };

        assert_eq!(
            validate_mmio_bar_size(6, 0x1000),
            Err(MmioBoundsError::BarIndexOutOfRange)
        );
        assert_eq!(
            validate_mmio_bar_size(0, 0),
            Err(MmioBoundsError::ZeroSizedBar)
        );
        assert_eq!(
            validate_mmio_bar_size(0, MAX_MMIO_BAR_BYTES + 1),
            Err(MmioBoundsError::BarTooLarge),
        );
        assert_eq!(cache_mode_for_bar(true), MmioCacheMode::WriteCombining);
        assert_eq!(cache_mode_for_bar(false), MmioCacheMode::Uncacheable);

        let desc = build_mmio_window(0, 0xfebf_0000, 0x1000, false).expect("valid BAR");
        assert_eq!(desc.len, 0x1000);
        assert_eq!(desc.cache_mode, MmioCacheMode::Uncacheable);

        serial_println!("device_host B.2 bounds-reexport test passed");
    }

    // -- Track B.3 — sys_device_dma_alloc integration tests ------------------

    /// B.3: sys_device_dma_alloc returns a (user_va, iova, len) handle whose
    /// views of the backing frame are consistent — write via user_va, read
    /// via iova-equivalent kernel view, get the same byte.
    #[test_case]
    fn device_host_dma_alloc_yields_consistent_user_and_iova_views() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_count, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("B.3 dma_alloc test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE40;
        let _ = test_release_for_pid(PID);
        let _ = test_dma_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "B.3 dma_alloc test skipped: BDF {:02x}:{:02x}.{} busy",
                    key.bus,
                    key.dev,
                    key.func,
                );
                return;
            }
            Err(e) => panic!("unexpected claim error: {:?}", e),
        }

        let before = test_dma_count();
        let snap = test_dma_alloc_for_pid(PID, key, 4096, 4096)
            .expect("dma_alloc must succeed for a claimed device");
        assert_eq!(snap.len, 4096, "len must be rounded to the request");
        assert_ne!(snap.iova, 0, "iova must be non-zero");
        assert_ne!(snap.user_va, 0, "user_va must be non-zero");
        assert_eq!(
            test_dma_count(),
            before + 1,
            "registry must record the new allocation"
        );

        let sentinel: u8 = 0xA5;
        unsafe {
            core::ptr::write_volatile(snap.user_va as *mut u8, sentinel);
        }

        let kvirt_of_iova = (crate::mm::phys_offset() + snap.iova) as *const u8;
        let read_back = unsafe { core::ptr::read_volatile(kvirt_of_iova) };
        assert_eq!(
            read_back, sentinel,
            "user VA and IOVA must alias the same frame",
        );

        let flip: u8 = 0x5A;
        unsafe {
            core::ptr::write_volatile(kvirt_of_iova as *mut u8, flip);
        }
        let read_back_user = unsafe { core::ptr::read_volatile(snap.user_va as *const u8) };
        assert_eq!(
            read_back_user, flip,
            "IOVA-view write must be visible through user VA",
        );

        let freed = test_dma_release_for_pid(PID);
        assert_eq!(freed, 1, "release must free exactly one allocation");
        let _ = test_release_for_pid(PID);
        serial_println!("device_host B.3 dma_alloc integration test passed");
    }

    /// B.3: handle-info returns the registered `(user_va, iova, len)` triple
    /// verbatim.
    #[test_case]
    fn device_host_dma_handle_info_returns_registered_triple() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_handle_info, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("B.3 handle_info test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE41;
        let _ = test_release_for_pid(PID);
        let _ = test_dma_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => return,
            Err(e) => panic!("claim failed: {:?}", e),
        }

        let alloc_snap = test_dma_alloc_for_pid(PID, key, 8192, 0).expect("dma_alloc must succeed");
        let info_snap = test_dma_handle_info(PID, alloc_snap.id)
            .expect("handle_info must find the live allocation");
        assert_eq!(alloc_snap, info_snap);

        const OTHER: crate::process::Pid = 0xC0FF_EE42;
        assert!(test_dma_handle_info(OTHER, alloc_snap.id).is_none());

        let _ = test_dma_release_for_pid(PID);
        let _ = test_release_for_pid(PID);
        serial_println!("device_host B.3 handle_info integration test passed");
    }

    /// B.3: dma_alloc against a non-claimed BDF returns NoDevice.
    #[test_case]
    fn device_host_dma_alloc_rejects_unclaimed_device() {
        use crate::syscall::device_host::{TestDmaError, test_dma_alloc_for_pid};
        use kernel_core::device_host::DeviceCapKey;

        let key = DeviceCapKey::new(0, 0xFF, 0x1F, 7);
        const PID: crate::process::Pid = 0xC0FF_EE43;
        let err = test_dma_alloc_for_pid(PID, key, 4096, 4096)
            .expect_err("alloc must fail without a prior claim");
        assert_eq!(err, TestDmaError::NoDevice);
    }

    /// B.3: allocation-rollback discipline — bad size returns InvalidArg and
    /// leaves no state in the registry.
    #[test_case]
    fn device_host_dma_alloc_rollback_on_validation_error() {
        use crate::syscall::device_host::{
            TestClaimError, TestDmaError, test_dma_alloc_for_pid, test_dma_count,
            test_dma_release_for_pid, test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("B.3 rollback test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE44;
        let _ = test_release_for_pid(PID);
        let _ = test_dma_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => return,
            Err(e) => panic!("claim failed: {:?}", e),
        }

        let before_count = test_dma_count();
        let before_frames = crate::mm::frame_allocator::available_count();

        assert_eq!(
            test_dma_alloc_for_pid(PID, key, 0, 4096),
            Err(TestDmaError::InvalidArg)
        );
        assert_eq!(
            test_dma_alloc_for_pid(PID, key, 4096, 3),
            Err(TestDmaError::InvalidArg)
        );
        assert_eq!(
            test_dma_alloc_for_pid(PID, key, 4096, 8192),
            Err(TestDmaError::InvalidArg)
        );

        assert_eq!(test_dma_count(), before_count, "no registry entries added");
        crate::mm::frame_allocator::drain_per_cpu_caches();
        let after_frames = crate::mm::frame_allocator::available_count();
        assert_eq!(
            after_frames, before_frames,
            "no frames leaked on validation error (before={} after={})",
            before_frames, after_frames,
        );

        let _ = test_release_for_pid(PID);
        serial_println!("device_host B.3 rollback integration test passed");
    }

    /// B.3: cross-device negative — two distinct BDFs each get their own
    /// DMA allocation; a driver cannot introspect another driver's handle.
    #[test_case]
    fn device_host_dma_alloc_cross_device_is_independent() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_handle_info, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        crate::pci::init();
        let mut keys: alloc::vec::Vec<DeviceCapKey> = alloc::vec::Vec::new();
        let mut idx = 0;
        while let Some(dev) = crate::pci::pci_device(idx) {
            let k = DeviceCapKey::new(0, dev.bus, dev.device, dev.function);
            if crate::syscall::device_host::test_owner_of(k).is_none() {
                keys.push(k);
                if keys.len() == 2 {
                    break;
                }
            }
            idx += 1;
        }
        if keys.len() < 2 {
            serial_println!("B.3 cross-device test skipped: <2 free PCI devices");
            return;
        }
        let key_a = keys[0];
        let key_b = keys[1];

        const PID_A: crate::process::Pid = 0xC0FF_EE50;
        const PID_B: crate::process::Pid = 0xC0FF_EE51;
        let _ = test_release_for_pid(PID_A);
        let _ = test_release_for_pid(PID_B);
        let _ = test_dma_release_for_pid(PID_A);
        let _ = test_dma_release_for_pid(PID_B);

        if test_try_claim_for_pid(PID_A, key_a).is_err() {
            return;
        }
        if test_try_claim_for_pid(PID_B, key_b).is_err() {
            let _ = test_release_for_pid(PID_A);
            return;
        }

        let snap_a =
            test_dma_alloc_for_pid(PID_A, key_a, 4096, 4096).expect("PID_A dma_alloc on key_a");
        let snap_b =
            test_dma_alloc_for_pid(PID_B, key_b, 4096, 4096).expect("PID_B dma_alloc on key_b");

        assert_ne!(snap_a.id, snap_b.id);

        assert!(
            test_dma_handle_info(PID_A, snap_b.id).is_none(),
            "PID_A must not observe PID_B's allocation"
        );
        assert!(
            test_dma_handle_info(PID_B, snap_a.id).is_none(),
            "PID_B must not observe PID_A's allocation"
        );

        assert!(test_dma_handle_info(PID_A, snap_a.id).is_some());
        assert!(test_dma_handle_info(PID_B, snap_b.id).is_some());

        let _ = test_dma_release_for_pid(PID_A);
        let _ = test_dma_release_for_pid(PID_B);
        let _ = test_release_for_pid(PID_A);
        let _ = test_release_for_pid(PID_B);
        serial_println!("device_host B.3 cross-device test passed");
    }

    /// B.3: process-exit cleanup — every live DMA entry owned by the exiting
    /// PID is freed (registry entry gone, frames returned to buddy).
    #[test_case]
    fn device_host_dma_release_on_exit_is_clean() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_count, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("B.3 on-exit cleanup test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE60;
        let _ = test_release_for_pid(PID);
        let _ = test_dma_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => return,
            Err(e) => panic!("claim failed: {:?}", e),
        }

        crate::mm::frame_allocator::drain_per_cpu_caches();
        let frames_before = crate::mm::frame_allocator::available_count();

        let _ = test_dma_alloc_for_pid(PID, key, 4096, 4096).expect("alloc 1");
        let _ = test_dma_alloc_for_pid(PID, key, 8192, 4096).expect("alloc 2");
        let _ = test_dma_alloc_for_pid(PID, key, 4096, 4096).expect("alloc 3");
        assert_eq!(test_dma_count(), 3, "three live allocations");

        let freed = test_dma_release_for_pid(PID);
        assert_eq!(freed, 3, "release_for_pid freed all allocations");
        assert_eq!(test_dma_count(), 0, "registry empty after release");

        crate::mm::frame_allocator::drain_per_cpu_caches();
        let frames_after = crate::mm::frame_allocator::available_count();
        assert_eq!(
            frames_after, frames_before,
            "all DMA frames must be returned to the buddy allocator \
             (before={} after={})",
            frames_before, frames_after,
        );

        let _ = test_release_for_pid(PID);
        serial_println!("device_host B.3 on-exit cleanup integration test passed");
    }

    // -- Track B.4 — sys_device_irq_subscribe integration test ---------------

    /// Track B.4: a synthetic device IRQ delivered through the device-IRQ
    /// dispatch table (the same path a real MSI vector would take) sets the
    /// requested bit atomically on the bound notification. `release_for_pid`
    /// tears the binding down so the vector is reusable.
    #[test_case]
    fn device_host_irq_subscribe_signals_notification_bit() {
        use crate::syscall::device_host::{
            TestClaimError, test_release_for_pid, test_synthetic_irq_subscribe_and_signal,
            test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("device_host B.4 test skipped: no free PCI device in QEMU");
            return;
        };

        const PID_D: crate::process::Pid = 0xC0FF_EE04;
        let _ = test_release_for_pid(PID_D);

        match test_try_claim_for_pid(PID_D, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "device_host B.4 test skipped: BDF {:02x}:{:02x}.{} already claimed",
                    key.bus,
                    key.dev,
                    key.func,
                );
                return;
            }
            Err(e) => panic!("B.4 claim failed: {:?}", e),
        }

        // Bind bit 3 to vector offset 0.
        let pending = match test_synthetic_irq_subscribe_and_signal(PID_D, key, 3, 0) {
            Ok(p) => p,
            Err(e) => panic!("B.4 synthetic bind/signal failed: {:?}", e),
        };
        assert_eq!(
            pending,
            1u64 << 3,
            "ISR shim must have set exactly bit 3 on the bound notification (got {:#x})",
            pending,
        );

        // Re-arm with a different bit/vector.
        let pending_bit7 = match test_synthetic_irq_subscribe_and_signal(PID_D, key, 7, 1) {
            Ok(p) => p,
            Err(e) => panic!("B.4 second synthetic bind failed: {:?}", e),
        };
        assert_eq!(pending_bit7, 1u64 << 7);

        let freed = test_release_for_pid(PID_D);
        assert_eq!(freed, 1, "exactly one claim freed on exit");

        serial_println!("device_host B.4 integration test passed");
    }

    // -- Track B.4b — caller-provided NotifId path ----------------------------

    /// Track B.4b: the caller-provided notification path.
    ///
    /// The test pre-allocates a `Notification`, passes it to the synthetic
    /// IRQ bind helper (simulating what `sys_device_irq_subscribe` does when
    /// `notification_arg != SENTINEL_NEW`), verifies the ISR shim delivers
    /// to the correct bit, and confirms that the process-exit teardown does
    /// NOT free the caller-owned notification slot (pool count unchanged after
    /// the binding is torn down).
    #[test_case]
    fn device_host_irq_subscribe_caller_provided_notif() {
        use crate::syscall::device_host::{
            TestClaimError, test_release_for_pid,
            test_synthetic_irq_subscribe_and_signal_with_existing_notif, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("device_host B.4b test skipped: no free PCI device in QEMU");
            return;
        };

        const PID_E: crate::process::Pid = 0xC0FF_EE05;
        let _ = test_release_for_pid(PID_E);

        match test_try_claim_for_pid(PID_E, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "device_host B.4b test skipped: BDF {:02x}:{:02x}.{} already claimed",
                    key.bus,
                    key.dev,
                    key.func,
                );
                return;
            }
            Err(e) => panic!("B.4b claim failed: {:?}", e),
        }

        // Pre-allocate a notification the "caller" owns.
        let caller_notif = crate::ipc::notification::try_create()
            .expect("notification pool must have a free slot");
        let pool_before = crate::ipc::notification::allocated_count();

        // Bind bit 5 to vector offset 2 using the caller-provided notification.
        let pending = match test_synthetic_irq_subscribe_and_signal_with_existing_notif(
            PID_E,
            key,
            caller_notif,
            5,
            2,
        ) {
            Ok(p) => p,
            Err(e) => {
                crate::ipc::notification::release(caller_notif);
                let _ = test_release_for_pid(PID_E);
                panic!("B.4b synthetic bind/signal (caller-notif) failed: {:?}", e);
            }
        };

        assert_eq!(
            pending,
            1u64 << 5,
            "ISR shim must have set exactly bit 5 (got {:#x})",
            pending,
        );

        // The notification must still be allocated — the helper did NOT release it.
        let pool_after_unbind = crate::ipc::notification::allocated_count();
        assert_eq!(
            pool_after_unbind, pool_before,
            "caller-owned notification must not be freed on IRQ unbind \
             (before={}, after={})",
            pool_before, pool_after_unbind,
        );

        // Cleanup: release the claim and then manually free the notification
        // (simulating the caller's own cap table teardown).
        let _ = test_release_for_pid(PID_E);

        // After release_for_pid on a caller-owned notif, the pool count must
        // still be the same (the process-exit path must not have freed it).
        let pool_after_exit = crate::ipc::notification::allocated_count();
        assert_eq!(
            pool_after_exit, pool_before,
            "caller-owned notification must not be freed by process-exit sweep \
             (before={}, after={})",
            pool_before, pool_after_exit,
        );

        // Now the caller explicitly frees it (cap table cleared).
        crate::ipc::notification::release(caller_notif);

        serial_println!("device_host B.4b caller-provided notif test passed");
    }

    // -- Track F.3 — Cross-device negative tests ------------------------------
    //
    // These four tests prove the central isolation invariant: a driver process
    // cannot access a BAR or DMA region belonging to a device it did not claim,
    // and forged / stale CapHandle values are rejected unconditionally.
    //
    // Tests 1, 2, 3 operate entirely through the test-harness helpers already
    // used by B.2 / B.3 (no live ring-3 process needed). Test 4 simulates
    // the post-crash handle-invalidation lifecycle using `test_release_for_pid`
    // + `test_try_claim_for_pid` as the stand-in for the supervisor kill+restart
    // cycle (the real end-to-end path is covered by F.2's process-restart
    // regression; we validate the handle-space invariant here at the registry
    // level).

    /// F.3 Test 1: cross-device MMIO denied.
    ///
    /// Simulates an NVMe driver (PID_NVME) holding a valid `Capability::Device`
    /// for its own BDF, then attempting to record an MMIO entry against a
    /// *different* BDF (which it has not claimed). The registry must reject
    /// this with `NotClaimed` — the same error the syscall boundary returns as
    /// `-EBADF` to the caller. No MMIO mapping is installed.
    #[test_case]
    fn cross_device_mmio_denied() {
        use crate::syscall::device_host::{
            TestClaimError, TestMmioError, test_mmio_count_for_pid, test_record_mmio,
            test_release_for_pid, test_try_claim_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        // Use a real PCI device for the NVMe driver's legitimate claim.
        let Some(nvme_key) = pick_free_pci_bdf() else {
            serial_println!("F.3 Test 1 skipped: no free PCI device for NVMe driver");
            return;
        };

        // e1000 BDF: fabricate a key the NVMe driver does NOT own.
        // We use a sentinel that is guaranteed to differ from nvme_key.
        let e1000_key = DeviceCapKey::new(0, 0xFE, 0x1F, 6);

        const PID_NVME: crate::process::Pid = 0xF3_0001;
        let _ = test_release_for_pid(PID_NVME);

        match test_try_claim_for_pid(PID_NVME, nvme_key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!("F.3 Test 1 skipped: nvme BDF busy");
                return;
            }
            Err(e) => panic!("F.3 Test 1 claim failed: {:?}", e),
        }

        // NVMe driver attempts to record an MMIO mapping against the e1000 BDF.
        let mmio_before = test_mmio_count_for_pid(PID_NVME);
        let result = test_record_mmio(PID_NVME, e1000_key, 0, 0x1000, 0xdead_4000);
        assert_eq!(
            result,
            Err(TestMmioError::NotClaimed),
            "F.3 Test 1: cross-device MMIO must be rejected with NotClaimed (-EBADF)",
        );
        assert_eq!(
            test_mmio_count_for_pid(PID_NVME),
            mmio_before,
            "F.3 Test 1: no MMIO entry installed after rejected cross-device attempt",
        );

        let _ = test_release_for_pid(PID_NVME);
        serial_println!("device_host F.3 Test 1 (cross_device_mmio_denied) passed");
    }

    /// F.3 Test 2: cross-device DMA denied.
    ///
    /// Simulates an NVMe driver (PID_NVME) attempting to allocate DMA against a
    /// BDF it has *not* claimed (the e1000's sentinel key). The DMA registry
    /// must reject this with `NoDevice` (the typed analogue of `-EBADF`). The
    /// driver's own claimed device is untouched; no allocation is recorded.
    ///
    /// IOMMU note: when the platform exposes an active IOMMU the `NoDevice`
    /// rejection happens before any IOMMU domain lookup, so the e1000's domain
    /// is never consulted. In identity-fallback mode the same registry check
    /// fires — the IOMMU layer is transparent to this test. Both paths return
    /// the same typed error.
    #[test_case]
    fn cross_device_dma_denied() {
        use crate::syscall::device_host::{
            TestClaimError, TestDmaError, test_dma_alloc_for_pid, test_dma_count,
            test_dma_release_for_pid, test_release_for_pid, test_try_claim_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        let Some(nvme_key) = pick_free_pci_bdf() else {
            serial_println!("F.3 Test 2 skipped: no free PCI device for NVMe driver");
            return;
        };

        // Sentinel BDF for the unclaimed "e1000" device.
        let e1000_key = DeviceCapKey::new(0, 0xFE, 0x1F, 5);

        const PID_NVME: crate::process::Pid = 0xF3_0002;
        let _ = test_release_for_pid(PID_NVME);
        let _ = test_dma_release_for_pid(PID_NVME);

        match test_try_claim_for_pid(PID_NVME, nvme_key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!("F.3 Test 2 skipped: nvme BDF busy");
                return;
            }
            Err(e) => panic!("F.3 Test 2 claim failed: {:?}", e),
        }

        let count_before = test_dma_count();

        // NVMe driver attempts DMA against e1000's unclaimed BDF.
        let result = test_dma_alloc_for_pid(PID_NVME, e1000_key, 4096, 4096);
        assert_eq!(
            result,
            Err(TestDmaError::NoDevice),
            "F.3 Test 2: DMA against unclaimed BDF must return NoDevice (-EBADF)",
        );
        assert_eq!(
            test_dma_count(),
            count_before,
            "F.3 Test 2: no DMA entry recorded after cross-device rejection",
        );

        let _ = test_dma_release_for_pid(PID_NVME);
        let _ = test_release_for_pid(PID_NVME);
        serial_println!("device_host F.3 Test 2 (cross_device_dma_denied) passed");
    }

    /// F.3 Test 3: forged CapHandle denied.
    ///
    /// A driver fabricates an arbitrary `CapHandle` value it never received from
    /// the kernel. Any device-host operation that validates ownership against the
    /// claim registry must reject it. We exercise this at the registry level by
    /// calling `test_record_mmio` and `test_dma_alloc_for_pid` under a PID that
    /// has no claim at all (never registered), passing plausible-looking BDF
    /// and handle values. Both operations must return the typed `NotClaimed` /
    /// `NoDevice` error with no side-effects.
    #[test_case]
    fn capability_forge_denied() {
        use crate::syscall::device_host::{
            TestDmaError, TestMmioError, test_dma_alloc_for_pid, test_dma_count,
            test_mmio_count_for_pid, test_record_mmio, test_release_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        // This PID has never claimed anything — simulates a driver that
        // fabricated a CapHandle out of thin air.
        const PID_FORGER: crate::process::Pid = 0xF3_0003;
        let _ = test_release_for_pid(PID_FORGER);

        // Use two arbitrary BDF keys the forger never claimed.
        let forge_key_a = DeviceCapKey::new(0, 0xFE, 0x1F, 4);
        let forge_key_b = DeviceCapKey::new(0, 0xFE, 0x1F, 3);

        let mmio_before = test_mmio_count_for_pid(PID_FORGER);
        let dma_before = test_dma_count();

        // Attempt forged MMIO record.
        let mmio_result = test_record_mmio(PID_FORGER, forge_key_a, 0, 0x2000, 0xcafe_0000);
        assert_eq!(
            mmio_result,
            Err(TestMmioError::NotClaimed),
            "F.3 Test 3: forged MMIO cap must be rejected with NotClaimed (-EBADF)",
        );

        // Attempt forged DMA alloc.
        let dma_result = test_dma_alloc_for_pid(PID_FORGER, forge_key_b, 4096, 4096);
        assert_eq!(
            dma_result,
            Err(TestDmaError::NoDevice),
            "F.3 Test 3: forged DMA cap must be rejected with NoDevice (-EBADF)",
        );

        // Verify no side-effects.
        assert_eq!(
            test_mmio_count_for_pid(PID_FORGER),
            mmio_before,
            "F.3 Test 3: MMIO count unchanged after forged MMIO attempt",
        );
        assert_eq!(
            test_dma_count(),
            dma_before,
            "F.3 Test 3: DMA count unchanged after forged DMA attempt",
        );

        serial_println!("device_host F.3 Test 3 (capability_forge_denied) passed");
    }

    /// F.3 Test 4: post-crash CapHandle values are invalid in the restarted
    /// process.
    ///
    /// Simulates the driver supervisor kill-and-restart lifecycle at the
    /// registry level:
    ///
    /// 1. Phase A (pre-crash): PID_PRE claims a BDF, records MMIO and DMA
    ///    entries, and captures the registry state.
    /// 2. Crash simulation: `test_release_for_pid(PID_PRE)` tears down all
    ///    claim, MMIO, and DMA state — exactly what the kernel does on process
    ///    exit (Phase 55b Track B.1 / B.2 / B.3 cleanup cascade).
    /// 3. Phase B (post-crash): a new PID (PID_POST, simulating the restarted
    ///    driver) claims the same BDF and receives fresh allocations. The handle
    ///    IDs from Phase A must not be visible to PID_POST.
    ///
    /// This validates the "handle-space is per-PID and non-transferable"
    /// invariant required by F.3 Acceptance item 4.
    #[test_case]
    fn post_crash_handles_invalid_in_restarted_process() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_handle_info, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("F.3 Test 4 skipped: no free PCI device");
            return;
        };

        // --- Phase A: pre-crash driver ---
        const PID_PRE: crate::process::Pid = 0xF3_0004;
        let _ = test_release_for_pid(PID_PRE);
        let _ = test_dma_release_for_pid(PID_PRE);

        match test_try_claim_for_pid(PID_PRE, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!("F.3 Test 4 skipped: BDF busy");
                return;
            }
            Err(e) => panic!("F.3 Test 4 pre-crash claim failed: {:?}", e),
        }

        let pre_snap = test_dma_alloc_for_pid(PID_PRE, key, 4096, 4096)
            .expect("F.3 Test 4: pre-crash DMA alloc must succeed");
        let pre_crash_id = pre_snap.id;

        // Pre-crash handle is visible to PID_PRE.
        assert!(
            test_dma_handle_info(PID_PRE, pre_crash_id).is_some(),
            "F.3 Test 4: pre-crash handle must be visible before crash",
        );

        // --- Crash simulation: supervisor calls release_for_pid ---
        let _ = test_dma_release_for_pid(PID_PRE);
        let released = test_release_for_pid(PID_PRE);
        assert_eq!(
            released, 1,
            "F.3 Test 4: exactly one claim must be freed on crash"
        );

        // Pre-crash handle is now gone even for PID_PRE.
        assert!(
            test_dma_handle_info(PID_PRE, pre_crash_id).is_none(),
            "F.3 Test 4: pre-crash handle must be invisible after crash teardown",
        );

        // --- Phase B: restarted driver with a fresh PID ---
        const PID_POST: crate::process::Pid = 0xF3_0005;
        let _ = test_release_for_pid(PID_POST);
        let _ = test_dma_release_for_pid(PID_POST);

        match test_try_claim_for_pid(PID_POST, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                panic!("F.3 Test 4: restarted driver must be able to re-claim BDF")
            }
            Err(e) => panic!("F.3 Test 4 post-crash claim failed: {:?}", e),
        }

        let post_snap = test_dma_alloc_for_pid(PID_POST, key, 4096, 4096)
            .expect("F.3 Test 4: post-crash DMA alloc must succeed");

        // The restarted driver must NOT see the pre-crash handle ID.
        assert!(
            test_dma_handle_info(PID_POST, pre_crash_id).is_none(),
            "F.3 Test 4: pre-crash CapHandle ID must be opaque to the restarted process",
        );

        // The restarted driver sees its own fresh allocation.
        assert!(
            test_dma_handle_info(PID_POST, post_snap.id).is_some(),
            "F.3 Test 4: restarted driver must see its own fresh allocation",
        );

        let _ = test_dma_release_for_pid(PID_POST);
        let _ = test_release_for_pid(PID_POST);
        serial_println!(
            "device_host F.3 Test 4 (post_crash_handles_invalid_in_restarted_process) passed"
        );
    }
}
