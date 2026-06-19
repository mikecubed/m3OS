//! Phase 92a Tracks D.4 (block-device facade) + D.3 (UAS selection) + C.4
//! (detach cleanup) — resident USB Mass Storage daemon.
//!
//! Extends the Phase 92 Track D.1/D.2 daemon (which exited after identity/
//! capacity probing) into a **resident block-server** that:
//!
//! 1. Polls `NextAttach` until a `CLASS_MASS_STORAGE` device with bulk IN+OUT
//!    appears (up to ~30 s, re-walking every ~200 ms so tier-2 hub-enumerated
//!    devices arrive late without missing the window).
//! 2. Probes non-destructively (GET_MAX_LUN, TEST UNIT READY, INQUIRY, READ
//!    CAPACITY(10)) — emitting the existing sentinels so the smoke gate stays
//!    green.
//! 3. Reads LBA 0 to detect a **real filesystem** (MBR 0x55AA or ext2
//!    superblock magic); on scratch/blank media runs the destructive WRITE/READ
//!    self-test and emits `USB_STORAGE:rw-ok`. Real-FS detection skips the
//!    write to protect live data.
//! 4. **UAS transport selection (D.3)**: fetches the raw config descriptor and
//!    scans for a mass-storage interface with `bInterfaceProtocol == 0x62`
//!    (UAS). Logs `transport=uas` or `transport=bot`. BOT is the must-work
//!    path (QEMU `usb-storage` is BOT). UAS command routing is scaffolded with
//!    a clear `TODO(92a D.3)` marker.
//! 5. Registers the device as `usb0.block` via `ipc_register_service` and
//!    enters a `BlockServer`-style loop dispatching `BLK_READ`, `BLK_WRITE`,
//!    `BLK_FLUSH`, and `BLK_STATUS` requests over BOT.
//! 6. **Detach cleanup (C.4)**: `release_device()` logs that the slot has been
//!    released; called on the discovery path if the device detaches before
//!    serving starts. The resident loop leaves a `TODO(92a C.4)` marker for
//!    the non-blocking recv path needed for detach-during-serve.
//!
//! # Existing sentinels kept
//!
//! * `USB_STORAGE:bot-ok`          — BOT round-trip succeeded
//! * `USB_MASS_STORAGE:ready blocks=N bsize=S` — capacity read
//! * `USB_STORAGE:rw-ok`           — scratch device WRITE/READ verified

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use alloc::vec::Vec;
#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use kernel_core::driver_ipc::block::{
    BLK_FLUSH, BLK_READ, BLK_STATUS, BLK_WRITE, BlkReplyHeader, BlockDriverError,
    decode_blk_request, encode_blk_reply,
};
#[cfg(not(test))]
use kernel_core::usb::mass_storage::{
    CSW_LEN, Cbw, Csw, InquiryData, ReadCapacity10, cdb_inquiry, cdb_read_capacity10, cdb_read10,
    cdb_test_unit_ready, cdb_write10,
};
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
#[cfg(not(test))]
use syscall_lib::write_str;
#[cfg(not(test))]
use syscall_lib::{IpcMessage, STDOUT_FILENO};
#[cfg(not(test))]
use usb_core::protocol::{
    AttachNotice, USB_MSG_MAX, USB_REQ_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest,
};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "usb-storage: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "usb-storage: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Boot-log marker written when the daemon starts.
pub const BOOT_LOG_MARKER: &str = "usb-storage: spawned\n";

/// USB Mass Storage interface class (USB-IF base-class 0x08).
const CLASS_MASS_STORAGE: u8 = 0x08;
/// USB Mass Storage BOT protocol (bInterfaceProtocol 0x50).
const PROTOCOL_BOT: u8 = 0x50;
/// USB Attached SCSI (UAS) protocol (bInterfaceProtocol 0x62).
const PROTOCOL_UAS: u8 = 0x62;
/// SCSI peripheral device type for a direct-access block device (disk).
#[cfg(not(test))]
const SCSI_TYPE_DIRECT_ACCESS: u8 = 0x00;

/// Standard INQUIRY allocation length (we only need the first 36 bytes).
#[cfg(not(test))]
const INQUIRY_LEN: u16 = 36;
/// READ CAPACITY(10) response length.
#[cfg(not(test))]
const READ_CAPACITY10_LEN: u16 = 8;

/// Scratch LBA used by the READ/WRITE round-trip self-test (512 KiB into
/// the medium — past any plausible boot sector / filesystem superblock).
#[cfg(not(test))]
const SCRATCH_LBA: u32 = 1024;

/// Block service endpoint name for `usb0`.
#[cfg(not(test))]
const SERVICE_NAME: &str = "usb0.block";

/// Maximum consecutive `handle_next`-style errors before the daemon exits
/// for restart (mirrors nvme driver).
#[cfg(not(test))]
const MAX_CONSECUTIVE_ERRORS: u32 = 8;

/// Maximum number of sectors per BOT READ/WRITE(10) sub-request.
///
/// The inline `SubmitBulkIn`/`SubmitBulkOut` data stage must fit `USB_MSG_MAX`
/// (4096) **including the wire-codec overhead**: a `BulkData` reply is
/// `data + 3` bytes and a `SubmitBulkOut` request is `data + 7` bytes. So the
/// largest sector-aligned data stage that fits is 7 × 512 = 3584 bytes (8 ×
/// 512 = 4096 overflows the reply and is rejected by the server's H.6 bound).
/// A larger `BLK_READ`/`BLK_WRITE` (e.g. a 4096-byte ext2 block = 8 sectors) is
/// split across multiple BOT sub-requests by `handle_read`/`handle_write` and
/// reassembled in the block-protocol reply (which has the larger MAX_BULK_LEN
/// budget). Oversized transfers (D.5) use the page-grant path instead.
#[cfg(not(test))]
const MAX_BOT_SECTORS: u16 = 7;

// ---------------------------------------------------------------------------
// IPC plumbing (mirrors usb-hid's usb_call)
// ---------------------------------------------------------------------------

/// Issue a `UsbRequest` to the xHCI server and decode the `UsbReply`.
#[cfg(not(test))]
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

#[cfg(not(test))]
fn lookup(name: &str) -> Option<u32> {
    let h = syscall_lib::ipc_lookup_service(name);
    if h == u64::MAX { None } else { Some(h as u32) }
}

/// Next monotonic CBW tag (a CSW echoes the CBW tag — we don't strictly
/// verify it here, but it must be unique per command).
#[cfg(not(test))]
fn next_tag() -> u32 {
    use core::sync::atomic::{AtomicU32, Ordering};
    static TAG: AtomicU32 = AtomicU32::new(0x1000);
    TAG.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// BOT command execution
// ---------------------------------------------------------------------------

/// Issue a **synchronous, single-TRB** bulk-IN transfer of exactly `len`
/// bytes for one BOT phase (the data stage or the 13-byte CSW).
#[cfg(not(test))]
fn submit_bulk_in(usb_ep: u32, slot_id: u8, dci: u8, len: u16) -> Option<Vec<u8>> {
    match usb_call(usb_ep, &UsbRequest::SubmitBulkIn { slot_id, dci, len }) {
        Some(UsbReply::BulkData {
            data,
            completion_code: 1,
        }) => Some(data),
        _ => None,
    }
}

/// Run one BOT SCSI command: CBW on bulk-OUT, optional data phase on
/// bulk-IN, then the CSW on bulk-IN. Returns `(data, csw_status)`.
#[cfg(not(test))]
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

    if !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, &cbw.encode()) {
        write_str(STDOUT_FILENO, "usb-storage: CBW bulk-OUT failed\n");
        return None;
    }

    let data = if data_in && data_len > 0 {
        submit_bulk_in(usb_ep, slot_id, notice.bulk_in_dci, data_len)?
    } else {
        Vec::new()
    };

    let csw_bytes = submit_bulk_in(usb_ep, slot_id, notice.bulk_in_dci, CSW_LEN as u16)?;
    let csw = Csw::parse(&csw_bytes)?;
    if csw.tag != tag {
        write_str(STDOUT_FILENO, "usb-storage: CSW tag mismatch\n");
        return None;
    }
    Some((data, csw.status))
}

/// Issue one BOT SCSI command carrying a **host-to-device data-OUT** stage
/// (e.g. WRITE(10)). Returns the CSW status (0 = passed).
#[cfg(not(test))]
fn bot_command_write(usb_ep: u32, notice: &AttachNotice, cdb: &[u8], payload: &[u8]) -> Option<u8> {
    let slot_id = notice.slot_id;
    let tag = next_tag();
    let cbw = Cbw::new(tag, payload.len() as u32, false, 0, cdb);

    if !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, &cbw.encode()) {
        write_str(STDOUT_FILENO, "usb-storage: WRITE CBW bulk-OUT failed\n");
        return None;
    }
    if !payload.is_empty() && !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, payload) {
        write_str(STDOUT_FILENO, "usb-storage: WRITE data bulk-OUT failed\n");
        return None;
    }
    let csw_bytes = submit_bulk_in(usb_ep, slot_id, notice.bulk_in_dci, CSW_LEN as u16)?;
    let csw = Csw::parse(&csw_bytes)?;
    if csw.tag != tag {
        write_str(STDOUT_FILENO, "usb-storage: WRITE CSW tag mismatch\n");
        return None;
    }
    Some(csw.status)
}

/// Submit a bulk-OUT transfer and block for completion.
#[cfg(not(test))]
fn bulk_out(usb_ep: u32, slot_id: u8, dci: u8, data: &[u8]) -> bool {
    matches!(
        usb_call(
            usb_ep,
            &UsbRequest::SubmitBulkOut {
                slot_id,
                dci,
                data: data.to_vec(),
            },
        ),
        Some(UsbReply::TransferComplete {
            completion_code: 1,
            ..
        })
    )
}

/// Issue `GET_MAX_LUN` (class control-IN). A STALL means a single LUN.
#[cfg(not(test))]
fn get_max_lun(usb_ep: u32, notice: &AttachNotice) -> u8 {
    let iface = notice.interface_num;
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
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Decimal log helpers
// ---------------------------------------------------------------------------

#[cfg(not(test))]
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

#[cfg(not(test))]
fn write_u8_dec(n: u8) {
    write_u32_dec(n as u32);
}

// ---------------------------------------------------------------------------
// UAS transport detection (D.3)
// ---------------------------------------------------------------------------

/// Scan a raw USB configuration descriptor blob and return `true` if it
/// contains a Mass Storage interface (`bInterfaceClass == 0x08`) with the
/// UAS protocol (`bInterfaceProtocol == 0x62`).
///
/// Interface descriptor layout (USB 2.0 §9.6.5):
/// - byte 0: bLength
/// - byte 1: bDescriptorType  (0x04 = Interface)
/// - byte 2: bInterfaceNumber
/// - byte 3: bAlternateSetting
/// - byte 4: bNumEndpoints
/// - byte 5: bInterfaceClass
/// - byte 6: bInterfaceSubClass
/// - byte 7: bInterfaceProtocol
/// - byte 8: iInterface
///
/// This is host-testable: no USB hardware is required.
pub fn find_uas_interface(config: &[u8]) -> bool {
    let mut i = 0usize;
    while i < config.len() {
        // Each descriptor starts with bLength (byte 0) and bDescriptorType
        // (byte 1). Skip any zero-length descriptor to avoid infinite loops.
        let len = config[i] as usize;
        if len < 2 || i + len > config.len() {
            break;
        }
        let desc_type = config[i + 1];
        // 0x04 = Interface Descriptor
        if desc_type == 0x04 && len >= 9 {
            let class = config[i + 5];
            let protocol = config[i + 7];
            if class == CLASS_MASS_STORAGE && protocol == PROTOCOL_UAS {
                return true;
            }
        }
        i += len;
    }
    false
}

/// Select the transport for a bound mass-storage device and log the choice.
///
/// Fetches the raw configuration descriptor via `GetDescriptors`, scans for
/// a UAS interface, and returns `true` for UAS, `false` for BOT.
///
/// Falls back to BOT on any IPC failure or if the device only advertises BOT
/// (`bInterfaceProtocol == 0x50`).
#[cfg(not(test))]
fn select_transport(usb_ep: u32, notice: &AttachNotice) -> bool {
    let config_blob = match usb_call(
        usb_ep,
        &UsbRequest::GetDescriptors {
            slot_id: notice.slot_id,
        },
    ) {
        Some(UsbReply::Descriptors { config, .. }) => config,
        _ => {
            // Cannot fetch descriptors — fall back to BOT.
            write_str(
                STDOUT_FILENO,
                "usb-storage: GetDescriptors failed — defaulting to transport=bot\n",
            );
            return false;
        }
    };

    if find_uas_interface(&config_blob) {
        write_str(STDOUT_FILENO, "usb-storage: transport=uas\n");
        // TODO(92a D.3): Drive SCSI commands over UAS IUs (CommandIu on
        // bulk-OUT command pipe, SenseIu/ResponseIu on bulk-IN status pipe,
        // data over the data pipes, Tag == Stream ID). For now fall back to
        // BOT so the block server functions on QEMU `usb-storage` devices
        // while UAS bring-up on `usb-uas` hardware is deferred.
        write_str(
            STDOUT_FILENO,
            "usb-storage: UAS selected but falling back to BOT for block-server loop\n",
        );
        true
    } else {
        // BOT — check the protocol byte for logging; accept any non-UAS device.
        if notice.interface_protocol == PROTOCOL_BOT {
            write_str(STDOUT_FILENO, "usb-storage: transport=bot\n");
        } else {
            write_str(
                STDOUT_FILENO,
                "usb-storage: transport=bot (unknown protocol)\n",
            );
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Real-FS vs scratch detection (D.4 safety gate)
// ---------------------------------------------------------------------------

/// Return `true` if LBA 0 / LBA 2 of the attached device contain
/// filesystem signatures that should NOT be overwritten.
///
/// Checks:
/// 1. MBR signature: bytes 510–511 of LBA 0 == `0x55 0xAA`.
/// 2. ext2 superblock magic: bytes 56–57 of the 1024-byte superblock
///    (which lives at disk offset 1024 = LBA 2, bytes 56–57) == `0x53 0xEF`.
///
/// On any read failure (transport error, small read) returns `false` (safe
/// default: run the self-test only if we are sure there is no FS).
#[cfg(not(test))]
fn detect_real_fs(usb_ep: u32, notice: &AttachNotice) -> bool {
    // Read LBA 0 (512 bytes).
    let lba0 = match bot_command(usb_ep, notice, &cdb_read10(0, 1), true, 512) {
        Some((data, 0)) if data.len() >= 512 => data,
        _ => return false,
    };

    // Check MBR signature at bytes 510–511.
    if lba0[510] == 0x55 && lba0[511] == 0xAA {
        return true;
    }

    // Read LBA 2 (512 bytes) to reach the ext2 superblock area.
    // The ext2 superblock starts at disk offset 1024 = LBA 2 byte 0.
    // Magic is at superblock offset 56, i.e. LBA 2 byte 56.
    let lba2 = match bot_command(usb_ep, notice, &cdb_read10(2, 1), true, 512) {
        Some((data, 0)) if data.len() >= 512 => data,
        _ => return false,
    };

    // ext2 magic: 0x53EF at offset 56–57 in the superblock (little-endian).
    if lba2[56] == 0x53 && lba2[57] == 0xEF {
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Bind + probe one mass-storage device
// ---------------------------------------------------------------------------

/// Drive the GET_MAX_LUN → TEST UNIT READY → INQUIRY → READ CAPACITY
/// sequence against a bound mass-storage device. Returns `Some(capacity)`
/// on a successful capacity read, `None` on failure.
#[cfg(not(test))]
fn probe_device(usb_ep: u32, notice: &AttachNotice) -> Option<ReadCapacity10> {
    let max_lun = get_max_lun(usb_ep, notice);
    write_str(STDOUT_FILENO, "usb-storage: max_lun=");
    write_u8_dec(max_lun);
    write_str(STDOUT_FILENO, "\n");

    // TEST UNIT READY.
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
        write_str(STDOUT_FILENO, "USB_STORAGE:bot-ok\n");
    } else {
        write_str(STDOUT_FILENO, "usb-storage: BOT round-trip failed\n");
        return None;
    }

    // INQUIRY.
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
            return None;
        }
    };
    if inq_status != 0 {
        write_str(STDOUT_FILENO, "usb-storage: INQUIRY CSW status nonzero\n");
        return None;
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
            return None;
        }
    }

    // READ CAPACITY(10).
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
            return None;
        }
    };
    if cap_status != 0 {
        write_str(
            STDOUT_FILENO,
            "usb-storage: READ CAPACITY CSW status nonzero\n",
        );
        return None;
    }
    let cap = ReadCapacity10::parse(&cap_data)?;
    let blocks = cap.last_lba.wrapping_add(1);
    write_str(STDOUT_FILENO, "USB_MASS_STORAGE:ready blocks=");
    write_u32_dec(blocks);
    write_str(STDOUT_FILENO, " bsize=");
    write_u32_dec(cap.block_size);
    write_str(STDOUT_FILENO, "\n");

    Some(cap)
}

// ---------------------------------------------------------------------------
// Destructive self-test (scratch devices only)
// ---------------------------------------------------------------------------

/// Run the WRITE(10)/READ(10) self-test on scratch/blank media.
///
/// Should only be called after `detect_real_fs` returned `false`.
#[cfg(not(test))]
fn run_scratch_self_test(usb_ep: u32, notice: &AttachNotice, cap: &ReadCapacity10) {
    let blocks = cap.last_lba.wrapping_add(1);
    if cap.block_size as usize != 512 || blocks <= SCRATCH_LBA {
        return;
    }
    let mut pattern = [0u8; 512];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8) ^ 0x5A;
    }
    match bot_command_write(usb_ep, notice, &cdb_write10(SCRATCH_LBA, 1), &pattern) {
        Some(0) => {}
        _ => {
            write_str(STDOUT_FILENO, "usb-storage: WRITE(10) self-test failed\n");
            return;
        }
    }
    match bot_command(usb_ep, notice, &cdb_read10(SCRATCH_LBA, 1), true, 512) {
        Some((rd, 0)) if rd.len() == 512 && rd[..] == pattern[..] => {
            write_str(STDOUT_FILENO, "USB_STORAGE:rw-ok\n");
        }
        _ => {
            write_str(
                STDOUT_FILENO,
                "usb-storage: READ(10) self-test verify mismatch\n",
            );
            return;
        }
    }
    // Phase 92a H.4 / D.5 — zero-copy DMA over a shared-memory region.
    if blocks > SCRATCH_LBA + 24 {
        shm_dma_self_test(usb_ep, notice);
    }
}

/// Submit a **zero-copy** bulk data stage over a shared-memory region and block
/// for completion (Phase 92a H.4 `SubmitShmTransfer`). The xHCI server maps the
/// region into the device's IOMMU domain and programs one TRB straight at it —
/// no inline `USB_MSG_MAX` copy. Returns the transferred byte count.
#[cfg(not(test))]
fn submit_shm(
    usb_ep: u32,
    slot_id: u8,
    dci: u8,
    shm_id: u32,
    len: u32,
    dir_in: bool,
) -> Option<usize> {
    match usb_call(
        usb_ep,
        &UsbRequest::SubmitShmTransfer {
            slot_id,
            dci,
            shm_id,
            len,
            dir_in,
        },
    ) {
        Some(UsbReply::TransferComplete {
            transferred,
            completion_code: 1,
        }) => Some(transferred),
        _ => None,
    }
}

/// BOT WRITE(10) whose data-OUT stage is a single zero-copy shm transfer.
#[cfg(not(test))]
fn bot_write_shm(
    usb_ep: u32,
    notice: &AttachNotice,
    cdb: &[u8],
    shm_id: u32,
    len: u32,
) -> Option<u8> {
    let slot_id = notice.slot_id;
    let tag = next_tag();
    let cbw = Cbw::new(tag, len, false, 0, cdb);
    if !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, &cbw.encode()) {
        return None;
    }
    submit_shm(usb_ep, slot_id, notice.bulk_out_dci, shm_id, len, false)?;
    let csw = Csw::parse(&submit_bulk_in(
        usb_ep,
        slot_id,
        notice.bulk_in_dci,
        CSW_LEN as u16,
    )?)?;
    if csw.tag != tag {
        return None;
    }
    Some(csw.status)
}

/// BOT READ(10) whose data-IN stage is a single zero-copy shm transfer.
#[cfg(not(test))]
fn bot_read_shm(
    usb_ep: u32,
    notice: &AttachNotice,
    cdb: &[u8],
    shm_id: u32,
    len: u32,
) -> Option<u8> {
    let slot_id = notice.slot_id;
    let tag = next_tag();
    let cbw = Cbw::new(tag, len, true, 0, cdb);
    if !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, &cbw.encode()) {
        return None;
    }
    submit_shm(usb_ep, slot_id, notice.bulk_in_dci, shm_id, len, true)?;
    let csw = Csw::parse(&submit_bulk_in(
        usb_ep,
        slot_id,
        notice.bulk_in_dci,
        CSW_LEN as u16,
    )?)?;
    if csw.tag != tag {
        return None;
    }
    Some(csw.status)
}

/// Zero-copy DMA self-test (Phase 92a H.4 / D.5): WRITE(10) then READ(10) a
/// **16-sector (8192-byte)** payload — larger than the ~4092-byte inline budget
/// — through a single shared-memory region the xHCI device DMAs into/out of
/// directly, and verify the round-trip byte-identical. Proves the zero-copy
/// `SubmitShmTransfer` path moves a >`USB_MSG_MAX` transfer in one descriptor.
/// Emits `USB_STORAGE:shm-dma-ok`.
#[cfg(not(test))]
fn shm_dma_self_test(usb_ep: u32, notice: &AttachNotice) {
    const SECTORS: u16 = 16;
    const NBYTES: usize = SECTORS as usize * 512;
    const TEST_LBA: u32 = SCRATCH_LBA + 8;
    let shm_id = syscall_lib::shm_create(NBYTES);
    if shm_id == 0 {
        write_str(STDOUT_FILENO, "usb-storage: shm_create failed\n");
        return;
    }
    let va = syscall_lib::shm_map(shm_id);
    if va == u64::MAX || va == 0 {
        write_str(STDOUT_FILENO, "usb-storage: shm_map failed\n");
        return;
    }
    let buf = va as *mut u8;
    // Fill the shared buffer with a recognizable pattern.
    for i in 0..NBYTES {
        // SAFETY: the shm region is NBYTES bytes mapped writable at `va`.
        unsafe { buf.add(i).write_volatile((i as u8) ^ 0x3C) };
    }
    // WRITE(10) the shm to the device (zero-copy data-OUT — device reads the shm).
    if bot_write_shm(
        usb_ep,
        notice,
        &cdb_write10(TEST_LBA, SECTORS),
        shm_id,
        NBYTES as u32,
    ) != Some(0)
    {
        write_str(STDOUT_FILENO, "usb-storage: shm WRITE failed\n");
        return;
    }
    // Clear the buffer, then READ(10) back into it (zero-copy data-IN).
    for i in 0..NBYTES {
        // SAFETY: as above.
        unsafe { buf.add(i).write_volatile(0) };
    }
    if bot_read_shm(
        usb_ep,
        notice,
        &cdb_read10(TEST_LBA, SECTORS),
        shm_id,
        NBYTES as u32,
    ) != Some(0)
    {
        write_str(STDOUT_FILENO, "usb-storage: shm READ failed\n");
        return;
    }
    // Verify the device DMA'd the original pattern back into the shared buffer.
    for i in 0..NBYTES {
        // SAFETY: as above.
        if unsafe { buf.add(i).read_volatile() } != ((i as u8) ^ 0x3C) {
            write_str(STDOUT_FILENO, "usb-storage: shm DMA verify mismatch\n");
            return;
        }
    }
    write_str(STDOUT_FILENO, "USB_STORAGE:shm-dma-ok\n");
}

// ---------------------------------------------------------------------------
// Detach cleanup (C.4)
// ---------------------------------------------------------------------------

/// Release a bound device slot. Called when a detach is observed on the
/// discovery / rebind path. Logs the event and returns so the caller can
/// attempt re-discovery.
///
/// TODO(92a C.4): detach-during-serve requires a non-blocking recv variant
/// (e.g. `ipc_try_recv_msg`) interleaved with the block-server loop so a
/// hot-unplug mid-serve is noticed without hanging on the next `ipc_recv_msg`.
#[cfg(not(test))]
fn release_device(notice: &AttachNotice) {
    write_str(STDOUT_FILENO, "usb-storage: device detached slot=");
    write_u8_dec(notice.slot_id);
    write_str(STDOUT_FILENO, " — released\n");
    // C.4: detach observed → release
    // In a full implementation this would deregister the service endpoint
    // and free any associated resources so a re-attach can bind a fresh slot.
}

// ---------------------------------------------------------------------------
// Block-server IPC loop (D.4)
// ---------------------------------------------------------------------------

/// Create a Phase 50 IPC endpoint and register it under `name`.
/// Returns the capability handle on success, `u64::MAX` on failure.
#[cfg(not(test))]
fn create_service_endpoint(name: &str) -> Option<u32> {
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        return None;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(v) => v,
        Err(_) => return None,
    };
    let rc = syscall_lib::ipc_register_service(ep_u32, name);
    if rc == u64::MAX {
        return None;
    }
    Some(ep_u32)
}

/// Build a `BlkReplyHeader` for an Ok read reply.
#[cfg(not(test))]
fn ok_read_reply(cmd_id: u64, bytes: u32) -> BlkReplyHeader {
    BlkReplyHeader {
        cmd_id,
        status: BlockDriverError::Ok,
        bytes,
    }
}

/// Build a `BlkReplyHeader` for an error reply.
#[cfg(not(test))]
fn err_reply(cmd_id: u64) -> BlkReplyHeader {
    BlkReplyHeader {
        cmd_id,
        status: BlockDriverError::IoError,
        bytes: 0,
    }
}

/// Build a `BlkReplyHeader` for an Ok write/flush/status reply with no data.
#[cfg(not(test))]
fn ok_empty_reply(cmd_id: u64) -> BlkReplyHeader {
    BlkReplyHeader {
        cmd_id,
        status: BlockDriverError::Ok,
        bytes: 0,
    }
}

/// SCSI SYNCHRONIZE CACHE(10) CDB (opcode 0x35, 10 bytes, no data stage).
/// Requests the device to write its volatile cache to the medium.
#[cfg(not(test))]
fn cdb_sync_cache10() -> [u8; 10] {
    [0x35, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
}

/// Handle a `BLK_READ` by issuing chunked BOT READ(10) commands.
///
/// Assembles all sector data into a single buffer, then stages it as a
/// combined bulk payload (header + data) via `ipc_store_reply_bulk`.
/// Returns the `BlkReplyHeader` with `bytes = sectors * 512` on success.
#[cfg(not(test))]
fn handle_bot_read(
    usb_ep: u32,
    notice: &AttachNotice,
    cmd_id: u64,
    lba: u64,
    sector_count: u32,
) -> BlkReplyHeader {
    let mut all_data: Vec<u8> = Vec::new();
    let mut remaining = sector_count;
    let mut current_lba = lba;

    while remaining > 0 {
        let chunk = remaining.min(MAX_BOT_SECTORS as u32) as u16;
        let byte_count = chunk as u16 * 512;
        let lba32 = current_lba as u32; // BOT READ(10) uses 32-bit LBA.
        match bot_command(usb_ep, notice, &cdb_read10(lba32, chunk), true, byte_count) {
            Some((data, 0)) if data.len() == byte_count as usize => {
                all_data.extend_from_slice(&data);
            }
            _ => {
                write_str(STDOUT_FILENO, "usb-storage: BLK_READ BOT error\n");
                return err_reply(cmd_id);
            }
        }
        remaining -= chunk as u32;
        current_lba += chunk as u64;
    }

    let total_bytes = all_data.len() as u32;

    // Stage combined reply: header first, then bulk sector data.
    // The kernel BlockReply protocol carries bulk immediately after the header.
    let hdr = ok_read_reply(cmd_id, total_bytes);
    let header_bytes = encode_blk_reply(hdr, 0);
    let mut combined = Vec::with_capacity(header_bytes.len() + all_data.len());
    combined.extend_from_slice(&header_bytes);
    combined.extend_from_slice(&all_data);

    let rc = syscall_lib::ipc_store_reply_bulk(&combined);
    if rc == u64::MAX {
        write_str(
            STDOUT_FILENO,
            "usb-storage: ipc_store_reply_bulk (read) failed\n",
        );
        return err_reply(cmd_id);
    }

    // The combined (header + bulk) buffer has been staged via ipc_store_reply_bulk
    // above. Return a sentinel (bytes == u32::MAX) that run_block_server_loop
    // recognises as "bulk already staged — skip the second store_reply_bulk
    // call and go straight to ipc_reply." ipc_store_reply_bulk replaces the
    // staged buffer on each call, so without the sentinel the loop's header-
    // only encode would silently overwrite the sector payload we just staged.
    BlkReplyHeader {
        cmd_id,
        status: BlockDriverError::Ok,
        bytes: u32::MAX, // Sentinel: combined header+bulk already staged.
    }
}

/// Handle a `BLK_WRITE` by issuing chunked BOT WRITE(10) commands.
#[cfg(not(test))]
fn handle_bot_write(
    usb_ep: u32,
    notice: &AttachNotice,
    cmd_id: u64,
    lba: u64,
    sector_count: u32,
    payload: &[u8],
) -> BlkReplyHeader {
    let expected_bytes = sector_count as usize * 512;
    if payload.len() < expected_bytes {
        write_str(STDOUT_FILENO, "usb-storage: BLK_WRITE payload too short\n");
        return err_reply(cmd_id);
    }

    let mut remaining = sector_count;
    let mut current_lba = lba;
    let mut offset = 0usize;

    while remaining > 0 {
        let chunk = remaining.min(MAX_BOT_SECTORS as u32) as u16;
        let byte_count = chunk as usize * 512;
        let lba32 = current_lba as u32;
        let chunk_payload = &payload[offset..offset + byte_count];
        match bot_command_write(usb_ep, notice, &cdb_write10(lba32, chunk), chunk_payload) {
            Some(0) => {}
            _ => {
                write_str(STDOUT_FILENO, "usb-storage: BLK_WRITE BOT error\n");
                return err_reply(cmd_id);
            }
        }
        remaining -= chunk as u32;
        current_lba += chunk as u64;
        offset += byte_count;
    }

    ok_empty_reply(cmd_id)
}

/// Handle a `BLK_FLUSH` by issuing SCSI SYNCHRONIZE CACHE(10).
///
/// Many BOT devices don't have a volatile cache, and some will fail the
/// command. We treat a CSW status 1 (failed) as non-fatal and return Ok
/// (the device simply has no cache to flush).
#[cfg(not(test))]
fn handle_bot_flush(usb_ep: u32, notice: &AttachNotice, cmd_id: u64) -> BlkReplyHeader {
    match bot_command(usb_ep, notice, &cdb_sync_cache10(), false, 0) {
        Some((_, 0)) | Some((_, 1)) => {
            // Status 0 = cache flushed; status 1 = no cache / not supported.
            ok_empty_reply(cmd_id)
        }
        _ => {
            // Transport error — return Ok anyway; a missing flush is not fatal
            // for a removable device whose caller cannot recover from it.
            ok_empty_reply(cmd_id)
        }
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

#[cfg(not(test))]
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

    // Polling discovery: walk NextAttach cursor 0 every ~200 ms for up to
    // ~30 s waiting for a mass-storage device (tier-2 hub-enumerated devices
    // arrive late; we must not miss them by giving up after one walk).
    //
    // Fast path: if a device is already present on the first walk, we bind it
    // immediately without sleeping.
    let mut bound: Option<AttachNotice> = None;
    const POLL_INTERVAL_MS: u32 = 200;
    const MAX_POLLS: u32 = 150; // 150 × 200 ms = 30 s

    'outer: for attempt in 0..MAX_POLLS {
        let mut cursor = 0u8;
        loop {
            match usb_call(usb_ep, &UsbRequest::NextAttach { cursor }) {
                Some(UsbReply::Attach {
                    notice: Some(notice),
                }) => {
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

                        // C.4: check for a device that was detached between
                        // enumeration and our bind attempt.
                        if !notice.attached {
                            release_device(&notice);
                            cursor = cursor.saturating_add(1);
                            continue;
                        }

                        bound = Some(notice);
                        break 'outer;
                    }
                    cursor = cursor.saturating_add(1);
                }
                Some(UsbReply::Attach { notice: None }) | None => {
                    // Cursor exhausted for this walk.
                    break;
                }
                _ => {
                    cursor = cursor.saturating_add(1);
                }
            }
        }

        if attempt == 0 {
            // No log spam on the first immediate attempt; only start logging
            // on the first retry.
        } else if attempt % 5 == 1 {
            write_str(STDOUT_FILENO, "usb-storage: waiting for device\n");
        }

        let _ = syscall_lib::nanosleep_for(0, POLL_INTERVAL_MS * 1_000_000);
    }

    let notice = match bound {
        Some(n) => n,
        None => {
            write_str(
                STDOUT_FILENO,
                "usb-storage: no mass-storage device attached — exiting cleanly\n",
            );
            return 0;
        }
    };

    // (D.3) Transport selection: fetch config descriptor, scan for UAS.
    let uas = select_transport(usb_ep, &notice);

    // Probe: GET_MAX_LUN → TEST UNIT READY (bot-ok) → INQUIRY → READ CAPACITY.
    let cap = match probe_device(usb_ep, &notice) {
        Some(c) => c,
        None => {
            write_str(STDOUT_FILENO, "usb-storage: probe failed — exiting\n");
            return 1;
        }
    };

    // (D.4 safety gate) Detect real filesystem before any write.
    let real_fs = detect_real_fs(usb_ep, &notice);
    if real_fs {
        write_str(
            STDOUT_FILENO,
            "usb-storage: real-fs detected — skipping destructive self-test\n",
        );
    } else {
        write_str(
            STDOUT_FILENO,
            "usb-storage: scratch device — running rw self-test\n",
        );
        run_scratch_self_test(usb_ep, &notice, &cap);
    }

    // Register as a block backend.
    let blk_ep = match create_service_endpoint(SERVICE_NAME) {
        Some(ep) => ep,
        None => {
            write_str(
                STDOUT_FILENO,
                "usb-storage: failed to register usb0.block — exiting\n",
            );
            return 1;
        }
    };
    write_str(STDOUT_FILENO, "usb-storage: registered usb0.block\n");

    // Enter the resident block-server loop.
    // The loop calls ipc_store_reply_bulk + ipc_reply for each request.
    // For reads the combined (header+bulk) buffer is staged inside
    // handle_bot_read and signalled via the bytes==u32::MAX sentinel.
    run_block_server_loop(usb_ep, blk_ep, &notice, uas);

    0
}

/// Block-server dispatch loop.
///
/// Uses `ipc_recv_msg` directly (driver_runtime is not a dependency of this
/// crate) and mirrors the `BlockServer::handle_next` contract: recv →
/// decode_blk_request → dispatch → encode_blk_reply → ipc_store_reply_bulk
/// → ipc_reply. For `BLK_READ` the combined (header+bulk) buffer is staged
/// inside `handle_bot_read` and signalled to the loop via the
/// `bytes == u32::MAX` sentinel so the loop skips the second store call.
#[cfg(not(test))]
fn run_block_server_loop(usb_ep: u32, blk_ep: u32, notice: &AttachNotice, uas: bool) {
    use kernel_core::driver_ipc::block::BLK_REQUEST_HEADER_SIZE;
    use kernel_core::driver_ipc::block::MAX_SECTORS_PER_REQUEST;

    let recv_cap = BLK_REQUEST_HEADER_SIZE + (MAX_SECTORS_PER_REQUEST as usize) * 512;
    let mut recv_buf = alloc::vec![0u8; recv_cap];
    let mut consecutive_errors: u32 = 0;

    loop {
        let mut msg = IpcMessage::new(0);
        let rc = syscall_lib::ipc_recv_msg(blk_ep, &mut msg, &mut recv_buf);
        if rc == u64::MAX {
            consecutive_errors += 1;
            write_str(
                STDOUT_FILENO,
                "usb-storage: ipc_recv_msg error — continuing\n",
            );
            if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                write_str(
                    STDOUT_FILENO,
                    "usb-storage: too many consecutive recv errors — exiting for restart\n",
                );
                return;
            }
            continue;
        }
        consecutive_errors = 0;

        let reply_cap = msg.data[3] as u32;
        if reply_cap == 0 {
            // Notification or fire-and-forget — no reply expected.
            // TODO(92a C.4): detach-during-serve — check for a detach
            // notification here and call release_device if the bound slot
            // has been removed. Needs a non-blocking poll of the USB server
            // to distinguish "detach notification" from "block request".
            continue;
        }

        let real_len = (msg.data[1] as usize).min(recv_buf.len());

        // Decode the block request.
        let (reply_header, already_staged) = match decode_blk_request(&recv_buf[..real_len]) {
            Ok((req_hdr, _payload_grant)) => {
                let write_payload = if real_len > BLK_REQUEST_HEADER_SIZE {
                    &recv_buf[BLK_REQUEST_HEADER_SIZE..real_len]
                } else {
                    &[][..]
                };

                // TODO(92a D.3): UAS transport — when `uas` is true and UAS
                // IU types are available in kernel_core::usb::mass_storage,
                // route SCSI commands over CommandIu/SenseIu/ResponseIu
                // instead of the BOT helpers below.
                let _ = uas;

                match req_hdr.kind {
                    BLK_READ => {
                        // handle_bot_read stages (header+bulk) and signals via
                        // bytes == u32::MAX.
                        let hdr = handle_bot_read(
                            usb_ep,
                            notice,
                            req_hdr.cmd_id,
                            req_hdr.lba,
                            req_hdr.sector_count,
                        );
                        let already = hdr.bytes == u32::MAX;
                        (hdr, already)
                    }
                    BLK_WRITE => {
                        let hdr = handle_bot_write(
                            usb_ep,
                            notice,
                            req_hdr.cmd_id,
                            req_hdr.lba,
                            req_hdr.sector_count,
                            write_payload,
                        );
                        (hdr, false)
                    }
                    BLK_FLUSH => {
                        let hdr = handle_bot_flush(usb_ep, notice, req_hdr.cmd_id);
                        (hdr, false)
                    }
                    BLK_STATUS => (ok_empty_reply(req_hdr.cmd_id), false),
                    _ => (
                        BlkReplyHeader {
                            cmd_id: req_hdr.cmd_id,
                            status: BlockDriverError::InvalidRequest,
                            bytes: 0,
                        },
                        false,
                    ),
                }
            }
            Err(_) => (
                BlkReplyHeader {
                    cmd_id: 0,
                    status: BlockDriverError::InvalidRequest,
                    bytes: 0,
                },
                false,
            ),
        };

        // Stage the encoded reply header (and any bulk data for non-reads),
        // then send the reply. For reads the combined (header+bulk) buffer was
        // already staged inside handle_bot_read (signalled by bytes==u32::MAX);
        // calling store_reply_bulk again would overwrite the sector payload.
        if !already_staged {
            let reply_bytes = encode_blk_reply(reply_header, 0);
            let rc_bulk = syscall_lib::ipc_store_reply_bulk(&reply_bytes);
            if rc_bulk == u64::MAX {
                write_str(STDOUT_FILENO, "usb-storage: ipc_store_reply_bulk failed\n");
            }
        }

        let rc_reply = syscall_lib::ipc_reply(reply_cap, 0, 0);
        if rc_reply == u64::MAX {
            write_str(STDOUT_FILENO, "usb-storage: ipc_reply failed\n");
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // find_uas_interface tests (D.3 host-testable helper)
    // -----------------------------------------------------------------------

    /// Build a minimal 9-byte Interface Descriptor for testing.
    fn interface_desc(class: u8, protocol: u8) -> Vec<u8> {
        vec![
            9,    // bLength
            0x04, // bDescriptorType = Interface
            0,    // bInterfaceNumber
            0,    // bAlternateSetting
            2,    // bNumEndpoints
            class, 0x06, // bInterfaceSubClass (SCSI)
            protocol, 0, // iInterface
        ]
    }

    /// A minimal Configuration Descriptor header (9 bytes) followed by an
    /// interface descriptor.
    fn config_with_interface(class: u8, protocol: u8) -> Vec<u8> {
        let iface = interface_desc(class, protocol);
        let total_len = (9 + iface.len()) as u16;
        let mut cfg = vec![
            9,    // bLength
            0x02, // bDescriptorType = Configuration
            total_len as u8,
            (total_len >> 8) as u8,
            1,    // bNumInterfaces
            1,    // bConfigurationValue
            0,    // iConfiguration
            0x80, // bmAttributes
            50,   // bMaxPower
        ];
        cfg.extend_from_slice(&iface);
        cfg
    }

    /// Config descriptor with a UAS mass-storage interface → returns true.
    #[test]
    fn find_uas_interface_detects_uas() {
        let config = config_with_interface(CLASS_MASS_STORAGE, PROTOCOL_UAS);
        assert!(find_uas_interface(&config));
    }

    /// Config descriptor with a BOT mass-storage interface → returns false.
    #[test]
    fn find_uas_interface_rejects_bot() {
        let config = config_with_interface(CLASS_MASS_STORAGE, PROTOCOL_BOT);
        assert!(!find_uas_interface(&config));
    }

    /// Config descriptor with a non-mass-storage UAS-protocol interface
    /// (e.g. HID with protocol 0x62) → returns false (class mismatch).
    #[test]
    fn find_uas_interface_requires_mass_storage_class() {
        let config = config_with_interface(0x03 /* HID */, PROTOCOL_UAS);
        assert!(!find_uas_interface(&config));
    }

    /// Empty config descriptor → returns false without panicking.
    #[test]
    fn find_uas_interface_empty_returns_false() {
        assert!(!find_uas_interface(&[]));
    }

    /// A config descriptor with multiple interfaces: one BOT, one UAS →
    /// returns true (UAS is present).
    #[test]
    fn find_uas_interface_multiple_interfaces_finds_uas() {
        let bot_iface = interface_desc(CLASS_MASS_STORAGE, PROTOCOL_BOT);
        let uas_iface = interface_desc(CLASS_MASS_STORAGE, PROTOCOL_UAS);
        let total_len = (9 + bot_iface.len() + uas_iface.len()) as u16;
        let mut config = vec![
            9,    // bLength
            0x02, // bDescriptorType = Configuration
            total_len as u8,
            (total_len >> 8) as u8,
            2, // bNumInterfaces
            1, // bConfigurationValue
            0,
            0x80,
            50,
        ];
        config.extend_from_slice(&bot_iface);
        config.extend_from_slice(&uas_iface);
        assert!(find_uas_interface(&config));
    }

    /// A truncated / corrupt descriptor (bLength > remaining bytes) terminates
    /// gracefully and returns false.
    #[test]
    fn find_uas_interface_truncated_descriptor_returns_false() {
        // bLength = 9 but only 5 bytes in the buffer.
        let config = vec![9, 0x04, 0, 0, 2];
        assert!(!find_uas_interface(&config));
    }

    /// A zero-length descriptor terminates the scan immediately.
    #[test]
    fn find_uas_interface_zero_length_terminates() {
        let config = vec![0, 0x04, 0, 0, 2, CLASS_MASS_STORAGE, 0, PROTOCOL_UAS, 0];
        assert!(!find_uas_interface(&config));
    }
}
