//! Bochs Display Interface (BDI) / VBE register access for hardware
//! double-buffering.
//!
//! The QEMU `-vga std` device (also exposed by `bochs-display` and the
//! q35 default VGA) implements Bochs VBE, a tiny I/O-port protocol that
//! lets us pan the scanout cursor around a virtual framebuffer that is
//! larger than the visible area. Programming a `VIRT_HEIGHT` of `2 ×
//! YRES` and toggling `Y_OFFSET` between `0` and `YRES` gives us a true
//! page flip on the display device — replacing the per-frame memcpy
//! the userspace compositor used to do with one I/O port write.
//!
//! Detection is best-effort: `enable_doublebuffer` reads the `ID`
//! register and bails on anything outside the documented Bochs VBE
//! range, then verifies the `VIRT_HEIGHT` readback after the write so a
//! firmware that lies about the device or runs out of VRAM gracefully
//! falls back to the single-buffered memcpy path.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use x86_64::instructions::port::Port;

const VBE_DISPI_IOPORT_INDEX: u16 = 0x01CE;
const VBE_DISPI_IOPORT_DATA: u16 = 0x01CF;

const VBE_DISPI_INDEX_ID: u16 = 0;
#[allow(dead_code)]
const VBE_DISPI_INDEX_XRES: u16 = 1;
const VBE_DISPI_INDEX_YRES: u16 = 2;
#[allow(dead_code)]
const VBE_DISPI_INDEX_BPP: u16 = 3;
const VBE_DISPI_INDEX_ENABLE: u16 = 4;
const VBE_DISPI_INDEX_VIRT_WIDTH: u16 = 6;
const VBE_DISPI_INDEX_VIRT_HEIGHT: u16 = 7;
#[allow(dead_code)]
const VBE_DISPI_INDEX_X_OFFSET: u16 = 8;
const VBE_DISPI_INDEX_Y_OFFSET: u16 = 9;

/// Documented Bochs VBE ID range. QEMU's stdvga reports `0xB0C5`; older
/// emulators reported earlier values in the same family.
const VBE_ID_MIN: u16 = 0xB0C0;
const VBE_ID_MAX: u16 = 0xB0CF;

/// Tracks whether `enable_doublebuffer` succeeded so the syscall path
/// can refuse `pageflip` calls when the hardware never agreed.
static DOUBLEBUFFER_READY: AtomicBool = AtomicBool::new(false);
/// Visible height in pixels — the only legal non-zero `Y_OFFSET` value
/// for the page flip is exactly this. Stored at enable time so the
/// syscall handler can validate without re-reading the device.
static VISIBLE_HEIGHT: AtomicU32 = AtomicU32::new(0);

/// Reasons `enable_doublebuffer` may decline. All are non-fatal; the
/// caller falls back to the single-buffered memcpy path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnableError {
    /// The `ID` register reported a value outside the Bochs VBE range —
    /// either the device is not VBE or the I/O ports are unmapped.
    NotBochsVbe,
    /// Reported visible resolution from the `YRES` register is zero.
    NoYRes,
    /// `VIRT_HEIGHT` readback did not match the requested value — the
    /// device clamped, almost certainly because `2 × YRES × stride`
    /// exceeds the configured `vgamem_mb`.
    VirtHeightClamped { requested: u16, actual: u16 },
}

/// Try to enable double-buffering. On success, registers
/// `VIRT_HEIGHT = 2 × YRES` and `Y_OFFSET = 0`. Returns the visible
/// height (which doubles as the back-buffer Y offset).
///
/// # Safety
/// Must be called from ring 0. Touches I/O ports `0x1CE` / `0x1CF`. The
/// caller is expected to invoke this at most once per boot, before any
/// other code touches the VBE registers.
pub unsafe fn enable_doublebuffer() -> Result<u32, EnableError> {
    let id = unsafe { read_register(VBE_DISPI_INDEX_ID) };
    if !(VBE_ID_MIN..=VBE_ID_MAX).contains(&id) {
        return Err(EnableError::NotBochsVbe);
    }

    let yres = unsafe { read_register(VBE_DISPI_INDEX_YRES) };
    if yres == 0 {
        return Err(EnableError::NoYRes);
    }
    let virt_height = match yres.checked_mul(2) {
        Some(v) => v,
        None => return Err(EnableError::NoYRes),
    };

    // XRES is the natural choice for VIRT_WIDTH; the GOP framebuffer's
    // stride may carry padding pixels (e.g. on weird widths), so trust
    // the device-reported XRES rather than recomputing from stride.
    let xres = unsafe { read_register(VBE_DISPI_INDEX_XRES) };

    unsafe {
        write_register(VBE_DISPI_INDEX_VIRT_WIDTH, xres);
        write_register(VBE_DISPI_INDEX_VIRT_HEIGHT, virt_height);
        write_register(VBE_DISPI_INDEX_Y_OFFSET, 0);
    }

    let actual_virt_height = unsafe { read_register(VBE_DISPI_INDEX_VIRT_HEIGHT) };
    if actual_virt_height != virt_height {
        // Roll back so we leave the device in the pre-call state when
        // we fall back to the single-buffered path.
        unsafe { write_register(VBE_DISPI_INDEX_VIRT_HEIGHT, yres) };
        return Err(EnableError::VirtHeightClamped {
            requested: virt_height,
            actual: actual_virt_height,
        });
    }

    // Defensively confirm ENABLE didn't get cleared by our writes —
    // some legacy emulators reset state on VIRT_HEIGHT writes. If it
    // looks disabled, leave the device alone (the bootloader's GOP
    // framebuffer is still mapped at the LFB base) and surface clamp
    // so the caller falls back.
    let enable = unsafe { read_register(VBE_DISPI_INDEX_ENABLE) };
    if enable & 1 == 0 {
        unsafe { write_register(VBE_DISPI_INDEX_VIRT_HEIGHT, yres) };
        return Err(EnableError::VirtHeightClamped {
            requested: virt_height,
            actual: 0,
        });
    }

    VISIBLE_HEIGHT.store(yres as u32, Ordering::Release);
    DOUBLEBUFFER_READY.store(true, Ordering::Release);
    Ok(yres as u32)
}

/// True iff [`enable_doublebuffer`] succeeded and `pageflip` is safe
/// to call.
pub fn doublebuffer_ready() -> bool {
    DOUBLEBUFFER_READY.load(Ordering::Acquire)
}

/// Visible height the device reported at enable time. The only legal
/// non-zero `Y_OFFSET` argument to `pageflip` is exactly this value;
/// the userspace compositor toggles between `0` and this.
pub fn visible_height() -> u32 {
    VISIBLE_HEIGHT.load(Ordering::Acquire)
}

/// Reasons `pageflip` may refuse a flip request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlipError {
    /// `enable_doublebuffer` never succeeded for this boot — no
    /// double-buffering plumbing in place.
    NotReady,
    /// `y_offset` was neither `0` nor `visible_height()`. Only those
    /// two values map to a clean page flip; arbitrary offsets would
    /// scanout a partial seam between the two buffers.
    BadOffset,
}

/// Write `y_offset` to the VBE `Y_OFFSET` register. Bochs VBE applies
/// the new offset on the next scanout cycle, so the flip is effectively
/// instantaneous from userspace's perspective.
///
/// # Safety
/// Must be called from ring 0. Touches I/O ports `0x1CE` / `0x1CF`.
pub unsafe fn pageflip(y_offset: u32) -> Result<(), FlipError> {
    if !doublebuffer_ready() {
        return Err(FlipError::NotReady);
    }
    let visible = visible_height();
    if y_offset != 0 && y_offset != visible {
        return Err(FlipError::BadOffset);
    }
    let value = match u16::try_from(y_offset) {
        Ok(v) => v,
        Err(_) => return Err(FlipError::BadOffset),
    };
    unsafe { write_register(VBE_DISPI_INDEX_Y_OFFSET, value) };
    Ok(())
}

/// Read a 16-bit VBE register. The Bochs interface is a classic
/// index/data port pair: write the register number to `INDEX`, then
/// read the value from `DATA`.
///
/// # Safety
/// Caller must be in ring 0.
unsafe fn read_register(index: u16) -> u16 {
    let mut idx: Port<u16> = Port::new(VBE_DISPI_IOPORT_INDEX);
    let mut data: Port<u16> = Port::new(VBE_DISPI_IOPORT_DATA);
    unsafe {
        idx.write(index);
        data.read()
    }
}

/// Write a 16-bit VBE register.
///
/// # Safety
/// Caller must be in ring 0.
unsafe fn write_register(index: u16, value: u16) {
    let mut idx: Port<u16> = Port::new(VBE_DISPI_IOPORT_INDEX);
    let mut data: Port<u16> = Port::new(VBE_DISPI_IOPORT_DATA);
    unsafe {
        idx.write(index);
        data.write(value);
    }
}
