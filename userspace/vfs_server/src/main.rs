//! Userspace VFS service for m3OS (Phase 54, first slice).
//!
//! Owns read-only ext2 file I/O for the `/etc/` path class.  The kernel
//! intercepts `open("/etc/...", O_RDONLY)` and routes it here via IPC.
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

use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;
use kernel_core::fs::ext2::{
    Ext2BlockGroupDescriptor, Ext2DirEntry, Ext2Inode, Ext2Superblock, inode_block_group,
    inode_index_in_group,
};
use kernel_core::fs::mbr;
use kernel_core::fs::vfs_protocol::{VFS_CLOSE, VFS_MAX_READ, VFS_OPEN, VFS_READ};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: alloc error\n");
    syscall_lib::exit(99)
}

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
    /// `path` must start with "/". We walk from root inode (2).
    fn resolve_path(&self, path: &str) -> Result<u32, u64> {
        let path = path.strip_prefix('/').unwrap_or(path);
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
            let blocks_count = inode.size / self.block_size;
            while file_block <= blocks_count {
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
}

// ---------------------------------------------------------------------------
// Open handle table
// ---------------------------------------------------------------------------

/// Maximum concurrent open handles.
const MAX_HANDLES: usize = 32;

/// An open handle tracked by the server.
struct OpenHandle {
    inode_num: u32,
    file_size: u32,
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
            in_use: false,
        };
        HandleTable {
            handles: [EMPTY; MAX_HANDLES],
        }
    }

    fn alloc(&mut self, inode_num: u32, file_size: u32) -> Option<u64> {
        for (i, h) in self.handles.iter_mut().enumerate() {
            if !h.in_use {
                h.inode_num = inode_num;
                h.file_size = file_size;
                h.in_use = true;
                return Some(i as u64);
            }
        }
        None
    }

    fn get(&self, handle: u64) -> Option<&OpenHandle> {
        let idx = handle as usize;
        if idx < MAX_HANDLES && self.handles[idx].in_use {
            Some(&self.handles[idx])
        } else {
            None
        }
    }

    fn free(&mut self, handle: u64) {
        let idx = handle as usize;
        if idx < MAX_HANDLES {
            self.handles[idx].in_use = false;
        }
    }
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
        _ => (NEG_EINVAL, 0),
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
    let path_len = msg.data[1] as usize;
    if path_len == 0 || path_len > recv_buf.len() {
        return (NEG_EINVAL, 0);
    }

    let path = match core::str::from_utf8(&recv_buf[..path_len]) {
        Ok(s) => s,
        Err(_) => return (NEG_EINVAL, 0),
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

    // Reply: label=0 (success), data[0]=handle, data[1]=file_size.
    // (kernel picks up data[0] and data[1] from the reply Message)
    // Since ipc_reply only supports label+data0, we pack file_size into
    // a reply bulk payload (4 bytes, LE). The kernel extracts it.
    //
    // Actually, the kernel uses call_msg() which returns a full Message.
    // But our reply via ipc_reply(cap, label, data0) only sets label and
    // data[0]. The kernel reads reply.data[1] for file_size.
    //
    // Workaround: store file_size in the reply bulk (4 bytes).
    // The kernel can read it from data[0] packed:
    //   data[0] = handle | (file_size << 32)
    //
    // Simpler: pack handle in low 32 bits, file_size in high 32 bits of data0.
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

    // Store read data as reply bulk.
    if bytes_read > 0 {
        syscall_lib::ipc_store_reply_bulk(&data);
    }

    (0, bytes_read as u64)
}

// ---------------------------------------------------------------------------
// VFS_CLOSE
// ---------------------------------------------------------------------------

fn handle_close(handles: &mut HandleTable, msg: &syscall_lib::IpcMessage) -> (u64, u64) {
    let handle_id = msg.data[0];
    handles.free(handle_id);
    (0, 0)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: PANIC\n");
    syscall_lib::exit(101)
}
