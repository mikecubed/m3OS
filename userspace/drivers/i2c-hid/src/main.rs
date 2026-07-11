//! Phase 102 Track D — ring-3 **I2C-HID touchpad** driver.
//!
//! Brings up the Dell Tiger Lake's built-in multitouch touchpad (Elan, ACPI
//! `_HID` `DLL0945`, on an Intel LPSS DesignWare I2C controller) and drives the
//! GUI cursor by injecting `PointerEvent`s into `mouse_server` — the exact path
//! `usb-hid` uses, so the compositor is unchanged.
//!
//! **What runs where.** The bus-agnostic pure logic — the DesignWare transfer
//! planner + `TX_ABRT` decode, the HID-over-I2C v1.0 codec, and the multitouch
//! report decode — is host-tested in `kernel-core` (`i2c::designware`,
//! `i2c::hid_over_i2c`, `usb::hid_report::decode_touchpad_report`). This daemon
//! is the hardware glue: it discovers the controller + touchpad via ACPI
//! (`acpid`'s `"acpi"` service — `_HID` match + `_CRS` decode), does the
//! controller's 32-bit register I/O over the `/drivers/`-gated
//! `SYS_ACPI_MEM_READ/WRITE` syscalls (there is no mapped-MMIO-window path for a
//! non-PCI ACPI device yet — a documented follow-up), runs the master + the
//! I2C-HID transport, decodes, and injects.
//!
//! **QEMU vs the Dell.** QEMU models no LPSS I2C and no I2C-HID device, so
//! `ACPI_FIND_BY_HID("DLL0945")` returns the not-present sentinel and the daemon
//! **exits cleanly (0)** — the same skip-with-reason shape `usb-hid`/`e1000`
//! use. The live datapath is bench-validated on the Dell per
//! `docs/appendix/bare-metal-validation.md`.
//!
//! **Bring-up subset / bench-tunable.** No GpioInt routing exists for a non-PCI
//! device yet, so input reports are **polled** at the report interval (the
//! charter's explicit fallback). The HID descriptor register address (normally
//! from the touchpad's `_DSM`) and the SCL timing / pointer-scale constants are
//! marked `BENCH:` and confirmed on the Dell; everything above them is exact.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::acpi::resource::{ResourceItem, decode_crs};
use kernel_core::i2c::designware::{
    self as dw, AbortReason, DW_IC_CLR_INTR, DW_IC_CLR_TX_ABRT, DW_IC_CON, DW_IC_DATA_CMD,
    DW_IC_ENABLE, DW_IC_ENABLE_STATUS, DW_IC_FS_SCL_HCNT, DW_IC_FS_SCL_LCNT, DW_IC_RAW_INTR_STAT,
    DW_IC_RXFLR, DW_IC_STATUS, DW_IC_TAR, DW_IC_TX_ABRT_SOURCE, Speed, TransferStatus,
};
use kernel_core::i2c::hid_over_i2c::{
    self as ihid, I2C_HID_DESC_LENGTH, I2cHidDescriptor, InputReport,
};
use kernel_core::input::events::{ModifierState, PointerButton, PointerEvent};
use kernel_core::usb::hid_report::{ReportField, decode_touchpad_report, parse_report_descriptor};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    klog("i2c-hid: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    klog("i2c-hid: PANIC\n");
    syscall_lib::exit(101)
}

/// Mirror a line into the kernel `dmesg` ring (`sys_debug_print` → `[userspace]
/// …`), so the driver's lifecycle is visible over SSH on the serial-less Dell —
/// a ring-3 driver's fd-1 output is NOT captured by `/proc/kmsg` (Phase 100 §3).
fn klog(msg: &str) {
    syscall_lib::serial_print(msg);
}

syscall_lib::entry_point!(program_main);

// ─── Constants ───────────────────────────────────────────────────────────────

/// The Dell Precision 5560 built-in touchpad (Elan) ACPI `_HID`.
const TOUCHPAD_HID: &str = "DLL0945";

/// `acpid`'s IPC service + verb labels (see `userspace/drivers/acpid`).
const ACPI_SERVICE: &str = "acpi";
const ACPI_FIND_BY_HID: u64 = 2;
const ACPI_GET_CRS: u64 = 3;
/// `acpid` replies label 0 on success; `u64::MAX` is its error sentinel.
const ACPI_REPLY_ERR: u64 = u64::MAX;

/// `mouse_server` inject: label 3, a 37-byte `PointerEvent` payload.
const MOUSE_SERVICE: &str = "mouse";
const MOUSE_EVENT_INJECT: u64 = 3;

/// `SYS_ACPI_{MEM_READ,MEM_WRITE}` — per-access physical register I/O (the only
/// ring-3 path to a non-PCI controller's MMIO today). Width 4 for DW-I2C regs.
use kernel_core::device_host::syscalls::{SYS_ACPI_MEM_READ, SYS_ACPI_MEM_WRITE};

/// `DW_IC_STATUS` bits used for FIFO flow control.
const STATUS_TFNF: u32 = 1 << 1; // TX FIFO not full
const STATUS_RFNE: u32 = 1 << 3; // RX FIFO not empty

/// Largest ACPI reply we take (an ASL path or a `_CRS` template — both small).
const ACPI_REPLY_MAX: usize = 512;
/// Max report-descriptor bytes we fetch + parse.
const REPORT_DESC_MAX: usize = 640;
/// Bounded poll budget for a single I2C transfer before declaring a timeout.
const XFER_MAX_POLLS: u32 = 200;

/// **BENCH:** the HID descriptor register address. Per HID-over-I2C this comes
/// from the touchpad's `_DSM` (function 1); `0x0001` is the near-universal Elan
/// value and is confirmed / corrected on the Dell (a `_DSM` eval is the follow-
/// up once `acpid` grows argument-package evaluation).
const HID_DESC_REGISTER: u16 = 0x0001;

/// **BENCH:** Fast-mode SCL counts, written only if the firmware left them 0
/// (BIOS usually pre-programs LPSS I2C). Tiger Lake LPSS runs ~133 MHz; these
/// are conservative 400 kHz values confirmed on the bench.
const FS_HCNT_DEFAULT: u32 = 0x0087;
const FS_LCNT_DEFAULT: u32 = 0x011f;

/// **BENCH:** pointer sensitivity — absolute touchpad units per cursor pixel.
/// The touchpad's logical range is large (thousands); this scales a contact
/// delta into a sane `PointerEvent` delta. Tuned on the panel.
const POINTER_SCALE: i32 = 6;
/// **BENCH:** two-finger scroll divisor (touchpad units per wheel notch).
const SCROLL_SCALE: i32 = 40;

// ─── Register access over SYS_ACPI_MEM_READ/WRITE ────────────────────────────

/// The DesignWare controller's register bank, addressed by physical base +
/// offset via the per-access ACPI memory syscalls (no mapped window yet).
struct Regs {
    base: u64,
}

impl Regs {
    #[inline]
    fn r32(&self, off: usize) -> u32 {
        // SAFETY: SYS_ACPI_MEM_READ is a pure `/drivers/`-gated syscall; width 4.
        unsafe { syscall_lib::syscall2(SYS_ACPI_MEM_READ, self.base + off as u64, 4) as u32 }
    }

    #[inline]
    fn w32(&self, off: usize, val: u32) {
        // SAFETY: SYS_ACPI_MEM_WRITE is a pure `/drivers/`-gated syscall; width 4.
        unsafe {
            syscall_lib::syscall3(SYS_ACPI_MEM_WRITE, self.base + off as u64, 4, val as u64);
        }
    }
}

// ─── ACPI discovery (via the `acpid` "acpi" service) ─────────────────────────

/// Issue one `acpid` verb and take its reply bulk. Returns `None` when the verb
/// fails (`ACPI_REPLY_ERR`), the device/resource is absent, or the reply is
/// empty — every one of which is a clean "not on this machine" on QEMU.
fn acpi_verb(ep: u32, label: u64, payload: &[u8]) -> Option<Vec<u8>> {
    let reply = syscall_lib::ipc_call_buf(ep, label, payload.len() as u64, payload);
    if reply != 0 || reply == ACPI_REPLY_ERR {
        return None;
    }
    let mut buf = [0u8; ACPI_REPLY_MAX];
    let n = syscall_lib::ipc_take_pending_bulk(&mut buf) as usize;
    if n == 0 || n > buf.len() {
        return None;
    }
    Some(buf[..n].to_vec())
}

/// What discovery resolves before the controller is touched.
struct Discovered {
    controller_base: u64,
    touchpad_addr: u16,
    speed: Speed,
}

/// Resolve the controller MMIO base + the touchpad's I2C address/speed from
/// ACPI: `_HID`-match the touchpad → decode its `_CRS` (`I2cSerialBus` gives the
/// address/speed + the controller path in `source`) → decode the controller's
/// `_CRS` (`Memory32Fixed` gives the MMIO base). Any missing piece → `None`
/// (the QEMU / non-Dell path).
fn discover(acpi_ep: u32) -> Option<Discovered> {
    let path = acpi_verb(acpi_ep, ACPI_FIND_BY_HID, TOUCHPAD_HID.as_bytes())?;
    let touch_crs = acpi_verb(acpi_ep, ACPI_GET_CRS, &path)?;
    let touch_res = decode_crs(&touch_crs).ok()?;

    let (touchpad_addr, speed_hz, controller_path) = match touch_res.i2c()? {
        ResourceItem::I2cSerialBus {
            address,
            speed_hz,
            source,
            ..
        } => (*address, *speed_hz, source.clone()),
        _ => return None,
    };

    let ctrl_crs = acpi_verb(acpi_ep, ACPI_GET_CRS, controller_path.as_bytes())?;
    let ctrl_res = decode_crs(&ctrl_crs).ok()?;
    let controller_base = ctrl_res.items.iter().find_map(|r| match r {
        ResourceItem::Memory32Fixed { base, .. } => Some(*base as u64),
        _ => None,
    })?;

    let speed = if speed_hz >= 400_000 {
        Speed::Fast
    } else {
        Speed::Standard
    };
    Some(Discovered {
        controller_base,
        touchpad_addr,
        speed,
    })
}

// ─── DesignWare master ───────────────────────────────────────────────────────

/// Bring the controller up for master transfers to `target` (7-bit): disable →
/// program `CON`/timings/`TAR` → enable. Idempotent enough for bring-up.
fn master_init(regs: &Regs, target: u16, speed: Speed) {
    // Disable and wait for the enable status to clear (bounded).
    regs.w32(DW_IC_ENABLE, 0);
    for _ in 0..100 {
        if regs.r32(DW_IC_ENABLE_STATUS) & 1 == 0 {
            break;
        }
    }
    regs.w32(DW_IC_CON, dw::compose_con(speed, false));
    // Respect BIOS-programmed SCL timings; only fill them if left zero.
    if regs.r32(DW_IC_FS_SCL_HCNT) == 0 {
        regs.w32(DW_IC_FS_SCL_HCNT, FS_HCNT_DEFAULT);
    }
    if regs.r32(DW_IC_FS_SCL_LCNT) == 0 {
        regs.w32(DW_IC_FS_SCL_LCNT, FS_LCNT_DEFAULT);
    }
    regs.w32(DW_IC_TAR, u32::from(target) & 0x3ff);
    regs.w32(DW_IC_ENABLE, 1);
    let _ = regs.r32(DW_IC_CLR_INTR); // clear any latched interrupts
}

/// Run one combined write-then-read I2C transaction using the host-tested
/// planner ([`dw::plan_transfer`]): push each `DW_IC_DATA_CMD` word (waiting for
/// TX-FIFO space), drain received bytes against `RXFLR`, and poll
/// `RAW_INTR_STAT` for STOP_DET / TX_ABRT. Returns the abort reason on a NACK.
fn i2c_transfer(regs: &Regs, write: &[u8], read: &mut [u8]) -> Result<(), AbortReason> {
    let _ = regs.r32(DW_IC_CLR_INTR);
    let plan = dw::plan_transfer(write, read.len());
    let mut rx = 0usize;
    let mut polls = 0u32;

    for &cmd in &plan {
        // Wait for TX-FIFO space, opportunistically draining RX so a long read
        // never overflows the RX FIFO while we push.
        loop {
            if regs.r32(DW_IC_STATUS) & STATUS_TFNF != 0 {
                break;
            }
            drain_rx(regs, read, &mut rx);
            if !poll_ok(regs, &mut polls)? {
                return Err(AbortReason::Other(0)); // timeout
            }
        }
        regs.w32(DW_IC_DATA_CMD, u32::from(cmd));
        drain_rx(regs, read, &mut rx);
    }

    // Drain the tail + wait for completion / abort.
    loop {
        drain_rx(regs, read, &mut rx);
        match dw::transfer_status(regs.r32(DW_IC_RAW_INTR_STAT)) {
            TransferStatus::Aborted => {
                let src = regs.r32(DW_IC_TX_ABRT_SOURCE);
                let _ = regs.r32(DW_IC_CLR_TX_ABRT);
                return Err(dw::decode_abort(src));
            }
            TransferStatus::Complete if rx >= read.len() => {
                let _ = regs.r32(DW_IC_CLR_INTR);
                return Ok(());
            }
            _ => {}
        }
        if !poll_ok(regs, &mut polls)? {
            return Err(AbortReason::Other(0)); // timeout
        }
    }
}

/// Drain any bytes currently in the RX FIFO into `read`.
fn drain_rx(regs: &Regs, read: &mut [u8], rx: &mut usize) {
    while *rx < read.len()
        && (regs.r32(DW_IC_RXFLR) > 0 || regs.r32(DW_IC_STATUS) & STATUS_RFNE != 0)
    {
        read[*rx] = (regs.r32(DW_IC_DATA_CMD) & 0xff) as u8;
        *rx += 1;
    }
}

/// Advance the bounded poll budget with a ≥1 ms deschedule (a sub-ms sleep would
/// busy-spin the core — Phase 100 §3). Returns `Ok(false)` on timeout.
fn poll_ok(regs: &Regs, polls: &mut u32) -> Result<bool, AbortReason> {
    // Cheap fast-path: if an abort already latched, surface it immediately.
    if regs.r32(DW_IC_RAW_INTR_STAT) & dw::INTR_TX_ABRT != 0 {
        let src = regs.r32(DW_IC_TX_ABRT_SOURCE);
        let _ = regs.r32(DW_IC_CLR_TX_ABRT);
        return Err(dw::decode_abort(src));
    }
    *polls += 1;
    if *polls > XFER_MAX_POLLS {
        return Ok(false);
    }
    syscall_lib::nanosleep_for(0, 1_000_000);
    Ok(true)
}

/// Read `len` bytes from I2C-HID register `reg` (a combined write-address +
/// read). `len` is clamped to `buf`.
fn read_register(regs: &Regs, reg: u16, buf: &mut [u8]) -> Result<(), AbortReason> {
    let [lo, hi] = reg.to_le_bytes();
    i2c_transfer(regs, &[lo, hi], buf)
}

// ─── I2C-HID transport ───────────────────────────────────────────────────────

/// The parsed descriptor + report fields the poll loop needs.
struct Device {
    desc: I2cHidDescriptor,
    fields: Vec<ReportField>,
}

/// Bring the touchpad up: read the HID descriptor, fetch + parse the report
/// descriptor, RESET, then SET_POWER(ON). Returns the decoded device or a
/// bring-up error string.
fn bringup(regs: &Regs) -> Result<Device, &'static str> {
    let mut dbuf = [0u8; I2C_HID_DESC_LENGTH];
    read_register(regs, HID_DESC_REGISTER, &mut dbuf).map_err(|_| "HID descriptor read NACK")?;
    let desc = I2cHidDescriptor::parse(&dbuf).ok_or("short HID descriptor")?;
    if !desc.looks_valid() {
        return Err("HID descriptor failed the length/version check");
    }

    let want = (desc.report_desc_length as usize).min(REPORT_DESC_MAX);
    let mut rdesc = vec![0u8; want];
    read_register(regs, desc.report_desc_register, &mut rdesc)
        .map_err(|_| "report descriptor read NACK")?;
    let fields = parse_report_descriptor(&rdesc);
    if fields.is_empty() {
        return Err("report descriptor parsed to no fields");
    }

    // RESET, then wait for the reset-complete (zero-length input report).
    let reset = ihid::reset_command(desc.command_register);
    i2c_transfer(regs, &reset, &mut []).map_err(|_| "RESET command NACK")?;
    wait_reset_complete(regs, &desc);

    let power = ihid::set_power_command(desc.command_register, ihid::POWER_ON);
    i2c_transfer(regs, &power, &mut []).map_err(|_| "SET_POWER(ON) NACK")?;

    Ok(Device { desc, fields })
}

/// After a RESET the device signals completion with a zero-length input report;
/// poll a bounded number of times for it (bring-up-tolerant).
fn wait_reset_complete(regs: &Regs, desc: &I2cHidDescriptor) {
    let cap = (desc.max_input_length as usize).clamp(2, 64);
    let mut buf = vec![0u8; cap];
    for _ in 0..64 {
        if read_register(regs, desc.input_register, &mut buf).is_ok()
            && matches!(ihid::parse_input_report(&buf), Some(InputReport::Empty))
        {
            return;
        }
        syscall_lib::nanosleep_for(0, 2_000_000);
    }
}

// ─── Multitouch → PointerEvent mapping ───────────────────────────────────────

/// The previous frame's state, for absolute→relative differencing + edge
/// detection.
#[derive(Default)]
struct PointerState {
    last_x: i32,
    last_y: i32,
    had_contact: bool,
    button_down: bool,
}

/// Map a decoded touchpad frame to at most one `PointerEvent`, updating `state`.
/// Absolute contact coordinates are differenced into relative deltas; a
/// two-finger frame becomes a wheel scroll; the clickpad button edge-detects
/// into `Down`/`Up`. Returns `None` when nothing changed (so we do not spam
/// `mouse_server` with idle frames).
fn map_frame(
    frame: &kernel_core::usb::hid_report::TouchpadFrame,
    state: &mut PointerState,
) -> Option<PointerEvent> {
    let active: Vec<_> = frame.active_contacts().collect();
    let two_finger = active.len() >= 2;
    let primary = active.first().copied();

    let mut ev = PointerEvent {
        timestamp_ms: 0,
        dx: 0,
        dy: 0,
        abs_position: None,
        button: PointerButton::None,
        wheel_dx: 0,
        wheel_dy: 0,
        modifiers: ModifierState::empty(),
    };

    if let Some(c) = primary {
        let (cx, cy) = (c.x as i32, c.y as i32);
        if state.had_contact {
            let raw_dx = cx - state.last_x;
            let raw_dy = cy - state.last_y;
            if two_finger {
                // Two fingers → vertical scroll (natural: finger up = wheel up).
                ev.wheel_dy = -raw_dy / SCROLL_SCALE;
            } else {
                ev.dx = raw_dx / POINTER_SCALE;
                ev.dy = raw_dy / POINTER_SCALE;
            }
        }
        state.last_x = cx;
        state.last_y = cy;
        state.had_contact = true;
    } else {
        state.had_contact = false;
    }

    // Clickpad button edge (left button).
    if frame.button && !state.button_down {
        ev.button = PointerButton::Down(0);
        state.button_down = true;
    } else if !frame.button && state.button_down {
        ev.button = PointerButton::Up(0);
        state.button_down = false;
    }

    let moved = ev.dx != 0 || ev.dy != 0 || ev.wheel_dy != 0 || ev.wheel_dx != 0;
    if moved || ev.button != PointerButton::None {
        Some(ev)
    } else {
        None
    }
}

fn inject(mouse_ep: u32, ev: &PointerEvent) {
    let mut buf = [0u8; kernel_core::input::events::POINTER_EVENT_WIRE_SIZE];
    if ev.encode(&mut buf).is_ok() {
        let _ = syscall_lib::ipc_call_buf(mouse_ep, MOUSE_EVENT_INJECT, 0, &buf);
    }
}

fn lookup(name: &str) -> Option<u32> {
    let h = syscall_lib::ipc_lookup_service(name);
    if h == u64::MAX { None } else { Some(h as u32) }
}

// ─── Entry ───────────────────────────────────────────────────────────────────

fn program_main(_args: &[&str]) -> i32 {
    klog("i2c-hid: start\n");

    // ACPI is our device-discovery substrate; wait briefly for it.
    if !syscall_lib::ipc_wait_service(ACPI_SERVICE, 10_000) {
        klog("i2c-hid: no acpi service — exiting\n");
        return 0;
    }
    let Some(acpi_ep) = lookup(ACPI_SERVICE) else {
        klog("i2c-hid: acpi lookup failed — exiting\n");
        return 0;
    };

    let Some(found) = discover(acpi_ep) else {
        // The QEMU / non-Dell path: no DLL0945 touchpad in the namespace.
        klog("i2c-hid: no I2C-HID touchpad (DLL0945) in ACPI — exiting (expected on QEMU)\n");
        syscall_lib::write_str(STDOUT_FILENO, "i2c-hid: no touchpad; exiting\n");
        return 0;
    };

    let regs = Regs {
        base: found.controller_base,
    };
    klog("i2c-hid: touchpad found; bringing up DesignWare controller\n");
    master_init(&regs, found.touchpad_addr, found.speed);

    let device = match bringup(&regs) {
        Ok(d) => d,
        Err(reason) => {
            klog("i2c-hid: bring-up failed: ");
            klog(reason);
            klog("\n");
            return 1;
        }
    };
    klog("i2c-hid: touchpad up; report descriptor parsed; polling input\n");

    // The pointer sink. mouse_server is always present in a GUI session.
    let _ = syscall_lib::ipc_wait_service(MOUSE_SERVICE, 10_000);
    let Some(mouse_ep) = lookup(MOUSE_SERVICE) else {
        klog("i2c-hid: no mouse service — exiting\n");
        return 1;
    };

    // Poll loop (no GpioInt routing for a non-PCI device yet — bench follow-up).
    let mut state = PointerState::default();
    let cap = (device.desc.max_input_length as usize).clamp(2, 256);
    let mut buf = vec![0u8; cap];
    loop {
        match read_register(&regs, device.desc.input_register, &mut buf) {
            Ok(()) => {
                if let Some(InputReport::Report(body)) = ihid::parse_input_report(&buf) {
                    let frame = decode_touchpad_report(&device.fields, body);
                    if let Some(ev) = map_frame(&frame, &mut state) {
                        inject(mouse_ep, &ev);
                    }
                }
            }
            Err(_) => {
                // A transient NACK (device briefly asleep) — keep polling.
            }
        }
        // ~8 ms ≈ 125 Hz, and ≥1 ms so the poll deschedules the core.
        syscall_lib::nanosleep_for(0, 8_000_000);
    }
}
