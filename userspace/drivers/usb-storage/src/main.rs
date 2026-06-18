//! Phase 92 Track D — ring-3 USB Mass Storage (Bulk-Only Transport) class
//! driver.
//!
//! Binds a `CLASS_MASS_STORAGE` (0x08) interface surfaced by the xHCI `usb`
//! server (the same `NextAttach` walk + bulk-IN/OUT primitives the Phase 96
//! bulk substrate exposes) and drives SCSI-over-BOT:
//!
//! 1. `GET_MAX_LUN` (class control-IN over `ControlRequest`) — how many LUNs.
//! 2. `TEST UNIT READY` — wait for the medium.
//! 3. `INQUIRY` — device identity (vendor / product / type).
//! 4. `READ CAPACITY(10)` — block count + block size.
//!
//! Each SCSI command is a CBW on bulk-OUT, an optional data phase on bulk-IN,
//! and a CSW on bulk-IN — all framed by `kernel_core::usb::mass_storage`. The
//! ring-3 daemon parses SCSI so the kernel stays SCSI-unaware.
//!
//! D.1/D.2 here prove the transport + identity/capacity read. The
//! `RemoteBlockDevice` facade + `/mnt/usb<n>` mount (D.4), UAS (D.3), and the
//! page-grant overflow path (D.5) build on this.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::usb::mass_storage::{
    CSW_LEN, Cbw, Csw, InquiryData, ReadCapacity10, cdb_inquiry, cdb_read_capacity10,
    cdb_test_unit_ready,
};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;
use syscall_lib::write_str;
use usb_core::protocol::{
    AttachNotice, USB_MSG_MAX, USB_REQ_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest,
};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "usb-storage: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "usb-storage: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

/// Boot-log marker written when the daemon starts.
pub const BOOT_LOG_MARKER: &str = "usb-storage: spawned\n";

/// USB Mass Storage interface class (USB-IF base-class 0x08).
const CLASS_MASS_STORAGE: u8 = 0x08;
/// SCSI peripheral device type for a direct-access block device (disk).
const SCSI_TYPE_DIRECT_ACCESS: u8 = 0x00;

/// Standard INQUIRY allocation length (we only need the first 36 bytes).
const INQUIRY_LEN: u16 = 36;
/// READ CAPACITY(10) response length.
const READ_CAPACITY10_LEN: u16 = 8;

// ---------------------------------------------------------------------------
// IPC plumbing (mirrors usb-hid's usb_call)
// ---------------------------------------------------------------------------

/// Issue a `UsbRequest` to the xHCI server and decode the `UsbReply`.
fn usb_call(usb_ep: u32, req: &UsbRequest) -> Option<UsbReply> {
    let req_bytes = req.encode();
    let rc = syscall_lib::ipc_call_buf(usb_ep, USB_REQ_LABEL, 0, &req_bytes);
    if rc == u64::MAX {
        return None;
    }
    let mut reply_buf = [0u8; USB_MSG_MAX];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX {
        return None;
    }
    UsbReply::decode(&reply_buf[..n as usize])
}

fn lookup(name: &str) -> Option<u32> {
    let h = syscall_lib::ipc_lookup_service(name);
    if h == u64::MAX { None } else { Some(h as u32) }
}

/// Next monotonic CBW tag (a CSW echoes the CBW tag — we don't strictly verify
/// it here, but it must be unique per command).
fn next_tag() -> u32 {
    use core::sync::atomic::{AtomicU32, Ordering};
    static TAG: AtomicU32 = AtomicU32::new(0x1000);
    TAG.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// BOT command execution
// ---------------------------------------------------------------------------

/// Issue a **synchronous, single-TRB** bulk-IN transfer of exactly `len` bytes
/// for one BOT phase (the data stage or the 13-byte CSW). `len` MUST equal the
/// transfer the device will send for that phase — BOT requires the host's
/// bulk-IN length to match the CBW data length / CSW length exactly. The server
/// arms exactly one TRB and blocks for its completion (no streaming auto-re-arm,
/// so the device is never issued a surplus IN token while it is back in CBW-wait
/// state — which would STALL the endpoint, the Phase 96 streaming-path gap this
/// closes). Returns `None` on a transport failure / STALL.
fn submit_bulk_in(usb_ep: u32, slot_id: u8, dci: u8, len: u16) -> Option<Vec<u8>> {
    match usb_call(usb_ep, &UsbRequest::SubmitBulkIn { slot_id, dci, len }) {
        Some(UsbReply::BulkData {
            data,
            completion_code: 1,
        }) => Some(data),
        _ => None,
    }
}

/// Run one BOT SCSI command: CBW on bulk-OUT, optional data phase on bulk-IN,
/// then the CSW on bulk-IN. Returns `(data, csw_status)` where `csw_status` is
/// 0 = passed, 1 = failed, 2 = phase error. `None` on a transport failure.
fn bot_command(
    usb_ep: u32,
    notice: &AttachNotice,
    cdb: &[u8],
    data_in: bool,
    data_len: u16,
) -> Option<(Vec<u8>, u8)> {
    let slot_id = notice.slot_id;
    let tag = next_tag();
    let cbw = Cbw::new(tag, data_len as u32, data_in, 0, cdb);
    let cbw_bytes = cbw.encode();

    // (1) CBW on bulk-OUT (blocks for completion).
    match usb_call(
        usb_ep,
        &UsbRequest::SubmitBulkOut {
            slot_id,
            dci: notice.bulk_out_dci,
            data: cbw_bytes.to_vec(),
        },
    ) {
        Some(UsbReply::TransferComplete {
            completion_code: 1, ..
        }) => {}
        _ => {
            write_str(STDOUT_FILENO, "usb-storage: CBW bulk-OUT failed\n");
            return None;
        }
    }

    // (2) Data phase (device-to-host) — request exactly the CBW data length.
    let data = if data_in && data_len > 0 {
        submit_bulk_in(usb_ep, slot_id, notice.bulk_in_dci, data_len)?
    } else {
        Vec::new()
    };

    // (3) CSW on bulk-IN — request exactly 13 bytes.
    let csw_bytes = submit_bulk_in(usb_ep, slot_id, notice.bulk_in_dci, CSW_LEN as u16)?;
    let csw = Csw::parse(&csw_bytes)?;
    Some((data, csw.status))
}

/// Issue `GET_MAX_LUN` (class control-IN). A STALL (or any failure) means the
/// device has a single LUN, per the USB Mass Storage class spec §3.2.
fn get_max_lun(usb_ep: u32, notice: &AttachNotice) -> u8 {
    let iface = notice.interface_num;
    // bmRequestType 0xA1 (Class | Interface | D2H), bRequest 0xFE, wValue 0,
    // wIndex = interface, wLength 1.
    let setup = [0xA1, 0xFE, 0x00, 0x00, iface, 0x00, 0x01, 0x00];
    match usb_call(
        usb_ep,
        &UsbRequest::ControlRequest {
            slot_id: notice.slot_id,
            setup,
            length: 1,
        },
    ) {
        Some(UsbReply::ControlData {
            data,
            completion_code: 1,
        }) if !data.is_empty() => data[0],
        // STALL / no data → single LUN.
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Decimal log helpers
// ---------------------------------------------------------------------------

fn write_u32_dec(mut n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    if n == 0 {
        write_str(STDOUT_FILENO, "0");
        return;
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    // SAFETY: buf[i..] is ASCII digits.
    write_str(STDOUT_FILENO, unsafe {
        core::str::from_utf8_unchecked(&buf[i..])
    });
}

fn write_u8_dec(n: u8) {
    write_u32_dec(n as u32);
}

// ---------------------------------------------------------------------------
// Bind + probe one mass-storage device
// ---------------------------------------------------------------------------

/// Drive the GET_MAX_LUN → TEST UNIT READY → INQUIRY → READ CAPACITY sequence
/// against a bound mass-storage device. Returns true on a successful capacity
/// read.
fn probe_device(usb_ep: u32, notice: &AttachNotice) -> bool {
    let max_lun = get_max_lun(usb_ep, notice);
    write_str(STDOUT_FILENO, "usb-storage: max_lun=");
    write_u8_dec(max_lun);
    write_str(STDOUT_FILENO, "\n");

    // TEST UNIT READY — a no-data SCSI command that proves a full BOT
    // CBW-out + CSW-in round-trip over the bulk pair (the load-bearing transport
    // proof). Any CSW reply (even a NOT-READY status while a fresh medium spins
    // up) means the framing round-tripped; retry a few times for status 0.
    let mut bot_ok = false;
    for _ in 0..5 {
        match bot_command(usb_ep, notice, &cdb_test_unit_ready(), false, 0) {
            Some((_, status)) => {
                bot_ok = true;
                if status == 0 {
                    break;
                }
                let _ = syscall_lib::nanosleep_for(0, 20_000_000);
            }
            None => break,
        }
    }
    if bot_ok {
        // The validated D.1 milestone: a CBW/CSW BOT round-trip over the Phase 96
        // bulk pair, on a real (SuperSpeed) device.
        write_str(STDOUT_FILENO, "USB_STORAGE:bot-ok\n");
    } else {
        write_str(STDOUT_FILENO, "usb-storage: BOT round-trip failed\n");
        return false;
    }

    // INQUIRY — device identity. The data-IN phase rides the synchronous
    // single-TRB `SubmitBulkIn` path (Phase 92 Track D), which arms exactly one
    // bulk-IN TRB per phase and never leaves a surplus TRB queued — closing the
    // Phase 96 streaming-path gap where the device STALLed the bulk-IN endpoint
    // on the surplus IN tokens issued after its data + CSW.
    let (inq_data, inq_status) = match bot_command(
        usb_ep,
        notice,
        &cdb_inquiry(INQUIRY_LEN as u8),
        true,
        INQUIRY_LEN,
    ) {
        Some(r) => r,
        None => {
            write_str(STDOUT_FILENO, "usb-storage: INQUIRY transport failed\n");
            return false;
        }
    };
    if inq_status != 0 {
        write_str(STDOUT_FILENO, "usb-storage: INQUIRY CSW status nonzero\n");
        return false;
    }
    if let Some(inq) = InquiryData::parse(&inq_data) {
        write_str(STDOUT_FILENO, "usb-storage: INQUIRY type=");
        write_u8_dec(inq.peripheral_device_type);
        write_str(
            STDOUT_FILENO,
            if inq.removable {
                " removable"
            } else {
                " fixed"
            },
        );
        write_str(STDOUT_FILENO, "\n");
        if inq.peripheral_device_type != SCSI_TYPE_DIRECT_ACCESS {
            write_str(
                STDOUT_FILENO,
                "usb-storage: not a direct-access block device — skipping\n",
            );
            return false;
        }
    }

    // READ CAPACITY(10) — block count + block size.
    let (cap_data, cap_status) = match bot_command(
        usb_ep,
        notice,
        &cdb_read_capacity10(),
        true,
        READ_CAPACITY10_LEN,
    ) {
        Some(r) => r,
        None => {
            write_str(
                STDOUT_FILENO,
                "usb-storage: READ CAPACITY transport failed\n",
            );
            return false;
        }
    };
    if cap_status != 0 {
        write_str(
            STDOUT_FILENO,
            "usb-storage: READ CAPACITY CSW status nonzero\n",
        );
        return false;
    }
    let Some(cap) = ReadCapacity10::parse(&cap_data) else {
        write_str(STDOUT_FILENO, "usb-storage: READ CAPACITY parse failed\n");
        return false;
    };
    // `last_lba` is the last addressable block, so block count = last_lba + 1.
    let blocks = cap.last_lba.wrapping_add(1);
    write_str(STDOUT_FILENO, "USB_MASS_STORAGE:ready blocks=");
    write_u32_dec(blocks);
    write_str(STDOUT_FILENO, " bsize=");
    write_u32_dec(cap.block_size);
    write_str(STDOUT_FILENO, "\n");
    true
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    if !syscall_lib::ipc_wait_service(USB_SERVICE_NAME, 10_000) {
        write_str(
            STDOUT_FILENO,
            "usb-storage: 'usb' service never appeared — exiting cleanly\n",
        );
        return 0;
    }
    let Some(usb_ep) = lookup(USB_SERVICE_NAME) else {
        write_str(
            STDOUT_FILENO,
            "usb-storage: 'usb' lookup failed — exiting\n",
        );
        return 0;
    };

    // Walk the NextAttach cursor for a mass-storage interface with a bulk
    // IN+OUT pair (D.1 guards: a device with no bulk pair is rejected, not
    // crashed).
    let mut bound: Vec<AttachNotice> = Vec::new();
    let mut cursor = 0u8;
    while let Some(UsbReply::Attach {
        notice: Some(notice),
    }) = usb_call(usb_ep, &UsbRequest::NextAttach { cursor })
    {
        cursor = cursor.saturating_add(1);
        if notice.attached
            && notice.interface_class == CLASS_MASS_STORAGE
            && notice.bulk_in_dci != 0
            && notice.bulk_out_dci != 0
        {
            write_str(STDOUT_FILENO, "usb-storage: bound mass-storage slot=");
            write_u8_dec(notice.slot_id);
            write_str(STDOUT_FILENO, " in_dci=");
            write_u8_dec(notice.bulk_in_dci);
            write_str(STDOUT_FILENO, " out_dci=");
            write_u8_dec(notice.bulk_out_dci);
            write_str(STDOUT_FILENO, "\n");
            bound.push(notice);
        }
    }

    if bound.is_empty() {
        write_str(
            STDOUT_FILENO,
            "usb-storage: no mass-storage device attached — exiting cleanly\n",
        );
        return 0;
    }

    for notice in &bound {
        let _ = probe_device(usb_ep, notice);
    }

    // The D.1 milestone is the identity/capacity read above. The block-device
    // facade + mount (D.4) keep this process resident; until then, exit cleanly
    // so the service manager does not treat completion as a crash.
    0
}
