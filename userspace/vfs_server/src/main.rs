//! Userspace VFS service for m3OS (Phase 54).
//!
//! Owns the migrated ext2 pathname authority for the Phase 54 storage slice.
//! The kernel keeps per-process fd bookkeeping and virtual-filesystem carveouts,
//! while this service answers ext2-backed pathname, metadata, directory, and
//! mount-policy requests via IPC.
//!
//! # Architecture
//!
//! ```text
//! app → open("/etc/passwd") → kernel syscall handler
//!       → detects /etc/ + O_RDONLY + "vfs" registered
//!       → IPC call_msg(vfs_ep, VFS_OPEN, path)
//!       → this server: resolve path, open handle, reply
//!
//! app → read(fd, buf, n) → kernel sees FdBackend::VfsService
//!       → IPC call_msg(vfs_ep, VFS_READ, handle, offset, count)
//!       → this server: read data, store reply bulk, reply
//! ```
//!
//! Raw disk sectors are read via `sys_block_read` (Phase 54 syscall).
//! Ext2 parsing uses `kernel_core::fs::ext2` types.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;
use kernel_core::fs::ext2::{
    Ext2BlockGroupDescriptor, Ext2DirEntry, Ext2Inode, Ext2Superblock, inode_block_group,
    inode_index_in_group,
};
use kernel_core::fs::mbr;
use kernel_core::fs::vfs_protocol::{
    VFS_ACCESS_PATH, VFS_CLOSE, VFS_LIST_DIR, VFS_MAX_READ, VFS_MOUNT_EXT2_ROOT, VFS_MOUNT_POLICY,
    VFS_MOUNT_VFAT_DATA, VFS_NODE_DIR, VFS_NODE_FILE, VFS_NODE_SYMLINK, VFS_OPEN, VFS_READ,
    VFS_STAT_PATH, VFS_STAT_REPLY_SIZE, VFS_UMOUNT_EXT2_ROOT, VFS_UMOUNT_POLICY,
    VFS_UMOUNT_VFAT_DATA,
};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

// ---------------------------------------------------------------------------
// Negative errno constants (returned as reply labels)
// ---------------------------------------------------------------------------

const NEG_ENOENT: u64 = (-2i64) as u64;
const NEG_EIO: u64 = (-5i64) as u64;
const NEG_EBADF: u64 = (-9i64) as u64;
const NEG_ENOTDIR: u64 = (-20i64) as u64;
const NEG_EINVAL: u64 = (-22i64) as u64;
const NEG_ENFILE: u64 = (-23i64) as u64;

// ---------------------------------------------------------------------------
// Ext2 volume state (server-local)
// ---------------------------------------------------------------------------

/// In-process ext2 volume state — replaces `Ext2Volume` from the kernel.
struct Ext2State {
    base_lba: u64,
    superblock: Ext2Superblock,
    bgd_table: Vec<Ext2BlockGroupDescriptor>,
    block_size: u32,
    sectors_per_block: u32,
}

impl Ext2State {
    /// Read raw sectors from disk via the sys_block_read syscall.
    fn read_sectors(&self, start_lba: u64, count: usize, buf: &mut [u8]) -> Result<(), ()> {
        let ret = syscall_lib::block_read(start_lba, count, buf);
        if ret < 0 { Err(()) } else { Ok(()) }
    }

    /// Read one ext2 block into a freshly allocated buffer.
    fn read_block(&self, block_num: u32) -> Result<Vec<u8>, ()> {
        let lba = self.base_lba + (block_num as u64) * (self.sectors_per_block as u64);
        let mut buf = vec![0u8; self.block_size as usize];
        let sector_count = self.sectors_per_block as usize;
        self.read_sectors(lba, sector_count, &mut buf)?;
        Ok(buf)
    }

    /// Read an inode by number.
    fn read_inode(&self, inode_num: u32) -> Result<Ext2Inode, ()> {
        let bg = inode_block_group(inode_num, self.superblock.inodes_per_group);
        let idx = inode_index_in_group(inode_num, self.superblock.inodes_per_group);
        let bgd = self.bgd_table.get(bg as usize).ok_or(())?;

        let inode_size = self.superblock.inode_size as u32;
        let byte_offset = idx * inode_size;
        let block_offset = byte_offset / self.block_size;
        let offset_in_block = (byte_offset % self.block_size) as usize;

        let inode_table_block = bgd.inode_table + block_offset;
        let block_data = self.read_block(inode_table_block)?;

        Ext2Inode::parse(&block_data[offset_in_block..]).map_err(|_| ())
    }

    /// Resolve a block pointer from an inode, handling indirect blocks.
    fn resolve_block(&self, inode: &Ext2Inode, file_block: u32) -> Result<u32, ()> {
        let ptrs_per_block = self.block_size / 4;

        if file_block < 12 {
            return Ok(inode.block[file_block as usize]);
        }

        let file_block = file_block - 12;
        if file_block < ptrs_per_block {
            // Single indirect.
            let indirect_block = inode.block[12];
            if indirect_block == 0 {
                return Ok(0);
            }
            let data = self.read_block(indirect_block)?;
            let off = (file_block as usize) * 4;
            return Ok(u32::from_le_bytes([
                data[off],
                data[off + 1],
                data[off + 2],
                data[off + 3],
            ]));
        }

        let file_block = file_block - ptrs_per_block;
        if file_block < ptrs_per_block * ptrs_per_block {
            // Double indirect.
            let dind_block = inode.block[13];
            if dind_block == 0 {
                return Ok(0);
            }
            let dind_data = self.read_block(dind_block)?;
            let idx1 = (file_block / ptrs_per_block) as usize;
            let off1 = idx1 * 4;
            let ind_block = u32::from_le_bytes([
                dind_data[off1],
                dind_data[off1 + 1],
                dind_data[off1 + 2],
                dind_data[off1 + 3],
            ]);
            if ind_block == 0 {
                return Ok(0);
            }
            let ind_data = self.read_block(ind_block)?;
            let idx2 = (file_block % ptrs_per_block) as usize;
            let off2 = idx2 * 4;
            return Ok(u32::from_le_bytes([
                ind_data[off2],
                ind_data[off2 + 1],
                ind_data[off2 + 2],
                ind_data[off2 + 3],
            ]));
        }

        // Triple indirect — not needed for /etc/ config files.
        Err(())
    }

    /// Resolve a path like "/etc/passwd" to its inode number.
    ///
    /// `path` must start with "/" — relative paths are rejected with
    /// `NEG_EINVAL`. Walks from root inode (2).
    fn resolve_path(&self, path: &str) -> Result<u32, u64> {
        let path = path.strip_prefix('/').ok_or(NEG_EINVAL)?;
        let mut current_inode_num: u32 = 2; // root inode

        for component in path.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }
            // Read current inode — must be a directory.
            let inode = self.read_inode(current_inode_num).map_err(|_| NEG_EIO)?;
            if !inode.is_dir() {
                return Err(NEG_ENOTDIR);
            }
            // Scan directory entries.
            let mut found = false;
            let mut file_block = 0u32;
            let blocks_count = inode.size.div_ceil(self.block_size);
            while file_block < blocks_count {
                let block_num = self
                    .resolve_block(&inode, file_block)
                    .map_err(|_| NEG_EIO)?;
                if block_num == 0 {
                    file_block += 1;
                    continue;
                }
                let block_data = self.read_block(block_num).map_err(|_| NEG_EIO)?;
                let entries = Ext2DirEntry::parse_block(&block_data).map_err(|_| NEG_EIO)?;
                for entry in &entries {
                    if entry.inode != 0 && entry.name == component {
                        current_inode_num = entry.inode;
                        found = true;
                        break;
                    }
                }
                if found {
                    break;
                }
                file_block += 1;
            }
            if !found {
                return Err(NEG_ENOENT);
            }
        }

        Ok(current_inode_num)
    }

    /// Read file data from an inode at a given byte offset.
    fn read_file_data(
        &self,
        inode: &Ext2Inode,
        offset: usize,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ()> {
        let file_size = inode.size as usize;
        if offset >= file_size {
            return Ok(Vec::new()); // EOF
        }
        let available = file_size - offset;
        let to_read = max_bytes.min(available);
        let bs = self.block_size as usize;

        let mut result = Vec::with_capacity(to_read);
        let mut remaining = to_read;
        let mut pos = offset;

        while remaining > 0 {
            let file_block = (pos / bs) as u32;
            let offset_in_block = pos % bs;
            let chunk = remaining.min(bs - offset_in_block);

            let block_num = self.resolve_block(inode, file_block).map_err(|_| ())?;
            if block_num == 0 {
                // Sparse block — zeros.
                result.extend(core::iter::repeat(0u8).take(chunk));
            } else {
                let block_data = self.read_block(block_num)?;
                result.extend_from_slice(&block_data[offset_in_block..offset_in_block + chunk]);
            }

            pos += chunk;
            remaining -= chunk;
        }

        Ok(result)
    }

    fn read_symlink_target(&self, inode: &Ext2Inode) -> Result<Vec<u8>, ()> {
        if !inode.is_symlink() {
            return Err(());
        }
        let target_len = inode.size as usize;
        if inode.blocks == 0 && target_len <= 60 {
            let mut raw = [0u8; 60];
            for (i, &slot) in inode.block.iter().enumerate() {
                let start = i * 4;
                raw[start..start + 4].copy_from_slice(&slot.to_le_bytes());
            }
            Ok(raw[..target_len].to_vec())
        } else {
            self.read_file_data(inode, 0, target_len)
        }
    }

    fn read_dir_entries(&self, inode: &Ext2Inode) -> Result<Vec<(u32, String, u8)>, u64> {
        let mut entries = Vec::new();
        let mut file_block = 0u32;
        let blocks_count = inode.size.div_ceil(self.block_size);
        while file_block < blocks_count {
            let block_num = self.resolve_block(inode, file_block).map_err(|_| NEG_EIO)?;
            if block_num == 0 {
                file_block += 1;
                continue;
            }
            let block_data = self.read_block(block_num).map_err(|_| NEG_EIO)?;
            let block_entries = Ext2DirEntry::parse_block(&block_data).map_err(|_| NEG_EIO)?;
            for entry in block_entries {
                if entry.inode == 0 {
                    continue;
                }
                let entry_inode = self.read_inode(entry.inode).map_err(|_| NEG_EIO)?;
                entries.push((
                    entry.inode,
                    entry.name,
                    inode_kind_to_dirent_type(&entry_inode),
                ));
            }
            file_block += 1;
        }
        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// Open handle table
// ---------------------------------------------------------------------------

/// Maximum concurrent open handles.
const MAX_HANDLES: usize = 32;
// Slot index occupies the low 16 bits of the packed handle. MAX_HANDLES
// (32) comfortably fits — the static check below guards against future
// bumps silently colliding with the generation field.
const _: () = assert!(MAX_HANDLES <= 0x1_0000);

/// Bits reserved for the slot index in the packed handle encoding.
const HANDLE_SLOT_BITS: u32 = 16;
/// Mask selecting the slot index from a packed handle.
const HANDLE_SLOT_MASK: u64 = (1 << HANDLE_SLOT_BITS) - 1;

/// An open handle tracked by the server.
struct OpenHandle {
    inode_num: u32,
    file_size: u32,
    /// Generation counter bumped on each (re-)allocation of this slot.
    /// Protects against force-closing a recycled handle when a stale
    /// `VFS_CLOSE` arrives out of order (defence-in-depth against
    /// kernel-side refcount races on SMP — the generation on an
    /// incoming request must match the slot's current generation, else
    /// the request is rejected as `EBADF`).
    generation: u16,
    in_use: bool,
}

struct HandleTable {
    handles: [OpenHandle; MAX_HANDLES],
}

impl HandleTable {
    fn new() -> Self {
        const EMPTY: OpenHandle = OpenHandle {
            inode_num: 0,
            file_size: 0,
            generation: 0,
            in_use: false,
        };
        HandleTable {
            handles: [EMPTY; MAX_HANDLES],
        }
    }

    fn alloc(&mut self, inode_num: u32, file_size: u32) -> Option<u64> {
        for (i, h) in self.handles.iter_mut().enumerate() {
            if !h.in_use {
                // Bump generation BEFORE marking in_use so a concurrent stale
                // request sees the new generation the moment the slot comes
                // back into circulation.
                h.generation = h.generation.wrapping_add(1);
                h.inode_num = inode_num;
                h.file_size = file_size;
                h.in_use = true;
                return Some(encode_handle(h.generation, i as u16));
            }
        }
        None
    }

    fn get(&self, handle: u64) -> Option<&OpenHandle> {
        let (generation, idx) = decode_handle(handle);
        let idx = idx as usize;
        if idx < MAX_HANDLES
            && self.handles[idx].in_use
            && self.handles[idx].generation == generation
        {
            Some(&self.handles[idx])
        } else {
            None
        }
    }

    fn free(&mut self, handle: u64) -> bool {
        let (generation, idx) = decode_handle(handle);
        let idx = idx as usize;
        if idx < MAX_HANDLES
            && self.handles[idx].in_use
            && self.handles[idx].generation == generation
        {
            self.handles[idx].in_use = false;
            true
        } else {
            false
        }
    }
}

/// Pack `(generation, slot)` into a `u64` handle.
fn encode_handle(generation: u16, slot: u16) -> u64 {
    ((generation as u64) << HANDLE_SLOT_BITS) | (slot as u64)
}

/// Unpack `(generation, slot)` from a `u64` handle. The kernel stores the
/// handle as the low 32 bits of the packed VFS_OPEN reply, which leaves
/// 16 bits of generation + 16 bits of slot — plenty for `MAX_HANDLES = 32`.
fn decode_handle(handle: u64) -> (u16, u16) {
    let generation = ((handle >> HANDLE_SLOT_BITS) & 0xFFFF) as u16;
    let slot = (handle & HANDLE_SLOT_MASK) as u16;
    (generation, slot)
}

// ---------------------------------------------------------------------------
// IPC constants
// ---------------------------------------------------------------------------

const REPLY_CAP_HANDLE: u32 = 1;
const MAX_BULK_BUF: usize = VFS_MAX_READ;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: starting\n");

    // 1. Probe MBR for ext2 partition.
    let mut sector0 = [0u8; 512];
    if syscall_lib::block_read(0, 1, &mut sector0) < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: failed to read MBR\n");
        return 1;
    }

    let entries = match mbr::parse_mbr(&sector0) {
        Ok(e) => e,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "vfs_server: bad MBR signature\n");
            return 1;
        }
    };

    let (base_lba, _sector_count) = match mbr::find_ext2_partition(&entries) {
        Some(v) => v,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "vfs_server: no ext2 partition found\n");
            return 1;
        }
    };

    // 2. Read superblock (offset 1024 = LBA + 2).
    let sb_lba = base_lba + 2;
    let mut sb_raw = [0u8; 1024];
    if syscall_lib::block_read(sb_lba, 2, &mut sb_raw) < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: failed to read superblock\n");
        return 1;
    }

    let superblock = match Ext2Superblock::parse(&sb_raw) {
        Ok(sb) => sb,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "vfs_server: bad ext2 superblock\n");
            return 1;
        }
    };

    let block_size = superblock.block_size();
    let sectors_per_block = block_size / 512;
    let bg_count = superblock.block_group_count();

    // 3. Read block group descriptor table.
    let bgd_block = if block_size == 1024 { 2 } else { 1 };
    let bgd_lba = base_lba + (bgd_block as u64) * (sectors_per_block as u64);
    let bgd_size = (bg_count as usize) * 32;
    let bgd_sectors = bgd_size.div_ceil(512);
    let mut bgd_raw = vec![0u8; bgd_sectors * 512];
    if syscall_lib::block_read(bgd_lba, bgd_sectors, &mut bgd_raw) < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: failed to read BGD table\n");
        return 1;
    }

    let bgd_table = match Ext2BlockGroupDescriptor::parse_table(&bgd_raw, bg_count) {
        Ok(t) => t,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "vfs_server: bad BGD table\n");
            return 1;
        }
    };

    let ext2 = Ext2State {
        base_lba,
        superblock,
        bgd_table,
        block_size,
        sectors_per_block,
    };

    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: ext2 mounted\n");

    // 4. Create IPC endpoint and register as "vfs".
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: create_endpoint failed\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    let ret = syscall_lib::ipc_register_service(ep_handle, "vfs");
    if ret == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: register_service failed\n");
        return 1;
    }

    syscall_lib::write_str(
        STDOUT_FILENO,
        "vfs_server: registered, entering server loop\n",
    );

    // 5. Server loop.
    server_loop(&ext2, ep_handle);
}

// ---------------------------------------------------------------------------
// Server loop
// ---------------------------------------------------------------------------

fn server_loop(ext2: &Ext2State, ep_handle: u32) -> ! {
    let mut handles = HandleTable::new();
    let mut msg = syscall_lib::IpcMessage::new(0);
    let mut recv_buf = [0u8; MAX_BULK_BUF];

    // First receive — blocks until the kernel sends us a request.
    syscall_lib::ipc_recv_msg(ep_handle, &mut msg, &mut recv_buf);

    loop {
        let (reply_label, reply_data0) = handle_request(ext2, &mut handles, &msg, &recv_buf);

        // Store reply bulk data if any was prepared by handle_request.
        // (read path stores data via ipc_store_reply_bulk before we get here)

        // Reply to the caller and wait for the next message.
        // We use two separate syscalls (reply + recv) because we need to
        // send data words in the reply (not just a label), and the
        // combined reply_recv_msg only supports a label.
        syscall_lib::ipc_reply(REPLY_CAP_HANDLE, reply_label, reply_data0);

        msg = syscall_lib::IpcMessage::new(0);
        syscall_lib::ipc_recv_msg(ep_handle, &mut msg, &mut recv_buf);
    }
}

/// Dispatch a single request.  Returns `(reply_label, reply_data0)`.
fn handle_request(
    ext2: &Ext2State,
    handles: &mut HandleTable,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    match msg.label {
        VFS_OPEN => handle_open(ext2, handles, msg, recv_buf),
        VFS_READ => handle_read(ext2, handles, msg),
        VFS_CLOSE => handle_close(handles, msg),
        VFS_STAT_PATH => handle_stat_path(ext2, msg, recv_buf),
        VFS_LIST_DIR => handle_list_dir(ext2, msg, recv_buf),
        VFS_ACCESS_PATH => handle_access_path(ext2, msg, recv_buf),
        VFS_MOUNT_POLICY => handle_mount_policy(msg, recv_buf),
        VFS_UMOUNT_POLICY => handle_umount_policy(msg, recv_buf),
        _ => (NEG_EINVAL, 0),
    }
}

fn decode_path<'a>(recv_buf: &'a [u8], path_len: usize) -> Result<&'a str, u64> {
    if path_len == 0 || path_len > recv_buf.len() {
        return Err(NEG_EINVAL);
    }
    core::str::from_utf8(&recv_buf[..path_len]).map_err(|_| NEG_EINVAL)
}

fn inode_kind(inode: &Ext2Inode) -> Result<u64, u64> {
    if inode.is_regular() {
        Ok(VFS_NODE_FILE)
    } else if inode.is_dir() {
        Ok(VFS_NODE_DIR)
    } else if inode.is_symlink() {
        Ok(VFS_NODE_SYMLINK)
    } else {
        Err(NEG_EINVAL)
    }
}

fn inode_kind_to_dirent_type(inode: &Ext2Inode) -> u8 {
    if inode.is_dir() {
        4
    } else if inode.is_regular() {
        8
    } else if inode.is_symlink() {
        10
    } else {
        0
    }
}

fn encode_stat_header(
    ext2: &Ext2State,
    inode_num: u32,
    inode: &Ext2Inode,
) -> Result<[u8; VFS_STAT_REPLY_SIZE], u64> {
    let kind = inode_kind(inode)?;
    let words = [
        kind,
        inode.mode as u64,
        inode.uid as u64,
        inode.gid as u64,
        inode_num as u64,
        inode.size as u64,
        inode.links_count as u64,
        ext2.block_size as u64,
        inode.atime as u64,
        inode.mtime as u64,
        inode.ctime as u64,
    ];
    let mut out = [0u8; VFS_STAT_REPLY_SIZE];
    for (idx, word) in words.iter().enumerate() {
        let start = idx * 8;
        out[start..start + 8].copy_from_slice(&word.to_le_bytes());
    }
    Ok(out)
}

fn mount_policy_action(target: &str, fstype: &str) -> Result<u64, u64> {
    match (target, fstype) {
        ("/", "ext2") => Ok(VFS_MOUNT_EXT2_ROOT),
        ("/data", "vfat") => Ok(VFS_MOUNT_VFAT_DATA),
        _ => Err(NEG_EINVAL),
    }
}

fn umount_policy_action(target: &str) -> Result<u64, u64> {
    match target {
        "/" => Ok(VFS_UMOUNT_EXT2_ROOT),
        "/data" => Ok(VFS_UMOUNT_VFAT_DATA),
        _ => Err(NEG_EINVAL),
    }
}

// ---------------------------------------------------------------------------
// VFS_OPEN
// ---------------------------------------------------------------------------

fn handle_open(
    ext2: &Ext2State,
    handles: &mut HandleTable,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    // Defensive flag validation. The kernel's `vfs_service_should_route`
    // already gates this path on read-only, non-creating, non-truncating
    // opens — but the server owns its own contract. Reject anything with
    // an access mode other than O_RDONLY, or with creation / truncation /
    // exclusive bits set, so a future kernel change or a misbehaving
    // caller surfaces a clear EINVAL instead of a silent success.
    const O_ACCMODE: u64 = 0o3;
    // O_CREAT=0x40, O_EXCL=0x80, O_TRUNC=0x200, O_APPEND=0x400.
    const MUTATING_FLAGS: u64 = 0x40 | 0x80 | 0x200 | 0x400;
    let flags = msg.data[0];
    if flags & O_ACCMODE != 0 || flags & MUTATING_FLAGS != 0 {
        return (NEG_EINVAL, 0);
    }

    let path = match decode_path(recv_buf, msg.data[1] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };

    // Resolve path to inode.
    let inode_num = match ext2.resolve_path(path) {
        Ok(n) => n,
        Err(errno) => return (errno, 0),
    };

    // Read the inode to verify it's a regular file and get file size.
    let inode = match ext2.read_inode(inode_num) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };

    if !inode.is_regular() {
        // Only regular files for this slice.
        return (NEG_EINVAL, 0);
    }

    let file_size = inode.size;

    // Allocate a handle.
    let handle = match handles.alloc(inode_num, file_size) {
        Some(h) => h,
        None => return (NEG_ENFILE, 0),
    };

    // Reply: label=0, data[0] packs the handle in the low 32 bits and the
    // file size (clamped to u32::MAX) in the high 32 bits. The kernel
    // unpacks both fields to seed its FdBackend::VfsService entry — see
    // kernel_core::fs::vfs_protocol::VFS_OPEN for the canonical contract.
    let packed = handle | ((file_size as u64) << 32);
    (0, packed)
}

// ---------------------------------------------------------------------------
// VFS_READ
// ---------------------------------------------------------------------------

fn handle_read(
    ext2: &Ext2State,
    handles: &HandleTable,
    msg: &syscall_lib::IpcMessage,
) -> (u64, u64) {
    let handle_id = msg.data[0];
    let offset = msg.data[1] as usize;
    let max_bytes = (msg.data[2] as usize).min(MAX_BULK_BUF);

    let handle = match handles.get(handle_id) {
        Some(h) => h,
        None => return (NEG_EBADF, 0),
    };

    let inode = match ext2.read_inode(handle.inode_num) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };

    let data = match ext2.read_file_data(&inode, offset, max_bytes) {
        Ok(d) => d,
        Err(_) => return (NEG_EIO, 0),
    };

    let bytes_read = data.len();

    // Store read data as reply bulk. Propagate store failure as EIO so the
    // kernel doesn't see a "success + missing bulk" response.
    if bytes_read > 0 && syscall_lib::ipc_store_reply_bulk(&data) != 0 {
        return (NEG_EIO, 0);
    }

    (0, bytes_read as u64)
}

// ---------------------------------------------------------------------------
// VFS_CLOSE
// ---------------------------------------------------------------------------

fn handle_close(handles: &mut HandleTable, msg: &syscall_lib::IpcMessage) -> (u64, u64) {
    let handle_id = msg.data[0];
    if handles.free(handle_id) {
        (0, 0)
    } else {
        // Stale or unknown handle — reject cleanly so a racing refcount bug
        // on the kernel side cannot force-close a recycled slot.
        (NEG_EBADF, 0)
    }
}

fn handle_stat_path(
    ext2: &Ext2State,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    let path = match decode_path(recv_buf, msg.data[0] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };
    let inode_num = match ext2.resolve_path(path) {
        Ok(n) => n,
        Err(errno) => return (errno, 0),
    };
    let inode = match ext2.read_inode(inode_num) {
        Ok(inode) => inode,
        Err(_) => return (NEG_EIO, 0),
    };
    let mut stat = match encode_stat_header(ext2, inode_num, &inode) {
        Ok(stat) => stat.to_vec(),
        Err(errno) => return (errno, 0),
    };
    if inode.is_symlink() {
        let target = match ext2.read_symlink_target(&inode) {
            Ok(target) => target,
            Err(_) => return (NEG_EIO, 0),
        };
        stat.extend_from_slice(&target);
    }
    if syscall_lib::ipc_store_reply_bulk(&stat) != 0 {
        return (NEG_EIO, 0);
    }
    (0, 0)
}

fn handle_list_dir(ext2: &Ext2State, msg: &syscall_lib::IpcMessage, recv_buf: &[u8]) -> (u64, u64) {
    let path = match decode_path(recv_buf, msg.data[0] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };
    let offset = msg.data[1] as usize;
    let max_bytes = (msg.data[2] as usize).min(MAX_BULK_BUF);

    let inode_num = match ext2.resolve_path(path) {
        Ok(n) => n,
        Err(errno) => return (errno, 0),
    };
    let inode = match ext2.read_inode(inode_num) {
        Ok(inode) => inode,
        Err(_) => return (NEG_EIO, 0),
    };
    if !inode.is_dir() {
        return (NEG_ENOTDIR, 0);
    }

    let entries = match ext2.read_dir_entries(&inode) {
        Ok(entries) => entries,
        Err(errno) => return (errno, 0),
    };

    let mut out = Vec::new();
    let mut idx = offset;
    while idx < entries.len() {
        let (inode_num, name, d_type) = &entries[idx];
        let name_bytes = name.as_bytes();
        let reclen = (19 + name_bytes.len() + 1 + 7) & !7;
        if out.len() + reclen > max_bytes {
            if out.is_empty() {
                return (NEG_EINVAL, 0);
            }
            break;
        }
        let start = out.len();
        out.resize(start + reclen, 0);
        let d_ino = *inode_num as u64;
        let d_off = (idx + 1) as i64;
        out[start..start + 8].copy_from_slice(&d_ino.to_ne_bytes());
        out[start + 8..start + 16].copy_from_slice(&d_off.to_ne_bytes());
        out[start + 16..start + 18].copy_from_slice(&(reclen as u16).to_ne_bytes());
        out[start + 18] = *d_type;
        out[start + 19..start + 19 + name_bytes.len()].copy_from_slice(name_bytes);
        idx += 1;
    }

    if !out.is_empty() && syscall_lib::ipc_store_reply_bulk(&out) != 0 {
        return (NEG_EIO, 0);
    }
    let packed = (out.len() as u64) | ((idx as u64) << 32);
    (0, packed)
}

fn handle_access_path(
    ext2: &Ext2State,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    let path = match decode_path(recv_buf, msg.data[0] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };
    match ext2.resolve_path(path) {
        Ok(_) => (0, 0),
        Err(errno) => (errno, 0),
    }
}

fn handle_mount_policy(msg: &syscall_lib::IpcMessage, recv_buf: &[u8]) -> (u64, u64) {
    let target_len = msg.data[0] as usize;
    let fstype_len = msg.data[1] as usize;
    if target_len == 0 || fstype_len == 0 || target_len + fstype_len > recv_buf.len() {
        return (NEG_EINVAL, 0);
    }
    let target = match core::str::from_utf8(&recv_buf[..target_len]) {
        Ok(target) => target,
        Err(_) => return (NEG_EINVAL, 0),
    };
    let fstype = match core::str::from_utf8(&recv_buf[target_len..target_len + fstype_len]) {
        Ok(fstype) => fstype,
        Err(_) => return (NEG_EINVAL, 0),
    };
    match mount_policy_action(target, fstype) {
        Ok(action) => (0, action),
        Err(errno) => (errno, 0),
    }
}

fn handle_umount_policy(msg: &syscall_lib::IpcMessage, recv_buf: &[u8]) -> (u64, u64) {
    let target = match decode_path(recv_buf, msg.data[0] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };
    match umount_policy_action(target) {
        Ok(action) => (0, action),
        Err(errno) => (errno, 0),
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: PANIC\n");
    syscall_lib::exit(101)
}
