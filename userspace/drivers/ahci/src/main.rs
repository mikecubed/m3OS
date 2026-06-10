//! Phase 82 — ring-3 AHCI/SATA storage driver entry point.
//!
//! Run-time flow: write the boot marker, discover an AHCI controller by PCI
//! class `0x010601`, claim it (the device-host claim path auto-enables Memory
//! Space + Bus Master), map BAR5 (ABAR), bring up the HBA (GHC.AE → GHC.HR reset
//! → CAP/PI/VS re-read → CAP2.BOH handoff gate), bring up the first implemented
//! SATA port through the spec engine-stop/start ordering, IDENTIFY it, then
//! either run the boot self-test (on a blank scratch disk — the `ahci-smoke`
//! gate path) or probe the MBR partition table (on a disk with a valid MBR — the
//! `--device ahci` data-disk path), emit `AHCI_SMOKE:server:READY`, register the
//! `"ahci.block"` service, and serve the `driver_ipc::block` protocol.

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
use driver_runtime::ipc::EndpointCap;
#[cfg(not(test))]
use driver_runtime::ipc::block::{BlkReply, BlockServer};
#[cfg(not(test))]
use driver_runtime::{DeviceCapKey, DeviceHandle, DriverRuntimeError, Mmio};
#[cfg(not(test))]
use kernel_core::device_host::DeviceHostError;
#[cfg(not(test))]
use kernel_core::driver_ipc::block::{
    BLK_FLUSH, BLK_READ, BLK_STATUS, BLK_WRITE, BlkReplyHeader, BlkRequestHeader, BlockDriverError,
};
#[cfg(not(test))]
use kernel_core::fs::mbr::{find_ext2_partition, find_fat32_partition, parse_mbr};
#[cfg(not(test))]
use kernel_core::storage::ahci::{
    AHCI_ABAR_BAR_INDEX, AHCI_PCI_CLASS, AHCI_PCI_PROG_IF, AHCI_PCI_SUBCLASS, PortDeviceType,
    ahci_pci_match, is_driveable,
};
#[cfg(not(test))]
use kernel_core::storage::ata::AtaIdentify;
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
#[cfg(not(test))]
use syscall_lib::write_str;

#[cfg(not(test))]
use ahci_driver::init::{AhciAbar, HbaCaps, bios_os_handoff, enable_ahci, read_caps, reset_hba};
#[cfg(not(test))]
use ahci_driver::port::{Port, device_type_str};
#[cfg(not(test))]
use ahci_driver::{
    AHCI_ABAR_LEN, BOOT_LOG_MARKER, SERVER_READY_SENTINEL, SERVICE_NAME, cmd, request_is_oversized,
};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "ahci_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "ahci_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Sector size the smoke self-test and the block facade assume.
#[cfg(not(test))]
const SECTOR_BYTES: usize = 512;

/// Discover an AHCI-mode SATA controller by PCI class `0x010601` (no vendor/
/// device-ID dependency — gating on a vendor ID would miss most controllers).
#[cfg(not(test))]
fn find_ahci() -> Option<DeviceCapKey> {
    let candidates =
        driver_runtime::enumerate_pci_class(AHCI_PCI_CLASS, AHCI_PCI_SUBCLASS, AHCI_PCI_PROG_IF)
            .ok()?;
    // Confirm each candidate with the shared, host-tested class predicate.
    candidates.into_iter().find(|&key| {
        let (class, subclass, prog_if) = match driver_runtime::pci_config_read(key, 0x08, 4) {
            Ok(reg) => kernel_core::device_host::pci_enum::decode_class_dword(reg),
            Err(_) => (AHCI_PCI_CLASS, AHCI_PCI_SUBCLASS, AHCI_PCI_PROG_IF),
        };
        ahci_pci_match(class, subclass, prog_if)
    })
}

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    let key = match find_ahci() {
        Some(k) => k,
        None => {
            // No AHCI controller (e.g. QEMU without -device ich9-ahci) — exit
            // cleanly so init's on-failure policy marks the service stopped.
            write_str(STDOUT_FILENO, "ahci_driver: no AHCI controller present\n");
            return 0;
        }
    };

    // The device-host registry may not have the BDF ready on first boot; init's
    // restart=on-failure re-execs and the next attempt succeeds (same accepted
    // early-boot pattern as nvme/hda).
    let device = match DeviceHandle::claim(key) {
        Ok(d) => d,
        // The controller is already owned (e.g. the D.3 bootstrap instance that
        // init spawned directly already claimed it, and the service-manager
        // instance lost the race) or no device is present at the BDF. Both
        // collapse to "the AHCI controller is not ours to drive" — exit cleanly
        // (rc 0) so init's on-failure policy does not burn the restart budget.
        Err(DriverRuntimeError::Device(
            DeviceHostError::AlreadyClaimed | DeviceHostError::NotClaimed,
        )) => {
            write_str(
                STDOUT_FILENO,
                "ahci_driver: AHCI controller unavailable (claimed/absent) — exiting cleanly\n",
            );
            return 0;
        }
        Err(_) => {
            write_str(
                STDOUT_FILENO,
                "ahci_driver: device claim failed (will retry)\n",
            );
            return 1;
        }
    };

    let mmio = match Mmio::<AhciAbar>::map(&device, AHCI_ABAR_BAR_INDEX, AHCI_ABAR_LEN) {
        Ok(m) => m,
        Err(_) => {
            write_str(STDOUT_FILENO, "ahci_driver: BAR5 (ABAR) map failed\n");
            return 2;
        }
    };

    // HBA bring-up: AHCI-enable, global reset, re-read CAP/PI/VS, BIOS/OS handoff.
    if !enable_ahci(&mmio) {
        return 3;
    }
    if !reset_hba(&mmio) {
        return 3;
    }
    let caps = read_caps(&mmio);
    bios_os_handoff(&mmio);

    let mut port = match bring_up_first_sata_port(&device, &mmio, &caps) {
        Some(p) => p,
        None => {
            write_str(
                STDOUT_FILENO,
                "ahci_driver: no driveable SATA port present\n",
            );
            return 0;
        }
    };

    // IDENTIFY DEVICE — capacity / LBA48 / flush capability.
    let id = match cmd::identify(&mut port) {
        Ok(id) => id,
        Err(_) => {
            write_str(STDOUT_FILENO, "AHCI_SMOKE:identify:FAIL\n");
            return 4;
        }
    };
    write_str(
        STDOUT_FILENO,
        &alloc::format!(
            "AHCI: identify sectors={} sector_bytes={} flush={}\n",
            id.lba48_sectors,
            id.logical_sector_bytes,
            if id.has_flush_ext { 1 } else { 0 }
        ),
    );

    // The whole driver — `SECTOR_BYTES`, the self-test, the MBR walker, the
    // block facade, and the kernel's 512-byte sector copy — assumes 512-byte
    // logical sectors. A drive reporting any other size (e.g. a 4Kn "4096-byte
    // logical" disk) would make every READ/WRITE length, MBR offset, and kernel
    // sector copy inconsistent and corrupt I/O. Fail CLOSED: log and exit
    // cleanly (rc 0, like the no-driveable-port path) without writing, probing,
    // or serving the block protocol. The size is fixed for a given drive, so a
    // restart cannot help — exiting non-zero would only burn init's restart
    // budget. QEMU `ide-hd` presents 512-byte sectors, so this never trips on
    // the smoke/data-disk paths; it guards real 4Kn hardware.
    if id.logical_sector_bytes != SECTOR_BYTES as u32 {
        write_str(
            STDOUT_FILENO,
            &alloc::format!(
                "AHCI: unsupported logical sector size {} (only {} supported) — exiting cleanly\n",
                id.logical_sector_bytes,
                SECTOR_BYTES
            ),
        );
        return 0;
    }

    // Read LBA 0 once and decide what to do with the disk. The driver carries no
    // scratch-vs-data flag in argv, so this sector-0 classification is the only
    // safeguard against the destructive self-test clobbering a real disk:
    //   * an *obviously blank* scratch disk (LBA 0 is all zero) → run the
    //     destructive boot self-test (the `ahci-smoke` gate path, whose scratch
    //     `ide-hd` is freshly zeroed each run);
    //   * ANY other disk — a valid MBR, a raw filesystem superblock, GPT-less
    //     data, or a corrupted/non-`0x55AA` sector 0 → take the read-only
    //     `partition_probe`, NEVER the destructive write. Gating on "blank"
    //     instead of "not a valid MBR" is what keeps the self-test from
    //     destroying a real non-MBR disk.
    //   * a LBA-0 read *error* fails CLOSED — neither branch runs — because a
    //     transient read failure must never fall through to a destructive write.
    match read_lba0(&mut port) {
        Some(s) if is_blank_scratch(&s) => run_self_test(&mut port, &id),
        Some(s) => partition_probe(&s),
        None => {
            write_str(
                STDOUT_FILENO,
                "AHCI: LBA0 read failed — skipping self-test (fail-safe, no destructive write)\n",
            );
        }
    }

    write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);

    run_block_server(&mut port)
}

/// Enumerate `PI`, count present ports, and bring up the first driveable SATA
/// port through the spec stop/start engine ordering.
#[cfg(not(test))]
fn bring_up_first_sata_port<'a>(
    device: &DeviceHandle,
    mmio: &'a Mmio<AhciAbar>,
    caps: &HbaCaps,
) -> Option<Port<'a>> {
    let mut present = 0u32;
    for index in 0..32u8 {
        if caps.pi & (1u32 << index) == 0 {
            continue;
        }
        if Port::present(mmio, index) {
            present += 1;
        }
    }
    write_str(
        STDOUT_FILENO,
        &alloc::format!("AHCI: ports_found={}\n", present),
    );

    for index in 0..32u8 {
        if caps.pi & (1u32 << index) == 0 {
            continue;
        }
        if !Port::present(mmio, index) {
            continue;
        }
        let mut port = match Port::allocate(device, mmio, index, caps) {
            Ok(p) => p,
            Err(_) => {
                write_str(
                    STDOUT_FILENO,
                    &alloc::format!("AHCI: port {} DMA alloc failed\n", index),
                );
                continue;
            }
        };
        // Spec stop/start ordering: stop → program DMA structures → FRE →
        // COMRESET → wait ready → classify → (skip non-SATA) → clear W1C → start.
        // If the engine refuses to stop, never reprogram PxCLB/PxFB on a live
        // engine — skip this port instead of corrupting its command-list pointer.
        if !port.stop_engine() {
            write_str(
                STDOUT_FILENO,
                &alloc::format!("AHCI: port {} engine stop failed — skipping\n", index),
            );
            continue;
        }
        port.program_dma_structures();
        port.enable_fis_rx();
        port.comreset();
        if !port.wait_ready() {
            write_str(
                STDOUT_FILENO,
                &alloc::format!("AHCI: port {} not ready (BSY/DRQ stuck)\n", index),
            );
            continue;
        }
        let dt = port.classify();
        if !is_driveable(dt) {
            let sig = match dt {
                PortDeviceType::Unknown(s) => s,
                PortDeviceType::Satapi => kernel_core::storage::ahci::SIG_ATAPI,
                PortDeviceType::PortMultiplier => kernel_core::storage::ahci::SIG_PM,
                PortDeviceType::Semb => kernel_core::storage::ahci::SIG_SEMB,
                _ => 0,
            };
            write_str(
                STDOUT_FILENO,
                &alloc::format!("AHCI: port {} skipped non-SATA sig={:#010x}\n", index, sig),
            );
            continue;
        }
        port.clear_errors();
        if !port.start_engine() {
            write_str(
                STDOUT_FILENO,
                &alloc::format!("AHCI: port {} engine start failed\n", index),
            );
            continue;
        }
        write_str(
            STDOUT_FILENO,
            &alloc::format!(
                "AHCI: port {} classified {}, engine started\n",
                index,
                device_type_str(dt)
            ),
        );
        return Some(port);
    }
    None
}

/// `true` when LBA 0 is entirely zero — an obviously-blank scratch disk that is
/// safe for the destructive boot self-test. Any non-zero byte (an MBR/partition
/// table, a raw filesystem superblock, or stale data) marks the disk as "real"
/// and routes it to the read-only partition probe instead, so the self-test can
/// never write over a disk that holds data.
#[cfg(not(test))]
fn is_blank_scratch(sector0: &[u8; SECTOR_BYTES]) -> bool {
    sector0.iter().all(|&b| b == 0)
}

/// Read LBA 0 into an owned 512-byte buffer, or `None` on I/O error.
#[cfg(not(test))]
fn read_lba0(port: &mut Port) -> Option<[u8; SECTOR_BYTES]> {
    cmd::read_sectors(port, 0, 1).ok()?;
    let mut out = [0u8; SECTOR_BYTES];
    out.copy_from_slice(&port.data_slice(SECTOR_BYTES)[..SECTOR_BYTES]);
    Some(out)
}

/// Boot self-test (the `ahci-smoke` gate path): IDENTIFY → write a known
/// pattern (single + multi-block) → read it back and byte-compare → FLUSH CACHE
/// EXT → IDENTIFY again, emitting the binding `AHCI_SMOKE:*` sentinel set.
#[cfg(not(test))]
fn run_self_test(port: &mut Port, id: &AtaIdentify) {
    write_str(
        STDOUT_FILENO,
        &alloc::format!(
            "AHCI_SMOKE:identify:PASS sectors={} sector_bytes={} flush={}\n",
            id.lba48_sectors,
            id.logical_sector_bytes,
            if id.has_flush_ext { 1 } else { 0 }
        ),
    );

    // Single-block pattern at LBA 0; multi-block pattern at LBA 8 (one PRDT).
    let single = [0xA5u8; SECTOR_BYTES];
    let mut multi = alloc::vec![0u8; 8 * SECTOR_BYTES];
    for (i, b) in multi.iter_mut().enumerate() {
        *b = ((i * 7 + 13) & 0xFF) as u8;
    }

    if cmd::write_sectors(port, 0, 1, &single).is_err()
        || cmd::write_sectors(port, 8, 8, &multi).is_err()
    {
        write_str(STDOUT_FILENO, "AHCI_SMOKE:write:FAIL\n");
        return;
    }
    write_str(STDOUT_FILENO, "AHCI_SMOKE:write:PASS\n");

    // Read back single block and compare.
    if cmd::read_sectors(port, 0, 1).is_err() || port.data_slice(SECTOR_BYTES) != &single[..] {
        write_str(STDOUT_FILENO, "AHCI_SMOKE:readback:FAIL single-block\n");
        return;
    }
    // Read back multi-block and compare.
    if cmd::read_sectors(port, 8, 8).is_err() || port.data_slice(8 * SECTOR_BYTES) != &multi[..] {
        write_str(STDOUT_FILENO, "AHCI_SMOKE:readback:FAIL multi-block\n");
        return;
    }
    write_str(STDOUT_FILENO, "AHCI_SMOKE:readback:PASS\n");

    // FLUSH CACHE EXT for durability.
    if cmd::flush(port).is_err() {
        write_str(STDOUT_FILENO, "AHCI_SMOKE:flush:FAIL\n");
        return;
    }
    write_str(STDOUT_FILENO, "AHCI: flush durable lba=0\n");
    write_str(STDOUT_FILENO, "AHCI_SMOKE:flush:PASS\n");

    // IDENTIFY again (proves the issue/PRDT/completion path still works after a
    // write/flush cycle).
    match cmd::identify(port) {
        Ok(id2) => write_str(
            STDOUT_FILENO,
            &alloc::format!("AHCI_SMOKE:identify2:PASS sectors={}\n", id2.lba48_sectors),
        ),
        Err(_) => write_str(STDOUT_FILENO, "AHCI_SMOKE:identify2:FAIL\n"),
    };

    // Track C.4 error recovery, exercised under QEMU: issue a READ at an
    // out-of-range LBA (one past the last addressable sector). QEMU's IDE core
    // aborts the command (ABRT), the AHCI layer latches `PxIS.TFES`, and
    // `cmd::run_command` routes through `Port::recover_port` (stop engine →
    // clear `PxSERR`/`PxIS` → COMRESET → restart). Then prove the port is
    // recovered, not wedged: the command engine is running again (`PxCMD.CR`)
    // and a valid READ at LBA 0 completes. This is the gate assertion behind the
    // C.4 acceptance — without it the recovery path would ride unverified.
    let bad_lba = id.lba48_sectors.max(1);
    let induced_err = cmd::read_sectors(port, bad_lba, 1).is_err();
    let engine_up = port.engine_running();
    let revalidated = cmd::read_sectors(port, 0, 1).is_ok();
    if induced_err && engine_up && revalidated {
        write_str(STDOUT_FILENO, "AHCI_SMOKE:recover:PASS\n");
    } else {
        write_str(
            STDOUT_FILENO,
            &alloc::format!(
                "AHCI_SMOKE:recover:FAIL induced_err={} engine_up={} revalidated={}\n",
                induced_err,
                engine_up,
                revalidated
            ),
        );
    }
}

/// Probe the MBR partition table (the `--device ahci` data-disk path) and log
/// the discovered ext2/FAT32 partition the VFS will mount. Adds no new
/// partition-table code — reuses the shared `kernel_core::fs::mbr` walker.
#[cfg(not(test))]
fn partition_probe(sector0: &[u8; SECTOR_BYTES]) {
    match parse_mbr(sector0) {
        Ok(entries) => {
            if let Some((start, count)) = find_ext2_partition(&entries) {
                write_str(
                    STDOUT_FILENO,
                    &alloc::format!(
                        "AHCI: ext2 partition found start={} count={}\n",
                        start,
                        count
                    ),
                );
            } else if let Some((start, count)) = find_fat32_partition(&entries) {
                write_str(
                    STDOUT_FILENO,
                    &alloc::format!(
                        "AHCI: fat32 partition found start={} count={}\n",
                        start,
                        count
                    ),
                );
            } else {
                write_str(
                    STDOUT_FILENO,
                    "AHCI: no ext2/fat32 partition on SATA disk\n",
                );
            }
        }
        Err(_) => {
            write_str(STDOUT_FILENO, "AHCI: no valid MBR on SATA disk\n");
        }
    };
}

/// Register `"ahci.block"` and serve the `driver_ipc::block` protocol, routing
/// `BLK_READ`/`BLK_WRITE` to the Track C data path.
#[cfg(not(test))]
fn run_block_server(port: &mut Port) -> i32 {
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        write_str(STDOUT_FILENO, "ahci_driver: endpoint create failed\n");
        return 5;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(id) => id,
        Err(_) => return 6,
    };
    if syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME) == u64::MAX {
        write_str(STDOUT_FILENO, "ahci_driver: service register failed\n");
        return 7;
    }
    write_str(
        STDOUT_FILENO,
        "ahci_driver: service registered (ahci.block)\n",
    );

    let server = BlockServer::new(EndpointCap::new(ep_u32));

    const MAX_CONSECUTIVE_ERRORS: u32 = 8;
    let mut consecutive_errors: u32 = 0;

    loop {
        let result = server.handle_next(|req| match req.header.kind {
            BLK_READ => {
                let (header, bulk) = handle_read(port, &req.header);
                BlkReply {
                    header,
                    payload_grant: 0,
                    bulk,
                }
            }
            BLK_WRITE => {
                let header = handle_write(port, &req.header, &req.bulk);
                BlkReply {
                    header,
                    payload_grant: 0,
                    bulk: Vec::new(),
                }
            }
            BLK_STATUS => BlkReply {
                header: BlkReplyHeader {
                    cmd_id: req.header.cmd_id,
                    status: BlockDriverError::Ok,
                    bytes: 0,
                },
                payload_grant: 0,
                bulk: Vec::new(),
            },
            BLK_FLUSH => {
                // Commit the drive's volatile write cache to media. Writes are
                // now write-back (handle_write no longer flushes per request),
                // so this is the durability barrier — issued by the kernel at
                // clean shutdown via blk::flush.
                let status = if cmd::flush(port).is_ok() {
                    BlockDriverError::Ok
                } else {
                    BlockDriverError::IoError
                };
                BlkReply {
                    header: BlkReplyHeader {
                        cmd_id: req.header.cmd_id,
                        status,
                        bytes: 0,
                    },
                    payload_grant: 0,
                    bulk: Vec::new(),
                }
            }
            _ => BlkReply {
                header: BlkReplyHeader {
                    cmd_id: req.header.cmd_id,
                    status: BlockDriverError::InvalidRequest,
                    bytes: 0,
                },
                payload_grant: 0,
                bulk: Vec::new(),
            },
        });
        match result {
            Ok(()) => consecutive_errors = 0,
            Err(_) => {
                consecutive_errors += 1;
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    write_str(
                        STDOUT_FILENO,
                        "ahci_driver: too many consecutive handle_next errors — exiting for restart\n",
                    );
                    return 8;
                }
            }
        }
    }
}

/// Serve one `BLK_READ`: read `sector_count` sectors into the bounce buffer and
/// return the data as the reply bulk (grant-carried, like the NVMe driver).
#[cfg(not(test))]
fn handle_read(port: &mut Port, hdr: &BlkRequestHeader) -> (BlkReplyHeader, Vec<u8>) {
    let count = hdr.sector_count;
    if count == 0 || request_is_oversized(count) {
        return (
            BlkReplyHeader {
                cmd_id: hdr.cmd_id,
                status: BlockDriverError::InvalidRequest,
                bytes: 0,
            },
            Vec::new(),
        );
    }
    match cmd::read_sectors(port, hdr.lba, count as u16) {
        Ok(()) => {
            let bytes = count as usize * SECTOR_BYTES;
            let data = port.data_slice(bytes).to_vec();
            (
                BlkReplyHeader {
                    cmd_id: hdr.cmd_id,
                    status: BlockDriverError::Ok,
                    bytes: bytes as u32,
                },
                data,
            )
        }
        Err(e) => (
            BlkReplyHeader {
                cmd_id: hdr.cmd_id,
                status: e.into(),
                bytes: 0,
            },
            Vec::new(),
        ),
    }
}

/// Serve one `BLK_WRITE`: write the request bulk and report success once it
/// reaches the drive's (volatile) write-back cache.
///
/// Write-back: a `WRITE DMA EXT` completion lands in the drive cache, not yet on
/// media. We deliberately do NOT issue a per-write `FLUSH CACHE EXT` — that
/// durability barrier per request was ~20x slower than the transfer itself. The
/// kernel issues one `BLK_FLUSH` (→ `cmd::flush`) at clean shutdown via
/// `blk::flush`, matching the virtio-blk write-back model. Within a boot, reads
/// see prior writes (drive cache is coherent); a host crash / power loss before
/// the shutdown flush loses cached writes (standard write-back tradeoff).
#[cfg(not(test))]
fn handle_write(port: &mut Port, hdr: &BlkRequestHeader, bulk: &[u8]) -> BlkReplyHeader {
    let count = hdr.sector_count;
    let needed = count as usize * SECTOR_BYTES;
    if count == 0 || request_is_oversized(count) || bulk.len() < needed {
        return BlkReplyHeader {
            cmd_id: hdr.cmd_id,
            status: BlockDriverError::InvalidRequest,
            bytes: 0,
        };
    }
    match cmd::write_sectors(port, hdr.lba, count as u16, bulk) {
        Ok(()) => BlkReplyHeader {
            cmd_id: hdr.cmd_id,
            status: BlockDriverError::Ok,
            bytes: needed as u32,
        },
        Err(e) => BlkReplyHeader {
            cmd_id: hdr.cmd_id,
            status: e.into(),
            bytes: 0,
        },
    }
}
